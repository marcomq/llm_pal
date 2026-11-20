//! Chat replies from a server through mq-bridge: Ollama and OpenAI with `openai_chat`, Anthropic's Messages API
//! with the streaming `http_bulk` recipe. Each text piece goes to `stream_to` as it arrives; a message on
//! `cancel_from` with the turn as `correlation_id` stops the reply.

use mq_bridge::models::Endpoint;
use serde_json::json;

use crate::think::llm::{Message, Role};
use crate::{Error, Result};

const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Anthropic requires a limit; it also covers the model's thinking, so it is far above a spoken reply.
const ANTHROPIC_MAX_TOKENS: u32 = 1024;
/// Longest silence of the server. Ollama may load the model first; after that, a reply may take long.
const SILENCE_MS: u64 = 30_000;

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

impl RemoteConfig {
    fn url(&self) -> &str {
        if self.url.is_empty() { self.provider.default_url() } else { &self.url }
    }

    /// The endpoint that answers a [`RemoteConfig::payload`], streaming its text to `stream_to`.
    pub fn endpoint(&self, stream_to: &Endpoint, cancel_from: &Endpoint) -> Result<Endpoint> {
        // mq-bridge's built-in Mozilla roots, the same on every platform (Android has no OS store reqwest can use).
        let tls = json!({"ca_file": "$.bundled"});
        let endpoint = match self.provider {
            Provider::Anthropic => json!({"http_bulk": {
                "url": self.url(),
                "tls": tls,
                "headers": {"x-api-key": self.api_key, "anthropic-version": ANTHROPIC_VERSION},
                "request_timeout_ms": SILENCE_MS,
                "query": {
                    "path": "/messages",
                    "format": "json_array",
                    "per_message": true,
                    "request": anthropic_request(&self.model),
                    "value": "/delta/text",
                    "stop_reason": "/delta/stop_reason",
                    "error": "/error/message",
                    "stream_to": stream_to,
                    "cancel_from": cancel_from,
                },
            }}),
            provider => json!({"openai_chat": {
                "url": self.url(),
                "tls": tls,
                "api_key": (!self.api_key.is_empty()).then_some(&self.api_key),
                "model": self.model,
                "messages_from_payload": true,
                // Same sampling as the local model; OpenAI's reasoning models reject a temperature.
                "options": (provider == Provider::Ollama).then(|| json!({"temperature": 0.5})),
                "request_timeout_ms": SILENCE_MS,
                "stream_to": stream_to,
                "cancel_from": cancel_from,
            }}),
        };
        serde_json::from_value(endpoint).map_err(|e| Error::Llm(format!("server endpoint: {e}")))
    }

    /// The request's payload: the messages, for Anthropic with the system prompt apart.
    pub fn payload(&self, messages: &[Message]) -> String {
        match self.provider {
            Provider::Anthropic => {
                let system: Vec<&str> =
                    messages.iter().filter(|m| m.role == Role::System).map(|m| m.content.as_str()).collect();
                let turns: Vec<&Message> = messages.iter().filter(|m| m.role != Role::System).collect();
                json!({"system": system.join("\n\n"), "messages": turns}).to_string()
            }
            _ => json!(messages).to_string(),
        }
    }
}

/// The Messages request around a payload. Thinking would delay the first word; low effort keeps it short.
fn anthropic_request(model: &str) -> String {
    let body = json!({
        "model": model,
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "system": "SYSTEM",
        "messages": "MESSAGES",
        "output_config": {"effort": "low"},
        "stream": true,
    });
    // Serializing escapes the literal text; the template escapes what the payload puts in.
    body.to_string().replacen(r#""system":"SYSTEM""#, r#""system":"${payload:system}""#, 1).replacen(
        r#""messages":"MESSAGES""#,
        r#""messages":${payload:messages | raw}"#,
        1,
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    use mq_bridge::models::MemoryConfig;
    use mq_bridge::{CanonicalMessage, Publisher, get_or_create_channel};
    use serde_json::Value;

    use super::*;

    /// Answers one request with `response`, or holds it unanswered when `None`, and hands back the request.
    fn serve_once(response: Option<String>) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let request = read_request(&mut socket);
            match response {
                Some(response) => socket.write_all(response.as_bytes()).unwrap(),
                None => std::thread::sleep(Duration::from_secs(5)),
            }
            request
        });
        (url, server)
    }

    /// Headers and the whole body (by `content-length`), however many reads they take.
    fn read_request(socket: &mut std::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        loop {
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let length = String::from_utf8_lossy(&request[..end])
                    .lines()
                    .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    return String::from_utf8_lossy(&request).into_owned();
                }
            }
            let n = socket.read(&mut buf).expect("the request within the read timeout");
            assert!(n > 0, "the client closed before the whole request");
            request.extend_from_slice(&buf[..n]);
        }
    }

    fn sse(events: &str) -> Option<String> {
        Some(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{events}"))
    }

    fn remote(provider: Provider, url: String) -> RemoteConfig {
        RemoteConfig { provider, url, model: "m".into(), api_key: "secret".into() }
    }

    fn meta<'a>(msg: &'a CanonicalMessage, key: &str) -> &'a str {
        msg.metadata.get(key).map_or("", String::as_str)
    }

    /// Asks as turn "t" on topics named after `name`; returns the answer and the token messages.
    fn ask(
        cfg: &RemoteConfig,
        messages: &[Message],
        name: &str,
        cancel_after: Option<Duration>,
    ) -> (mq_bridge::anyhow::Result<CanonicalMessage>, Vec<CanonicalMessage>) {
        let (tokens, cancels) = (format!("{name}.tokens"), format!("{name}.cancel"));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let answer = rt.block_on(async {
            let cancel_from = Endpoint::new_memory(&cancels, 16);
            let endpoint = cfg.endpoint(&Endpoint::new_memory(&tokens, 64), &cancel_from).unwrap();
            let server = Publisher::new(endpoint).await.unwrap();
            if let Some(after) = cancel_after {
                let cancel = Publisher::new(cancel_from).await.unwrap();
                let rt = tokio::runtime::Handle::current();
                std::thread::spawn(move || {
                    std::thread::sleep(after);
                    rt.block_on(cancel.send(CanonicalMessage::from("").with_metadata_kv("correlation_id", "t")))
                });
            }
            let request = CanonicalMessage::from(cfg.payload(messages)).with_metadata_kv("correlation_id", "t");
            server.request(request).await
        });
        (answer, get_or_create_channel(&MemoryConfig::new(tokens, None)).drain_messages())
    }

    fn body(request: &str) -> Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    #[test]
    fn openai_streams_pieces_and_ends_complete() {
        let (url, server) = serve_once(sse("data: {\"choices\":[{\"delta\":{\"content\":\"Hallo\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\" Michael.\"},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n"));
        let messages = [Message::new(Role::System, "Be KITT."), Message::new(Role::User, "Hi")];
        let (answer, tokens) = ask(&remote(Provider::OpenAi, url), &messages, "test.openai", None);
        assert_eq!(answer.unwrap().get_payload_str(), "\"Hallo Michael.\"");
        let texts: Vec<_> = tokens.iter().map(|t| t.get_payload_str().to_string()).collect();
        assert_eq!(texts, ["Hallo", " Michael.", ""]);
        let last = tokens.last().unwrap();
        assert_eq!((meta(last, "correlation_id"), meta(last, "mqb.stream.status")), ("t", "complete"));
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/chat/completions"), "{request}");
        assert!(request.to_lowercase().contains("authorization: bearer secret"), "{request}");
        assert_eq!(body(&request)["messages"][0], json!({"role": "system", "content": "Be KITT."}));
    }

    #[test]
    fn anthropic_gets_the_system_prompt_apart() {
        let (url, server) = serve_once(sse(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hm\"}}\n\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
             event: ping\ndata: {\"type\":\"ping\"}\n\n\
             event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n\
             event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ));
        let messages = [Message::new(Role::System, "Be \"KITT\"."), Message::new(Role::User, "Hi")];
        let (answer, _) = ask(&remote(Provider::Anthropic, url), &messages, "test.anthropic", None);
        assert_eq!(answer.unwrap().get_payload_str(), "\"Hello\"");
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/messages"), "{request}");
        assert!(request.contains("x-api-key: secret") && request.contains("anthropic-version"), "{request}");
        let body = body(&request);
        assert_eq!(body["system"], "Be \"KITT\".");
        assert_eq!(body["messages"], json!([{"role": "user", "content": "Hi"}]));
        assert_eq!((&body["model"], &body["stream"]), (&json!("m"), &json!(true)));
    }

    #[test]
    fn a_refused_request_ends_with_the_server_message_before_any_text() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let (url, _server) = serve_once(Some(response));
        let (answer, tokens) = ask(&remote(Provider::Anthropic, url), &[], "test.refused", None);
        assert!(answer.is_err());
        let [last] = tokens.as_slice() else { panic!("{tokens:?}") };
        assert_eq!((meta(last, "mqb.stream.status"), meta(last, "mqb.stream.index")), ("error", "0"));
        let error = last.get_payload_str();
        assert!(error.contains("401") && error.contains("invalid x-api-key"), "{error}");
    }

    #[test]
    fn a_cancel_stops_a_silent_server() {
        let (url, _server) = serve_once(None);
        let started = Instant::now();
        let (answer, tokens) =
            ask(&remote(Provider::Ollama, url), &[], "test.cancel", Some(Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert_eq!(meta(&answer.unwrap(), "mqb.stream.status"), "cancelled");
        assert_eq!(meta(tokens.last().unwrap(), "mqb.stream.status"), "cancelled");
    }
}
