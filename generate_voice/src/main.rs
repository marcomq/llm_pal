//! Clones a voice from a short recording into a Pocket TTS voice state:
//! `pal-voice <german|english> <sample.wav> <voice.safetensors> [probe.wav] [text]`.
//! Reloads the written state the way Pal does and speaks a probe sentence with it.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use pal_tts::{PocketTts, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [language, sample, out, rest @ ..] = args.as_slice() else {
        eprintln!("usage: pal-voice <german|english> <sample.wav> <voice.safetensors> [probe.wav] [text]");
        std::process::exit(2);
    };
    if !out.ends_with(".safetensors") {
        eprintln!("the voice file must end in .safetensors: {out}");
        std::process::exit(2);
    }
    let probe = rest.first().cloned().unwrap_or_else(|| out.replace(".safetensors", ".wav"));
    let text = rest.get(1).cloned().unwrap_or_else(|| default_text(language).into());

    let t = Instant::now();
    PocketTts::load(language, sample)?.export_voice(Path::new(out))?;
    println!("cloned in {:.1}s -> {out}", t.elapsed().as_secs_f32());

    let tts = PocketTts::load(language, out)?;
    let t = Instant::now();
    let seconds = speak(&tts, &text, &probe)?;
    let took = t.elapsed().as_secs_f32();
    println!("probe {seconds:.1}s in {took:.1}s ({:.1}x real time) -> {probe}", seconds / took);
    Ok(())
}

fn default_text(language: &str) -> &'static str {
    match language {
        "german" => "Guten Abend. Das ist eine kurze Probe meiner Stimme. Wie klinge ich?",
        _ => "Good evening. This is a short test of my voice. How do I sound?",
    }
}

/// Returns the seconds of audio written.
fn speak(tts: &PocketTts, text: &str, path: &str) -> Result<f32> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: tts.sample_rate(),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(path, spec)?;
    let mut samples = 0usize;
    tts.synthesize(text, &AtomicBool::new(false), &mut |pcm| {
        samples += pcm.len();
        pcm.iter().try_for_each(|&s| wav.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16))?;
        Ok(())
    })?;
    wav.finalize()?;
    Ok(samples as f32 / tts.sample_rate() as f32)
}
