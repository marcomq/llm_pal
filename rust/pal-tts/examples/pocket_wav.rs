//! Writes one sentence as WAV: `cargo run --release -p pal-tts --example pocket_wav -- out.wav [voice] [text]`.
//! With `PAL_TTS_DIR` set, the model files come from that folder instead of Hugging Face.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() -> pal_tts::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "pocket.wav".into());
    let voice = args.next().unwrap_or_else(|| "juergen".into());
    let text = args.next().unwrap_or_else(|| {
        "Guten Morgen, Marco. Ich habe dein System überprüft: Alles läuft einwandfrei. \
         Möchtest du, dass ich die Lichter im Wohnzimmer einschalte?"
            .into()
    });

    if let Some(dir) = std::env::var_os("PAL_TTS_DIR") {
        pal_tts::use_local_files(dir.into());
    }
    let t = Instant::now();
    let tts = pal_tts::PocketTts::load("german", &voice)?;
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
