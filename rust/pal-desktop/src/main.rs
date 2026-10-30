//! Test harness for the audio processor: writes each segment as WAV (16 kHz mono) plus JSON features.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pal_core::{Event, ProcessorConfig, Segment, SegmenterConfig};

#[cfg(feature = "bridge")]
mod bridge;
mod comment;

#[derive(Parser)]
#[command(about = "llm_pal audio processor harness")]
struct Cli {
    /// Silero VAD model.
    #[arg(long, global = true, default_value = "models/silero_vad.onnx")]
    model: PathBuf,
    /// Output directory for seg_NNNN.wav / .json.
    #[arg(long, global = true, default_value = "out")]
    out: PathBuf,
    /// Also publish each segment to this mq-bridge endpoint, e.g. '{"file": {"path": "out/segments.jsonl"}}'.
    #[cfg(feature = "bridge")]
    #[arg(long, global = true)]
    publish: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Segment the microphone until Ctrl-C.
    Listen {
        /// Part of the input device name.
        #[arg(long)]
        device: Option<String>,
        /// Pretend audio output is playing, so speech also reports a barge-in.
        #[arg(long)]
        simulate_output: bool,
    },
    /// Segment a WAV file.
    File { input: PathBuf },
    /// List input devices.
    Devices,
    /// Listen and answer as KITT (STT -> LLM -> TTS).
    Comment(comment::CommentArgs),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let sink = Sink {
        dir: cli.out.clone(),
        #[cfg(feature = "bridge")]
        bridge: cli.publish.as_deref().map(bridge::Bridge::new).transpose()?,
    };
    let config =
        |device| ProcessorConfig {
        vad_model: cli.model.clone(),
        segmenter: SegmenterConfig::default(),
        device,
        drop_echo: false,
    };
    match &cli.cmd {
        Cmd::Devices => pal_core::capture::input_device_names().iter().for_each(|n| println!("{n}")),
        Cmd::Comment(args) => comment::run(cli.model.clone(), args)?,
        Cmd::File { input } => {
            let (samples, rate) = read_wav(input)?;
            std::fs::create_dir_all(&cli.out)?;
            for event in pal_core::process_samples(&config(None), &samples, rate)? {
                sink.handle(event)?;
            }
        }
        Cmd::Listen { device, simulate_output } => {
            std::fs::create_dir_all(&cli.out)?;
            let (processor, events) = pal_core::Processor::start(&config(device.clone()))?;
            processor.output_active().store(*simulate_output, Ordering::Relaxed);
            eprintln!("listening on '{}' at {} Hz, Ctrl-C to stop", processor.device_name(), processor.sample_rate());
            let mut overruns = 0;
            loop {
                if let Ok(event) = events.recv_timeout(Duration::from_secs(1)) {
                    sink.handle(event)?;
                }
                let now = processor.overruns();
                if now != overruns {
                    eprintln!("warning: {} samples dropped", now - overruns);
                    overruns = now;
                }
            }
        }
    }
    Ok(())
}

struct Sink {
    dir: PathBuf,
    #[cfg(feature = "bridge")]
    bridge: Option<bridge::Bridge>,
}

impl Sink {
    fn handle(&self, event: Event) -> Result<()> {
        match event {
            Event::SpeechStart { at_ms } => println!("{at_ms:>8} ms  speech start"),
            Event::BargeIn { at_ms } => println!("{at_ms:>8} ms  BARGE-IN"),
            Event::Pause(seg) => println!("{:>8} ms  pause", seg.end_ms),
            Event::Resume => println!("            resume"),
            Event::Segment(seg) => {
                let f = &seg.features;
                let hz = |v: Option<f32>| v.map_or("-".into(), |v| format!("{v:.0}"));
                println!(
                    "{:>8} ms  segment {} ({:.2} s{}) f0 {} [{}..{}] Hz, slope {} st/s, {:.1} dB, {:.1} syl/s",
                    seg.end_ms,
                    seg.id,
                    f.duration_s,
                    if seg.forced_split { ", split" } else { "" },
                    hz(f.f0_median_hz),
                    hz(f.f0_min_hz),
                    hz(f.f0_max_hz),
                    f.f0_slope_st_per_s.map_or("-".into(), |s| format!("{s:+.1}")),
                    f.rms_mean_db,
                    f.syllable_rate,
                );
                let wav = write_segment(&self.dir, &seg)?;
                #[cfg(feature = "bridge")]
                if let Some(b) = &self.bridge {
                    b.publish(&seg, &wav)?;
                }
                #[cfg(not(feature = "bridge"))]
                let _ = wav;
            }
        }
        Ok(())
    }
}

/// Writes seg_NNNN.wav and .json; returns the WAV path.
fn write_segment(out: &Path, seg: &Segment) -> Result<PathBuf> {
    let base = out.join(format!("seg_{:04}", seg.id));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(base.with_extension("wav"), spec)?;
    for s in &seg.pcm {
        wav.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    wav.finalize()?;
    std::fs::write(base.with_extension("json"), serde_json::to_string_pretty(seg)?)?;
    Ok(base.with_extension("wav"))
}

/// Mono f32 samples and the sample rate; channels are averaged.
fn read_wav(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 / scale)).collect::<Result<_, _>>()?
        }
    };
    let ch = spec.channels.max(1) as usize;
    let mono = interleaved.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    Ok((mono, spec.sample_rate))
}
