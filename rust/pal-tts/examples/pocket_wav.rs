//! Writes one sentence as WAV:
//! `cargo run --release -p pal-tts --features download --example pocket_wav -- out.wav [language] [voice] [text]`.
//! With `PAL_TTS_DIR` set, the model files come from that folder instead of Hugging Face.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() -> pal_tts::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "pocket.wav".into());
    let language = args.next().unwrap_or_else(|| "english".into());
    let voice = args.next().unwrap_or_else(|| "juergen".into());
    let text = args.next().unwrap_or_else(|| {
        "Good morning. I have checked your system: everything runs smoothly. \
         Shall I turn on the lights in the living room?"
            .into()
    });

    if let Some(dir) = std::env::var_os("PAL_TTS_DIR") {
        pal_tts::use_local_files(dir.into());
    }
    let t = Instant::now();
    let tts = pal_tts::PocketTts::load(&language, &voice)?;
    println!("loaded in {:.2}s", t.elapsed().as_secs_f32());

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: tts.sample_rate(),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(&out, spec)?;
    let mut samples = 0usize;
    let t = Instant::now();
    tts.synthesize(&text, &AtomicBool::new(false), &mut |pcm| {
        samples += pcm.len();
        pcm.iter().try_for_each(|&s| wav.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16))?;
        Ok(())
    })?;
    wav.finalize()?;
    let audio = samples as f32 / tts.sample_rate() as f32;
    let took = t.elapsed().as_secs_f32();
    println!("{audio:.1}s audio in {took:.2}s ({:.1}x real time) -> {out}", audio / took);
    #[cfg(target_os = "macos")]
    std::process::Command::new("afplay").arg(&out).status()?;
    Ok(())
}
