//! Imported wikis and reader settings, kept in the PDDB.
//!
//! Layout (all in the default basis):
//!   * dict `localwiki.wikis` — one key per wiki, key = the wiki's name, value = its JSON
//!     exactly as imported (see `wiki.rs`). Importing a wiki with the same name replaces it.
//!   * dict `localwiki.state` — `url` (last import address), `wiki` (the wiki last read),
//!     `font` (text size).

use std::io::{Read, Write};

use pddb::Pddb;

use crate::layout::FontSize;

const WIKIS_DICT: &str = "localwiki.wikis";
const STATE_DICT: &str = "localwiki.state";
const URL_KEY: &str = "url";
const WIKI_KEY: &str = "wiki";
const FONT_KEY: &str = "font";

/// PDDB key names are limited to 95 bytes, so a longer wiki name is cut short to fit.
const MAX_KEY_BYTES: usize = 95;

pub struct Store {
    pddb: Pddb,
}

impl Store {
    pub fn new() -> Self { Store { pddb: Pddb::new() } }

    /// Block until the PDDB is mounted (the user has unlocked it).
    pub fn wait_mounted(&self) { self.pddb.is_mounted_blocking(); }

    /// The stored wikis' keys (their names), sorted ignoring case.
    pub fn names(&self) -> Vec<String> {
        let mut names = self.pddb.list_keys(WIKIS_DICT, None).unwrap_or_default();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    pub fn read(&self, key: &str) -> Option<Vec<u8>> {
        let mut k = self.pddb.get(WIKIS_DICT, key, None, false, false, None, None::<fn()>).ok()?;
        let mut buf = Vec::new();
        k.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    /// Store a wiki's JSON under its name, replacing any wiki of the same name, and
    /// return the key used. Doesn't `sync()`; an import syncs once at the end.
    pub fn save(&self, name: &str, json: &[u8]) -> std::io::Result<String> {
        let key = key_for(name);
        // delete-then-create so a shorter new value can't leave stale trailing bytes
        self.pddb.delete_key(WIKIS_DICT, &key, None).ok();
        let mut k = self.pddb.get(WIKIS_DICT, &key, None, true, true, Some(json.len()), None::<fn()>)?;
        k.write_all(json)?;
        Ok(key)
    }

    pub fn delete(&self, key: &str) {
        self.pddb.delete_key(WIKIS_DICT, key, None).ok();
        self.pddb.sync().ok();
    }

    pub fn sync(&self) { self.pddb.sync().ok(); }

    pub fn url(&self) -> Option<String> { self.get_state(URL_KEY) }

    pub fn set_url(&self, url: &str) { self.set_state(URL_KEY, url); }

    pub fn last_wiki(&self) -> Option<String> { self.get_state(WIKI_KEY) }

    pub fn set_last_wiki(&self, key: &str) { self.set_state(WIKI_KEY, key); }

    pub fn font(&self) -> Option<FontSize> { self.get_state(FONT_KEY).and_then(|s| FontSize::from_label(&s)) }

    pub fn set_font(&self, font: FontSize) { self.set_state(FONT_KEY, font.label()); }

    fn get_state(&self, key: &str) -> Option<String> {
        let mut k = self.pddb.get(STATE_DICT, key, None, false, false, None, None::<fn()>).ok()?;
        let mut buf = Vec::new();
        k.read_to_end(&mut buf).ok()?;
        let s = String::from_utf8_lossy(&buf).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    fn set_state(&self, key: &str, value: &str) {
        if self.get_state(key).as_deref() == Some(value) {
            return;
        }
        self.pddb.delete_key(STATE_DICT, key, None).ok();
        if let Ok(mut k) = self.pddb.get(STATE_DICT, key, None, true, true, None, None::<fn()>) {
            k.write_all(value.as_bytes()).ok();
            drop(k);
            self.pddb.sync().ok();
        }
    }
}

/// The PDDB key for a wiki name: the name itself, cut short at a character boundary if
/// it's longer than a key may be.
fn key_for(name: &str) -> String {
    let mut end = name.len().min(MAX_KEY_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}
