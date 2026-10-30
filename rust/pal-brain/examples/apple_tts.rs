//! Speaks two sentences and stops the second at once; prints the latencies.
//! `cargo run -p pal-brain --example apple_tts` (macOS only).

#[cfg(target_os = "macos")]
fn main() {
    use std::time::{Duration, Instant};

    use pal_brain::apple_tts::{AppleTts, run_main_loop};
    use pal_brain::speaker::Speaker;

    std::thread::spawn(|| {
        let t = Instant::now();
        let mut tts = AppleTts::new("Anna", 1.0).unwrap();
        println!("warm-up {} ms", t.elapsed().as_millis());
        let t = Instant::now();
        tts.say("Na, das war ja wieder ein Satz, der ziemlich lange dauert.").unwrap();
        tts.say("Dieser Satz sollte nie zu hören sein.").unwrap();
        while tts.level() < 0.01 {
            assert!(t.elapsed() < Duration::from_secs(2), "no sound after 2 s");
            std::thread::sleep(Duration::from_millis(5));
        }
        println!("first sound after {} ms", t.elapsed().as_millis());
        std::thread::sleep(Duration::from_millis(800));
        let t = Instant::now();
        tts.stop();
        while tts.speaking() {
            assert!(t.elapsed() < Duration::from_millis(500), "still speaking");
            std::thread::sleep(Duration::from_millis(5));
        }
        println!("stopped after {} ms", t.elapsed().as_millis());
        std::process::exit(0);
    });
    run_main_loop();
}

#[cfg(not(target_os = "macos"))]
fn main() {}
