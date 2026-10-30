//! `comment`: microphone -> segment -> STT -> LLM -> sentences -> TTS, with latencies per turn.

use std::path::PathBuf;

use anyhow::Result;
use pal_brain::agent::{Agent, AgentConfig, AgentEvent, Voice};
use pal_brain::language::Language;
use pal_brain::stt;

#[derive(clap::Args)]
pub struct CommentArgs {
    /// Part of the input device name.
    #[arg(long)]
    device: Option<String>,
    /// Speech during output stops it (use headphones). Default: speech during output is ignored.
    #[arg(long)]
    barge_in: bool,
    /// Print replies instead of speaking them.
    #[arg(long)]
    print: bool,
    /// Use the `say` command instead of in-process speech (seconds slower per sentence).
    #[arg(long)]
    say: bool,
    /// Kyutai Pocket TTS with this catalog voice, e.g. "juergen".
    #[arg(long)]
    pocket: Option<String>,
    /// macOS voice, see `say -v '?'`.
    #[arg(long, default_value = "Anna")]
    voice: String,
    /// Speaking rate as a factor of the voice's default.
    #[arg(long, default_value_t = 1.0)]
    rate: f32,
    #[arg(long, default_value = "models/ggml-base-q8_0.bin")]
    stt_model: PathBuf,
    #[arg(long, default_value = "models/Qwen3.5-0.8B-Q4_K_M.gguf")]
    llm_model: PathBuf,
}

/// On macOS the main thread runs the system run loop, which in-process speech needs.
pub fn run(vad_model: PathBuf, args: &CommentArgs) -> Result<()> {
    let agent = Agent::start(AgentConfig {
        vad_model,
        device: args.device.clone(),
        stt_library: stt::default_library(),
        stt_model: args.stt_model.clone(),
        llm_model: args.llm_model.clone(),
        voice: voice(args),
        barge_in: args.barge_in,
        language: Language::German,
    });
    let print = args.print;
    let printer = std::thread::spawn(move || {
        print_events(&agent, print);
        std::process::exit(1);
    });
    #[cfg(target_os = "macos")]
    {
        drop(printer);
        pal_brain::apple_tts::run_main_loop()
    }
    #[cfg(not(target_os = "macos"))]
    printer.join().map_err(|_| anyhow::anyhow!("printer thread panicked"))
}

/// Default rate of the `say` command.
const SAY_WPM: f32 = 175.0;

fn voice(args: &CommentArgs) -> Voice {
    if args.print {
        return Voice::Print;
    }
    if let Some(voice) = &args.pocket {
        return Voice::Pocket { voice: voice.clone() };
    }
    #[cfg(target_os = "macos")]
    if !args.say {
        return Voice::Apple { voice: args.voice.clone(), rate: args.rate };
    }
    Voice::Say { voice: args.voice.clone(), rate: Some((SAY_WPM * args.rate) as u32) }
}

/// Returns when the agent stops.
fn print_events(agent: &Agent, print: bool) {
    while let Some(event) = agent.next() {
        match event {
            AgentEvent::Ready { device } => eprintln!("listening on '{device}', Ctrl-C to stop"),
            AgentEvent::Heard(line) => println!("du:  {line}"),
            AgentEvent::Said(_) => {}
            AgentEvent::TurnDone { reply, stt_ms, first_sentence_ms, total_ms } => {
                if !print {
                    println!("pal: {reply}");
                }
                println!(
                    "     stt {stt_ms} ms, erster Satz {} ms, fertig {total_ms} ms nach Segment-Ende",
                    first_sentence_ms.unwrap_or(0)
                );
            }
            AgentEvent::Interrupted => println!("(unterbrochen)"),
            AgentEvent::Ignored => println!("(ignoriert: während der Ausgabe gesprochen)"),
            AgentEvent::Failed(e) => eprintln!("Error: {e}"),
        }
    }
}
