use core::fmt::Write as _;
use std::io::{Read, Write};

use gam::menu::*; /* brings in minigfx: Point, Rectangle, Circle, Line, DrawStyle, PixelColor, TextView,
                   TextBounds, GlyphStyle, Gid... */
use gam::*; // Gam, UxRegistration, GamObjectList, GamObjectType, APP_NAME_WORLDCLOCK...
use num_traits::*;
use pddb::Pddb;
use sunrise::{Coordinates, SolarDay, SolarEvent};
use xous::Message;

use super::*;
use crate::cities::{self, City};

/// Height of one clock row, in pixels.
const ROW_H: isize = 90;
/// Clock face radius.
const FACE_R: isize = 40;
/// Left edge of the text to the right of the clock.
const TEXT_X: isize = 6 + 2 * FACE_R + 10;
/// Key hint line at the bottom of the screen.
const FOOTER_H: isize = 18;

const DAY: i64 = 86400;

const CONFIG_DICT: &str = "worldclock.config";
const HOME_KEY: &str = "home";
const CITIES_KEY: &str = "cities";
const H24_KEY: &str = "h24";
/// Shown on first run, before the user has edited the list.
const DEFAULT_CITIES: [&str; 3] = ["London", "New York", "Tokyo"];

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Collects draw objects and sends them to the GAM in as few messages as possible.
struct Batch<'a> {
    gam: &'a Gam,
    gid: Gid,
    list: GamObjectList,
    count: usize,
}

impl<'a> Batch<'a> {
    fn new(gam: &'a Gam, gid: Gid) -> Self { Batch { gam, gid, list: GamObjectList::new(gid), count: 0 } }

    fn push(&mut self, obj: GamObjectType) {
        if let Err(obj) = self.list.push(obj) {
            self.flush();
            self.list.push(obj).ok();
        }
        self.count += 1;
    }

    fn rect(&mut self, x0: isize, y0: isize, x1: isize, y1: isize, style: DrawStyle) {
        self.push(GamObjectType::Rect(Rectangle::new_coords_with_style(x0, y0, x1, y1, style)));
    }

    fn circle(&mut self, cx: isize, cy: isize, r: isize, color: PixelColor) {
        self.push(GamObjectType::Circ(Circle::new_with_style(
            Point::new(cx, cy),
            r,
            DrawStyle::new(color, color, 1),
        )));
    }

    fn line(&mut self, x0: isize, y0: isize, x1: isize, y1: isize, color: PixelColor) {
        self.push(GamObjectType::Line(Line::new_with_style(
            Point::new(x0, y0),
            Point::new(x1, y1),
            DrawStyle::new(color, color, 1),
        )));
    }

    fn flush(&mut self) {
        if self.count > 0 {
            self.gam.draw_list(self.list).expect("couldn't execute draw list");
        }
        self.list = GamObjectList::new(self.gid);
        self.count = 0;
    }
}

/// Sun position for one city on its current local date.
enum Sun {
    /// sunrise and sunset as UTC seconds
    Times(i64, i64),
    PolarDay,
    PolarNight,
    /// no coordinates (local clock with no city chosen)
    Unknown,
}

fn sun_for(city: Option<&City>, utc: i64, offset_min: i32) -> Sun {
    let city = match city {
        Some(c) => c,
        None => return Sun::Unknown,
    };
    let days = (utc + offset_min as i64 * 60).div_euclid(DAY);
    // The calculation works on solar days at the city's longitude. Where the clock is far from the
    // sun (Apia is UTC+13 at 171°W), local noon falls on a neighbouring solar date.
    let shift = ((city.lon * 4.0 - offset_min as f64) / 1440.0).round() as i64;
    let (y, m, d) = cities::civil_from_days(days + shift);
    let (coord, date) = match (Coordinates::new(city.lat, city.lon), chrono::NaiveDate::from_ymd_opt(y, m, d))
    {
        (Some(c), Some(d)) => (c, d),
        _ => return Sun::Unknown,
    };
    let solar = SolarDay::new(coord, date);
    match (solar.event_time(SolarEvent::Sunrise), solar.event_time(SolarEvent::Sunset)) {
        (Some(rise), Some(set)) => Sun::Times(rise.timestamp(), set.timestamp()),
        _ => {
            // No sunrise or sunset today: the sun is either always up or always down. Which one
            // depends on whether the sun's declination is on the city's side of the equator.
            let doy = (days + shift - cities::days_from_civil(y, 1, 1)) as f64;
            let decl = -23.44 * (2.0 * core::f64::consts::PI / 365.0 * (doy + 10.0)).cos();
            if (decl > 0.0) == (city.lat > 0.0) { Sun::PolarDay } else { Sun::PolarNight }
        }
    }
}

/// Everything needed to draw one clock row.
struct ClockInfo {
    name: String,
    city: Option<&'static City>,
    offset_min: i32,
    home: bool,
}

pub(crate) struct WorldClock {
    gam: gam::Gam,
    gid: Gid,
    screensize: Point,
    _token: [u32; 4],
    modals: modals::Modals,
    time_conn: xous::CID,
    time_init: bool,

    /// the city the top (local time) clock is in, if the user has chosen one
    home: Option<&'static City>,
    /// the other clocks, top to bottom
    cities: Vec<&'static City>,
    h24: bool,

    /// selected row: 0 is the local clock, 1..=cities.len() the others, then the "add" row
    sel: usize,
    /// first visible row
    top: usize,
    /// F2 move mode: up/down carries the selected clock along
    moving: bool,
    /// minute (since the epoch) the screen was last drawn for
    shown_minute: i64,
}

impl WorldClock {
    pub(crate) fn new(sid: xous::SID) -> Self {
        let xns = xous_names::XousNames::new().expect("couldn't connect to Xous Namespace Server");
        let gam = gam::Gam::new(&xns).expect("can't connect to Graphical Abstraction Manager");

        let token = gam
            .register_ux(UxRegistration {
                app_name: String::from(gam::APP_NAME_WORLDCLOCK),
                ux_type: gam::UxType::Framebuffer,
                predictor: None,
                listener: sid.to_array(),
                redraw_id: AppOp::Redraw.to_u32().unwrap(),
                gotinput_id: None,
                audioframe_id: None,
                focuschange_id: Some(AppOp::FocusChange.to_u32().unwrap()),
                rawkeys_id: Some(AppOp::Rawkeys.to_u32().unwrap()),
            })
            .expect("couldn't register Ux context for worldclock")
            .unwrap();

        let gid = gam.request_content_canvas(token).expect("couldn't get content canvas");
        let screensize = gam.get_canvas_bounds(gid).expect("couldn't get dimensions of content canvas");
        let modals = modals::Modals::new(&xns).unwrap();
        let time_conn = xous::connect(xous::SID::from_bytes(b"timeserverpublic").unwrap()).unwrap();

        let mut app = WorldClock {
            gam,
            gid,
            screensize,
            _token: token,
            modals,
            time_conn,
            time_init: false,
            home: None,
            cities: Vec::new(),
            h24: false,
            sel: 0,
            top: 0,
            moving: false,
            shown_minute: -1,
        };
        app.load_config();
        app
    }

    // ---------------------------------------------------------------- time

    /// Asks the time server for a value. Replies come back as two 32-bit halves; `lo_first`
    /// selects their order, which differs between opcodes.
    fn time_scalar(&self, op: usize, lo_first: bool) -> Option<i64> {
        match xous::send_message(self.time_conn, Message::new_blocking_scalar(op, 0, 0, 0, 0)) {
            Ok(xous::Result::Scalar2(a, b)) => {
                let (hi, lo) = if lo_first { (b, a) } else { (a, b) };
                Some(((hi as u64) << 32 | lo as u64) as i64)
            }
            Ok(xous::Result::Scalar1(v)) => Some(v as i64),
            _ => None,
        }
    }

    /// (UTC seconds, local offset in minutes). Until the clock has been set, the UTC value is
    /// just the raw RTC count and means nothing.
    fn now(&mut self) -> (i64, i32) {
        if !self.time_init {
            // WallClockTimeInit (6): true once the RTC and time zone offsets have been set
            self.time_init = self.time_scalar(6, false).unwrap_or(0) != 0;
        }
        // GetUtcTimeMs (3) answers low half first, matching what libstd expects; GetLocalTimeMs (4)
        // answers high half first
        let utc_ms = self.time_scalar(3, true).unwrap_or(0);
        if !self.time_init {
            return (utc_ms / 1000, 0);
        }
        let local_ms = self.time_scalar(4, false).unwrap_or(utc_ms);
        // the two reads are a moment apart, so round to the nearest minute
        let offset_min = (local_ms - utc_ms + 30_000).div_euclid(60_000) as i32;
        (utc_ms / 1000, offset_min)
    }

    // ---------------------------------------------------------------- config

    fn load_config(&mut self) {
        let pddb = Pddb::new();
        pddb.is_mounted_blocking();
        self.home = read_key(&pddb, HOME_KEY).and_then(|n| cities::find(&n));
        self.cities = match read_key(&pddb, CITIES_KEY) {
            Some(list) => list.lines().filter_map(|n| cities::find(n.trim())).collect(),
            None => DEFAULT_CITIES.iter().filter_map(|n| cities::find(n)).collect(),
        };
        self.h24 = read_key(&pddb, H24_KEY).map(|s| s == "true").unwrap_or(false);
    }

    fn save_config(&self) {
        let pddb = Pddb::new();
        write_key(&pddb, HOME_KEY, self.home.map(|c| c.name).unwrap_or(""));
        let names: Vec<&str> = self.cities.iter().map(|c| c.name).collect();
        // a single space marks "deliberately empty", as an empty value reads back as unset
        let list = names.join("\n");
        write_key(&pddb, CITIES_KEY, if list.is_empty() { " " } else { &list });
        write_key(&pddb, H24_KEY, if self.h24 { "true" } else { "false" });
        pddb.sync().ok();
    }

    // ---------------------------------------------------------------- rows

    fn add_row(&self) -> usize { self.cities.len() + 1 }

    fn rows_visible(&self) -> usize { ((self.screensize.y - FOOTER_H) / ROW_H).max(1) as usize }

    fn scroll_to_sel(&mut self) {
        let vis = self.rows_visible();
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + vis {
            self.top = self.sel + 1 - vis;
        }
    }

    fn clock_info(&self, row: usize, utc: i64, local_offset: i32) -> ClockInfo {
        if row == 0 {
            let name = match self.home {
                Some(c) => String::from(c.name),
                None => String::from("Local time"),
            };
            ClockInfo { name, city: self.home, offset_min: local_offset, home: true }
        } else {
            let c = self.cities[row - 1];
            ClockInfo {
                name: String::from(c.name),
                city: Some(c),
                offset_min: cities::offset_min(c, utc),
                home: false,
            }
        }
    }

    fn fmt_time(&self, min_of_day: i64, out: &mut String) {
        let (h, m) = (min_of_day / 60, min_of_day % 60);
        if self.h24 {
            write!(out, "{:02}:{:02}", h, m).ok();
        } else {
            let h12 = if h % 12 == 0 { 12 } else { h % 12 };
            write!(out, "{}:{:02} {}", h12, m, if h < 12 { "AM" } else { "PM" }).ok();
        }
    }

    // ---------------------------------------------------------------- drawing

    fn text(&self, x0: isize, y0: isize, x1: isize, y1: isize, style: GlyphStyle, s: &str) {
        let mut tv = TextView::new(
            self.gid,
            TextBounds::BoundingBox(Rectangle::new(Point::new(x0, y0), Point::new(x1, y1))),
        );
        tv.draw_border = false;
        tv.clear_area = false;
        tv.margin = Point::new(0, 0);
        tv.style = style;
        write!(tv.text, "{}", s).ok();
        self.gam.post_textview(&mut tv).ok();
    }

    /// An analogue face: light for daytime, solid dark with light markings at night.
    /// `hm` is the (hour, minute) to show; `None` draws an empty face.
    fn draw_face(&self, b: &mut Batch, cx: isize, cy: isize, hm: Option<(i64, i64)>, night: bool) {
        let (bg, fg) =
            if night { (PixelColor::Dark, PixelColor::Light) } else { (PixelColor::Light, PixelColor::Dark) };
        let r = FACE_R;
        b.circle(cx, cy, r, PixelColor::Dark);
        b.circle(cx, cy, r - 3, bg);
        let point = |angle: f64, len: f64| -> (isize, isize) {
            (
                (cx as f64 + len * angle.sin()).round() as isize,
                (cy as f64 - len * angle.cos()).round() as isize,
            )
        };
        let tau = 2.0 * core::f64::consts::PI;
        for k in 0..12 {
            let a = tau * k as f64 / 12.0;
            let major = k % 3 == 0;
            let (x0, y0) = point(a, (r - 5) as f64);
            let (x1, y1) = point(a, (r - if major { 14 } else { 9 }) as f64);
            b.line(x0, y0, x1, y1, fg);
            if major {
                // double up the quarter-hour marks
                let (dx, dy) = if k % 6 == 0 { (1, 0) } else { (0, 1) };
                b.line(x0 + dx, y0 + dy, x1 + dx, y1 + dy, fg);
            }
        }
        let hand = |b: &mut Batch, angle: f64, len: f64, offsets: &[(isize, isize)]| {
            let (x1, y1) = point(angle, len);
            for (dx, dy) in offsets {
                b.line(cx + dx, cy + dy, x1 + dx, y1 + dy, fg);
            }
        };
        let (hour, minute) = match hm {
            Some(hm) => hm,
            None => return,
        };
        let hour_angle = tau * ((hour % 12) as f64 + minute as f64 / 60.0) / 12.0;
        let minute_angle = tau * minute as f64 / 60.0;
        hand(
            b,
            hour_angle,
            r as f64 * 0.5,
            &[(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, -1), (1, -1), (-1, 1)],
        );
        hand(b, minute_angle, r as f64 * 0.8, &[(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)]);
        b.circle(cx, cy, 4, fg);
    }

    fn draw_row(&self, row: usize, y: isize, utc: i64, local_offset: i32) {
        let w = self.screensize.x;
        let selected = row == self.sel;
        let mut b = Batch::new(&self.gam, self.gid);
        // the selected row gets an outline (inverted text isn't available to apps); a heavier one in
        // move mode
        let style = if selected {
            DrawStyle::new(PixelColor::Light, PixelColor::Dark, if self.moving { 3 } else { 1 })
        } else {
            DrawStyle::new(PixelColor::Light, PixelColor::Light, 1)
        };
        b.rect(1, y + 1, w - 2, y + ROW_H - 2, style);

        if row == self.add_row() {
            b.flush();
            self.text(
                TEXT_X,
                y + ROW_H / 2 - 8,
                w - 6,
                y + ROW_H / 2 + 10,
                GlyphStyle::Regular,
                "+ Add a city",
            );
            let (cx, cy) = (6 + FACE_R, y + ROW_H / 2);
            let mut b = Batch::new(&self.gam, self.gid);
            b.rect(cx - 13, cy - 1, cx + 13, cy + 1, DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1));
            b.rect(cx - 1, cy - 13, cx + 1, cy + 13, DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1));
            b.flush();
            return;
        }

        let info = self.clock_info(row, utc, local_offset);
        if !self.time_init {
            // Without a wall-clock time there's nothing meaningful to show for any city.
            self.draw_face(&mut b, 6 + FACE_R, y + ROW_H / 2, None, false);
            b.flush();
            self.text(TEXT_X, y + 6, w - 6, y + 23, GlyphStyle::Bold, &info.name);
            self.text(TEXT_X, y + 26, w - 6, y + 60, GlyphStyle::ExtraLarge, "--:--");
            self.text(TEXT_X, y + 65, w - 6, y + 84, GlyphStyle::Regular, "Clock not set");
            return;
        }
        let local = utc + info.offset_min as i64 * 60;
        let days = local.div_euclid(DAY);
        let min_of_day = local.rem_euclid(DAY) / 60;
        let sun = sun_for(info.city, utc, info.offset_min);
        let night = match sun {
            Sun::Times(rise, set) => utc < rise || utc >= set,
            Sun::PolarDay => false,
            Sun::PolarNight => true,
            // no position known: call 6 am to 6 pm daytime
            Sun::Unknown => min_of_day < 6 * 60 || min_of_day >= 18 * 60,
        };
        self.draw_face(&mut b, 6 + FACE_R, y + ROW_H / 2, Some((min_of_day / 60, min_of_day % 60)), night);
        b.flush();

        // line 1: name, plus the date for the local clock or the day and difference for the others
        let mut line = String::new();
        write!(line, "{}  {}", info.name, WEEKDAYS[cities::weekday(days) as usize]).ok();
        if info.home {
            let (_, m, d) = cities::civil_from_days(days);
            write!(line, " {} {}", d, MONTHS[m as usize - 1]).ok();
        } else {
            let diff = info.offset_min - local_offset;
            if diff == 0 {
                line.push_str(" same");
            } else {
                let sign = if diff < 0 { '-' } else { '+' };
                let (h, m) = (diff.abs() / 60, diff.abs() % 60);
                if m == 0 {
                    write!(line, " {}{}h", sign, h).ok();
                } else {
                    write!(line, " {}{}:{:02}", sign, h, m).ok();
                }
            }
        }
        self.text(TEXT_X, y + 6, w - 6, y + 23, GlyphStyle::Bold, &line);

        // line 2: the digital readout
        let mut line = String::new();
        self.fmt_time(min_of_day, &mut line);
        self.text(TEXT_X, y + 26, w - 6, y + 60, GlyphStyle::ExtraLarge, &line);

        // line 3: sunrise and sunset in the city's own time
        let mut line = String::new();
        match sun {
            Sun::Times(rise, set) => {
                let off = info.offset_min as i64 * 60;
                line.push_str("Rise ");
                self.fmt_time((rise + off).rem_euclid(DAY) / 60, &mut line);
                line.push_str("  Set ");
                self.fmt_time((set + off).rem_euclid(DAY) / 60, &mut line);
            }
            Sun::PolarDay => line.push_str("Sun up all day"),
            Sun::PolarNight => line.push_str("Sun down all day"),
            Sun::Unknown => {
                let off = info.offset_min;
                write!(line, "UTC{}{}", if off < 0 { '-' } else { '+' }, off.abs() / 60).ok();
                if off % 60 != 0 {
                    write!(line, ":{:02}", off.abs() % 60).ok();
                }
                line.push_str("  F3: set your city");
            }
        }
        self.text(TEXT_X, y + 65, w - 6, y + 84, GlyphStyle::Regular, &line);
    }

    fn draw_footer(&self) {
        let (w, h) = (self.screensize.x, self.screensize.y);
        let mut b = Batch::new(&self.gam, self.gid);
        b.rect(0, h - FOOTER_H, w - 1, h - FOOTER_H, DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1));
        b.flush();
        let hint = if self.moving {
            "MOVING: up/down to place, F2 when done"
        } else if !self.time_init {
            "Clock not set: main menu > Set time"
        } else {
            "F1 del  F2 move  F3 city  F4 12/24h"
        };
        self.text(4, h - FOOTER_H + 3, w - 4, h - 1, GlyphStyle::Small, hint);
    }

    /// Full redraw of the whole screen.
    pub(crate) fn redraw_all(&mut self) {
        let (utc, local_offset) = self.now();
        self.shown_minute = utc.div_euclid(60);
        self.scroll_to_sel();
        let mut b = Batch::new(&self.gam, self.gid);
        b.rect(
            0,
            0,
            self.screensize.x - 1,
            self.screensize.y - 1,
            DrawStyle::new(PixelColor::Light, PixelColor::Light, 1),
        );
        b.flush();
        let last = (self.top + self.rows_visible()).min(self.add_row() + 1);
        for (k, row) in (self.top..last).enumerate() {
            self.draw_row(row, k as isize * ROW_H, utc, local_offset);
        }
        // hint that there are more rows above or below
        let mut b = Batch::new(&self.gam, self.gid);
        let w = self.screensize.x;
        if self.top > 0 {
            b.rect(w / 2 - 12, 0, w / 2 + 12, 1, DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1));
        }
        if last <= self.add_row() {
            let yb = (last - self.top) as isize * ROW_H - 2;
            b.rect(w / 2 - 12, yb, w / 2 + 12, yb + 1, DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1));
        }
        b.flush();
        self.draw_footer();
        self.gam.redraw().unwrap();
    }

    /// Called about once a second; the clocks only change when the minute does.
    pub(crate) fn tick(&mut self) {
        let was_init = self.time_init;
        let (utc, _) = self.now();
        // also redraw the moment the clock gets set, so the placeholders don't linger
        if utc.div_euclid(60) != self.shown_minute || self.time_init != was_init {
            self.redraw_all();
        }
    }

    // ---------------------------------------------------------------- input

    pub(crate) fn key(&mut self, k: char) {
        match k {
            '↑' | '↓' => {
                let up = k == '↑';
                if self.moving {
                    let i = self.sel - 1; // index into `cities`
                    if up && i > 0 {
                        self.cities.swap(i, i - 1);
                        self.sel -= 1;
                    } else if !up && i + 1 < self.cities.len() {
                        self.cities.swap(i, i + 1);
                        self.sel += 1;
                    }
                } else if up {
                    self.sel = self.sel.saturating_sub(1);
                } else {
                    self.sel = (self.sel + 1).min(self.add_row());
                }
                self.redraw_all();
            }
            // F1 or backspace: delete
            '\u{11}' | '\u{8}' => self.delete_selected(),
            // F2: move mode
            '\u{12}' => {
                if self.moving {
                    self.moving = false;
                    self.save_config();
                } else if self.sel >= 1 && self.sel <= self.cities.len() {
                    self.moving = true;
                } else {
                    self.notify("Select one of the other cities to move it. The local clock stays on top.");
                }
                self.redraw_all();
            }
            // F3 or enter: choose the city for this row
            '\u{13}' | '\r' | '\n' => {
                if self.moving {
                    self.moving = false;
                    self.save_config();
                    self.redraw_all();
                } else {
                    self.configure_selected();
                }
            }
            // F4: 12/24 hour
            '\u{14}' => {
                self.h24 = !self.h24;
                self.save_config();
                self.redraw_all();
            }
            _ => {}
        }
    }

    fn notify(&self, text: &str) { self.modals.show_notification(text, None).ok(); }

    fn delete_selected(&mut self) {
        if self.moving {
            return;
        }
        if self.sel == 0 || self.sel == self.add_row() {
            self.notify(
                "Only the other cities can be removed. The local clock always shows this device's time.",
            );
            return;
        }
        let name = self.cities[self.sel - 1].name;
        self.modals.add_list_item("Remove").ok();
        self.modals.add_list_item("Keep").ok();
        let prompt = format!("Remove {}?", name);
        if let Ok(choice) = self.modals.get_radiobutton(&prompt) {
            if choice == "Remove" {
                self.cities.remove(self.sel - 1);
                self.sel = self.sel.min(self.add_row());
                self.save_config();
            }
        }
        self.redraw_all();
    }

    fn configure_selected(&mut self) {
        let row = self.sel;
        if row == 0 {
            if let Some(pick) = self.pick_city("Your city (for the local clock)", true) {
                self.home = pick;
                self.save_config();
            }
        } else if let Some(Some(city)) = self.pick_city("City for this clock", false) {
            if row == self.add_row() {
                self.cities.push(city);
                // keep the cursor on the new clock, so it can be moved straight away
                self.sel = self.cities.len();
            } else {
                self.cities[row - 1] = city;
            }
            self.save_config();
        }
        self.redraw_all();
    }

    /// Asks for a few letters of a name, then offers the matching cities. `Some(None)` is
    /// "automatic", only offered when `allow_none` is set.
    fn pick_city(&self, title: &str, allow_none: bool) -> Option<Option<&'static City>> {
        let query = {
            let mut builder = self.modals.alert_builder(title);
            let builder = builder.field(Some(String::from("Part of the name, or blank for all")), None);
            match builder.build() {
                Ok(p) => p.content()[0].content.as_str().trim().to_lowercase(),
                Err(_) => return None,
            }
        };
        const NO_CITY: &str = "(none: just show the time)";
        let matches: Vec<&'static City> =
            cities::CITIES.iter().filter(|c| c.name.to_lowercase().contains(&query)).collect();
        if matches.is_empty() {
            self.notify(&format!("No city matching \"{}\". Try part of a nearby big city's name.", query));
            return None;
        }
        if matches.len() == 1 && !allow_none {
            return Some(Some(matches[0]));
        }
        if allow_none {
            self.modals.add_list_item(NO_CITY).ok();
        }
        for c in matches.iter() {
            self.modals.add_list_item(c.name).ok();
        }
        match self.modals.get_radiobutton("Choose a city") {
            Ok(name) if name == NO_CITY => Some(None),
            Ok(name) => cities::find(&name).map(Some),
            Err(_) => None,
        }
    }
}

fn read_key(pddb: &Pddb, key: &str) -> Option<String> {
    let mut k = pddb.get(CONFIG_DICT, key, None, false, false, None, None::<fn()>).ok()?;
    let mut buf = Vec::new();
    k.read_to_end(&mut buf).ok()?;
    let s = String::from_utf8_lossy(&buf).to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn write_key(pddb: &Pddb, key: &str, val: &str) {
    // delete-then-create so a shorter new value can't leave stale trailing bytes.
    pddb.delete_key(CONFIG_DICT, key, None).ok();
    if let Ok(mut k) = pddb.get(CONFIG_DICT, key, None, true, true, None, None::<fn()>) {
        k.write_all(val.as_bytes()).ok();
    }
}

pub(crate) fn worldclock_pump_thread(cid_to_main: xous::CID, pump_sid: xous::SID) {
    let _ = std::thread::spawn({
        let cid_to_main = cid_to_main;
        let sid = pump_sid;
        move || {
            let tt = ticktimer_server::Ticktimer::new().unwrap();
            let cid_to_self = xous::connect(sid).unwrap();
            let mut run = false;
            // a Pump message is in flight; keeps a quick Stop/Run from starting a second chain
            let mut chain = false;
            loop {
                let msg = xous::receive_message(sid).unwrap();
                match FromPrimitive::from_usize(msg.body.id()) {
                    Some(PumpOp::Run) => {
                        run = true;
                        if !chain {
                            chain = true;
                            xous::send_message(
                                cid_to_self,
                                Message::new_scalar(PumpOp::Pump.to_usize().unwrap(), 0, 0, 0, 0),
                            )
                            .expect("couldn't pump the main loop event thread");
                        }
                    }
                    Some(PumpOp::Stop) => run = false,
                    Some(PumpOp::Pump) => {
                        if run {
                            xous::send_message(
                                cid_to_main,
                                Message::new_blocking_scalar(AppOp::Pump.to_usize().unwrap(), 0, 0, 0, 0),
                            )
                            .expect("couldn't pump the main loop event thread");
                            tt.sleep_ms(WORLDCLOCK_TICK_MS).unwrap();
                            xous::send_message(
                                cid_to_self,
                                Message::new_scalar(PumpOp::Pump.to_usize().unwrap(), 0, 0, 0, 0),
                            )
                            .expect("couldn't pump the main loop event thread");
                        } else {
                            chain = false;
                        }
                    }
                    Some(PumpOp::Quit) => {
                        xous::return_scalar(msg.sender, 1).expect("couldn't ack the quit message");
                        break;
                    }
                    _ => log::error!("Got unrecognized message: {:?}", msg),
                }
            }
            unsafe { xous::disconnect(cid_to_self).ok() };
        }
    });
}
