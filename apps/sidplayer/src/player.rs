//! Drives the 6502 + SID engine to produce a stream of 8 kHz i16 samples.
//!
//! On construction it loads the PSID program into 6502 memory and runs the
//! tune's `init` routine. Thereafter `next_sample()` returns one 8 kHz output
//! sample, calling the tune's `play` routine once per emulated video frame and
//! feeding the resulting cycle-stamped SID register writes into the engine as
//! the batched clock advances.

extern crate alloc;
use alloc::vec::Vec;

use crate::cpu6502::{Cpu, RegWrite};
use crate::psid::{Clock, Psid};
use crate::sid::{MAX_STEP, Sid};

/// Final codec output rate. Must match `sid::CODEC_RATE`, from which the SID
/// filter derives its (oversampled) sample rate (`CODEC_RATE * oversample`).
pub const OUTPUT_RATE: u32 = 8000;

/// Cap on 6502 instructions per init/play call, to bound runaway tunes.
const INIT_INSN_CAP: u32 = 2_000_000;
const PLAY_INSN_CAP: u32 = 200_000;
/// Sentinel return address pushed before calling init/play; when the emulated
/// PC reaches it we know the routine has returned.
const SENTINEL: u16 = 0xffff;

pub struct Player {
    cpu: Cpu,
    sid: Sid,
    play_addr: u16,
    frame_cycles: u32,
    /// Q16 chip-cycles per output sample.
    cyc_per_sample_q16: u32,
    cyc_acc: u32,

    // frame / register-write scheduling
    cycle_in_frame: u32,
    writes: Vec<RegWrite>,
    write_idx: usize,
    started: bool,

    /// Waveform+filter evaluations per output sample (see `sid::DEFAULT_OVERSAMPLE`).
    oversample: u32,
}

impl Player {
    /// `oversample` sets the quality/CPU trade-off (see `sid::DEFAULT_OVERSAMPLE`);
    /// it is clamped and used to build the SID's filter and drive `next_sample`.
    pub fn new(psid: &Psid, song0: u16, oversample: u32) -> Self {
        let mut cpu = Cpu::new();

        // Load the program at its effective load address.
        let (load_addr, body) = psid.program();
        for (i, &b) in body.iter().enumerate() {
            let addr = load_addr as usize + i;
            if addr < 65536 {
                cpu.mem[addr] = b;
            }
        }

        let chip_clock = match psid.clock {
            Clock::Pal => 985_248u32,
            Clock::Ntsc => 1_022_727u32,
        };
        let frame_cycles = match psid.clock {
            Clock::Pal => 19_656u32,  // 63 * 312
            Clock::Ntsc => 17_095u32, // 65 * 263
        };
        let cyc_per_sample_q16 = ((chip_clock as u64) << 16) as u64 / OUTPUT_RATE as u64;

        let sid = Sid::new(matches!(psid.model, crate::psid::SidModel::Mos8580), oversample);
        let oversample = sid.oversample(); // clamped value actually in use

        let mut player = Player {
            cpu,
            sid,
            play_addr: psid.play_address,
            frame_cycles,
            cyc_per_sample_q16: cyc_per_sample_q16 as u32,
            cyc_acc: 0,
            cycle_in_frame: 0,
            writes: Vec::new(),
            write_idx: 0,
            started: false,
            oversample,
        };

        player.run_init(psid.init_address, song0);
        player
    }

    /// Run the tune's init routine with A = song index.
    fn run_init(&mut self, init_addr: u16, song0: u16) {
        self.cpu.a = song0 as u8;
        self.cpu.x = 0;
        self.cpu.y = 0;
        self.call(init_addr, INIT_INSN_CAP);
        // Discard any register writes the init routine made directly; most tunes
        // set up state but the play routine drives the audio. Keep them applied
        // to the SID so the initial register state is sane.
        let writes = core::mem::take(&mut self.cpu.writes);
        for w in &writes {
            self.sid.write_reg(w.reg, w.val);
        }
    }

    /// Set up a sentinel-return call and run until it returns or the cap trips.
    fn call(&mut self, addr: u16, cap: u32) { run_call(&mut self.cpu, addr, cap); }

    /// Call the play routine for a new frame and capture its register-write log.
    fn run_play(&mut self) {
        self.cpu.writes.clear();
        self.call(self.play_addr, PLAY_INSN_CAP);
        self.writes = core::mem::take(&mut self.cpu.writes);
        self.write_idx = 0;
    }

    /// Apply due register writes and advance the oscillators/envelopes by up to
    /// `max` cycles (bounded by MAX_STEP and the frame boundary). Returns the
    /// number of cycles actually advanced. This deliberately does NOT compute the
    /// analogue output — that is the expensive part (waveforms + filter) and is
    /// done once per output sample in `next_sample`, not per internal step.
    fn advance(&mut self, max: u32) -> u32 {
        // Start of stream, or a frame boundary: run the next play() call.
        if !self.started || self.cycle_in_frame >= self.frame_cycles {
            if self.cycle_in_frame >= self.frame_cycles {
                self.cycle_in_frame -= self.frame_cycles;
            }
            self.run_play();
            self.started = true;
        }

        // Apply due writes.
        while self.write_idx < self.writes.len() && self.writes[self.write_idx].cycle <= self.cycle_in_frame {
            let w = self.writes[self.write_idx];
            self.sid.write_reg(w.reg, w.val);
            self.write_idx += 1;
        }

        // Pick a step size that doesn't overshoot the frame boundary.
        let to_boundary = self.frame_cycles - self.cycle_in_frame;
        let step = max.min(MAX_STEP).min(to_boundary).max(1);

        self.sid.clock(step);
        self.cycle_in_frame += step;
        step
    }

    /// Produce one 8 kHz output sample. The oscillators/envelopes advance in
    /// small sub-steps (for exact noise/sync clocking); the waveform + filter are
    /// then evaluated `self.oversample` times across this sample's worth of chip
    /// cycles and box-averaged. Oversampling folds the noise/pulse energy above
    /// the 4 kHz codec Nyquist down as a lowered noise floor instead of letting
    /// it alias into the audible band, which is what gives noise bursts their
    /// hiss-with-a-transient character rather than a dull click. The chip itself
    /// is still batch-clocked, so the extra cost is `oversample` waveform+filter
    /// evaluations, not `oversample`x the emulation.
    pub fn next_sample(&mut self) -> i16 {
        self.cyc_acc += self.cyc_per_sample_q16;
        let cycles = self.cyc_acc >> 16;
        self.cyc_acc &= 0xffff;

        let n = self.oversample;
        let mut sum: i32 = 0;
        let mut done: u32 = 0;
        for i in 0..n {
            // Advance to this sub-sample's share of the cycle budget, distributing
            // any remainder evenly across the `n` points.
            let target = (cycles * (i + 1)) / n;
            let mut remaining = target - done;
            while remaining > 0 {
                remaining -= self.advance(remaining);
            }
            done = target;
            sum += self.sid.output();
        }
        (sum / n as i32).clamp(-32767, 32767) as i16
    }
}

/// Set up a sentinel-return call on `cpu` and run until it returns (PC reaches the
/// sentinel) or the instruction cap trips. Shared by the live [`Player`] and the
/// headless [`classify_music_songs`] probe.
fn run_call(cpu: &mut Cpu, addr: u16, cap: u32) {
    cpu.cycle = 0;
    cpu.pc = addr;
    // push sentinel-1 (RTS adds 1) so RTS lands on SENTINEL
    let ret = SENTINEL.wrapping_sub(1);
    cpu.sp = 0xfd;
    cpu.mem[0x01ff] = (ret >> 8) as u8;
    cpu.mem[0x01fe] = (ret & 0xff) as u8;

    let mut insns = 0u32;
    loop {
        if cpu.pc == SENTINEL {
            break;
        }
        cpu.step();
        insns += 1;
        if insns >= cap {
            log::warn!("sidplayer: instruction cap hit at pc={:04x}", cpu.pc);
            break;
        }
    }
}

/// Number of emulated ~50 Hz frames to run per subtune when classifying — ~6 s.
const PROBE_FRAMES: u32 = 300;
/// A subtune counts as "music" only if it was still gating fresh notes at least
/// this far into the probe (~2.4 s in). Short SFX/jingles gate a burst up front
/// and then fall silent well before this.
const MUSIC_MIN_LAST_FRAME: u32 = 120;
/// ...and produced at least this many distinct note-on (gate rising) events.
const MUSIC_MIN_GATES: u32 = 6;

/// Voice control-register offsets within the SID register file ($D400-relative):
/// voice 1 = $04, voice 2 = $0B, voice 3 = $12. Bit 0 of each is the gate.
const CTRL_REGS: [u8; 3] = [0x04, 0x0b, 0x12];

/// Headlessly classify which subtunes of `psid` are actual music (vs short sound
/// effects / jingles) by running each one's 6502 init+play driver for a few
/// emulated seconds and watching SID gate activity — no audio synthesis, so this
/// is cheap. Returns the 0-based indices judged to be music. Falls back to *all*
/// subtunes if the heuristic would otherwise hide everything (e.g. a percussive
/// tune that never trips the gate heuristic), so a file is never left unplayable.
pub fn classify_music_songs(psid: &Psid) -> Vec<u16> {
    let songs = psid.songs.max(1);
    let mut music = Vec::new();
    for song in 0..songs {
        if probe_is_music(psid, song) {
            music.push(song);
        }
    }
    if music.is_empty() {
        music.extend(0..songs);
    }
    music
}

/// Run one subtune headlessly and decide whether it looks like sustained music.
fn probe_is_music(psid: &Psid, song: u16) -> bool {
    let mut cpu = Cpu::new();
    let (load_addr, body) = psid.program();
    for (i, &b) in body.iter().enumerate() {
        let addr = load_addr as usize + i;
        if addr < 65536 {
            cpu.mem[addr] = b;
        }
    }

    // init(A = song index)
    cpu.a = song as u8;
    cpu.x = 0;
    cpu.y = 0;
    run_call(&mut cpu, psid.init_address, INIT_INSN_CAP);

    // Track per-voice gate state. `apply` walks a frame's writes and, when
    // `count_frame` is Some, records each gate rising edge as a fresh note-on.
    let mut prev_gate = [false; 3];
    let mut last_gate_frame = 0u32;
    let mut gate_on_count = 0u32;
    let mut apply = |writes: &[RegWrite], prev: &mut [bool; 3], count_frame: Option<u32>| {
        for w in writes {
            if let Some(v) = CTRL_REGS.iter().position(|&cr| cr == w.reg) {
                let g = w.val & 0x01 != 0;
                if g && !prev[v] {
                    if let Some(f) = count_frame {
                        last_gate_frame = f;
                        gate_on_count += 1;
                    }
                }
                prev[v] = g;
            }
        }
    };

    // Seed gate state from whatever init left in the control registers, so a note
    // held open across init->play isn't miscounted as a fresh note-on.
    apply(&cpu.writes, &mut prev_gate, None);

    for frame in 0..PROBE_FRAMES {
        cpu.writes.clear();
        run_call(&mut cpu, psid.play_address, PLAY_INSN_CAP);
        apply(&cpu.writes, &mut prev_gate, Some(frame));
    }

    gate_on_count >= MUSIC_MIN_GATES && last_gate_frame >= MUSIC_MIN_LAST_FRAME
}
