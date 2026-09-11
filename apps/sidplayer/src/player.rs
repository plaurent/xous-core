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
}

impl Player {
    pub fn new(psid: &Psid, song0: u16) -> Self {
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

        let sid = Sid::new(matches!(psid.model, crate::psid::SidModel::Mos8580));

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
    fn call(&mut self, addr: u16, cap: u32) {
        self.cpu.cycle = 0;
        self.cpu.pc = addr;
        // push sentinel-1 (RTS adds 1) so RTS lands on SENTINEL
        let ret = SENTINEL.wrapping_sub(1);
        self.cpu.sp = 0xfd;
        self.cpu.mem[0x01ff] = (ret >> 8) as u8;
        self.cpu.mem[0x01fe] = (ret & 0xff) as u8;

        let mut insns = 0u32;
        loop {
            if self.cpu.pc == SENTINEL {
                break;
            }
            self.cpu.step();
            insns += 1;
            if insns >= cap {
                log::warn!("sidplayer: instruction cap hit at pc={:04x}", self.cpu.pc);
                break;
            }
        }
    }

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
    /// small sub-steps (for exact noise/sync clocking), but the waveform + filter
    /// are evaluated just once, at the end — sampling the chip state at the 8 kHz
    /// rate. This keeps the per-sample cost low enough for real time on RV32.
    pub fn next_sample(&mut self) -> i16 {
        self.cyc_acc += self.cyc_per_sample_q16;
        let cycles = self.cyc_acc >> 16;
        self.cyc_acc &= 0xffff;

        let mut remaining = cycles;
        while remaining > 0 {
            remaining -= self.advance(remaining);
        }
        self.sid.output().clamp(-32767, 32767) as i16
    }
}
