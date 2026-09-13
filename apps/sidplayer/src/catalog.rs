//! Local tune library backed by the PDDB.
//!
//! Layout (all in the default basis):
//!   * dict `sidplayer.tunes` — one key per downloaded file, key = filename,
//!     value = the raw `.sid` bytes.
//!   * dict `sidplayer.meta`  — one key per file, key = filename, value = a single
//!     tab-separated metadata line (see [`TuneMeta::to_line`]). Cached so we don't
//!     re-parse/re-probe every tune on each launch.
//!   * dict `sidplayer.state` — app state; key `url` = last directory URL entered.

use std::io::{Read, Write};

use pddb::Pddb;

const TUNES_DICT: &str = "sidplayer.tunes";
const META_DICT: &str = "sidplayer.meta";
const STATE_DICT: &str = "sidplayer.state";
const URL_KEY: &str = "url";

/// Per-tune metadata shown in the browser and used to drive playback.
#[derive(Clone)]
pub struct TuneMeta {
    /// PDDB key / on-server filename, e.g. `Commando.sid`.
    pub filename: String,
    /// Display name from the PSID header (falls back to the filename).
    pub name: String,
    pub author: String,
    /// Total number of subtunes in the file.
    pub songs: u16,
    /// 1-based default subtune from the PSID header.
    pub start_song: u16,
    /// 0-based indices of the subtunes classified as music (never empty).
    pub music_songs: Vec<u16>,
}

impl TuneMeta {
    /// Serialize to one tab-separated line (no embedded tabs/newlines survive).
    pub fn to_line(&self) -> String {
        let music: Vec<String> = self.music_songs.iter().map(|s| s.to_string()).collect();
        format!(
            "{}\t{}\t{}\t{}\t{}",
            sanitize(&self.name),
            sanitize(&self.author),
            self.songs,
            self.start_song,
            music.join(",")
        )
    }

    fn from_line(filename: &str, line: &str) -> Option<TuneMeta> {
        let mut it = line.split('\t');
        let name = it.next()?.to_string();
        let author = it.next()?.to_string();
        let songs: u16 = it.next()?.parse().ok()?;
        let start_song: u16 = it.next()?.parse().ok()?;
        let music_songs: Vec<u16> =
            it.next().unwrap_or("").split(',').filter_map(|s| s.parse().ok()).collect();
        let music_songs = if music_songs.is_empty() { (0..songs.max(1)).collect() } else { music_songs };
        Some(TuneMeta {
            filename: filename.to_string(),
            name: if name.is_empty() { filename.to_string() } else { name },
            author,
            songs: songs.max(1),
            start_song: start_song.max(1),
            music_songs,
        })
    }
}

fn sanitize(s: &str) -> String { s.chars().map(|c| if c == '\t' || c == '\n' { ' ' } else { c }).collect() }

pub struct Catalog {
    pddb: Pddb,
}

impl Catalog {
    pub fn new() -> Self { Catalog { pddb: Pddb::new() } }

    /// Block until the PDDB is mounted (the user has unlocked it).
    pub fn wait_mounted(&self) { self.pddb.is_mounted_blocking(); }

    /// All stored tunes, sorted by display name. Missing/corrupt metadata yields a
    /// minimal single-song entry so the file is still listed and playable.
    pub fn list(&self) -> Vec<TuneMeta> {
        let mut out = Vec::new();
        let keys = self.pddb.list_keys(TUNES_DICT, None).unwrap_or_default();
        for filename in keys {
            let meta = self
                .read_meta(&filename)
                .unwrap_or_else(|| TuneMeta::from_line(&filename, "\t\t1\t1\t0").unwrap());
            out.push(meta);
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    fn read_meta(&self, filename: &str) -> Option<TuneMeta> {
        let mut key = self.pddb.get(META_DICT, filename, None, false, false, None, None::<fn()>).ok()?;
        let mut buf = Vec::new();
        key.read_to_end(&mut buf).ok()?;
        let line = String::from_utf8_lossy(&buf);
        TuneMeta::from_line(filename, line.trim_end_matches(['\n', '\r']))
    }

    /// True if a tune with this filename is already stored (skips re-downloading).
    pub fn has(&self, filename: &str) -> bool {
        self.pddb.get(TUNES_DICT, filename, None, false, false, None, None::<fn()>).is_ok()
    }

    /// Read the raw `.sid` bytes for a stored tune.
    pub fn read_tune(&self, filename: &str) -> Option<Vec<u8>> {
        let mut key = self.pddb.get(TUNES_DICT, filename, None, false, false, None, None::<fn()>).ok()?;
        let mut buf = Vec::new();
        key.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    /// Store a tune's bytes and metadata. Does not `sync()` — the caller batches a
    /// single sync after a whole download run (see [`Catalog::sync`]).
    pub fn store(&self, meta: &TuneMeta, bytes: &[u8]) -> std::io::Result<()> {
        // delete-then-create so a shorter new value can't leave stale trailing bytes
        self.pddb.delete_key(TUNES_DICT, &meta.filename, None).ok();
        let mut k = self.pddb.get(
            TUNES_DICT,
            &meta.filename,
            None,
            true,
            true,
            Some(bytes.len()),
            None::<fn()>,
        )?;
        k.write_all(bytes)?;
        drop(k);

        let line = meta.to_line();
        self.pddb.delete_key(META_DICT, &meta.filename, None).ok();
        let mut m = self.pddb.get(META_DICT, &meta.filename, None, true, true, None, None::<fn()>)?;
        m.write_all(line.as_bytes())?;
        drop(m);
        Ok(())
    }

    /// Remove a stored tune's bytes and metadata, and flush to flash.
    pub fn delete(&self, filename: &str) {
        self.pddb.delete_key(TUNES_DICT, filename, None).ok();
        self.pddb.delete_key(META_DICT, filename, None).ok();
        self.pddb.sync().ok();
    }

    /// Remove every stored tune and its metadata (used by "replace all").
    pub fn clear_all(&self) {
        if let Ok(keys) = self.pddb.list_keys(TUNES_DICT, None) {
            for k in keys {
                self.pddb.delete_key(TUNES_DICT, &k, None).ok();
                self.pddb.delete_key(META_DICT, &k, None).ok();
            }
        }
        self.pddb.sync().ok();
    }

    pub fn sync(&self) { self.pddb.sync().ok(); }

    /// Last directory URL the user entered, if any.
    pub fn get_url(&self) -> Option<String> {
        let mut key = self.pddb.get(STATE_DICT, URL_KEY, None, false, false, None, None::<fn()>).ok()?;
        let mut buf = Vec::new();
        key.read_to_end(&mut buf).ok()?;
        let s = String::from_utf8_lossy(&buf).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    pub fn set_url(&self, url: &str) {
        self.pddb.delete_key(STATE_DICT, URL_KEY, None).ok();
        if let Ok(mut key) = self.pddb.get(STATE_DICT, URL_KEY, None, true, true, None, None::<fn()>) {
            key.write_all(url.as_bytes()).ok();
            drop(key);
            self.pddb.sync().ok();
        }
    }
}
