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
//! A send blocks (network + inference), so it runs on a worker thread that
//! streams the reply into a shared [`Request`], pinging our server with
//! `AppOp::Progress` as text arrives and `AppOp::ResponseReady` at the end. A
//! second watchdog thread ticks the status line once a second and decides when a
//! request has died: while waiting for the first words it probes the server every
//! [`PROBE_EVERY`], and once text is flowing it gives up after [`STALL_LIMIT`] of
//! silence. (Xous TCP has no keep-alives, so a silent connection can't be asked
//! whether it's still there.)

use core::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// While waiting for the first words of a reply, check the server is still up
/// this often.
const PROBE_EVERY: Duration = Duration::from_secs(15);
/// Give up waiting after this many failed probes in a row.
const PROBE_FAILURES_TO_GIVE_UP: u32 = 2;
/// Once a reply is streaming, this long without any data means it has stalled.
const STALL_LIMIT: Duration = Duration::from_secs(90);
/// Redraw a streaming reply at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// checking the TLS certificate / opening the connection
    Connecting,
    /// request sent; nothing back yet (prompt evaluation, model load)
    Waiting,
    /// reply chunks are arriving
    Receiving,
}

/// One chat request in flight, shared between the UI, the worker thread that
/// streams the reply, and the watchdog thread. Each request gets its own, so a
/// worker that outlives its request (abandoned after a stall) can't disturb the
/// next one.
struct Request {
    started: Instant,
    phase: Phase,
    /// the reply so far
    text: String,
    /// the latest chunk was reasoning rather than answer text
    reasoning: bool,
    last_data: Instant,
    /// result of the latest server probe, if any has finished
    server_ok: Option<bool>,
    probe_failures: u32,
    probing: bool,
    /// set when the request ends, by whichever thread ends it
    outcome: Option<Result<String, String>>,
    /// tells the worker to stop reading
    cancelled: bool,
}

impl Request {
    fn new() -> Self {
        let now = Instant::now();
        Request {
            started: now,
            phase: Phase::Connecting,
            text: String::new(),
            reasoning: false,
            last_data: now,
            server_ok: None,
            probe_failures: 0,
            probing: false,
            outcome: None,
            cancelled: false,
        }
    }

    /// End the request with an error, unless it has already ended.
    fn fail(&mut self, msg: &str) {
        if self.outcome.is_none() {
            self.outcome = Some(Err(String::from(msg)));
        }
        self.cancelled = true;
    }
}

fn ping(cid: xous::CID, op: AppOp) {
    xous::send_message(cid, xous::Message::new_scalar(op.to_usize().unwrap(), 0, 0, 0, 0)).ok();
}

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

/// Glyph metrics used to size the layout. Heights come from blitstr2
/// (Regular = 15 px, Large = 24 px). `font_char_w` is a rough Regular-font
/// estimate, used only to truncate the chrome lines; the transcript is wrapped
/// by real glyph widths (see [`glyph_advance`]).
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
    /// Index in `lines` where the last transcript message starts, so a streaming
    /// reply can be re-wrapped without redoing the whole transcript.
    last_msg_line: usize,
    /// Index of the first visible transcript line.
    scroll: usize,
    /// How many transcript lines fit on screen.
    visible_rows: usize,
    /// Pixel width a transcript line may fill before wrapping.
    max_px: isize,
    /// When true, draw the transcript in the Large glyph style.
    large_font: bool,

    /// When true, ↑/↓/←/→ scroll the transcript; when false they edit the input
    /// line (via the IME). Toggled with F3.
    scroll_mode: bool,

    /// True while a request is in flight (blocks further sends).
    busy: bool,
    status: String,

    /// The request in flight, if any.
    request: Option<Arc<Mutex<Request>>>,
    /// Transcript index of the reply being streamed, once its first words arrive.
    streaming_msg: Option<usize>,
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
        let max_px = transcript_width(screensize, false);

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
            last_msg_line: 0,
            scroll: 0,
            visible_rows,
            max_px,
            large_font: false,
            scroll_mode: false,
            busy: false,
            status: String::new(),
            request: None,
            streaming_msg: None,
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
        self.push_lines(self.transcript.len() - 1);
    }

    /// Append the display lines for transcript message `i`: a blank separator
    /// (except before the first), a role header, then the word-wrapped body.
    fn push_lines(&mut self, i: usize) {
        if !self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.last_msg_line = self.lines.len();
        let (role, text) = &self.transcript[i];
        self.lines.push(role.header().to_string());
        let body = wrap(text, self.max_px, self.large_font);
        self.lines.extend(body);
    }

    /// Rebuild `lines` from `transcript` at the current wrap width.
    fn rewrap(&mut self) {
        self.lines.clear();
        for i in 0..self.transcript.len() {
            self.push_lines(i);
        }
    }

    /// Re-wrap only the last message (a reply that's still streaming in).
    fn rewrap_last(&mut self) {
        if self.transcript.is_empty() {
            return;
        }
        // drop its lines and the separator before it, then lay it out again
        let cut = if self.last_msg_line > 0 { self.last_msg_line - 1 } else { 0 };
        self.lines.truncate(cut);
        self.push_lines(self.transcript.len() - 1);
    }

    /// Recompute wrap width + visible-row count for the current font, then
    /// re-wrap and clamp the scroll position.
    fn recompute_layout(&mut self) {
        self.max_px = transcript_width(self.screensize, self.large_font);
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
    /// in the Regular font, whatever the transcript font is.
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
        const STOP_ITEM: &str = "Stop waiting for the reply";
        if self.busy {
            self.modals.add_list_item(STOP_ITEM).ok();
        }
        let font_item =
            if self.large_font { "Font size: switch to Regular" } else { "Font size: switch to Large" };
        self.modals.add_list_item(font_item).ok();
        self.modals.add_list_item("Clear conversation").ok();
        self.modals.add_list_item("Cancel").ok();
        match self.modals.get_radiobutton("Display:") {
            Ok(choice) if choice == STOP_ITEM => self.cancel_request(),
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
            self.edit_server_address();
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
        let request = Arc::new(Mutex::new(Request::new()));
        self.request = Some(Arc::clone(&request));
        self.streaming_msg = None;
        let cid = self.self_conn;
        let config = self.config.clone();
        let history = self.history.clone();
        {
            let request = Arc::clone(&request);
            let config = config.clone();
            std::thread::spawn(move || {
                let mut last_progress = Instant::now();
                let result = net::chat_stream(
                    &config,
                    &history,
                    || request.lock().unwrap().phase = Phase::Waiting,
                    |content, reasoning| {
                        let mut r = request.lock().unwrap();
                        if r.cancelled {
                            return false;
                        }
                        r.phase = Phase::Receiving;
                        r.last_data = Instant::now();
                        r.reasoning = reasoning;
                        r.text.push_str(content);
                        drop(r);
                        if last_progress.elapsed() >= PROGRESS_EVERY {
                            last_progress = Instant::now();
                            ping(cid, AppOp::Progress);
                        }
                        true
                    },
                );
                let mut r = request.lock().unwrap();
                // the watchdog may already have given up on this request
                if r.outcome.is_none() {
                    r.outcome = Some(result);
                }
                drop(r);
                ping(cid, AppOp::ResponseReady);
            });
        }
        std::thread::spawn(move || watchdog(request, config, cid));
    }

    /// The streaming reply has grown: show the new text.
    pub(crate) fn on_progress(&mut self) {
        let changed = self.sync_streaming_text();
        self.update_busy_status();
        // new text can land within the last line, which `paint_sig` doesn't see
        if changed {
            self.force_redraw();
        } else {
            self.redraw();
        }
    }

    /// Once a second while a request is in flight: refresh the elapsed time.
    pub(crate) fn on_tick(&mut self) {
        if self.request.is_some() {
            self.update_busy_status();
            self.redraw();
        }
    }

    /// Copy the request's text so far into the transcript, adding the reply
    /// message when the first words arrive. Returns whether anything changed.
    fn sync_streaming_text(&mut self) -> bool {
        let text = match &self.request {
            Some(r) => r.lock().unwrap().text.clone(),
            None => return false,
        };
        if text.is_empty() {
            return false;
        }
        match self.streaming_msg {
            Some(i) => {
                if self.transcript[i].1.len() == text.len() {
                    return false;
                }
                self.transcript[i].1 = text;
                self.rewrap_last();
            }
            None => {
                self.add_message(Role::Assistant, &text);
                self.streaming_msg = Some(self.transcript.len() - 1);
            }
        }
        // follow the text as it arrives, unless the user is scrolling around
        if !self.scroll_mode {
            self.scroll = self.max_scroll();
        }
        true
    }

    fn update_busy_status(&mut self) {
        let status = match &self.request {
            Some(r) => {
                let r = r.lock().unwrap();
                let secs = r.started.elapsed().as_secs();
                match r.phase {
                    Phase::Connecting => format!("Connecting… {}s", secs),
                    Phase::Waiting => match r.server_ok {
                        Some(true) => format!("Thinking… {}s, server OK", secs),
                        Some(false) => format!("Thinking… {}s, server not answering", secs),
                        None => format!("Thinking… {}s", secs),
                    },
                    Phase::Receiving if r.reasoning && r.text.is_empty() => format!("Reasoning… {}s", secs),
                    Phase::Receiving => format!("Receiving… {}s", secs),
                }
            }
            None => return,
        };
        self.set_status(&status);
    }

    /// F4 while busy: stop waiting. The worker notices and drops the connection.
    fn cancel_request(&mut self) {
        if let Some(r) = &self.request {
            r.lock().unwrap().fail("Cancelled.");
        }
        self.on_response();
    }

    /// The request has ended (finished, failed, stalled, or cancelled).
    pub(crate) fn on_response(&mut self) {
        let outcome = match &self.request {
            Some(r) => r.lock().unwrap().outcome.take(),
            None => None,
        };
        // a stale wake-up from an abandoned worker, or the request isn't over yet
        let outcome = match outcome {
            Some(o) => o,
            None => return,
        };
        // pick up any text that arrived since the last progress update
        self.sync_streaming_text();
        self.request = None;
        let streamed = self.streaming_msg.take();
        self.busy = false;
        // where the reply starts on screen, to anchor the view below
        let reply_top = match streamed {
            Some(_) => self.last_msg_line,
            None => self.lines.len(),
        };
        match outcome {
            Ok(reply) => {
                let reply = reply.trim().to_string();
                self.history.push(ChatMessage::assistant(&reply));
                match streamed {
                    Some(i) => {
                        self.transcript[i].1 = reply;
                        self.rewrap_last();
                    }
                    None => self.add_message(
                        Role::Assistant,
                        if reply.is_empty() { "(empty reply)" } else { &reply },
                    ),
                }
                self.set_status("Reply received. F3 to scroll.");
            }
            Err(e) => {
                if let Some(i) = streamed {
                    self.transcript[i].1.push_str(" […cut off]");
                    self.rewrap_last();
                }
                // Drop the failed turn from the context so a retry isn't poisoned.
                self.history.pop();
                self.add_message(Role::System, &format!("Error: {}", e));
                self.set_status(if e == "Cancelled." {
                    "Cancelled."
                } else {
                    "Request failed — see message above."
                });
            }
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
            self.edit_server_address();
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
        self.transcript.clear();
        self.lines.clear();
        self.scroll = 0;
        self.scroll_mode = false;
        self.add_message(Role::System, &format!("Conversation cleared — model \"{}\".", self.config.model));
        self.set_status("Type a message and press Enter.");
        self.force_redraw();
    }

    /// F1 server settings — a small menu (mail-style) so each concern gets its
    /// own dialog. The connection entry shows the current scheme inline, so the
    /// user can see whether HTTPS is on without opening the sub-dialog.
    fn edit_settings(&mut self) {
        const ADDRESS: &str = "Server address & model";
        let conn_item = format!("Connection: {}", if self.config.use_tls { "HTTPS" } else { "HTTP" });
        self.modals.add_list_item(ADDRESS).ok();
        self.modals.add_list_item(&conn_item).ok();
        self.modals.add_list_item("Cancel").ok();
        match self.modals.get_radiobutton("Server settings:") {
            Ok(choice) if choice == ADDRESS => self.edit_server_address(),
            Ok(choice) if choice == conn_item => self.edit_connection(),
            _ => self.force_redraw(),
        }
    }

    /// F1 -> "Connection": a checkbox toggling HTTPS (TLS). Pre-checked with the
    /// current state; dismissing leaves it unchanged.
    fn edit_connection(&mut self) {
        const HTTPS: &str = "Use HTTPS (TLS)";
        self.modals.add_stateful_list_item(self.config.use_tls, HTTPS).ok();
        let checked = match self.modals.get_checkbox("Connection") {
            Ok(c) => c,
            Err(_) => return, // dismissed: leave the setting unchanged
        };
        self.config.use_tls = checked.iter().any(|s| s == HTTPS);
        self.config.save();
        self.add_message(
            Role::System,
            &format!("Connection set to {}.", if self.config.use_tls { "HTTPS" } else { "HTTP" }),
        );
        self.scroll = self.max_scroll();
        self.force_redraw();
    }

    /// Prompt for host / port / model in one modal and persist the result.
    fn edit_server_address(&mut self) {
        let host =
            if self.config.host.is_empty() { "192.168.1.20".to_string() } else { self.config.host.clone() };
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
            &format!(
                "Server set to {}:{}, model \"{}\".",
                self.config.host, self.config.port, self.config.model
            ),
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
        write!(
            title,
            "Ollama · {} · {}",
            self.config.model,
            if self.scroll_mode { "SCROLL" } else { "EDIT" }
        )
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
            // already wrapped to `max_px`, so no truncation needed
            self.text_styled(6, y, &self.lines[idx], style);
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

/// Runs alongside each request until it ends: ticks the UI once a second, probes
/// the server while no reply has started, and fails the request if the server
/// stops answering or a streaming reply stalls.
fn watchdog(request: Arc<Mutex<Request>>, config: Config, cid: xous::CID) {
    let mut next_probe = Instant::now() + PROBE_EVERY;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        {
            let mut r = request.lock().unwrap();
            if r.outcome.is_some() || r.cancelled {
                return;
            }
            match r.phase {
                Phase::Waiting if r.probe_failures >= PROBE_FAILURES_TO_GIVE_UP => {
                    r.fail(
                        "The server stopped answering while the reply was being prepared. Check \
                         Wi-Fi and that ollama is still running, then send your message again.",
                    );
                    drop(r);
                    ping(cid, AppOp::ResponseReady);
                    return;
                }
                Phase::Receiving if r.last_data.elapsed() >= STALL_LIMIT => {
                    r.fail(&format!(
                        "The reply stalled: nothing arrived for {} seconds, so the connection was \
                         probably lost. Send your message again to retry.",
                        STALL_LIMIT.as_secs()
                    ));
                    drop(r);
                    ping(cid, AppOp::ResponseReady);
                    return;
                }
                Phase::Waiting if !r.probing && Instant::now() >= next_probe => {
                    // probe on its own thread, so the ticks keep coming
                    r.probing = true;
                    next_probe = Instant::now() + PROBE_EVERY;
                    let request = Arc::clone(&request);
                    let config = config.clone();
                    std::thread::spawn(move || {
                        let ok = net::probe(&config);
                        let mut r = request.lock().unwrap();
                        r.probing = false;
                        r.server_ok = Some(ok);
                        r.probe_failures = if ok { 0 } else { r.probe_failures + 1 };
                    });
                }
                _ => {}
            }
        }
        ping(cid, AppOp::Tick);
    }
}

/// Transcript lines start at x = 6. Leave a gap on the right so the TextView
/// never needs to wrap a line itself; Large glyphs are drawn doubled and can
/// spill a few pixels past their measured width, so they get a wider gap.
fn transcript_width(screensize: Point, large: bool) -> isize {
    let right_gap = if large { 22 } else { 8 };
    (screensize.x - 6 - right_gap).max(40)
}

/// Horizontal advance of one glyph in pixels, as the GAM typesetter lays it
/// out: glyph width plus kerning, with spaces unkerned. Large is the Small font
/// drawn at double size. Characters outside the Latin fonts (emoji, CJK) are
/// given a full-square width.
fn glyph_advance(ch: char, large: bool) -> isize {
    let glyph = if large { blitstr2::large_glyph(ch) } else { blitstr2::regular_glyph(ch) };
    match glyph {
        Ok(g) if ch == ' ' => g.wide as isize,
        Ok(g) => (g.wide + g.kern) as isize,
        Err(_) => {
            if large {
                32
            } else {
                16
            }
        }
    }
}

fn text_px(s: &str, large: bool) -> isize { s.chars().map(|c| glyph_advance(c, large)).sum() }

/// Word-wrap `text` to `width` pixels in the Regular or Large font. Preserves
/// paragraph breaks (`\n`) and hard-splits any single word wider than a line.
fn wrap(text: &str, width: isize, large: bool) -> Vec<String> {
    let space = glyph_advance(' ', large);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let words: Vec<&str> = para.split_whitespace().collect();
        if words.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_px = 0isize;
        for word in words {
            let wpx = text_px(word, large);
            if wpx > width {
                // flush the current line, then emit the long word in line-sized chunks
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut chunk = String::new();
                let mut cpx = 0;
                for ch in word.chars() {
                    let adv = glyph_advance(ch, large);
                    if cpx + adv > width && !chunk.is_empty() {
                        out.push(std::mem::take(&mut chunk));
                        cpx = 0;
                    }
                    chunk.push(ch);
                    cpx += adv;
                }
                cur = chunk;
                cur_px = cpx;
            } else if cur.is_empty() {
                cur.push_str(word);
                cur_px = wpx;
            } else if cur_px + space + wpx <= width {
                cur.push(' ');
                cur.push_str(word);
                cur_px += space + wpx;
            } else {
                out.push(std::mem::take(&mut cur));
                cur.push_str(word);
                cur_px = wpx;
            }
        }
        if !cur.is_empty() {
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
