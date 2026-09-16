//! The chat UI: a scrollable transcript pane above GAM's standard predictive
//! text-entry line (the same input area ShellChat and edlin use).
//!
//! We register a `UxType::Chat` context, so GAM/IMEF own the input + prediction
//! canvases at the bottom of the screen and hand us a pre-shrunk *content* canvas
//! above them. Typing is therefore handled by the IME (fast — it repaints only the
//! small input canvas), and a finished prompt arrives as one `String` via the
//! `Line` opcode when the user presses Enter.
//!
//! Scrolling a long reply is the wrinkle: the input area also wants the arrow keys
//! (to move the text cursor). GAM delivers every keystroke to *both* the IME and,
//! independently, to our `rawkeys` callback — so we read arrows there and act on
//! them only in **scroll mode**, toggled with **F3**. In edit mode we ignore the
//! arrows and let the IME move the input cursor.
//!
//! A send blocks (network + inference), so it runs on a worker thread; when the
//! reply (or error) is ready the worker drops it in a shared slot and pings our
//! server with `AppOp::ResponseReady`.

use core::fmt::Write as _;
use std::sync::{Arc, Mutex};

use gam::*;
use num_traits::ToPrimitive;

use crate::AppOp;
use crate::config::{Config, DEFAULT_MODEL};
use crate::net::{self, ChatMessage};

/// Layout metrics, in pixels. Regular glyphs are ~15 px tall. The chrome (title,
/// status, hints) is always Regular; only the transcript changes with the font
/// toggle (see the `font_*` helpers).
const LINE_H: isize = 16;
/// Top of the transcript, below the one-line title bar.
const TRANSCRIPT_TOP: isize = 20;
/// Footer inside our content canvas: a status line + a key-hint line.
/// (The input & prediction area below this is drawn by GAM, not us.)
const FOOTER_H: isize = 2 * LINE_H + 4;

/// Who authored a line of the transcript (drives its prefix header).
#[derive(Clone, Copy)]
enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    fn header(self) -> &'static str {
        match self {
            Role::User => "> You",
            Role::Assistant => "* Ollama",
            Role::System => "-- system",
        }
    }
}

/// Approximate glyph metrics used to size the transcript layout. Heights come
/// from blitstr2 (Regular = 15 px, Large = 24 px); widths are proportional-font
/// estimates for the wrap column count.
fn font_char_w(large: bool) -> isize { if large { 13 } else { 8 } }
fn font_row_h(large: bool) -> isize { if large { 26 } else { 16 } }
fn font_style(large: bool) -> GlyphStyle { if large { GlyphStyle::Large } else { GlyphStyle::Regular } }

pub(crate) struct OllamaClient {
    gam: gam::Gam,
    _token: [u32; 4],
    gid: Gid,
    screensize: Point,
    modals: modals::Modals,

    config: Config,

    /// Conversation in ollama wire format — the context sent on each request.
    history: Vec<ChatMessage>,
    /// The transcript as (author, original text) — the source of truth that
    /// `lines` is (re-)wrapped from, e.g. when the font size changes.
    transcript: Vec<(Role, String)>,
    /// Word-wrapped display lines derived from `transcript` (what we render).
    lines: Vec<String>,
    /// Index of the first visible transcript line.
    scroll: usize,
    /// How many transcript lines fit on screen.
    visible_rows: usize,
    /// Max characters that fit on a line before wrapping.
    max_chars: usize,
    /// When true, draw the transcript in the Large glyph style.
    large_font: bool,

    /// When true, ↑/↓/←/→ scroll the transcript; when false they edit the input
    /// line (via the IME). Toggled with F3.
    scroll_mode: bool,

    /// True while a request is in flight (blocks further sends).
    busy: bool,
    status: String,

    /// Filled by the worker thread with the reply text or an error message.
    pending: Arc<Mutex<Option<Result<String, String>>>>,
    /// Connection back to our own server, so the worker can wake the UI.
    self_conn: xous::CID,

    /// Only paint when we hold focus (GAM may poke redraw while backgrounded).
    allow_redraw: bool,
    /// Signature of the last painted screen, to skip redundant redraws.
    last_paint_sig: u64,
}

impl OllamaClient {
    pub(crate) fn new(sid: xous::SID) -> Self {
        let xns = xous_names::XousNames::new().expect("couldn't connect to Xous Namespace Server");
        let gam = gam::Gam::new(&xns).expect("can't connect to Graphical Abstraction Manager");

        // Bring up our no-op predictor before registering, so GAM can connect to it
        // when the input line is first focused.
        crate::predictor::start();

        let token = gam
            .register_ux(UxRegistration {
                app_name: String::from(gam::APP_NAME_OLLAMA_CLIENT),
                // Chat gives us the standard predictive text-entry line; GAM shrinks
                // our content canvas to sit above it.
                ux_type: gam::UxType::Chat,
                // A predictor is required for the input line to function; ours offers
                // no suggestions (empty prediction bar).
                predictor: Some(String::from(crate::predictor::SERVER_NAME_OLLAMA_PREDICTOR)),
                listener: sid.to_array(),
                redraw_id: AppOp::Redraw.to_u32().unwrap(),
                // completed input lines (Enter) arrive here as a String
                gotinput_id: Some(AppOp::Line.to_u32().unwrap()),
                audioframe_id: None,
                focuschange_id: Some(AppOp::FocusChange.to_u32().unwrap()),
                // arrows / F-keys still reach us here, in parallel with the IME
                rawkeys_id: Some(AppOp::Rawkeys.to_u32().unwrap()),
            })
            .expect("couldn't register Ux context for ollama-client")
            .unwrap();

        let gid = gam.request_content_canvas(token).expect("couldn't get content canvas");
        // Bounds already exclude the input/prediction area GAM manages for us.
        let screensize = gam.get_canvas_bounds(gid).expect("couldn't get dimensions of content canvas");

        let modals = modals::Modals::new(&xns).expect("couldn't connect to Modals");
        let self_conn = xous::connect(sid).unwrap();

        let visible_rows = ((screensize.y - TRANSCRIPT_TOP - FOOTER_H) / font_row_h(false)).max(1) as usize;
        let max_chars = ((screensize.x - 12) / font_char_w(false)).max(8) as usize;

        // Config::load() blocks until the PDDB is mounted.
        let config = Config::load();

        let mut app = OllamaClient {
            gam,
            _token: token,
            gid,
            screensize,
            modals,
            config,
            history: Vec::new(),
            transcript: Vec::new(),
            lines: Vec::new(),
            scroll: 0,
            visible_rows,
            max_chars,
            large_font: false,
            scroll_mode: false,
            busy: false,
            status: String::new(),
            pending: Arc::new(Mutex::new(None)),
            self_conn,
            allow_redraw: false,
            last_paint_sig: 0,
        };

        app.add_message(Role::System, &format!("Ollama chat — model \"{}\"", app.config.model));
        if app.config.is_ready() {
            app.set_status("Type a message and press Enter.");
        } else {
            app.add_message(
                Role::System,
                "No server set. Press F1 to enter the ollama host and port, then F2 to pick a model.",
            );
            app.set_status("Press F1 to configure the server.");
        }
        app
    }

    // ---- transcript model -------------------------------------------------

    /// Append a message to the transcript and re-wrap the display.
    fn add_message(&mut self, role: Role, text: &str) {
        self.transcript.push((role, text.to_string()));
        self.rewrap();
    }

    /// Rebuild `lines` from `transcript` at the current wrap width: a blank
    /// separator between messages, a role header, then the word-wrapped body.
    fn rewrap(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        for (role, text) in &self.transcript {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(role.header().to_string());
            for l in wrap(text, self.max_chars) {
                lines.push(l);
            }
        }
        self.lines = lines;
    }

    /// Recompute wrap width + visible-row count for the current font, then
    /// re-wrap and clamp the scroll position.
    fn recompute_layout(&mut self) {
        self.max_chars = ((self.screensize.x - 12) / font_char_w(self.large_font)).max(8) as usize;
        self.visible_rows =
            ((self.screensize.y - TRANSCRIPT_TOP - FOOTER_H) / font_row_h(self.large_font)).max(1) as usize;
        self.rewrap();
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn set_font(&mut self, large: bool) {
        self.large_font = large;
        self.recompute_layout();
        self.set_status(if large { "Large font." } else { "Regular font." });
        self.force_redraw();
    }

    fn max_scroll(&self) -> usize { self.lines.len().saturating_sub(self.visible_rows) }

    /// Character budget for the chrome (title/status/hints), which is always drawn
    /// in the Regular font — so it must NOT use `max_chars`, which tracks the
    /// (possibly Large) transcript font and would over-truncate these lines.
    fn chrome_chars(&self) -> usize { ((self.screensize.x - 8) / font_char_w(false)).max(8) as usize }

    fn overflowing(&self) -> bool { self.lines.len() > self.visible_rows }

    fn scroll_by(&mut self, delta: isize) {
        let target = (self.scroll as isize + delta).max(0) as usize;
        self.scroll = target.min(self.max_scroll());
        self.force_redraw();
    }

    // ---- input / actions --------------------------------------------------

    /// Raw arrow / function keys, delivered in parallel with the IME. Printable
    /// keys, backspace and Enter are handled by the IME (Enter → `submit_line`),
    /// so we deliberately ignore them here.
    pub(crate) fn key(&mut self, k: char) {
        match k {
            // F1 server settings; F2 pick a model; F3 toggle scroll/edit;
            // F4 display menu (font size / clear).
            '\u{11}' => self.edit_settings(),
            '\u{12}' => self.select_model(),
            '\u{13}' => self.toggle_scroll_mode(),
            '\u{14}' => self.display_menu(),
            // Arrows only act in scroll mode; in edit mode the IME uses them to
            // move the input cursor, so we leave them alone.
            '↑' if self.scroll_mode => self.scroll_by(-1),
            '↓' if self.scroll_mode => self.scroll_by(1),
            '←' if self.scroll_mode => self.scroll_by(-(self.visible_rows as isize - 1).max(1)),
            '→' if self.scroll_mode => self.scroll_by((self.visible_rows as isize - 1).max(1)),
            _ => {}
        }
    }

    fn toggle_scroll_mode(&mut self) {
        self.scroll_mode = !self.scroll_mode;
        self.set_status(if self.scroll_mode {
            "Scroll mode. ↑↓ line, ←→ page."
        } else {
            "Edit mode. Type your message."
        });
        self.force_redraw();
    }

    /// F4: a small display menu — toggle the transcript font size, or clear.
    fn display_menu(&mut self) {
        let font_item =
            if self.large_font { "Font size: switch to Regular" } else { "Font size: switch to Large" };
        self.modals.add_list_item(font_item).ok();
        self.modals.add_list_item("Clear conversation").ok();
        self.modals.add_list_item("Cancel").ok();
        match self.modals.get_radiobutton("Display:") {
            Ok(choice) if choice == font_item => self.set_font(!self.large_font),
            Ok(choice) if choice == "Clear conversation" => self.clear_conversation(),
            _ => self.force_redraw(),
        }
    }

    /// A finished prompt line committed from the IME (user pressed Enter).
    pub(crate) fn submit_line(&mut self, text: &str) {
        let prompt = text.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        if self.busy {
            self.set_status("Still waiting on the previous reply…");
            self.force_redraw();
            return;
        }
        if !self.config.is_ready() {
            self.edit_settings();
            if !self.config.is_ready() {
                return;
            }
        }

        // Submitting means we're composing again — return to edit mode.
        self.scroll_mode = false;
        self.add_message(Role::User, &prompt);
        self.history.push(ChatMessage::user(&prompt));
        self.busy = true;
        self.set_status(&format!("Thinking… ({})", self.config.model));
        self.scroll = self.max_scroll();
        self.force_redraw();

        // Hand the request to a worker thread so the UI stays responsive (the user
        // can still toggle scroll mode and read while the model is thinking).
        let pending = Arc::clone(&self.pending);
        let cid = self.self_conn;
        let config = self.config.clone();
        let history = self.history.clone();
        std::thread::spawn(move || {
            let result = net::chat(&config, &history);
            *pending.lock().unwrap() = Some(result);
            xous::send_message(
                cid,
                xous::Message::new_scalar(AppOp::ResponseReady.to_usize().unwrap(), 0, 0, 0, 0),
            )
            .ok();
        });
    }

    /// Handle the worker thread's result (called from the main loop on wake-up).
    pub(crate) fn on_response(&mut self) {
        let result = self.pending.lock().unwrap().take();
        self.busy = false;
        let reply_top = self.lines.len();
        match result {
            Some(Ok(reply)) => {
                let reply = reply.trim().to_string();
                self.history.push(ChatMessage::assistant(&reply));
                self.add_message(Role::Assistant, if reply.is_empty() { "(empty reply)" } else { &reply });
                self.set_status("Reply received. F3 to scroll.");
            }
            Some(Err(e)) => {
                // Drop the failed turn from the context so a retry isn't poisoned.
                self.history.pop();
                self.add_message(Role::System, &format!("Error: {}", e));
                self.set_status("Request failed — see message above.");
            }
            None => return, // spurious wake-up; nothing to do
        }
        // Anchor the view at the top of the just-added reply, and if it runs off
        // the bottom of the screen switch to scroll mode so the arrows read it.
        self.scroll = reply_top.min(self.max_scroll());
        if self.overflowing() {
            self.scroll_mode = true;
        }
        self.force_redraw();
    }

    /// Fetch the server's installed models and let the user pick one.
    fn select_model(&mut self) {
        if self.busy {
            return;
        }
        if !self.config.is_ready() {
            self.edit_settings();
            if !self.config.is_ready() {
                return;
            }
        }
        self.set_status("Fetching model list…");
        self.force_redraw(); // flush the status before the blocking fetch

        match net::list_models(&self.config) {
            Ok(models) if !models.is_empty() => {
                for m in &models {
                    self.modals.add_list_item(m).ok();
                }
                match self.modals.get_radiobutton("Select model:") {
                    Ok(choice) => {
                        self.config.model = choice.clone();
                        self.config.save();
                        self.add_message(Role::System, &format!("Model set to \"{}\".", choice));
                        self.set_status("Type a message and press Enter.");
                        self.scroll = self.max_scroll();
                    }
                    Err(_) => self.set_status("Model selection cancelled."),
                }
            }
            Ok(_) => {
                self.modals
                    .show_notification(
                        "No models installed on the server. Pull one there with `ollama pull <model>`.",
                        None,
                    )
                    .ok();
                self.set_status("No models on server.");
            }
            Err(e) => {
                self.modals.show_notification(&format!("Could not list models:\n\n{}", e), None).ok();
                self.set_status("Couldn't list models — check F1.");
            }
        }
        self.force_redraw();
    }

    fn clear_conversation(&mut self) {
        self.history.clear();
        self.lines.clear();
        self.scroll = 0;
        self.scroll_mode = false;
        self.add_message(Role::System, &format!("Conversation cleared — model \"{}\".", self.config.model));
        self.set_status("Type a message and press Enter.");
        self.force_redraw();
    }

    /// Prompt for host / port / model in one modal and persist the result.
    fn edit_settings(&mut self) {
        let host = if self.config.host.is_empty() { "192.168.1.20".to_string() } else { self.config.host.clone() };
        let port = self.config.port.to_string();
        let model =
            if self.config.model.is_empty() { DEFAULT_MODEL.to_string() } else { self.config.model.clone() };

        let payloads = {
            let mut builder = self.modals.alert_builder("Ollama server settings");
            let builder = builder.field_placeholder_persist(Some(host), None);
            let builder = builder.field_placeholder_persist(Some(port), None);
            let builder = builder.field_placeholder_persist(Some(model), None);
            match builder.build() {
                Ok(p) => p,
                Err(_) => return, // dismissed — keep existing settings
            }
        };
        let content = payloads.content();
        let host = content[0].content.as_str().trim().to_string();
        let port_str = content[1].content.as_str().trim().to_string();
        let model = content[2].content.as_str().trim().to_string();

        self.config.host = host;
        if let Ok(p) = port_str.parse::<u16>() {
            self.config.port = p;
        }
        if !model.is_empty() {
            self.config.model = model;
        }
        self.config.save();

        self.add_message(
            Role::System,
            &format!("Server set to {}:{}, model \"{}\".", self.config.host, self.config.port, self.config.model),
        );
        self.set_status(if self.config.is_ready() {
            "Type a message and press Enter."
        } else {
            "Host still empty — press F1 again."
        });
        self.scroll = self.max_scroll();
        self.force_redraw();
    }

    fn set_status(&mut self, s: &str) {
        self.status.clear();
        self.status.push_str(s);
    }

    // ---- drawing ----------------------------------------------------------

    pub(crate) fn on_focus(&mut self, foreground: bool) {
        self.allow_redraw = foreground;
        if foreground {
            self.force_redraw();
        }
    }

    /// GAM-requested repaint: skip it if backgrounded or nothing visible changed.
    pub(crate) fn redraw(&mut self) {
        if self.allow_redraw && self.paint_sig() != self.last_paint_sig {
            self.force_redraw();
        }
    }

    /// A cheap signature of everything drawn on screen, to skip redundant redraws.
    fn paint_sig(&self) -> u64 {
        let mut h: u64 = 1469598103934665603; // FNV-1a
        let mut mix = |b: &[u8]| {
            for &x in b {
                h ^= x as u64;
                h = h.wrapping_mul(1099511628211);
            }
        };
        mix(&(self.scroll as u64).to_le_bytes());
        mix(&(self.lines.len() as u64).to_le_bytes());
        mix(&[self.busy as u8, self.scroll_mode as u8, self.large_font as u8]);
        mix(self.status.as_bytes());
        h
    }

    /// Unconditional repaint of our content canvas.
    pub(crate) fn force_redraw(&mut self) {
        if !self.allow_redraw {
            return;
        }
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

        // title bar: model + mode + a scroll position indicator on the right.
        let mut title = String::new();
        write!(title, "Ollama · {} · {}", self.config.model, if self.scroll_mode { "SCROLL" } else { "EDIT" })
            .ok();
        self.text(4, 2, &truncate(&title, self.chrome_chars().saturating_sub(9)));
        if self.overflowing() {
            let last = (self.scroll + self.visible_rows).min(self.lines.len());
            let mut pos = String::new();
            write!(pos, "{}-{}/{}", self.scroll + 1, last, self.lines.len()).ok();
            let x = self.screensize.x - (pos.chars().count() as isize * 8) - 4;
            self.text(x.max(0), 2, &pos);
        }

        // transcript window (drawn in the selected font; chrome stays Regular)
        let row_h = font_row_h(self.large_font);
        let style = font_style(self.large_font);
        for i in 0..self.visible_rows {
            let idx = self.scroll + i;
            if idx >= self.lines.len() {
                break;
            }
            let y = TRANSCRIPT_TOP + (i as isize) * row_h;
            self.text_styled(6, y, &truncate(&self.lines[idx], self.max_chars), style);
        }

        // footer: status line + key hints (the input line itself is drawn by GAM)
        let fy = self.screensize.y - FOOTER_H;
        self.text(4, fy, &truncate(&self.status, self.chrome_chars()));
        let hint = if self.scroll_mode {
            "↑↓ line  ←→ page  F3edit F4disp"
        } else {
            "type+⏎  F1srv F2mdl F3scrl F4disp"
        };
        self.text(4, fy + LINE_H, &truncate(hint, self.chrome_chars()));

        self.gam.redraw().unwrap();
    }

    /// Draw chrome (title/status/hints) in the Regular glyph style.
    fn text(&self, x: isize, y: isize, s: &str) { self.text_styled(x, y, s, GlyphStyle::Regular); }

    fn text_styled(&self, x: isize, y: isize, s: &str, style: GlyphStyle) {
        let mut tv = TextView::new(
            self.gid,
            TextBounds::GrowableFromTl(Point::new(x, y), (self.screensize.x - x).max(1) as u16),
        );
        tv.draw_border = false;
        tv.clear_area = false;
        tv.margin = Point::new(0, 0);
        tv.style = style;
        write!(tv.text, "{}", s).ok();
        self.gam.post_textview(&mut { tv }).ok();
    }
}

/// Word-wrap `text` to `width` columns. Preserves paragraph breaks (`\n`) and
/// hard-splits any single word longer than `width`.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let words: Vec<&str> = para.split_whitespace().collect();
        if words.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_len = 0usize;
        for word in words {
            let wlen = word.chars().count();
            if wlen > width {
                // flush the current line, then emit the long word in width-sized chunks
                if cur_len > 0 {
                    out.push(std::mem::take(&mut cur));
                    cur_len = 0;
                }
                let mut chunk = String::new();
                let mut clen = 0;
                for ch in word.chars() {
                    chunk.push(ch);
                    clen += 1;
                    if clen == width {
                        out.push(std::mem::take(&mut chunk));
                        clen = 0;
                    }
                }
                if clen > 0 {
                    cur = chunk;
                    cur_len = clen;
                }
            } else if cur_len == 0 {
                cur.push_str(word);
                cur_len = wlen;
            } else if cur_len + 1 + wlen <= width {
                cur.push(' ');
                cur.push_str(word);
                cur_len += 1 + wlen;
            } else {
                out.push(std::mem::take(&mut cur));
                cur.push_str(word);
                cur_len = wlen;
            }
        }
        if cur_len > 0 {
            out.push(cur);
        }
    }
    out
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
