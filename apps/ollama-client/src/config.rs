//! Persistent connection settings for the ollama server, backed by the PDDB.
//!
//! Layout (all in the default basis):
//!   * dict `ollama.config` — one key per setting: `host`, `port`, `model`.
//!
//! Everything is stored as a short UTF-8 string. Missing/unparseable values fall
//! back to the defaults below, so a fresh install still starts in a sane state
//! (the user is prompted for the host on first send — see [`Config::is_ready`]).

use std::io::{Read, Write};

use pddb::Pddb;

const CONFIG_DICT: &str = "ollama.config";
const HOST_KEY: &str = "host";
const PORT_KEY: &str = "port";
const MODEL_KEY: &str = "model";

/// Default ollama port. Override in the settings modal (F1).
pub const DEFAULT_PORT: u16 = 11434;
/// A reasonable default model name; the user will usually change this to
/// whatever they have pulled locally (`ollama list`).
pub const DEFAULT_MODEL: &str = "llama3.2";

/// The current connection settings, held in memory and mirrored to the PDDB.
#[derive(Clone)]
pub struct Config {
    /// Hostname or IP of the ollama server, e.g. `192.168.1.20`. Empty until set.
    pub host: String,
    /// TCP port ollama listens on (default 11434).
    pub port: u16,
    /// Model name to chat with, e.g. `llama3.2` or `qwen2.5:3b`.
    pub model: String,
}

impl Config {
    /// Load settings from the PDDB, falling back to defaults for anything unset.
    /// Blocks until the PDDB is mounted (i.e. the user has unlocked it).
    pub fn load() -> Self {
        let pddb = Pddb::new();
        pddb.is_mounted_blocking();
        Config {
            host: read_key(&pddb, HOST_KEY).unwrap_or_default(),
            port: read_key(&pddb, PORT_KEY).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_PORT),
            model: {
                let m = read_key(&pddb, MODEL_KEY).unwrap_or_default();
                if m.is_empty() { DEFAULT_MODEL.to_string() } else { m }
            },
        }
    }

    /// Persist the current settings to the PDDB.
    pub fn save(&self) {
        let pddb = Pddb::new();
        write_key(&pddb, HOST_KEY, &self.host);
        write_key(&pddb, PORT_KEY, &self.port.to_string());
        write_key(&pddb, MODEL_KEY, &self.model);
        pddb.sync().ok();
    }

    /// True once a host has been configured; sends are blocked until then.
    pub fn is_ready(&self) -> bool { !self.host.trim().is_empty() }

    /// The server root, e.g. `http://192.168.1.20:11434`.
    pub fn base_url(&self) -> String { format!("http://{}:{}", self.host.trim(), self.port) }

    /// The ollama chat endpoint, e.g. `http://192.168.1.20:11434/api/chat`.
    pub fn chat_url(&self) -> String { format!("{}/api/chat", self.base_url()) }

    /// The endpoint that lists locally-installed models.
    pub fn tags_url(&self) -> String { format!("{}/api/tags", self.base_url()) }
}

fn read_key(pddb: &Pddb, key: &str) -> Option<String> {
    let mut k = pddb.get(CONFIG_DICT, key, None, false, false, None, None::<fn()>).ok()?;
    let mut buf = Vec::new();
    k.read_to_end(&mut buf).ok()?;
    let s = String::from_utf8_lossy(&buf).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn write_key(pddb: &Pddb, key: &str, val: &str) {
    // delete-then-create so a shorter new value can't leave stale trailing bytes.
    pddb.delete_key(CONFIG_DICT, key, None).ok();
    if let Ok(mut k) = pddb.get(CONFIG_DICT, key, None, true, true, None, None::<fn()>) {
        k.write_all(val.as_bytes()).ok();
    }
}
