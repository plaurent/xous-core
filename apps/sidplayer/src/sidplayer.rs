use core::fmt::Write as _;

use codec::*;
use gam::*;
use num_traits::ToPrimitive;

use crate::AppOp;
use crate::catalog::{Catalog, TuneMeta};
use crate::netfetch;
use crate::player::{OUTPUT_RATE, Player, classify_music_songs};
use crate::psid::Psid;

/// The embedded tune. Rob Hubbard's "Commando" (1985, Elite). Always present as
/// the first library entry so the app is useful before anything is downloaded.
static COMMANDO_SID: &[u8] = include_bytes!("commando.sid");
const COMMANDO_FILENAME: &str = "Commando (built-in)";

/// Layout metrics, in pixels. Regular glyphs are 15 px tall; we draw text with a
/// zero top-margin (see `text`) and pitch the lines a touch larger so rows and the
/// footer never overlap.
const LINE_H: isize = 16;
const ROW_H: isize = 18;
const LIST_TOP: isize = 20;
/// Footer holds four lines (status, info, two key-hint rows) plus a little room.
const FOOTER_H: isize = 4 * LINE_H + 4;

/// Where to route the audio. The codec drives the speaker and the headphones in
/// parallel with no hardware auto-switching, and the speaker path has ~+12 dB more
/// driver gain than the headphone path, so the speaker swamps the headphones unless
/// we explicitly mute it. Each mode mutes the unused path and sets a sensible gain
/// on the active one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Headphones,
    Speaker,
    Both,
}

impl OutputMode {
    fn next(self) -> Self {
        match self {
            OutputMode::Headphones => OutputMode::Speaker,
            OutputMode::Speaker => OutputMode::Both,
            OutputMode::Both => OutputMode::Headphones,
        }
    }

    fn label(self) -> &'static str {
        match self {
            OutputMode::Headphones => "HP",
            OutputMode::Speaker => "Spkr",
            OutputMode::Both => "Both",
        }
    }
}

/// One library entry: a tune file plus its metadata.
struct Entry {
    meta: TuneMeta,
    /// If true, bytes come from the embedded [`COMMANDO_SID`] rather than the PDDB.
    builtin: bool,
}

/// One selectable line in the flattened browser list: a (file, subtune) pair.
#[derive(Clone, Copy)]
struct Row {
    /// Index into `self.entries`.
    entry: usize,
    /// 0-based subtune within that file.
    song: u16,
    /// True for the first shown row of a file (the one that shows the file name).
    first: bool,
}

pub(crate) struct SidPlayer {
    gam: gam::Gam,
    _token: [u32; 4],
    gid: Gid,
    screensize: Point,
    codec: codec::Codec,
    /// connection back to our own main server, for codec fill callbacks
    self_conn: xous::CID,
    ticktimer: ticktimer_server::Ticktimer,
    modals: modals::Modals,
    catalog: Catalog,

    /// Metadata for the built-in Commando tune (classified once at startup).
    builtin_meta: TuneMeta,
    /// The library: built-in tune followed by everything stored in the PDDB.
    entries: Vec<Entry>,
    /// Flattened, selectable rows derived from `entries` + `show_all`.
    rows: Vec<Row>,
    /// Highlighted row (inverted). Index into `rows`.
    cursor: usize,
    /// Index of the first visible row.
    scroll: usize,
    /// How many list rows fit on screen.
    visible_rows: usize,
    /// Max characters that fit on a row before truncation.
    max_chars: usize,
    /// When false, show only music subtunes; when true, show every subtune.
    show_all: bool,

    // --- playback ---
    /// Raw bytes of the tune currently loaded for playback.
    play_bytes: Vec<u8>,
    player: Option<Player>,
    playing: bool,
    hooked: bool,
    frames_played: u32,
    underruns: u32,
    /// (file key, subtune) currently playing, for the ▶ marker.
    now_playing: Option<(String, u16)>,
    /// currently selected/playing subtune (0-based)
    song: u16,
    /// total number of subtunes in the loaded file
    songs: u16,
    output: OutputMode,
    /// headphone analog gain in dB (0 = loudest, more negative = quieter)
    hp_gain_db: f32,

    /// Random-shuffle mode: play a random row, advance to another random row every
    /// `shuffle_secs`. Driven by the audio-callback sample counter, no timer thread.
    shuffle: bool,
    shuffle_secs: u32,
    /// xorshift PRNG state for shuffle (seeded lazily from the ticktimer).
    rng_state: u32,

    status: String,
    /// signature of the last painted screen, for skipping redundant redraws
    last_paint_sig: u64,
}

impl SidPlayer {
    pub(crate) fn new(sid: xous::SID) -> Self {
        let xns = xous_names::XousNames::new().expect("couldn't connect to Xous Namespace Server");
        let gam = gam::Gam::new(&xns).expect("can't connect to Graphical Abstraction Manager");

        let token = gam
            .register_ux(UxRegistration {
                app_name: String::from(gam::APP_NAME_SIDPLAYER),
                ux_type: gam::UxType::Framebuffer,
                predictor: None,
                listener: sid.to_array(),
                redraw_id: AppOp::Redraw.to_u32().unwrap(),
                gotinput_id: None,
                audioframe_id: None,
                focuschange_id: Some(AppOp::FocusChange.to_u32().unwrap()),
                rawkeys_id: Some(AppOp::Rawkeys.to_u32().unwrap()),
            })
            .expect("couldn't register Ux context for sidplayer")
            .unwrap();

        let gid = gam.request_content_canvas(token).expect("couldn't get content canvas");
        let screensize = gam.get_canvas_bounds(gid).expect("couldn't get dimensions of content canvas");

        let codec = codec::Codec::new(&xns).expect("couldn't connect to CODEC");
        let ticktimer = ticktimer_server::Ticktimer::new().unwrap();
        let modals = modals::Modals::new(&xns).expect("couldn't connect to Modals");
        let self_conn = xous::connect(sid).unwrap();

        let catalog = Catalog::new();
        catalog.wait_mounted();

        // Classify the built-in tune's music subtunes once, up front.
        let builtin_meta = match Psid::parse(COMMANDO_SID) {
            Ok(p) => TuneMeta {
                filename: String::from(COMMANDO_FILENAME),
                name: nonempty(p.name.as_str(), COMMANDO_FILENAME),
                author: p.author.as_str().trim().to_string(),
                songs: p.songs.max(1),
                start_song: p.start_song.max(1),
                music_songs: classify_music_songs(&p),
            },
            Err(_) => TuneMeta {
                filename: String::from(COMMANDO_FILENAME),
                name: String::from(COMMANDO_FILENAME),
                author: String::new(),
                songs: 1,
                start_song: 1,
                music_songs: vec![0],
            },
        };

        let visible_rows = ((screensize.y - LIST_TOP - FOOTER_H) / ROW_H).max(1) as usize;
        let max_chars = ((screensize.x - 12) / 8).max(8) as usize;

        let mut app = SidPlayer {
            gam,
            _token: token,
            gid,
            screensize,
            codec,
            self_conn,
            ticktimer,
            modals,
            catalog,
            builtin_meta,
            entries: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            scroll: 0,
            visible_rows,
            max_chars,
            show_all: false,
            play_bytes: Vec::new(),
            player: None,
            playing: false,
            hooked: false,
            frames_played: 0,
            underruns: 0,
            now_playing: None,
            song: 0,
            songs: 1,
            output: OutputMode::Headphones,
            hp_gain_db: 0.0,
            shuffle: false,
            shuffle_secs: 0,
            rng_state: 0,
            status: String::from("↑↓ pick  ⏎ play  1-9 shuffle  d get"),
            last_paint_sig: u64::MAX, // force the first paint
        };
        app.rebuild_entries();
        app
    }

    /// Rebuild the library from the built-in tune plus everything in the PDDB.
    fn rebuild_entries(&mut self) {
        let mut entries = Vec::new();
        entries.push(Entry { meta: self.builtin_meta.clone(), builtin: true });
        for m in self.catalog.list() {
            entries.push(Entry { meta: m, builtin: false });
        }
        self.entries = entries;
        self.rebuild_rows();
    }

    /// Flatten `entries` into selectable rows, honoring the show-all toggle.
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for (ei, e) in self.entries.iter().enumerate() {
            let songs: Vec<u16> =
                if self.show_all { (0..e.meta.songs).collect() } else { e.meta.music_songs.clone() };
            for (j, &s) in songs.iter().enumerate() {
                rows.push(Row { entry: ei, song: s, first: j == 0 });
            }
        }
        self.rows = rows;
        if self.cursor >= self.rows.len() {
            self.cursor = self.rows.len().saturating_sub(1);
        }
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + self.visible_rows {
            self.scroll = self.cursor + 1 - self.visible_rows;
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
        self.ensure_visible();
        self.force_redraw();
    }

    /// Jump the cursor to the first row of the previous/next file.
    fn step_file(&mut self, forward: bool) {
        if self.rows.is_empty() {
            return;
        }
        let cur_entry = self.rows[self.cursor].entry;
        let target = if forward { cur_entry + 1 } else { cur_entry.wrapping_sub(1) };
        if let Some(idx) = self.rows.iter().position(|r| r.entry == target && r.first) {
            self.cursor = idx;
            self.ensure_visible();
            self.force_redraw();
        }
    }

    /// Push the current output-mode routing and gains to the codec. Muting the
    /// unused path is what actually makes headphone-only playback work, since the
    /// codec otherwise drives both outputs at once.
    fn apply_output(&mut self) {
        match self.output {
            OutputMode::Headphones => {
                self.codec.set_speaker_volume(VolumeOps::Mute, None).ok();
                self.codec.set_headphone_volume(VolumeOps::Set, Some(self.hp_gain_db)).ok();
            }
            OutputMode::Speaker => {
                self.codec.set_speaker_volume(VolumeOps::RestoreDefault, None).ok();
                self.codec.set_headphone_volume(VolumeOps::Mute, None).ok();
            }
            OutputMode::Both => {
                self.codec.set_speaker_volume(VolumeOps::RestoreDefault, None).ok();
                self.codec.set_headphone_volume(VolumeOps::Set, Some(self.hp_gain_db)).ok();
            }
        }
    }

    /// Play the (file, subtune) under the cursor. If it's already the tune that's
    /// playing, stop instead (toggle).
    fn play_selected(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let row = self.rows[self.cursor];
        let key = self.entries[row.entry].meta.filename.clone();

        if self.playing && self.now_playing.as_ref() == Some(&(key, row.song)) {
            self.stop();
            self.force_redraw();
            return;
        }
        self.play_row(self.cursor);
        self.force_redraw();
    }

    /// Load and play the given row. Hot-swaps the engine if already playing (no
    /// codec re-setup), otherwise starts the stream. Does not redraw.
    fn play_row(&mut self, idx: usize) {
        if idx >= self.rows.len() {
            return;
        }
        let row = self.rows[idx];
        let key = self.entries[row.entry].meta.filename.clone();
        let bytes = if self.entries[row.entry].builtin {
            COMMANDO_SID.to_vec()
        } else {
            match self.catalog.read_tune(&key) {
                Some(b) => b,
                None => {
                    self.set_status("Read error");
                    return;
                }
            }
        };
        self.play_bytes = bytes;
        self.song = row.song;
        self.now_playing = Some((key, row.song));
        if self.playing {
            self.load_player();
        } else {
            self.start();
        }
    }

    /// A cheap xorshift PRNG, seeded lazily from the ticktimer.
    fn next_rand(&mut self) -> u32 {
        if self.rng_state == 0 {
            self.rng_state = (self.ticktimer.elapsed_ms() as u32) | 1;
        }
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        x
    }

    /// A random row index, avoiding the current one when there's more than one.
    fn random_row(&mut self) -> usize {
        let n = self.rows.len();
        if n <= 1 {
            return 0;
        }
        let cur = self.cursor;
        loop {
            let idx = (self.next_rand() as usize) % n;
            if idx != cur {
                return idx;
            }
        }
    }

    /// Start (or re-seed) shuffle: play a random row and set the per-tune interval
    /// to `mins` minutes. Pressing a number key while already shuffling lands here
    /// too, jumping to a new random tune and updating the interval.
    fn start_shuffle(&mut self, mins: u32) {
        if self.rows.is_empty() {
            return;
        }
        self.shuffle = true;
        self.shuffle_secs = mins * 60;
        let idx = self.random_row();
        self.cursor = idx;
        self.ensure_visible();
        self.play_row(idx);
        self.set_status(&format!("Shuffle: {} min/tune", mins));
        self.force_redraw();
    }

    /// Advance shuffle to another random tune (called when the interval elapses).
    fn advance_shuffle(&mut self) {
        let idx = self.random_row();
        self.cursor = idx;
        self.ensure_visible();
        self.play_row(idx);
        self.force_redraw();
    }

    /// Delete the downloaded tune under the cursor (after a confirmation). The
    /// built-in tune can't be deleted. If the tune being deleted is playing, it's
    /// stopped first.
    fn delete_selected(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let entry = self.rows[self.cursor].entry;
        if self.entries[entry].builtin {
            self.set_status("Can't delete the built-in tune");
            self.force_redraw();
            return;
        }
        let key = self.entries[entry].meta.filename.clone();
        let name = self.entries[entry].meta.name.clone();

        // Confirm — Cancel is listed first so it's the default selection.
        self.modals.add_list_item("Cancel").ok();
        self.modals.add_list_item("Delete").ok();
        let confirmed = matches!(
            self.modals.get_radiobutton(&format!("Delete {}?", name)),
            Ok(choice) if choice == "Delete"
        );
        if !confirmed {
            self.force_redraw();
            return;
        }

        if self.now_playing.as_ref().map(|(f, _)| f == &key).unwrap_or(false) {
            self.stop();
        }
        self.catalog.delete(&key);
        self.rebuild_entries();
        self.set_status(&format!("Deleted {}", name));
        self.force_redraw();
    }

    /// Parse the loaded tune and build a fresh `Player` for the selected subtune,
    /// resetting the frame/underrun counters. Returns false and sets an error
    /// status if the file won't parse. Does not touch the codec.
    fn load_player(&mut self) -> bool {
        let psid = match Psid::parse(&self.play_bytes) {
            Ok(p) => p,
            Err(e) => {
                self.set_status(&format!("Parse error: {}", e.0));
                log::error!("sidplayer: PSID parse error: {}", e.0);
                return false;
            }
        };
        self.songs = psid.songs.max(1);
        if self.song >= self.songs {
            self.song = psid.start_song.saturating_sub(1).min(self.songs - 1);
        }
        log::info!(
            "sidplayer: playing '{}' by {} (song {}/{})",
            psid.name.as_str(),
            psid.author.as_str(),
            self.song + 1,
            self.songs
        );
        self.player = Some(Player::new(&psid, self.song));
        self.frames_played = 0;
        self.underruns = 0;
        true
    }

    fn start(&mut self) {
        if !self.load_player() {
            return;
        }

        self.codec.setup_8k_stream().expect("couldn't set up 8k stream");
        self.ticktimer.sleep_ms(50).unwrap();
        self.apply_output();

        if !self.hooked {
            self.codec.hook_frame_callback(AppOp::AudioFrame.to_u32().unwrap(), self.self_conn).unwrap();
            self.hooked = true;
        }
        self.codec.resume().unwrap();

        self.playing = true;
        self.set_status(&format!("Playing at {} Hz", OUTPUT_RATE));
    }

    fn stop(&mut self) {
        if self.playing {
            self.codec.abort().ok();
            self.codec.power_off().ok();
        }
        self.playing = false;
        self.player = None;
        self.now_playing = None;
        self.shuffle = false;
        self.set_status("Stopped");
    }

    /// Codec "give me more frames" callback. `free_play` is how many play frames
    /// the codec can currently accept.
    pub(crate) fn audio_frame(&mut self, free_play: usize) {
        if !self.playing || self.player.is_none() {
            return;
        }

        let mut frames: FrameRing = FrameRing::new();
        let ring_max = frames.writeable_count();
        // If the codec can accept the whole ring, its play buffer had fully
        // drained since our last fill: an underrun (dropout) occurred. Skip the
        // very first fill after resume, which legitimately starts from empty.
        if self.frames_played > 0 && free_play >= ring_max {
            self.underruns += 1;
        }

        let to_push = ring_max.min(free_play);
        {
            let player = self.player.as_mut().unwrap();
            for _ in 0..to_push {
                let mut frame: [u32; codec::FIFO_DEPTH] =
                    [ZERO_PCM as u32 | (ZERO_PCM as u32) << 16; codec::FIFO_DEPTH];
                for slot in frame.iter_mut() {
                    let s = player.next_sample();
                    let l = s as u16;
                    let r = s as u16;
                    *slot = r as u32 | (l as u32) << 16;
                }
                frames.nq_frame(frame).ok();
            }
        }
        self.frames_played += to_push as u32;
        self.codec.swap_frames(&mut frames).unwrap();

        // Shuffle timer: advance to a new random tune once this one has played for
        // the chosen interval. This is the only redraw allowed on this path, and it
        // fires at most once per interval (minutes), so the brief GAM stall at the
        // track change is fine — unlike a per-frame redraw, which would stall the
        // fill continuously and cause dropouts.
        if self.shuffle && self.shuffle_secs > 0 {
            let elapsed_secs = (self.frames_played as u64 * codec::FIFO_DEPTH as u64) / OUTPUT_RATE as u64;
            if elapsed_secs >= self.shuffle_secs as u64 {
                self.advance_shuffle();
            }
        }
        // NOTE: aside from the shuffle advance above, never redraw here. GAM IPC is
        // slow, and blocking this callback drains the codec buffer and causes
        // dropouts. The screen is otherwise refreshed only on user interaction —
        // deliberately no background timer thread, since a continuously-scheduled
        // thread on this single-core CPU destabilised the whole device.
    }

    /// Prompt for a directory URL (pre-filled with the last one used), download
    /// every `.sid` file linked on that page that we don't already have, classify
    /// each, and store it. Blocks the UI for the duration, redrawing progress
    /// between files — deliberately no background thread (see `audio_frame`).
    fn do_download(&mut self) {
        // Downloading while playing fights over the CPU and causes dropouts; stop.
        if self.playing {
            self.stop();
        }
        let url = match self.prompt_url() {
            Some(u) => u,
            None => {
                self.force_redraw();
                return;
            }
        };

        // If there are existing downloads, ask whether to append or replace them.
        // (With nothing downloaded yet the choice is moot, so skip the prompt.)
        let has_downloads = self.entries.iter().any(|e| !e.builtin);
        let replace = if has_downloads {
            match self.prompt_download_mode() {
                Some(r) => r,
                None => {
                    self.force_redraw();
                    return;
                }
            }
        } else {
            false
        };

        self.catalog.set_url(&url);

        self.set_status("Checking certificate…");
        self.force_redraw();

        // Establish TLS trust first (prompts to trust a CA on first use of a host);
        // otherwise the HTTPS fetch fails with an opaque "untrusted chain" error.
        if let Err(e) = netfetch::ensure_trusted(&url) {
            self.modals.show_notification(&e, None).ok();
            self.set_status("Certificate not trusted");
            self.force_redraw();
            return;
        }

        self.set_status("Connecting…");
        self.force_redraw();

        let agent = netfetch::make_agent();
        let links = match netfetch::list_sid_files(&agent, &url) {
            Ok(l) => l,
            Err(e) => {
                // Show the full URL + reason in a popup — the status line is far too
                // narrow to read a URL and error message without truncating them.
                self.modals
                    .show_notification(&format!("Could not fetch index.\n\nURL: {}\n\n{}", url, e), None)
                    .ok();
                self.set_status("Index fetch failed");
                self.force_redraw();
                return;
            }
        };
        if links.is_empty() {
            self.modals
                .show_notification(&format!("No .sid files linked on that page.\n\nURL: {}", url), None)
                .ok();
            self.set_status("No .sid files found");
            self.force_redraw();
            return;
        }

        // "Replace all": now that we have a valid, non-empty index, it's safe to
        // wipe the existing downloads before pulling the fresh set. Doing this only
        // after a successful fetch means a bad/unreachable URL never leaves the
        // library empty. (The built-in tune is not in the PDDB, so it's untouched.)
        if replace {
            self.catalog.clear_all();
        }

        let total = links.len();
        let (mut got, mut skipped, mut failed) = (0u32, 0u32, 0u32);
        for (i, link) in links.iter().enumerate() {
            if self.catalog.has(&link.filename) {
                skipped += 1;
                continue;
            }
            self.set_status(&format!("{}/{}: {}", i + 1, total, link.filename));
            self.force_redraw();
            match netfetch::download(&agent, &link.url) {
                Ok(bytes) => match self.build_meta(&link.filename, &bytes) {
                    Some(meta) => {
                        if self.catalog.store(&meta, &bytes).is_ok() {
                            got += 1;
                        } else {
                            failed += 1;
                        }
                    }
                    None => {
                        log::warn!("sidplayer: {} is not a playable PSID, skipping", link.filename);
                        failed += 1;
                    }
                },
                Err(e) => {
                    log::warn!("sidplayer: download of {} failed: {}", link.filename, e);
                    failed += 1;
                }
            }
        }
        self.catalog.sync();
        self.rebuild_entries();
        self.set_status(&format!("Done: {} new, {} had, {} failed", got, skipped, failed));
        self.force_redraw();
    }

    /// Parse a downloaded file's header and classify its music subtunes. Returns
    /// None if it isn't a PSID we can play.
    fn build_meta(&self, filename: &str, bytes: &[u8]) -> Option<TuneMeta> {
        let psid = Psid::parse(bytes).ok()?;
        Some(TuneMeta {
            filename: filename.to_string(),
            name: nonempty(psid.name.as_str(), filename),
            author: psid.author.as_str().trim().to_string(),
            songs: psid.songs.max(1),
            start_song: psid.start_song.max(1),
            music_songs: classify_music_songs(&psid),
        })
    }

    /// Ask whether to append to or replace the existing downloads.
    /// Returns Some(true) to replace, Some(false) to append, None to cancel.
    fn prompt_download_mode(&self) -> Option<bool> {
        const APPEND: &str = "Append new tunes";
        const REPLACE: &str = "Replace all downloads";
        // Append is listed first so it's the default (non-destructive) selection.
        self.modals.add_list_item(APPEND).ok();
        self.modals.add_list_item(REPLACE).ok();
        self.modals.add_list_item("Cancel").ok();
        match self.modals.get_radiobutton("Download:") {
            Ok(choice) if choice == APPEND => Some(false),
            Ok(choice) if choice == REPLACE => Some(true),
            _ => None,
        }
    }

    /// Show a text-entry modal for the directory URL, pre-filled with the last one.
    fn prompt_url(&self) -> Option<String> {
        let last = self.catalog.get_url().unwrap_or_default();
        let mut builder = self.modals.alert_builder("Directory URL:");
        // Rebind to the returned &mut Self so the final `build()` borrow is valid
        // (the builder's chaining methods return the mutable borrow they take).
        let builder = if last.is_empty() {
            builder.field(Some("https://".to_string()), None)
        } else {
            builder.field_placeholder_persist(Some(last), None)
        };
        match builder.build() {
            Ok(p) => {
                let s = p.content()[0].content.as_str().trim().to_string();
                if s.is_empty() || s == "https://" { None } else { Some(s) }
            }
            Err(_) => None,
        }
    }

    pub(crate) fn key(&mut self, k: char) {
        match k {
            '↑' => self.move_cursor(-1),
            '↓' => self.move_cursor(1),
            '←' => self.step_file(false),
            '→' => self.step_file(true),
            // Enter/space: stop shuffle if it's running, else play/stop the selection.
            ' ' | '∴' | '\r' => {
                if self.shuffle {
                    self.stop();
                    self.force_redraw();
                } else {
                    self.play_selected();
                }
            }
            // 1-9: start random shuffle at N minutes per tune (or, while shuffling,
            // jump to the next random tune and set the interval to N).
            '1'..='9' => self.start_shuffle((k as u8 - b'0') as u32),
            'x' | 'X' => {
                self.stop();
                self.force_redraw();
            }
            // backspace / delete: remove the selected downloaded tune
            '\u{8}' | '\u{7f}' => self.delete_selected(),
            'a' | 'A' => {
                self.show_all = !self.show_all;
                self.rebuild_rows();
                self.force_redraw();
            }
            'd' | 'D' => self.do_download(),
            'o' | 'O' => {
                self.output = self.output.next();
                if self.playing {
                    self.apply_output();
                }
                self.force_redraw();
            }
            // Volume up: F1 (0x11), or +/= as aliases. (Up/Down now scroll the list.)
            '\u{11}' | '+' | '=' => {
                self.hp_gain_db = (self.hp_gain_db + 3.0).min(0.0);
                if self.playing {
                    self.apply_output();
                }
                self.force_redraw();
            }
            // Volume down: F4 (0x14), or -/_ as aliases.
            '\u{14}' | '-' | '_' => {
                self.hp_gain_db = (self.hp_gain_db - 3.0).max(-42.0);
                if self.playing {
                    self.apply_output();
                }
                self.force_redraw();
            }
            _ => {}
        }
    }

    pub(crate) fn on_focus(&mut self, foreground: bool) {
        if !foreground && self.playing {
            self.stop();
        }
        // Always repaint on a focus change so the screen is correct on return.
        self.force_redraw();
    }

    fn set_status(&mut self, s: &str) {
        self.status.clear();
        self.status.push_str(s);
    }

    /// A cheap signature of everything drawn on screen (excludes the live elapsed
    /// time). If GAM asks us to repaint while this is unchanged, we skip the whole
    /// expensive draw.
    fn paint_sig(&self) -> u64 {
        let mut h = self.playing as u64;
        let mode = match self.output {
            OutputMode::Headphones => 0u64,
            OutputMode::Speaker => 1,
            OutputMode::Both => 2,
        };
        h = h.wrapping_mul(31).wrapping_add(mode);
        h = h.wrapping_mul(31).wrapping_add((self.hp_gain_db as i32 as i64 as u64) & 0xffff);
        h = h.wrapping_mul(31).wrapping_add(self.underruns as u64);
        h = h.wrapping_mul(31).wrapping_add(self.cursor as u64);
        h = h.wrapping_mul(31).wrapping_add(self.scroll as u64);
        h = h.wrapping_mul(31).wrapping_add(self.show_all as u64);
        h = h.wrapping_mul(31).wrapping_add(self.shuffle as u64);
        h = h.wrapping_mul(31).wrapping_add(self.shuffle_secs as u64);
        h = h.wrapping_mul(31).wrapping_add(self.rows.len() as u64);
        h = h.wrapping_mul(31).wrapping_add(self.song as u64);
        if let Some((f, s)) = &self.now_playing {
            for b in f.as_bytes() {
                h = h.wrapping_mul(31).wrapping_add(*b as u64);
            }
            h = h.wrapping_mul(31).wrapping_add(*s as u64);
        }
        for b in self.status.as_bytes() {
            h = h.wrapping_mul(31).wrapping_add(*b as u64);
        }
        h
    }

    /// GAM-driven repaint: skip the expensive draw if nothing visible changed.
    pub(crate) fn redraw(&mut self) {
        if self.paint_sig() != self.last_paint_sig {
            self.force_redraw();
        }
    }

    /// The display text for one row.
    fn row_text(&self, idx: usize) -> String {
        let row = self.rows[idx];
        let e = &self.entries[row.entry];
        let playing =
            self.now_playing.as_ref().map(|(f, s)| f == &e.meta.filename && *s == row.song).unwrap_or(false);
        let mark = if playing { "▶" } else { " " };

        let body = if row.first {
            let name = if e.meta.name.is_empty() { &e.meta.filename } else { &e.meta.name };
            if e.meta.songs <= 1 {
                format!("{} {}", mark, name)
            } else {
                format!("{} {}  ·  t{}/{}", mark, name, row.song + 1, e.meta.songs)
            }
        } else {
            format!("{}       t{}/{}", mark, row.song + 1, e.meta.songs)
        };
        truncate(&body, self.max_chars)
    }

    /// Unconditional repaint. Used for user actions, focus changes, and progress.
    pub(crate) fn force_redraw(&mut self) {
        self.last_paint_sig = self.paint_sig();
        // clear
        self.gam
            .draw_rectangle(
                self.gid,
                Rectangle::new_coords_with_style(
                    0,
                    0,
                    self.screensize.x,
                    self.screensize.y,
                    DrawStyle::new(PixelColor::Light, PixelColor::Light, 0),
                ),
            )
            .expect("couldn't clear screen");

        // title bar
        let mut title = String::from("SID Player");
        if self.shuffle {
            write!(title, "  [shuffle {}m]", self.shuffle_secs / 60).ok();
        }
        if self.show_all {
            title.push_str("  [all]");
        }
        self.text(4, 2, &title);

        // list
        if self.rows.is_empty() {
            self.text(6, LIST_TOP + 4, "No tunes. Press d to download.");
        } else {
            for i in 0..self.visible_rows {
                let idx = self.scroll + i;
                if idx >= self.rows.len() {
                    break;
                }
                let y = LIST_TOP + (i as isize) * ROW_H;
                let s = self.row_text(idx);
                self.draw_row(y, &s, idx == self.cursor);
            }
        }

        // footer: status + a short output/vol line + help
        let fy = self.screensize.y - FOOTER_H;
        let statusline = truncate(&self.status, self.max_chars);
        self.text(4, fy, &statusline);
        let mut info = String::new();
        write!(info, "Out {}  Vol {} dB", self.output.label(), self.hp_gain_db as i32).ok();
        if self.playing {
            write!(info, "  ur {}", self.underruns).ok();
        }
        self.text(4, fy + LINE_H, &info);
        self.text(4, fy + 2 * LINE_H, "⏎play/stop  1-9:shuffle  a:all");
        self.text(4, fy + 3 * LINE_H, "d:get  ⌫del  o:out  F1/F4:vol");

        self.gam.redraw().unwrap();
    }

    /// Draw one list row. The selected row is framed with an outline box (a dark
    /// 1 px border, light fill) rather than a solid inverted bar: GAM only honors
    /// the TextView `invert` bit for high-trust canvases, so a normal app can't
    /// draw light-on-dark text — a filled bar would just hide the text. The box
    /// keeps the text readable while clearly marking the selection.
    fn draw_row(&self, y: isize, s: &str, selected: bool) {
        if selected {
            self.gam
                .draw_rectangle(
                    self.gid,
                    Rectangle::new_coords_with_style(
                        1,
                        y,
                        self.screensize.x - 2,
                        y + ROW_H - 1,
                        DrawStyle::new(PixelColor::Light, PixelColor::Dark, 1),
                    ),
                )
                .ok();
        }
        let mut tv = TextView::new(
            self.gid,
            TextBounds::GrowableFromTl(Point::new(8, y + 1), (self.screensize.x - 16) as u16),
        );
        tv.draw_border = false;
        tv.clear_area = false;
        tv.margin = Point::new(0, 0);
        tv.style = GlyphStyle::Regular;
        write!(tv.text, "{}", s).ok();
        self.gam.post_textview(&mut { tv }).ok();
    }

    fn text(&self, x: isize, y: isize, s: &str) {
        let mut tv = TextView::new(
            self.gid,
            TextBounds::GrowableFromTl(Point::new(x, y), (self.screensize.x - x * 2) as u16),
        );
        tv.draw_border = false;
        tv.clear_area = false;
        // zero top-margin so callers control line spacing exactly (glyphs are 15 px)
        tv.margin = Point::new(0, 0);
        tv.style = GlyphStyle::Regular;
        write!(tv.text, "{}", s).ok();
        self.gam.post_textview(&mut { tv }).ok();
    }
}

/// Trim `s`, returning `fallback` if the result is empty.
fn nonempty(s: &str, fallback: &str) -> String {
    let t = s.trim();
    if t.is_empty() { fallback.to_string() } else { t.to_string() }
}

/// Truncate to at most `max` chars, appending '…' if anything was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        let mut out: String = s.chars().take(keep).collect();
        out.push('…');
        out
    }
}
