//! Chat replies from a server, streamed: Ollama and OpenAI in the OpenAI chat format, Anthropic in its Messages
//! format. Blocking, so it runs on the reply route's model thread like the local model.

use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::llm::{Llm, Message, Role};
use crate::{Error, Result};

const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Anthropic requires a limit; it also covers the model's thinking, so it is far above a spoken reply.
const ANTHROPIC_MAX_TOKENS: u32 = 1024;
const ERROR_BODY_CHARS: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Ollama,
    OpenAi,
    Anthropic,
}

impl Provider {
    /// "ollama", "openai" or "anthropic".
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "ollama" => Some(Self::Ollama),
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Self::Ollama => "http://localhost:11434/v1",
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub provider: Provider,
    /// The API base, such as "http://192.168.1.10:11434/v1"; empty for [`Provider::default_url`].
    pub url: String,
    pub model: String,
    /// Empty for none, as for Ollama.
    pub api_key: String,
}

pub struct RemoteLlm {
    cfg: RemoteConfig,
    agent: ureq::Agent,
}

impl RemoteLlm {
    pub fn new(cfg: RemoteConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self { cfg, agent }
    }

    fn url(&self, path: &str) -> String {
        let base = if self.cfg.url.is_empty() { self.cfg.provider.default_url() } else { &self.cfg.url };
        format!("{}/{path}", base.trim_end_matches('/'))
    }

    fn send(&self, messages: &[Message]) -> Result<ureq::Body> {
        let (provider, model, key) = (self.cfg.provider, &self.cfg.model, &self.cfg.api_key);
        let request = match provider {
            Provider::Anthropic => self
                .agent
                .post(self.url("messages"))
                .header("x-api-key", key)
                .header("anthropic-version", ANTHROPIC_VERSION),
            _ => {
                let request = self.agent.post(self.url("chat/completions"));
                if key.is_empty() { request } else { request.header("authorization", format!("Bearer {key}")) }
            }
        };
        let body = request_body(provider, model, messages);
        let response = request
            .header("content-type", "application/json")
            .send(body.to_string())
            .map_err(|e| Error::Llm(format!("{}: {e}", self.url(""))))?;
        let status = response.status();
        let mut body = response.into_body();
        if !status.is_success() {
            let text = body.read_to_string().unwrap_or_default();
            return Err(Error::Llm(format!("{status}: {}", error_message(&text))));
        }
        Ok(body)
    }
}

impl Llm for RemoteLlm {
    fn reply(&mut self, messages: &[Message], cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<String> {
        let body = self.send(messages)?;
        read_stream(self.cfg.provider, body.into_reader(), cancel, on_text)
    }
}

fn request_body(provider: Provider, model: &str, messages: &[Message]) -> Value {
    match provider {
        // Thinking would delay the first word; low effort keeps it short.
        Provider::Anthropic => {
            let system: Vec<&str> =
                messages.iter().filter(|m| m.role == Role::System).map(|m| m.content.as_str()).collect();
            let turns: Vec<&Message> = messages.iter().filter(|m| m.role != Role::System).collect();
            json!({
                "model": model,
                "max_tokens": ANTHROPIC_MAX_TOKENS,
                "system": system.join("\n\n"),
                "messages": turns,
                "output_config": {"effort": "low"},
                "stream": true,
            })
        }
        // Same sampling as the local model; OpenAI's reasoning models reject a temperature, so it keeps its default.
        Provider::Ollama => json!({"model": model, "messages": messages, "temperature": 0.5, "stream": true}),
        Provider::OpenAi => json!({"model": model, "messages": messages, "stream": true}),
    }
}

/// Reads server-sent events until the stream ends or `cancel` is set, passing each text piece on.
fn read_stream(
    provider: Provider,
    body: impl Read,
    cancel: &AtomicBool,
    on_text: &mut dyn FnMut(&str),
) -> Result<String> {
    let mut reply = String::new();
    for line in BufReader::new(body).lines() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let line = line.map_err(|e| Error::Llm(e.to_string()))?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        if data == "[DONE]" {
            break;
        }
        let event: Value = serde_json::from_str(data).map_err(|e| Error::Llm(format!("{e}: {data}")))?;
        match piece(provider, &event)? {
            Piece::Text(text) if !text.is_empty() => {
                on_text(text);
                reply.push_str(text);
            }
            Piece::End => break,
            _ => {}
        }
    }
    Ok(reply)
}

enum Piece<'a> {
    Text(&'a str),
    Other,
    End,
}

fn piece(provider: Provider, event: &Value) -> Result<Piece<'_>> {
    if let Some(error) = event.get("error") {
        let message = error.get("message").and_then(Value::as_str).unwrap_or("unknown error");
        return Err(Error::Llm(message.to_string()));
    }
    let text = match provider {
        Provider::Anthropic => match event["type"].as_str() {
            Some("message_stop") => return Ok(Piece::End),
            Some("content_block_delta") if event["delta"]["type"] == "text_delta" => event["delta"]["text"].as_str(),
            _ => None,
        },
        _ => event["choices"][0]["delta"]["content"].as_str(),
    };
    Ok(text.map_or(Piece::Other, Piece::Text))
}

/// The server's error message from a JSON error body, else the start of the body.
fn error_message(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed.as_ref().and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()));
    message.map_or_else(|| body.chars().take(ERROR_BODY_CHARS).collect(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(provider: Provider, sse: &str) -> Result<(String, Vec<String>)> {
        let mut pieces = Vec::new();
        let reply = read_stream(provider, sse.as_bytes(), &AtomicBool::new(false), &mut |t| pieces.push(t.into()))?;
        Ok((reply, pieces))
    }

    #[test]
    fn reads_openai_stream() {
        let sse = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"Hallo\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\" Michael.\"}}]}\n\n\
                   data: [DONE]\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n";
        let (reply, pieces) = stream(Provider::Ollama, sse).unwrap();
        assert_eq!(reply, "Hallo Michael.");
        assert_eq!(pieces, ["Hallo", " Michael."]);
    }

    #[test]
    fn reads_anthropic_stream_without_thinking() {
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n\
                   event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hm\"}}\n\n\
                   event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
                   event: ping\ndata: {\"type\":\"ping\"}\n\n\
                   event: message_stop\ndata: {\"type\":\"message_stop\"}\n";
        let (reply, _) = stream(Provider::Anthropic, sse).unwrap();
        assert_eq!(reply, "Hello");
    }

    #[test]
    fn stream_errors_are_reported() {
        let sse = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n";
        let e = stream(Provider::Anthropic, sse).unwrap_err();
        assert!(e.to_string().contains("Overloaded"), "{e}");
    }

    #[test]
    fn cancel_stops_reading() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n";
        let reply = read_stream(Provider::OpenAi, sse.as_bytes(), &AtomicBool::new(true), &mut |_| {}).unwrap();
        assert!(reply.is_empty());
    }

    #[test]
    fn anthropic_gets_the_system_prompt_apart() {
        let messages = [Message::new(Role::System, "Be KITT."), Message::new(Role::User, "Hi")];
        let body = request_body(Provider::Anthropic, "claude-opus-5-5", &messages);
        assert_eq!(body["system"], "Be KITT.");
        assert_eq!(body["messages"], json!([{"role": "user", "content": "Hi"}]));
    }

    /// Answers one request with `response` and hands back what was requested.
    fn serve_once(response: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = vec![0; 8192];
            let n = socket.read(&mut request).unwrap();
            socket.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        (url, server)
    }

    fn remote(provider: Provider, url: String) -> RemoteLlm {
        RemoteLlm::new(RemoteConfig { provider, url, model: "m".into(), api_key: "secret".into() })
    }

    #[test]
    fn streams_a_reply_over_http() {
        let (url, server) = serve_once(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n\n",
        );
        let reply = remote(Provider::OpenAi, url).reply(
            &[Message::new(Role::User, "Hello")],
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert_eq!(reply.unwrap(), "Hi");
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/chat/completions"), "{request}");
        assert!(request.contains("authorization: Bearer secret"), "{request}");
    }

    #[test]
    fn http_errors_carry_the_server_message() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let (url, server) = serve_once(Box::leak(response.into_boxed_str()));
        let e = remote(Provider::Anthropic, url).reply(&[], &AtomicBool::new(false), &mut |_| {}).unwrap_err();
        assert!(e.to_string().contains("401") && e.to_string().contains("invalid x-api-key"), "{e}");
        let request = server.join().unwrap();
        assert!(request.contains("x-api-key: secret") && request.contains("anthropic-version"), "{request}");
    }

    #[test]
    fn error_bodies_are_shortened_to_their_message() {
        assert_eq!(error_message(r#"{"error":{"message":"invalid x-api-key"}}"#), "invalid x-api-key");
        assert_eq!(error_message(r#"{"error":"model not found"}"#), "model not found");
        assert_eq!(error_message("plain"), "plain");
    }
}
