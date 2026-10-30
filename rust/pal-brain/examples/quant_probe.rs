//! Temporary: prefill and generation speed of GGUF files. Usage: quant_probe <model.gguf>...
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use pal_brain::llm::{Llm, LlmConfig, LocalLlm};
use pal_brain::persona::Persona;

fn main() {
    let persona = Persona::kitt();
    for path in std::env::args().skip(1) {
        let llm = LocalLlm::new(path.as_ref(), LlmConfig { max_tokens: 48, ..LlmConfig::default() }).unwrap();
        for run in 0..3 {
            let mut session = llm.session().unwrap();
            let t = Instant::now();
            session.prefill(&persona.context()).unwrap();
            let (n_ctx, ctx_s) = (session.last_prefill(), t.elapsed().as_secs_f64());
            let t = Instant::now();
            let (mut first, mut n) = (None, 0);
            let msgs = persona.messages("Erzähl mir was über Motoröl und warum du es so gern trinkst.");
            session
                .reply(&msgs, &AtomicBool::new(false), &mut |_| {
                    n += 1;
                    first.get_or_insert(t.elapsed());
                })
                .unwrap();
            let first = first.unwrap().as_secs_f64();
            let decode = t.elapsed().as_secs_f64() - first;
            println!(
                "{path} run {run}: prefill {n_ctx} tok {:.0} tok/s | first piece {first:.2}s | gen {:.1} tok/s",
                n_ctx as f64 / ctx_s,
                (n - 1) as f64 / decode
            );
        }
    }
}
