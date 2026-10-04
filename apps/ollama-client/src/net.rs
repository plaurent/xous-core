//! Talks to an ollama server over HTTP or HTTPS.
//!
//! Uses ollama's `/api/chat` endpoint with `"stream": true`: the reply arrives as
//! newline-delimited JSON chunks of a few tokens each. Besides showing the text as
//! it's written, the steady trickle of data is how we know the connection is still
//! alive — with `"stream": false` the socket sits silent for the whole generation,
//! and a dropped connection looks exactly like a slow model. The Xous `net` service
//! transparently backs `std::net::TcpStream`, so `ureq` "just works" with no
//! socket code of our own.
//!
//! When TLS is enabled (`Config::use_tls`, e.g. a cloud host), the agent uses the
//! Xous trust-store connector (`tls::xtls::TlsConnector`). The Precursor ships
//! with no root CAs, so *every* HTTPS host — public-CA or self-signed alike — is
//! trusted on first use: [`ensure_trusted`] probes the server's certificate chain
//! and, if nothing offered is already trusted, prompts the user to save one to the
//! PDDB. This must run *before* the request: the connector's own retry only
//! re-probes (it never saves trust), so an untrusted handshake would otherwise
//! never succeed.
//!
//! See: https://github.com/ollama/ollama/blob/main/docs/api.md#generate-a-chat-completion

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tls::xtls::TlsConnector;
use ureq::Agent;

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

    pub fn assistant(text: &str) -> Self { ChatMessage { role: "assistant".into(), content: text.into() } }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    /// Always true: the reply streams back as newline-delimited JSON chunks.
    stream: bool,
}

/// One line of a streamed `/api/chat` reply.
#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    message: Option<StreamMessage>,
    /// Set on the final chunk.
    #[serde(default)]
    done: bool,
    /// ollama reports failures that happen mid-generation in-band.
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct StreamMessage {
    #[serde(default)]
    content: String,
    /// Reasoning models (qwen3, deepseek-r1…) stream their reasoning here before
    /// the answer. We don't show it, but it proves the model is working.
    #[serde(default)]
    thinking: String,
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

/// Build a ureq agent wired to the Xous TLS trust-store connector. The connector
/// is only exercised for `https://` URLs; plain-HTTP requests ignore it entirely,
/// so it's safe to attach unconditionally.
fn make_agent(read_timeout: Duration, connect_timeout: Option<Duration>) -> Agent {
    let mut builder = ureq::builder().tls_connector(Arc::new(TlsConnector {})).timeout_read(read_timeout);
    if let Some(ct) = connect_timeout {
        builder = builder.timeout_connect(ct);
    }
    builder.build()
}

/// When TLS is enabled, ensure the ollama host's certificate chain is trusted
/// before we connect. Probes the *configured* port (ollama can serve TLS on any
/// port, so 443 is never assumed) and, if nothing offered is already trusted,
/// pops the trust modal so the user can save a certificate to the PDDB. Once
/// trusted, the connector's handshake verifies against it on the actual request.
///
/// No-op for plain HTTP. Returns a user-facing error if the probe fails or the
/// user trusts nothing (so we don't then spin in the connector's retry loop).
fn ensure_trusted(config: &Config) -> Result<(), String> {
    if !config.use_tls {
        return Ok(());
    }
    let host = config.host.trim();
    let tls = tls::Tls::new();
    match tls.probe_port(host, config.port) {
        Ok(certs) if !certs.is_empty() => {
            if certs.iter().any(|c| tls.is_trusted_cert(c.clone())) {
                Ok(()) // already have a trusted anchor for this chain
            } else if tls.trust_modal(certs) > 0 {
                Ok(()) // user just trusted at least one certificate
            } else {
                Err(format!(
                    "No certificate trusted for {}:{}.\n\nHTTPS needs you to trust the \
                     server's certificate when the list is offered. Try again and trust \
                     the root (or, for a self-signed server, the offered certificate).",
                    host, config.port
                ))
            }
        }
        Ok(_) => Err(format!(
            "{}:{} offered no TLS certificate.\n\nIs the server really using HTTPS on that \
             port? If it's plain HTTP, turn HTTPS off under F1.",
            host, config.port
        )),
        Err(e) => Err(format!(
            "Couldn't check the TLS certificate for {}:{}.\n\nCommon causes:\n\
             • The device clock is unset (Precursor defaults to year 2000, which makes \
             valid certificates look \"not valid yet\") — set the time, then retry,\n\
             • the host is unreachable, or isn't speaking TLS on that port.\n\n({})",
            host, config.port, e
        )),
    }
}

/// Query the ollama server for the list of locally-installed models
/// (`GET /api/tags`), returning their names sorted alphabetically.
///
/// Blocking; run it off the UI loop or accept a brief stall (it's a fast local
/// call, but a wrong/unreachable host can block up to the connect timeout).
pub fn list_models(config: &Config) -> Result<Vec<String>, String> {
    ensure_trusted(config)?;
    let url = config.tags_url();
    let agent = make_agent(Duration::from_secs(30), Some(Duration::from_secs(10)));

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
        Err(ureq::Error::Transport(t)) => Err(format!("Could not reach ollama at {}\n\n({})", url, t)),
    }
}

/// Quick liveness check: can we reach the ollama server at all right now?
/// Used while waiting for the first words of a reply, when the chat connection
/// itself is silent. Opens its own short-lived connection.
pub fn probe(config: &Config) -> bool {
    let url = format!("{}/api/version", config.base_url());
    // Here a short connect timeout is exactly what we want (see `chat_stream`).
    let agent = make_agent(Duration::from_secs(10), Some(Duration::from_secs(10)));
    agent.get(&url).call().is_ok()
}

/// Send the conversation `messages` to ollama and stream back the assistant's
/// reply. `on_sent` runs once the request is on its way; `on_chunk` runs for
/// every chunk received, with whatever answer text it carries (possibly empty, for
/// reasoning-only chunks) and whether it was reasoning. Returning `false` from
/// `on_chunk` abandons the request. Returns the full reply text, or a
/// human-readable error string suitable for showing to the user.
///
/// This blocks (network round-trip + model inference), so callers run it on a
/// worker thread, not on the UI message loop.
pub fn chat_stream(
    config: &Config,
    messages: &[ChatMessage],
    on_sent: impl FnOnce(),
    mut on_chunk: impl FnMut(&str, bool) -> bool,
) -> Result<String, String> {
    ensure_trusted(config)?;
    let url = config.chat_url();
    let req = ChatRequest { model: config.model.trim(), messages, stream: true };

    // IMPORTANT: do NOT set a short `timeout_connect` here. In the Xous `net`
    // stack the connect timeout is passed straight to smoltcp's `set_timeout()`,
    // which is a *whole-connection inactivity abort*, not just a connect-phase
    // limit (services/net/src/std_tcpstream.rs). Before the first token arrives
    // the socket is idle for the whole prompt evaluation (and model load, on a
    // cold start), so a 10s connect timeout would abort a perfectly healthy
    // request. The read timeout is the backstop; the UI's watchdog (probes while
    // waiting, a stall limit once text flows) notices trouble much sooner.
    let agent = make_agent(Duration::from_secs(300), None);
    on_sent();

    let resp = match agent.post(&url).send_json(&req) {
        Ok(resp) => resp,
        Err(e) => return Err(describe_error(config, &url, e)),
    };
    let mut reply = String::new();
    for line in BufReader::new(resp.into_reader()).lines() {
        let line = line.map_err(|e| format!("Lost the connection while receiving the reply.\n\n({})", e))?;
        if line.trim().is_empty() {
            continue;
        }
        let chunk: StreamChunk =
            serde_json::from_str(&line).map_err(|e| format!("Bad response from ollama: {}", e))?;
        if let Some(err) = chunk.error {
            return Err(format!("ollama reported an error: {}", err));
        }
        let (content, thinking) = match &chunk.message {
            Some(m) => (m.content.as_str(), m.content.is_empty() && !m.thinking.is_empty()),
            None => ("", false),
        };
        reply.push_str(content);
        if !on_chunk(content, thinking) {
            return Err(String::from("Cancelled."));
        }
        if chunk.done {
            return Ok(reply);
        }
    }
    Err(String::from("The server closed the connection before the reply was finished."))
}

/// Turn a failed request into a message for the user.
fn describe_error(config: &Config, url: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
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
            format!("Server error {}: {}{}", code, body.trim(), hint)
        }
        ureq::Error::Transport(t) => format!(
            "Could not reach ollama at {}.\n\nCheck that:\n\
             • Wi-Fi is connected,\n\
             • the server address/port under F1 are correct,\n\
             • ollama is running and bound to 0.0.0.0 (OLLAMA_HOST=0.0.0.0),\n\
             • the server is reachable on your network.\n\n({})",
            url, t
        ),
    }
}
