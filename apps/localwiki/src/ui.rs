//! The reader: one page at a time, with its links underlined.
//!
//! Keys:
//!   * ↑ ↓ scroll a line, space scrolls a page
//!   * ← → select the previous / next link (the selected link is boxed)
//!   * ⏎ or the centre key opens the selected link; ⌫ goes back
//!   * F1 (or w) wiki menu: open another wiki, import from a URL, delete
//!   * F2 (or i) index of every page; F3 (or h) the home page
//!   * F4 cycles the text size; + and - step it
//!
//! Importing works like apps/sidplayer: point it at an HTTP(S) directory index and every
//! wiki `.json` linked there is downloaded. A wiki with the same name is replaced.

use core::fmt::Write as _;

use gam::*;
use num_traits::ToPrimitive;

use crate::AppOp;
use crate::layout::{self, FontSize, Run, TextLine};
use crate::netfetch;
use crate::store::Store;
use crate::wiki::{self, Segment, Wiki};

/// Pitch of the chrome lines (title, status, hints), which are always Regular.
const LINE_H: isize = 16;
/// Top of the page text, below the title bar and its rule.
const BODY_TOP: isize = 24;
/// Status line + two key-hint lines, plus a rule above them.
const FOOTER_H: isize = 3 * LINE_H + 8;
/// Left edge of the page text.
const LEFT: isize = 6;
/// How many pages ⌫ can step back through.
const MAX_HISTORY: usize = 64;

const WELCOME: &str = "No wikis yet.\n\n\
    In PaklWiki on iPhone or Mac, choose Export → JSON for Precursor, and put the \
    .json file in a folder on a web server.\n\n\
    Then press F1, choose Import, and enter the folder's address (or the file's). \
    Every wiki file linked there is downloaded; a wiki with the same name is replaced.";

#[derive(Clone, Copy, PartialEq)]
enum View {
    Page(usize),
    Index,
}

/// Somewhere ⌫ can return to.
#[derive(Clone, Copy)]
struct Place {
    view: View,
    scroll: usize,
    selected: Option<usize>,
}

pub(crate) struct LocalWiki {
    gam: gam::Gam,
    _token: [u32; 4],
    gid: Gid,
    screensize: Point,
    modals: modals::Modals,
    store: Store,

    /// The open wiki and its key in the store.
    wiki: Option<Wiki>,
    wiki_key: Option<String>,
    view: View,
    history: Vec<Place>,

    /// The current view, wrapped for the current font and width.
    lines: Vec<TextLine>,
    /// The page each link in the current view opens.
    links: Vec<usize>,
    /// First and last line of each link.
    link_rows: Vec<(usize, usize)>,
    selected: Option<usize>,
    /// Index of the first visible line.
    scroll: usize,
    visible_rows: usize,
    /// Pixel width a line may fill before wrapping.
    max_px: isize,
    font: FontSize,

    status: String,
    /// Only paint when we hold focus (GAM may poke redraw while backgrounded).
    allow_redraw: bool,
    /// Signature of the last painted screen, to skip redundant redraws.
    last_paint_sig: u64,
}

impl LocalWiki {
    pub(crate) fn new(sid: xous::SID) -> Self {
        let xns = xous_names::XousNames::new().expect("couldn't connect to Xous Namespace Server");
        let gam = gam::Gam::new(&xns).expect("can't connect to Graphical Abstraction Manager");

        let token = gam
            .register_ux(UxRegistration {
                app_name: String::from(gam::APP_NAME_LOCALWIKI),
                ux_type: gam::UxType::Framebuffer,
                predictor: None,
                listener: sid.to_array(),
                redraw_id: AppOp::Redraw.to_u32().unwrap(),
                gotinput_id: None,
                audioframe_id: None,
                focuschange_id: Some(AppOp::FocusChange.to_u32().unwrap()),
                rawkeys_id: Some(AppOp::Rawkeys.to_u32().unwrap()),
            })
            .expect("couldn't register Ux context for localwiki")
            .unwrap();

        let gid = gam.request_content_canvas(token).expect("couldn't get content canvas");
        let screensize = gam.get_canvas_bounds(gid).expect("couldn't get dimensions of content canvas");
        let modals = modals::Modals::new(&xns).expect("couldn't connect to Modals");

        let store = Store::new();
        store.wait_mounted();
        let font = store.font().unwrap_or(FontSize::Regular);

        let mut app = LocalWiki {
            gam,
            _token: token,
            gid,
            screensize,
            modals,
            store,
            wiki: None,
            wiki_key: None,
            view: View::Index,
            history: Vec::new(),
            lines: Vec::new(),
            links: Vec::new(),
            link_rows: Vec::new(),
            selected: None,
            scroll: 0,
            visible_rows: 1,
            max_px: 40,
            font,
            status: String::new(),
            allow_redraw: false,
            last_paint_sig: u64::MAX, // force the first paint
        };
        app.recompute_metrics();

        // Reopen the wiki read last, or else the first one stored.
        let names = app.store.names();
        let last = app.store.last_wiki().filter(|name| names.contains(name));
        match last.or_else(|| names.first().cloned()) {
            Some(key) => {
                if !app.open_wiki(&key) {
                    app.show(View::Index, 0, None);
                }
            }
            None => app.show(View::Index, 0, None),
        }
        app
    }

    // ---- wikis ------------------------------------------------------------

    /// Open a stored wiki at its home page. Returns false (after saying why) if it
    /// can't be read.
    fn open_wiki(&mut self, key: &str) -> bool {
        let parsed = match self.store.read(key) {
            Some(bytes) => Wiki::parse(&bytes),
            None => Err(String::from("it couldn't be read from storage")),
        };
        match parsed {
            Ok(wiki) => {
                let start = wiki.home().map(View::Page).unwrap_or(View::Index);
                self.wiki = Some(wiki);
                self.wiki_key = Some(key.to_string());
                self.store.set_last_wiki(key);
                self.history.clear();
                self.status.clear();
                self.show(start, 0, None);
                true
            }
            Err(e) => {
                self.modals.show_notification(&format!("Couldn't open “{}”: {}.", key, e), None).ok();
                self.force_redraw();
                false
            }
        }
    }

    /// F1: open another wiki, import, or delete the open one.
    fn wiki_menu(&mut self) {
        const OPEN: &str = "Open another wiki…";
        const IMPORT: &str = "Import wikis from a URL…";
        let names = self.store.names();
        let others = names.iter().filter(|n| Some(*n) != self.wiki_key.as_ref()).count();
        let delete = self.wiki_key.as_ref().map(|key| format!("Delete “{}”…", key));

        if others > 0 {
            self.modals.add_list_item(OPEN).ok();
        }
        self.modals.add_list_item(IMPORT).ok();
        if let Some(delete) = &delete {
            self.modals.add_list_item(delete).ok();
        }
        self.modals.add_list_item("Cancel").ok();
        match self.modals.get_radiobutton("Wikis:") {
            Ok(choice) if choice == OPEN => self.pick_wiki(),
            Ok(choice) if choice == IMPORT => self.import(),
            Ok(choice) if Some(&choice) == delete.as_ref() => self.delete_wiki(),
            _ => self.force_redraw(),
        }
    }

    fn pick_wiki(&mut self) {
        let names: Vec<String> =
            self.store.names().into_iter().filter(|n| Some(n) != self.wiki_key.as_ref()).collect();
        for name in &names {
            self.modals.add_list_item(name).ok();
        }
        self.modals.add_list_item("Cancel").ok();
        match self.modals.get_radiobutton("Open wiki:") {
            Ok(choice) if names.contains(&choice) => {
                self.open_wiki(&choice);
            }
            _ => self.force_redraw(),
        }
    }

    fn delete_wiki(&mut self) {
        let key = match self.wiki_key.clone() {
            Some(k) => k,
            None => return,
        };
        self.modals.add_list_item("Delete").ok();
        self.modals.add_list_item("Cancel").ok();
        match self.modals.get_radiobutton(&format!("Delete “{}” from this device?", key)) {
            Ok(choice) if choice == "Delete" => {}
            _ => {
                self.force_redraw();
                return;
            }
        }
        self.store.delete(&key);
        self.wiki = None;
        self.wiki_key = None;
        self.history.clear();
        let next = self.store.names().into_iter().next();
        let opened = match next {
            Some(next) => self.open_wiki(&next),
            None => false,
        };
        if !opened {
            self.show(View::Index, 0, None);
        }
        self.set_status(&format!("Deleted “{}”.", key));
        self.force_redraw();
    }

    /// Download every wiki linked from a directory index (or a single `.json` URL),
    /// replacing stored wikis of the same name. Blocks the UI, redrawing progress
    /// between files, as apps/sidplayer does.
    fn import(&mut self) {
        let url = match self.prompt_url() {
            Some(u) => u,
            None => {
                self.force_redraw();
                return;
            }
        };
        self.store.set_url(&url);

        self.set_status("Checking certificate…");
        self.force_redraw();
        // Establish TLS trust first (prompts to trust a CA on first use of a host);
        // otherwise the HTTPS fetch fails with an opaque "untrusted chain" error.
        if let Err(e) = netfetch::ensure_trusted(&url) {
            self.modals.show_notification(&e, None).ok();
            self.set_status("Certificate not trusted.");
            self.force_redraw();
            return;
        }

        self.set_status("Fetching…");
        self.force_redraw();
        let agent = netfetch::make_agent();
        let files = match netfetch::list_wiki_files(&agent, &url) {
            Ok(f) if !f.is_empty() => f,
            Ok(_) => {
                self.modals
                    .show_notification(&format!("No .json files are linked on that page.\n\nURL: {}", url), None)
                    .ok();
                self.set_status("No wiki files found.");
                self.force_redraw();
                return;
            }
            Err(e) => {
                self.modals
                    .show_notification(&format!("Could not fetch the index.\n\nURL: {}\n\n{}", url, e), None)
                    .ok();
                self.set_status("Fetch failed.");
                self.force_redraw();
                return;
            }
        };

        let total = files.len();
        let mut imported: Vec<(String, usize)> = Vec::new();
        let mut problems: Vec<String> = Vec::new();
        for (i, file) in files.iter().enumerate() {
            self.set_status(&format!("{}/{}: {}", i + 1, total, file.filename));
            self.force_redraw();
            let store = &self.store;
            let result = netfetch::download(&agent, &file.url).and_then(|bytes| {
                let wiki = Wiki::parse(&bytes)?;
                let key = store.save(&wiki.name, &bytes).map_err(|e| format!("couldn't save it ({})", e))?;
                Ok((key, wiki.pages.len()))
            });
            match result {
                Ok((key, pages)) => {
                    imported.retain(|(k, _)| *k != key);
                    imported.push((key, pages));
                }
                Err(e) => problems.push(format!("{}: {}", file.filename, e)),
            }
        }
        self.store.sync();

        // Show the newly imported version of the open wiki, or open one if none was.
        let reopen = match &self.wiki_key {
            Some(open) => imported.iter().find(|(k, _)| k == open).map(|(k, _)| k.clone()),
            None => imported.first().map(|(k, _)| k.clone()),
        };
        if let Some(key) = reopen {
            self.open_wiki(&key);
        }

        let summary = match imported.len() {
            0 => String::from("Nothing imported."),
            1 => format!("Imported “{}” ({} pages).", imported[0].0, imported[0].1),
            n => format!("Imported {} wikis. F1 to switch.", n),
        };
        if !problems.is_empty() {
            let mut msg = String::new();
            if !imported.is_empty() {
                msg.push_str("Imported:\n");
                for (key, pages) in &imported {
                    writeln!(msg, "• {} ({} pages)", key, pages).ok();
                }
                msg.push('\n');
            }
            msg.push_str("Skipped:\n");
            for p in &problems {
                writeln!(msg, "• {}", p).ok();
            }
            self.modals.show_notification(msg.trim_end(), None).ok();
        }
        self.set_status(&summary);
        self.force_redraw();
    }

    /// Ask for the import address, pre-filled with the last one used.
    fn prompt_url(&self) -> Option<String> {
        let last = self.store.url().unwrap_or_default();
        let mut builder = self.modals.alert_builder("Wiki folder or file URL:");
        // Rebind to the returned &mut Self so the final `build()` borrow is valid.
        let builder = if last.is_empty() {
            builder.field(Some("https://".to_string()), None)
        } else {
            builder.field_placeholder_persist(Some(last), None)
        };
        match builder.build() {
            Ok(p) => {
                let s = p.content()[0].content.as_str().trim().to_string();
                if s.is_empty() || s == "https://" || s == "http://" { None } else { Some(s) }
            }
            Err(_) => None,
        }
    }

    // ---- views ------------------------------------------------------------

    /// Switch to `view`, scrolled to `scroll` with link `selected` boxed.
    fn show(&mut self, view: View, scroll: usize, selected: Option<usize>) {
        self.view = view;
        self.build_view();
        self.scroll = scroll.min(self.max_scroll());
        self.selected = selected.filter(|&s| s < self.links.len());
        self.force_redraw();
    }

    /// Lay out the current view: a page, the index of all pages, or the welcome text.
    fn build_view(&mut self) {
        let mut runs: Vec<Run> = Vec::new();
        let mut targets: Vec<usize> = Vec::new();
        match (&self.wiki, self.view) {
            (Some(wiki), View::Page(page)) => {
                for segment in wiki::segments(&wiki.pages[page].content) {
                    match segment {
                        Segment::Text(text) => runs.push(Run { text, link: None }),
                        Segment::Link(text) => match wiki.find(text) {
                            Some(target) => {
                                runs.push(Run { text, link: Some(targets.len()) });
                                targets.push(target);
                            }
                            // No such page: keep the brackets, as plain text.
                            None => {
                                runs.push(Run { text: "[", link: None });
                                runs.push(Run { text, link: None });
                                runs.push(Run { text: "]", link: None });
                            }
                        },
                    }
                }
            }
            (Some(wiki), View::Index) => {
                for (i, page) in wiki.pages.iter().enumerate() {
                    runs.push(Run { text: display_title(&page.title), link: Some(targets.len()) });
                    runs.push(Run { text: "\n", link: None });
                    targets.push(i);
                }
            }
            (None, _) => runs.push(Run { text: WELCOME, link: None }),
        }

        let lines = layout::layout(&runs, self.max_px, self.font);
        let mut link_rows = vec![(usize::MAX, 0); targets.len()];
        for (row, line) in lines.iter().enumerate() {
            for span in &line.links {
                let rows = &mut link_rows[span.link];
                rows.0 = rows.0.min(row);
                rows.1 = rows.1.max(row);
            }
        }
        self.lines = lines;
        self.links = targets;
        self.link_rows = link_rows;
    }

    fn go(&mut self, view: View) {
        if self.wiki.is_none() {
            return;
        }
        self.history.push(Place { view: self.view, scroll: self.scroll, selected: self.selected });
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        self.status.clear();
        self.show(view, 0, None);
    }

    fn back(&mut self) {
        match self.history.pop() {
            Some(place) => {
                self.status.clear();
                self.show(place.view, place.scroll, place.selected);
            }
            None => {
                self.set_status("Nothing to go back to.");
                self.force_redraw();
            }
        }
    }

    fn go_home(&mut self) {
        if let Some(home) = self.wiki.as_ref().and_then(|w| w.home()) {
            self.go(View::Page(home));
        }
    }

    fn follow(&mut self) {
        match self.selected.filter(|&s| self.link_visible(s)) {
            Some(link) => self.go(View::Page(self.links[link])),
            None => {
                self.set_status(if self.links.is_empty() {
                    "No links here."
                } else {
                    "Pick a link with ← →."
                });
                self.force_redraw();
            }
        }
    }

    // ---- scrolling and link selection --------------------------------------

    fn max_scroll(&self) -> usize { self.lines.len().saturating_sub(self.visible_rows) }

    fn overflowing(&self) -> bool { self.lines.len() > self.visible_rows }

    fn link_visible(&self, link: usize) -> bool {
        let (first, last) = self.link_rows[link];
        last >= self.scroll && first < self.scroll + self.visible_rows
    }

    fn scroll_by(&mut self, delta: isize) {
        let target = (self.scroll as isize + delta).max(0) as usize;
        self.scroll = target.min(self.max_scroll());
        // a link scrolled out of sight is no longer selected
        if self.selected.is_some_and(|s| !self.link_visible(s)) {
            self.selected = None;
        }
        self.force_redraw();
    }

    /// ← / →: select the previous or next link. With nothing selected on screen, start
    /// from the top (→) or bottom (←) of the view. Scrolls to show the new selection.
    fn select_link(&mut self, forward: bool) {
        let n = self.links.len();
        let candidate = match self.selected.filter(|&s| self.link_visible(s)) {
            Some(s) if forward => Some(s + 1).filter(|&l| l < n),
            Some(s) => s.checked_sub(1),
            None if forward => (0..n).find(|&l| self.link_rows[l].1 >= self.scroll),
            None => (0..n).rev().find(|&l| self.link_rows[l].0 < self.scroll + self.visible_rows),
        };
        match candidate {
            Some(link) => {
                self.selected = Some(link);
                self.reveal(link);
                self.status.clear();
            }
            None if n == 0 => self.set_status("No links here."),
            None => self.set_status(if forward { "No more links below." } else { "No more links above." }),
        }
        self.force_redraw();
    }

    /// Scroll just enough to show the whole link (or its start, if it's taller than the view).
    fn reveal(&mut self, link: usize) {
        let (first, last) = self.link_rows[link];
        if first < self.scroll {
            self.scroll = first;
        } else if last >= self.scroll + self.visible_rows {
            self.scroll = (last + 1 - self.visible_rows).min(first);
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    // ---- text size --------------------------------------------------------

    fn set_font(&mut self, font: FontSize) {
        if font == self.font {
            self.set_status(&format!("{} is the {} text size.", font.label(), match font {
                FontSize::Small => "smallest",
                FontSize::Large => "largest",
                FontSize::Regular => "current",
            }));
            self.force_redraw();
            return;
        }
        // Keep roughly the same place in the page.
        let old_lines = self.lines.len().max(1);
        let old_scroll = self.scroll;
        self.font = font;
        self.recompute_metrics();
        self.build_view();
        self.scroll = (old_scroll * self.lines.len() / old_lines).min(self.max_scroll());
        if let Some(link) = self.selected {
            self.reveal(link);
        }
        self.store.set_font(font);
        self.set_status(&format!("{} text.", font.label()));
        self.force_redraw();
    }

    fn recompute_metrics(&mut self) {
        self.max_px = (self.screensize.x - LEFT - self.font.right_gap()).max(40);
        self.visible_rows = ((self.screensize.y - BODY_TOP - FOOTER_H) / self.font.row_h()).max(1) as usize;
    }

    // ---- input ------------------------------------------------------------

    pub(crate) fn key(&mut self, k: char) {
        match k {
            '↑' => self.scroll_by(-1),
            '↓' => self.scroll_by(1),
            '0' => self.scroll_by((-1 * (self.visible_rows as isize - 1)).min(-1)),
            'u' => self.scroll_by((-1 * (self.visible_rows as isize - 1)).min(-1)),
            ' ' => self.scroll_by((self.visible_rows as isize - 1).max(1)),
            '←' => self.select_link(false),
            '→' => self.select_link(true),
            '∴' | '\r' | '\n' => self.follow(),
            '\u{8}' | '\u{7f}' => self.back(),
            '\u{11}' | 'w' | 'W' => self.wiki_menu(),
            '\u{12}' | 'i' | 'I' => self.go(View::Index),
            '\u{13}' | 'h' | 'H' => self.go_home(),
            '\u{14}' => self.set_font(self.font.cycle()),
            '+' | '=' => self.set_font(self.font.larger()),
            '-' | '_' => self.set_font(self.font.smaller()),
            _ => {}
        }
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
        let view = match self.view {
            View::Page(p) => p as u64,
            View::Index => u64::MAX,
        };
        mix(&view.to_le_bytes());
        mix(&(self.scroll as u64).to_le_bytes());
        mix(&(self.lines.len() as u64).to_le_bytes());
        mix(&self.selected.map_or(u64::MAX, |s| s as u64).to_le_bytes());
        mix(self.font.label().as_bytes());
        mix(self.wiki_key.as_deref().unwrap_or("").as_bytes());
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

        // title bar: the page title, and the scroll position on the right
        let title = match (&self.wiki, self.view) {
            (Some(wiki), View::Page(p)) => display_title(&wiki.pages[p].title).to_string(),
            (Some(wiki), View::Index) => format!("Index · {} pages", wiki.pages.len()),
            (None, _) => String::from("LocalWiki"),
        };
        let mut title_chars = self.chrome_chars();
        if self.overflowing() {
            let last = (self.scroll + self.visible_rows).min(self.lines.len());
            let mut pos = String::new();
            write!(pos, "{}-{}/{}", self.scroll + 1, last, self.lines.len()).ok();
            let pos_px = layout::text_px(&pos, FontSize::Regular);
            self.text(self.screensize.x - pos_px - 4, 2, &pos, GlyphStyle::Regular);
            title_chars = title_chars.saturating_sub(pos.chars().count() + 2);
        }
        self.text(4, 2, &truncate(&title, title_chars), GlyphStyle::Regular);
        self.rule(BODY_TOP - 5);

        // page text
        let row_h = self.font.row_h();
        let glyph_h = self.font.glyph_h();
        let style = self.font.style();
        for i in 0..self.visible_rows {
            let idx = self.scroll + i;
            let line = match self.lines.get(idx) {
                Some(line) => line,
                None => break,
            };
            let y = BODY_TOP + (i as isize) * row_h;
            // The selected link is boxed (normal apps can't draw inverted text), so draw
            // the box first and the text over it.
            for span in line.links.iter().filter(|s| Some(s.link) == self.selected) {
                self.gam
                    .draw_rectangle(
                        self.gid,
                        Rectangle::new_coords_with_style(
                            LEFT + span.x0 - 2,
                            y - 2,
                            LEFT + span.x1 + 1,
                            y + glyph_h + 2,
                            DrawStyle::new(PixelColor::Light, PixelColor::Dark, 1),
                        ),
                    )
                    .ok();
            }
            if !line.text.is_empty() {
                self.text(LEFT, y, &line.text, style);
            }
            for span in &line.links {
                self.gam
                    .draw_line(
                        self.gid,
                        Line::new_with_style(
                            Point::new(LEFT + span.x0, y + glyph_h),
                            Point::new(LEFT + span.x1 - 1, y + glyph_h),
                            DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1),
                        ),
                    )
                    .ok();
            }
        }

        // footer: status (or the wiki's name) and key hints
        let fy = self.screensize.y - FOOTER_H + 6;
        self.rule(fy - 4);
        let status = if !self.status.is_empty() {
            self.status.clone()
        } else if let Some(wiki) = &self.wiki {
            wiki.name.clone()
        } else {
            String::from("F1 to import a wiki")
        };
        self.text(4, fy, &truncate(&status, self.chrome_chars()), GlyphStyle::Regular);
        self.text(4, fy + LINE_H, "↑↓ scroll  ←→ link  ⏎ open  ⌫ back", GlyphStyle::Regular);
        self.text(4, fy + 2 * LINE_H, "F1 wikis  F2 index  F3 home  F4 size", GlyphStyle::Regular);

        self.gam.redraw().unwrap();
    }

    /// Character budget for the chrome lines, which are always Regular.
    fn chrome_chars(&self) -> usize { ((self.screensize.x - 8) / 8).max(8) as usize }

    fn rule(&self, y: isize) {
        self.gam
            .draw_line(
                self.gid,
                Line::new_with_style(
                    Point::new(0, y),
                    Point::new(self.screensize.x, y),
                    DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1),
                ),
            )
            .ok();
    }

    fn text(&self, x: isize, y: isize, s: &str, style: GlyphStyle) {
        let mut tv = TextView::new(
            self.gid,
            TextBounds::GrowableFromTl(Point::new(x, y), (self.screensize.x - x).max(1) as u16),
        );
        tv.draw_border = false;
        tv.clear_area = false;
        // zero margin so we control line spacing exactly
        tv.margin = Point::new(0, 0);
        tv.style = style;
        write!(tv.text, "{}", s).ok();
        self.gam.post_textview(&mut { tv }).ok();
    }
}

fn display_title(title: &str) -> &str { if title.trim().is_empty() { "Untitled" } else { title } }

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
