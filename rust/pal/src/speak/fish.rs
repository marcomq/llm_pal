//! Optional voice from Fish Audio, with the user's own API key, voice ID and model. The sentence's text goes to
//! Fish; Pocket TTS says a sentence when Fish fails on it.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

const URL: &str = "https://api.fish.audio/v1/tts";
const STALL: Duration = Duration::from_secs(5);
/// After a failure Pocket speaks for this long, so a dead server doesn't delay every sentence.
const REST: Duration = Duration::from_secs(60);
/// How often a waiting sentence looks whether it was cut off.
const WATCH: Duration = Duration::from_millis(20);
/// 100 ms of 16-bit audio at 24 kHz.
const READ_BYTES: usize = 4800;

type Piece = Result<Vec<f32>, String>;

#[derive(Clone)]
pub struct FishConfig {
    pub api_key: String,
    pub voice_id: String,
    /// Always sent: Fish answers an unknown or missing model with a paid one.
    pub model: String,
}

pub struct Fish {
    config: FishConfig,
    url: String,
    agent: ureq::Agent,
    rest_until: Option<Instant>,
}

impl Fish {
    pub fn new(config: FishConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_resolve(Some(STALL))
            .timeout_connect(Some(STALL))
            .timeout_recv_response(Some(STALL))
            .timeout_per_read(Some(STALL))
            .build()
            .into();
        Self { config, url: URL.into(), agent, rest_until: None }
    }

    /// Streams `text` to `play`. False if Fish failed on it: then Pocket should say the whole sentence. The request
    /// runs on a thread of its own, so a cut ends the wait at once.
    pub fn say<E>(
        &mut self,
        text: &str,
        rate: u32,
        cut: &AtomicBool,
        play: &mut impl FnMut(&[f32]) -> Result<(), E>,
    ) -> bool {
        if self.rest_until.is_some_and(|until| Instant::now() < until) {
            return false;
        }
        let (tx, rx) = channel();
        let (agent, url, config, text) = (self.agent.clone(), self.url.clone(), self.config.clone(), text.to_string());
        std::thread::spawn(move || {
            if let Err(e) = stream(&agent, &url, &config, &text, rate, &tx) {
                let _ = tx.send(Err(e.to_string()));
            }
        });
        let mut started = false;
        let error = loop {
            if cut.load(Ordering::SeqCst) {
                return true;
            }
            match rx.recv_timeout(WATCH) {
                Ok(Ok(pcm)) => {
                    started = true;
                    if play(&pcm).is_err() {
                        return true;
                    }
                }
                Ok(Err(e)) => break e,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) if started => return true,
                Err(RecvTimeoutError::Disconnected) => break "no audio".to_string(),
            }
        };
        eprintln!("llm_pal: fish audio: {error}");
        self.rest_until = Some(Instant::now() + REST);
        false
    }
}

fn stream(
    agent: &ureq::Agent,
    url: &str,
    config: &FishConfig,
    text: &str,
    rate: u32,
    pieces: &Sender<Piece>,
) -> Result<(), ureq::Error> {
    let body = serde_json::json!({
        "text": text,
        "reference_id": config.voice_id,
        "format": "pcm",
        "sample_rate": rate,
        "latency": "low",
    })
    .to_string();
    let response = agent
        .post(url)
        .header("authorization", format!("Bearer {}", config.api_key))
        .header("content-type", "application/json")
        .header("model", &config.model)
        .send(body.as_bytes())?;
    let mut reader = response.into_body().into_reader();
    let mut bytes = [0; READ_BYTES];
    let mut odd = None;
    loop {
        let n = reader.read(&mut bytes)?;
        if n == 0 {
            return Ok(());
        }
        let pcm = samples(&mut odd, &bytes[..n]);
        if !pcm.is_empty() && pieces.send(Ok(pcm)).is_err() {
            return Ok(());
        }
    }
}

/// 16-bit little-endian samples; a byte left over at the end of a read waits in `odd` for the next one.
fn samples(odd: &mut Option<u8>, bytes: &[u8]) -> Vec<f32> {
    let mut all = Vec::with_capacity(bytes.len() + 1);
    all.extend(odd.take());
    all.extend_from_slice(bytes);
    let pairs = all.chunks_exact(2);
    *odd = pairs.remainder().first().copied();
    pairs.map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// A local server that reads one request, sends `reply` and then holds the connection for `hold`.
    fn fish(reply: &'static [u8], hold: Duration) -> Fish {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut bytes = [0; 1024];
            while !request.ends_with(b"}") {
                let n = socket.read(&mut bytes).unwrap();
                request.extend_from_slice(&bytes[..n]);
            }
            socket.write_all(reply).unwrap();
            std::thread::sleep(hold);
        });
        let config = FishConfig { api_key: "key".into(), voice_id: "voice".into(), model: "model".into() };
        Fish { url, ..Fish::new(config) }
    }

    fn say(fish: &mut Fish, cut: &AtomicBool) -> (bool, usize) {
        let mut heard = 0;
        let spoken = fish.say("Hallo.", 24000, cut, &mut |pcm| {
            heard += pcm.len();
            Ok::<(), ()>(())
        });
        (spoken, heard)
    }

    #[test]
    fn a_sample_split_between_reads_stays_whole() {
        let mut odd = None;
        assert_eq!(samples(&mut odd, &[0x00, 0x40, 0x00]), [0.5]);
        assert_eq!(samples(&mut odd, &[0xC0]), [-0.5]);
        assert_eq!(odd, None);
    }

    #[test]
    fn a_whole_reply_is_played() {
        let mut fish = fish(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n\x00\x40\x00\xC0", Duration::ZERO);
        assert_eq!(say(&mut fish, &AtomicBool::new(false)), (true, 2));
        assert!(fish.rest_until.is_none());
    }

    #[test]
    fn a_reply_without_audio_leaves_the_sentence_to_pocket_and_rests() {
        let mut fish = fish(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", Duration::ZERO);
        assert_eq!(say(&mut fish, &AtomicBool::new(false)), (false, 0));
        assert!(fish.rest_until.is_some());
    }

    #[test]
    fn a_reply_that_breaks_off_leaves_the_sentence_to_pocket() {
        let mut fish = fish(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n\x00\x40\x00\xC0", Duration::ZERO);
        assert_eq!(say(&mut fish, &AtomicBool::new(false)), (false, 2));
        assert!(fish.rest_until.is_some());
    }

    #[test]
    fn a_cut_ends_the_wait_for_a_silent_server() {
        let mut fish = fish(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n", Duration::from_secs(3));
        let cut = std::sync::Arc::new(AtomicBool::new(false));
        let setter = cut.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            setter.store(true, Ordering::SeqCst);
        });
        let t = Instant::now();
        assert_eq!(say(&mut fish, &cut), (true, 0));
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(fish.rest_until.is_none());
    }
}
