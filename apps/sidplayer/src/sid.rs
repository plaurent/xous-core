//! A tier-2 (batched-clocking) MOS 6581/8580 SID model.
//!
//! Instead of advancing the chip one ~1 MHz cycle at a time, `clock(n)` advances
//! all state by up to `MAX_STEP` chip cycles at once. `n` is kept small enough
//! (<= 16) that at most one oscillator MSB wrap and one noise-LFSR clock can occur
//! per step, which keeps the batched arithmetic exact for the events that matter.
//!
//! Everything in the hot path (`clock`, `output`) is integer-only: no floats and
//! no 64-bit multiply/divide, so it stays cheap on RV32IMAC. The one place floats
//! appear is `new()`, which precomputes the filter cutoff table once at startup.
//!
//! The analogue models here are deliberately lean approximations (AND-combined
//! waveforms, a Chamberlin state-variable filter). They are not cycle-accurate,
//! but reproduce the essential character of classic tunes well at an 8 kHz output.

/// Largest number of chip cycles a single `clock()` step may advance. Chosen so
/// `freq(<=0xFFFF) * MAX_STEP < 2^20`, bounding MSB/noise events to one per step.
pub const MAX_STEP: u32 = 16;

/// reSID rate-counter periods indexed by the 4-bit attack/decay/release value.
#[rustfmt::skip]
static RATE_PERIODS: [u16; 16] = [
    9, 32, 63, 95, 149, 220, 267, 313,
    392, 977, 1954, 3126, 3907, 11720, 19532, 31251,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvState {
    Attack,
    DecaySustain,
    Release,
}

struct Voice {
    // register-derived parameters
    freq: u32,     // 16-bit
    pw: u32,       // 12-bit pulse width
    control: u8,   // waveform + gate/sync/ring/test bits
    attack: u8,    // 0..15
    decay: u8,     // 0..15
    sustain: u8,   // 0..15
    release: u8,   // 0..15

    // oscillator state
    acc: u32,       // 24-bit phase accumulator
    noise_lfsr: u32, // 23-bit
    prev_msb: bool,  // accumulator bit23 at end of previous step (for sync source)

    // envelope state
    env_state: EnvState,
    env: u8,          // 0..255
    rate_counter: u16,
    rate_period: u16,
    exp_counter: u8,
    exp_period: u8,
    hold_zero: bool,
}

impl Voice {
    fn new() -> Self {
        Voice {
            freq: 0,
            pw: 0,
            control: 0,
            attack: 0,
            decay: 0,
            sustain: 0,
            release: 0,
            acc: 0,
            noise_lfsr: 0x7f_fff8,
            prev_msb: false,
            env_state: EnvState::Release,
            env: 0,
            rate_counter: 0,
            rate_period: RATE_PERIODS[0],
            exp_counter: 0,
            exp_period: 1,
            hold_zero: true,
        }
    }

    #[inline]
    fn gate(&self) -> bool {
        self.control & 0x01 != 0
    }
    #[inline]
    fn sync(&self) -> bool {
        self.control & 0x02 != 0
    }
    #[inline]
    fn ring(&self) -> bool {
        self.control & 0x04 != 0
    }
    #[inline]
    fn test(&self) -> bool {
        self.control & 0x08 != 0
    }

    /// Advance the envelope generator by `n` cycles (batched over rate-hits).
    fn clock_env(&mut self, mut n: u32) {
        while n > 0 {
            let until = (self.rate_period - self.rate_counter) as u32;
            if until > n {
                self.rate_counter += n as u16;
                break;
            }
            n -= until;
            self.rate_counter = 0;
            self.env_hit();
        }
    }

    /// One rate-counter hit: the per-tick envelope logic from the reSID model.
    fn env_hit(&mut self) {
        let step = if self.env_state == EnvState::Attack {
            true
        } else {
            self.exp_counter = self.exp_counter.wrapping_add(1);
            if self.exp_counter >= self.exp_period {
                self.exp_counter = 0;
                true
            } else {
                false
            }
        };
        if !step || self.hold_zero {
            return;
        }
        match self.env_state {
            EnvState::Attack => {
                self.env = self.env.wrapping_add(1);
                if self.env == 0xff {
                    self.env_state = EnvState::DecaySustain;
                    self.rate_period = RATE_PERIODS[self.decay as usize];
                }
            }
            EnvState::DecaySustain => {
                let sustain_level = (self.sustain << 4) | self.sustain;
                if self.env != sustain_level && self.env != 0 {
                    self.env -= 1;
                }
            }
            EnvState::Release => {
                if self.env != 0 {
                    self.env -= 1;
                }
            }
        }
        self.update_exp_period();
    }

    fn update_exp_period(&mut self) {
        // reSID exponential-decay approximation thresholds.
        self.exp_period = match self.env {
            0xff => 1,
            0x5d => 2,
            0x36 => 4,
            0x1a => 8,
            0x0e => 16,
            0x06 => 30,
            0x00 => {
                self.hold_zero = true;
                1
            }
            _ => self.exp_period,
        };
    }

    /// Called when a register write changes the control byte; handles gate edges
    /// and the test-bit accumulator reset.
    fn set_control(&mut self, val: u8) {
        let old_gate = self.gate();
        let old_test = self.test();
        self.control = val;
        let new_gate = self.gate();
        if !old_gate && new_gate {
            // gate on: begin attack
            self.env_state = EnvState::Attack;
            self.rate_period = RATE_PERIODS[self.attack as usize];
            self.hold_zero = false;
        } else if old_gate && !new_gate {
            // gate off: begin release
            self.env_state = EnvState::Release;
            self.rate_period = RATE_PERIODS[self.release as usize];
        }
        if self.test() {
            self.acc = 0;
            self.noise_lfsr = 0x7f_fff8;
        }
        let _ = old_test;
    }
}

pub struct Sid {
    voices: [Voice; 3],
    // filter / global
    fc: u32,        // 11-bit cutoff
    res: u8,        // 4-bit resonance
    filt_mask: u8,  // which voices routed to filter (bits 0..2), bit3 = ext
    mode_vol: u8,   // bit0-3 volume, bit4 LP, bit5 BP, bit6 HP, bit7 3off
    // Chamberlin SVF state (Q0 integers, small scale)
    f_lp: i32,
    f_bp: i32,
    // precomputed cutoff coefficient table: fc(0..2047) -> Q16 coefficient
    w0_table: [i32; 2048],
}

impl Sid {
    pub fn new(_is_8580: bool) -> Self {
        // Internal step rate ~ chip_clock / MAX_STEP. Used only to size the
        // filter coefficients; PAL clock is close enough for NTSC too here.
        let fs_internal = 985_248.0f32 / (MAX_STEP as f32);
        let mut w0_table = [0i32; 2048];
        for (i, slot) in w0_table.iter_mut().enumerate() {
            // Map the 11-bit cutoff register to ~30 Hz .. 12 kHz (roughly linear;
            // the real chip curve differs but this is inaudible at 8 kHz output).
            let cutoff_hz = 30.0 + (i as f32 / 2047.0) * 11_800.0;
            let mut f = 2.0 * (core::f32::consts::PI * cutoff_hz / fs_internal).sin();
            if f > 1.6 {
                f = 1.6; // keep the SVF stable
            }
            *slot = (f * 65536.0) as i32;
        }
        Sid {
            voices: [Voice::new(), Voice::new(), Voice::new()],
            fc: 0,
            res: 0,
            filt_mask: 0,
            mode_vol: 0x0f,
            f_lp: 0,
            f_bp: 0,
            w0_table,
        }
    }

    /// Apply a single SID register write (register index 0x00..=0x18).
    pub fn write_reg(&mut self, reg: u8, val: u8) {
        match reg {
            0x00..=0x14 => {
                let v = (reg / 7) as usize;
                if v < 3 {
                    let r = reg % 7;
                    let voice = &mut self.voices[v];
                    match r {
                        0 => voice.freq = (voice.freq & 0xff00) | val as u32,
                        1 => voice.freq = (voice.freq & 0x00ff) | ((val as u32) << 8),
                        2 => voice.pw = (voice.pw & 0x0f00) | val as u32,
                        3 => voice.pw = (voice.pw & 0x00ff) | (((val as u32) & 0x0f) << 8),
                        4 => voice.set_control(val),
                        5 => {
                            voice.attack = val >> 4;
                            voice.decay = val & 0x0f;
                            if voice.env_state == EnvState::Attack {
                                voice.rate_period = RATE_PERIODS[voice.attack as usize];
                            } else if voice.env_state == EnvState::DecaySustain {
                                voice.rate_period = RATE_PERIODS[voice.decay as usize];
                            }
                        }
                        6 => {
                            voice.sustain = val >> 4;
                            voice.release = val & 0x0f;
                            if voice.env_state == EnvState::Release {
                                voice.rate_period = RATE_PERIODS[voice.release as usize];
                            }
                        }
                        _ => {}
                    }
                }
            }
            0x15 => self.fc = (self.fc & 0x7f8) | ((val as u32) & 0x07),
            0x16 => self.fc = (self.fc & 0x007) | ((val as u32) << 3),
            0x17 => {
                self.filt_mask = val & 0x0f;
                self.res = val >> 4;
            }
            0x18 => self.mode_vol = val,
            _ => {}
        }
    }

    /// Advance all voices by `n` chip cycles (n <= MAX_STEP).
    pub fn clock(&mut self, n: u32) {
        // Snapshot old accumulators / MSBs for sync + ring source calculation.
        let old_acc = [self.voices[0].acc, self.voices[1].acc, self.voices[2].acc];
        let old_msb = [
            old_acc[0] & 0x80_0000 != 0,
            old_acc[1] & 0x80_0000 != 0,
            old_acc[2] & 0x80_0000 != 0,
        ];

        // Advance oscillators and envelopes.
        let mut new_acc = old_acc;
        for v in 0..3 {
            let voice = &mut self.voices[v];
            voice.clock_env(n);
            if !voice.test() {
                let adv = voice.freq * n;
                let na = (voice.acc + adv) & 0xff_ffff;
                // Noise LFSR clocks on the rising edge of accumulator bit 19.
                if (voice.acc & 0x08_0000) == 0 && (na & 0x08_0000) != 0 {
                    Self::clock_noise(voice);
                }
                voice.acc = na;
                new_acc[v] = na;
            }
        }

        // Hard sync: voice v is synced by voice (v+2)%3 (0<-2, 1<-0, 2<-1).
        for v in 0..3 {
            let src = (v + 2) % 3;
            let src_rose = !old_msb[src] && (new_acc[src] & 0x80_0000 != 0);
            if self.voices[v].sync() && src_rose && !self.voices[v].test() {
                self.voices[v].acc = 0;
                new_acc[v] = 0;
            }
        }

        for v in 0..3 {
            self.voices[v].prev_msb = new_acc[v] & 0x80_0000 != 0;
        }
    }

    #[inline]
    fn clock_noise(voice: &mut Voice) {
        let reg = voice.noise_lfsr;
        let bit0 = ((reg >> 22) ^ (reg >> 17)) & 1;
        voice.noise_lfsr = ((reg << 1) | bit0) & 0x7f_ffff;
    }

    /// 12-bit waveform output for one voice, given the ring-mod source MSB.
    #[inline]
    fn waveform(voice: &Voice, ring_src_msb: bool) -> i32 {
        let ctrl = voice.control;
        if voice.test() && (ctrl & 0x80) == 0 {
            // test bit holds oscillator; only noise keeps its (frozen) value.
        }
        let acc = voice.acc;
        let mut out: u32 = 0xfff; // start all-ones; AND each selected waveform
        let mut any = false;

        // Triangle (bit 4)
        if ctrl & 0x10 != 0 {
            let msb = (acc & 0x80_0000 != 0) ^ (voice.ring() && ring_src_msb);
            let t = if msb { !acc } else { acc };
            out &= (t >> 11) & 0xfff;
            any = true;
        }
        // Sawtooth (bit 5)
        if ctrl & 0x20 != 0 {
            out &= (acc >> 12) & 0xfff;
            any = true;
        }
        // Pulse (bit 6)
        if ctrl & 0x40 != 0 {
            let p = if voice.test() || (acc >> 12) >= voice.pw {
                0xfff
            } else {
                0x000
            };
            out &= p;
            any = true;
        }
        // Noise (bit 7)
        if ctrl & 0x80 != 0 {
            let reg = voice.noise_lfsr;
            let n = (((reg >> 20) & 1) << 7)
                | (((reg >> 18) & 1) << 6)
                | (((reg >> 14) & 1) << 5)
                | (((reg >> 11) & 1) << 4)
                | (((reg >> 9) & 1) << 3)
                | (((reg >> 5) & 1) << 2)
                | (((reg >> 2) & 1) << 1)
                | ((reg >> 0) & 1);
            out &= n << 4;
            any = true;
        }
        if !any {
            return 0;
        }
        // center to signed and apply envelope (scaled down to keep values small)
        ((out as i32) - 2048) * (voice.env as i32) >> 8
    }

    /// Produce one output sample (signed, roughly +-24000 at full volume).
    pub fn output(&mut self) -> i32 {
        let master_vol = (self.mode_vol & 0x0f) as i32;
        let three_off = self.mode_vol & 0x80 != 0;

        let mut direct: i32 = 0;
        let mut filt_in: i32 = 0;

        for v in 0..3 {
            let src = (v + 2) % 3;
            let ring_src_msb = self.voices[src].prev_msb;
            let vo = Self::waveform(&self.voices[v], ring_src_msb);
            let routed = self.filt_mask & (1 << v) != 0;
            if routed {
                filt_in += vo;
            } else {
                // voice 3 can be muted from the direct path via the 3-off bit
                if v == 2 && three_off {
                    // muted
                } else {
                    direct += vo;
                }
            }
        }

        // Chamberlin state-variable filter (Q16 coefficient, small-scale state).
        let w0 = self.w0_table[(self.fc & 0x7ff) as usize];
        // damping factor 1/Q from resonance: higher res -> less damping.
        // q1 ranges ~1.3 (res 0) down to ~0.5 (res 15), in Q16.
        let q1 = (85000 - (self.res as i32) * 3400).max(20000);
        let hp = filt_in - self.f_lp - ((q1 * self.f_bp) >> 16);
        self.f_bp += (w0 * hp) >> 16;
        self.f_lp += (w0 * self.f_bp) >> 16;
        // clamp filter state to prevent runaway
        let lim = 1 << 20;
        if self.f_bp > lim {
            self.f_bp = lim;
        } else if self.f_bp < -lim {
            self.f_bp = -lim;
        }
        if self.f_lp > lim {
            self.f_lp = lim;
        } else if self.f_lp < -lim {
            self.f_lp = -lim;
        }

        let mut filt_out = 0;
        if self.mode_vol & 0x10 != 0 {
            filt_out += self.f_lp;
        }
        if self.mode_vol & 0x20 != 0 {
            filt_out += self.f_bp;
        }
        if self.mode_vol & 0x40 != 0 {
            filt_out += hp;
        }

        let total = direct + filt_out;
        // Apply master volume and scale into i16 territory; clamp.
        let s = (total * master_vol * 4) / 15;
        s.clamp(-32767, 32767)
    }
}
