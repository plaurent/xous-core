//! Talks to a local ollama server over plain HTTP.
//!
//! Uses ollama's `/api/chat` endpoint with `"stream": false`, so a single POST
//! returns the whole assistant reply as one JSON document. The Xous `net` service
//! transparently backs `std::net::TcpStream`, so `ureq` "just works" with no
//! socket code of our own — and because the endpoint is plain HTTP on the LAN we
//! don't need the TLS trust connector that `apps/sidplayer` uses for HTTPS.
//!
//! See: https://github.com/ollama/ollama/blob/main/docs/api.md#generate-a-chat-completion

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// One message in the chat transcript, in ollama's wire format.
#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"user"`, `"assistant"`, or `"system"`.
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn user(text: &str) -> Self { ChatMessage { role: "user".into(), content: text.into() } }
    pub fn assistant(text: &str) -> Self {
        ChatMessage { role: "assistant".into(), content: text.into() }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    /// We request the complete reply in one shot; token streaming would need us to
    /// read the body incrementally and parse newline-delimited JSON instead.
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    /// The name to use as the `model` field, e.g. `llama3.2:latest`.
    name: String,
}

/// Query the ollama server for the list of locally-installed models
/// (`GET /api/tags`), returning their names sorted alphabetically.
///
/// Blocking; run it off the UI loop or accept a brief stall (it's a fast local
/// call, but a wrong/unreachable host can block up to the connect timeout).
pub fn list_models(config: &Config) -> Result<Vec<String>, String> {
    let url = config.tags_url();
    let agent = ureq::builder()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .build();

    match agent.get(&url).call() {
        Ok(resp) => match resp.into_json::<TagsResponse>() {
            Ok(t) => {
                let mut names: Vec<String> = t.models.into_iter().map(|m| m.name).collect();
                names.sort();
                Ok(names)
            }
            Err(e) => Err(format!("Bad response from ollama: {}", e)),
        },
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(format!("Server error {}: {}", code, body.trim()))
        }
        Err(ureq::Error::Transport(t)) => {
            Err(format!("Could not reach ollama at {}\n\n({})", url, t))
        }
    }
}

/// Send the conversation `messages` to ollama and return the assistant's reply
/// text, or a human-readable error string suitable for showing to the user.
///
/// This blocks (network round-trip + model inference), so callers run it on a
/// worker thread, not on the UI message loop.
pub fn chat(config: &Config, messages: &[ChatMessage]) -> Result<String, String> {
    let url = config.chat_url();
    let req = ChatRequest { model: config.model.trim(), messages, stream: false };

    // IMPORTANT: do NOT set a short `timeout_connect` here. In the Xous `net`
    // stack the connect timeout is passed straight to smoltcp's `set_timeout()`,
    // which is a *whole-connection inactivity abort*, not just a connect-phase
    // limit (services/net/src/std_tcpstream.rs). With `stream: false` the socket
    // is idle for the entire generation, so a 10s connect timeout would abort the
    // request mid-inference — the server finishes but we've already hung up, and
    // ollama logs `context canceled` / 500. We instead bound liveness with the
    // read timeout (a genuinely dead connection still errors out after this).
    let agent = ureq::builder().timeout_read(Duration::from_secs(300)).build();

    match agent.post(&url).send_json(&req) {
        Ok(resp) => match resp.into_json::<ChatResponse>() {
            Ok(body) => Ok(body.message.content),
            Err(e) => Err(format!("Bad response from ollama: {}", e)),
        },
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let hint = if code == 404 {
                format!(
                    "\n\nModel \"{}\" may not be pulled on the server. Run `ollama pull {}` \
                     there, or pick a model you have with F1.",
                    config.model.trim(),
                    config.model.trim()
                )
            } else {
                String::new()
            };
            Err(format!("Server error {}: {}{}", code, body.trim(), hint))
        }
        Err(ureq::Error::Transport(t)) => Err(format!(
            "Could not reach ollama at {}.\n\nCheck that:\n\
             • Wi-Fi is connected,\n\
             • the server address/port under F1 are correct,\n\
             • ollama is running and bound to 0.0.0.0 (OLLAMA_HOST=0.0.0.0),\n\
             • the server is reachable on your network.\n\n({})",
            url, t
        )),
    }
}
