use core::fmt::Write as _;

use codec::*;
use gam::menu::*;
use gam::*;

use crate::player::{Player, OUTPUT_RATE};
use crate::psid::Psid;
use crate::AppOp;
use num_traits::ToPrimitive;

/// The embedded tune. Rob Hubbard's "Commando" (1985, Elite).
static COMMANDO_SID: &[u8] = include_bytes!("commando.sid");

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
            OutputMode::Headphones => "Headphones",
            OutputMode::Speaker => "Speaker",
            OutputMode::Both => "Both",
        }
    }
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

    player: Option<Player>,
    playing: bool,
    hooked: bool,
    frames_played: u32,
    underruns: u32,
    status: String,
    output: OutputMode,
    /// headphone analog gain in dB (0 = loudest, more negative = quieter)
    hp_gain_db: f32,
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
        let self_conn = xous::connect(sid).unwrap();

        SidPlayer {
            gam,
            _token: token,
            gid,
            screensize,
            codec,
            self_conn,
            ticktimer,
            player: None,
            playing: false,
            hooked: false,
            frames_played: 0,
            underruns: 0,
            status: String::from("Space/center: play   o: output   up/dn: volume"),
            output: OutputMode::Headphones,
            hp_gain_db: 0.0,
            last_paint_sig: u64::MAX, // force the first paint
        }
    }

    /// Push the current output-mode routing and gains to the codec. Muting the
    /// unused path is what actually makes headphone-only playback work, since the
    /// codec otherwise drives both outputs at once.
    fn apply_output(&mut self) {
        match self.output {
            OutputMode::Headphones => {
                self.codec.set_speaker_volume(VolumeOps::Mute, None).ok();
                self.codec
                    .set_headphone_volume(VolumeOps::Set, Some(self.hp_gain_db))
                    .ok();
            }
            OutputMode::Speaker => {
                self.codec.set_speaker_volume(VolumeOps::RestoreDefault, None).ok();
                self.codec.set_headphone_volume(VolumeOps::Mute, None).ok();
            }
            OutputMode::Both => {
                self.codec.set_speaker_volume(VolumeOps::RestoreDefault, None).ok();
                self.codec
                    .set_headphone_volume(VolumeOps::Set, Some(self.hp_gain_db))
                    .ok();
            }
        }
    }

    /// Toggle playback on/off.
    pub(crate) fn toggle(&mut self) {
        if self.playing {
            self.stop();
        } else {
            self.start();
        }
        self.force_redraw();
    }

    fn start(&mut self) {
        let psid = match Psid::parse(COMMANDO_SID) {
            Ok(p) => p,
            Err(e) => {
                self.status = String::new();
                write!(self.status, "Parse error: {}", e.0).ok();
                log::error!("sidplayer: PSID parse error: {}", e.0);
                return;
            }
        };
        let song0 = psid.start_song.saturating_sub(1);
        log::info!(
            "sidplayer: playing '{}' by {} (song {}/{})",
            psid.name.as_str(),
            psid.author.as_str(),
            song0 + 1,
            psid.songs
        );
        self.player = Some(Player::new(&psid, song0));
        self.frames_played = 0;
        self.underruns = 0;

        self.codec.setup_8k_stream().expect("couldn't set up 8k stream");
        self.ticktimer.sleep_ms(50).unwrap();
        self.apply_output();

        if !self.hooked {
            self.codec
                .hook_frame_callback(AppOp::AudioFrame.to_u32().unwrap(), self.self_conn)
                .unwrap();
            self.hooked = true;
        }
        self.codec.resume().unwrap();

        self.playing = true;
        self.status = String::new();
        write!(self.status, "Playing at {} Hz...", OUTPUT_RATE).ok();
    }

    fn stop(&mut self) {
        if self.playing {
            self.codec.abort().ok();
            self.codec.power_off().ok();
        }
        self.playing = false;
        self.player = None;
        self.status = String::from("Stopped. Space/center to play again");
    }

    /// Codec "give me more frames" callback. `free_play` is how many play frames
    /// the codec can currently accept.
    pub(crate) fn audio_frame(&mut self, free_play: usize) {
        if !self.playing {
            return;
        }
        let player = match self.player.as_mut() {
            Some(p) => p,
            None => return,
        };

        let mut frames: FrameRing = FrameRing::new();
        let ring_max = frames.writeable_count();
        // If the codec can accept the whole ring, its play buffer had fully
        // drained since our last fill: an underrun (dropout) occurred. Skip the
        // very first fill after resume, which legitimately starts from empty.
        if self.frames_played > 0 && free_play >= ring_max {
            self.underruns += 1;
        }

        let to_push = ring_max.min(free_play);
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
        self.frames_played += to_push as u32;
        self.codec.swap_frames(&mut frames).unwrap();
        // NOTE: never redraw here. GAM IPC is slow, and blocking this callback
        // drains the codec buffer and causes dropouts. The screen is refreshed
        // only on user interaction (key / focus / play-stop) — deliberately no
        // background timer thread, since a continuously-scheduled thread on this
        // single-core CPU destabilised the whole device.
    }

    pub(crate) fn key(&mut self, k: char) {
        match k {
            ' ' | '∴' | '\r' => self.toggle(),
            'o' | 'O' => {
                self.output = self.output.next();
                if self.playing {
                    self.apply_output();
                }
                self.force_redraw();
            }
            '↑' | '+' => {
                self.hp_gain_db = (self.hp_gain_db + 3.0).min(0.0);
                if self.playing {
                    self.apply_output();
                }
                self.force_redraw();
            }
            '↓' | '-' => {
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

    /// A cheap signature of everything actually drawn on screen (deliberately
    /// excludes the live elapsed time). If GAM asks us to repaint while this is
    /// unchanged, we can skip the whole expensive draw — which matters because a
    /// full clear + text + LCD flush is tens of ms of blocking GAM IPC, and doing
    /// it during playback stalls the single thread from feeding the codec.
    fn paint_sig(&self) -> u64 {
        let mode = match self.output {
            OutputMode::Headphones => 0u64,
            OutputMode::Speaker => 1,
            OutputMode::Both => 2,
        };
        let mut h = self.playing as u64;
        h = h.wrapping_mul(31).wrapping_add(mode);
        h = h.wrapping_mul(31).wrapping_add((self.hp_gain_db as i32 as i64 as u64) & 0xffff);
        h = h.wrapping_mul(31).wrapping_add(self.underruns as u64);
        h
    }

    /// GAM-driven repaint: skip the expensive draw if nothing visible changed.
    pub(crate) fn redraw(&mut self) {
        if self.paint_sig() != self.last_paint_sig {
            self.force_redraw();
        }
    }

    /// Unconditional repaint. Used for user actions and focus changes.
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

        self.text(4, 4, "SID Player");
        self.text(4, 40, "Commando");
        self.text(4, 64, "Rob Hubbard, 1985 Elite");

        // output routing + headphone volume
        let mut outline = String::new();
        write!(
            outline,
            "Output: {}   HP vol: {} dB",
            self.output.label(),
            self.hp_gain_db as i32
        )
        .ok();
        self.text(4, 100, &outline);

        let mut line = String::new();
        if self.playing {
            write!(line, "Playing   underruns: {}", self.underruns).ok();
        } else {
            write!(line, "{}", self.status).ok();
        }
        self.text(4, 136, &line);

        self.gam.redraw().unwrap();
    }

    fn text(&self, x: isize, y: isize, s: &str) {
        let mut tv = TextView::new(
            self.gid,
            TextBounds::GrowableFromTl(Point::new(x, y), (self.screensize.x - x * 2) as u16),
        );
        tv.draw_border = false;
        tv.clear_area = true;
        tv.style = GlyphStyle::Regular;
        write!(tv.text, "{}", s).ok();
        self.gam.post_textview(&mut { tv }).ok();
    }
}
