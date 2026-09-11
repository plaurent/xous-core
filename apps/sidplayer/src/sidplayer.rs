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
    status: String,
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
            status: String::from("Press Space or center to play"),
        }
    }

    /// Toggle playback on/off.
    pub(crate) fn toggle(&mut self) {
        if self.playing {
            self.stop();
        } else {
            self.start();
        }
        self.redraw();
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

        self.codec.setup_8k_stream().expect("couldn't set up 8k stream");
        self.ticktimer.sleep_ms(50).unwrap();
        self.codec.set_speaker_volume(VolumeOps::RestoreDefault, None).unwrap();
        self.codec.set_headphone_volume(VolumeOps::RestoreDefault, None).unwrap();

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
        let to_push = frames.writeable_count().min(free_play);
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

        // Periodically refresh the on-screen elapsed time.
        if self.frames_played % 32 == 0 {
            self.redraw();
        }
    }

    pub(crate) fn key(&mut self, k: char) {
        match k {
            ' ' | '∴' | '\r' => self.toggle(),
            _ => {}
        }
    }

    pub(crate) fn on_focus(&mut self, foreground: bool) {
        if !foreground && self.playing {
            self.stop();
        }
        self.redraw();
    }

    pub(crate) fn redraw(&mut self) {
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

        // elapsed time, at ~256 samples/frame / 8000 Hz = 32 ms per frame
        let mut line = String::new();
        if self.playing {
            let ms = (self.frames_played as u64 * codec::FIFO_DEPTH as u64 * 1000) / OUTPUT_RATE as u64;
            write!(line, "{}   {}.{:01}s", self.status, ms / 1000, (ms % 1000) / 100).ok();
        } else {
            write!(line, "{}", self.status).ok();
        }
        self.text(4, 100, &line);

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
