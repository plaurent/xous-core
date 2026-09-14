//! A tier-2 (batched-clocking) MOS 6581/8580 SID model.
//!
//! Instead of advancing the chip one ~1 MHz cycle at a time, `clock(n)` advances
//! all state by up to `MAX_STEP` chip cycles at once. `n` is kept small enough
//! that at most one oscillator MSB (bit-23) transition occurs per step (so hard
//! sync/ring stay exact), while noise-LFSR (bit-19) edges — which can happen
//! several times per step — are counted exactly and clocked in a short loop.
//!
//! The per-cycle oscillator/envelope path (`clock`, `waveform`) is integer-only
//! and 32-bit: no floats, no 64-bit multiply/divide, so it stays cheap on
//! RV32IMAC even when called ~1 M times a second. The *filter* (`output`) is a
//! different story: it runs only once per (oversampled) output sample —
//! `FILTER_RATE` = 32 kHz, a few thousand times per second — and there the
//! coefficient×state products genuinely exceed i32 range. Its state and
//! coefficients are kept i32 but each product/sum is taken in i64, so the
//! multiplies widen i32×i32 → i64 (a `mul`/`mulh` pair) rather than a full
//! i64×i64 — a handful per sample, negligible next to the per-cycle path.
//! `new()` uses floats once at startup to precompute the cutoff (`g`) table.
//!
//! The analogue models here are deliberately lean approximations (AND-combined
//! waveforms, a topology-preserving / zero-delay-feedback state-variable filter).
//! They are not cycle-accurate, but reproduce the essential character of classic
//! tunes well. The filter and waveform stages are oversampled `OVERSAMPLE`x above
//! the codec rate so noise bursts and narrow pulses retain their transient
//! character instead of aliasing to a dull thud.

/// Largest number of chip cycles a single `clock()` step may advance. Chosen so
/// `freq(<=0xFFFF) * MAX_STEP < 2^23`, which bounds hard-sync/ring MSB (bit-23)
/// events to at most one per step. Noise (bit-19) can rise several times per step
/// at this size; `clock()` counts those edges exactly rather than sub-stepping.
/// At 128 a whole 8 kHz output sample (~123 chip cycles) is one `clock()` call,
/// versus ~8 calls at the old value of 16 — a big cut in per-call overhead.
pub const MAX_STEP: u32 = 128;

/// Codec (final) output sample rate. Must match `player::OUTPUT_RATE`.
pub const CODEC_RATE: u32 = 8_000;
/// How many times the waveform + filter stage is evaluated per codec sample. The
/// chip is advanced by a fraction of the sample's cycles between each evaluation
/// and the results are box-averaged, so energy above the codec Nyquist folds down
/// as a lowered noise floor rather than aliasing into the audible band. Bumping
/// this up costs `OVERSAMPLE` waveform+filter evaluations per output sample (not
/// `OVERSAMPLE`x the whole chip emulation — `clock()` still batches the chip
/// state). Drop to 2 (or 1) if hardware profiling shows 4 is too expensive.
pub const OVERSAMPLE: u32 = 4;
/// Rate at which the filter (and waveform sampling) actually runs.
pub const FILTER_RATE: u32 = CODEC_RATE * OVERSAMPLE;

/// reSID rate-counter periods indexed by the 4-bit attack/decay/release value.
#[rustfmt::skip]
static RATE_PERIODS: [u16; 16] = [
    9, 32, 63, 95, 149, 220, 267, 313,
    392, 977, 1954, 3126, 3907, 11720, 19532, 31251,
];

/// Filter damping `k = 1/Q` indexed by the 4-bit resonance register, in Q16.
///
/// Runs from ~1.4 (res 0, `Q ≈ 0.71`, heavily damped, no resonant peak) down to
/// ~0.45 (res 15, `Q ≈ 2.2`) on a smooth exponential curve. That upper bound
/// matches the real 6581, whose resonance is famously weak (max `Q` around
/// 2.2) — a much stronger peak would be unrealistic and, at these signal levels,
/// would clip the output on resonant sweeps. The 6581's resonance is nonlinear,
/// so a 16-entry table is both simpler and cheaper than a formula. The ZDF filter
/// (see [`Sid::output`]) is unconditionally stable for any `k >= 0`, so unlike
/// the old Chamberlin form there is no `f + q1 < 2` constraint to satisfy; the
/// 0.45 floor also keeps res 15 from ringing indefinitely.
#[rustfmt::skip]
static RES_K_Q16: [i32; 16] = [
    91750, 85064, 78865, 73118, 67790, 62850, 58270, 54023,
    50086, 46436, 43052, 39915, 37006, 34310, 31809, 29491,
];

/// Hard-sync / ring-mod source voice for each voice: voice `v` is modulated by
/// voice `(v+2) % 3`. Precomputed as a table so the hot loops avoid a `% 3`
/// (a reciprocal-multiply sequence on RV32, which has no cheap general divide).
const SYNC_SRC: [usize; 3] = [2, 0, 1];

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvState {
    Attack,
    DecaySustain,
    Release,
}

struct Voice {
    // register-derived parameters
    freq: u32,   // 16-bit
    pw: u32,     // 12-bit pulse width
    control: u8, // waveform + gate/sync/ring/test bits
    attack: u8,  // 0..15
    decay: u8,   // 0..15
    sustain: u8, // 0..15
    release: u8, // 0..15

    // oscillator state
    acc: u32,        // 24-bit phase accumulator
    noise_lfsr: u32, // 23-bit
    prev_msb: bool,  // accumulator bit23 at end of previous step (for sync source)

    // envelope state
    env_state: EnvState,
    env: u8, // 0..255
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
    fn gate(&self) -> bool { self.control & 0x01 != 0 }

    #[inline]
    fn sync(&self) -> bool { self.control & 0x02 != 0 }

    #[inline]
    fn ring(&self) -> bool { self.control & 0x04 != 0 }

    #[inline]
    fn test(&self) -> bool { self.control & 0x08 != 0 }

    /// Advance the envelope generator by `n` cycles (batched over rate-hits).
    fn clock_env(&mut self, mut n: u32) {
        while n > 0 {
            // If the rate period was lowered below the current counter — which
            // happens constantly, e.g. a fast attack (period 9) gated on while
            // the counter is still thousands-deep into a slow release (period
            // 31251) — then `rate_period - rate_counter` underflows u16 into a
            // huge value, `until > n` is always true, and `env_hit()` would
            // never fire again: the envelope stalls at its current level
            // forever. (On this tune that silenced voice 1 entirely — drums and
            // all — and made other voices pump as notes intermittently failed to
            // attack.) Detect the overshoot and fire immediately to resync.
            if self.rate_counter >= self.rate_period {
                self.rate_counter = 0;
                self.env_hit();
                continue;
            }
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

/// 6581 filter cutoff curve as `(fc_register, cutoff_hz)` breakpoints, following
/// reSID's measured `f0_6581` shape: nearly flat around ~220 Hz until fc≈0x200,
/// then a steep, curved rise to ~8.6 kHz. Linearly interpolated into the 2048-
/// entry `g_table`. The real chip is markedly nonlinear here, and the old linear
/// 30 Hz..3800 Hz map made every mid/high cutoff sound the same.
#[rustfmt::skip]
static F0_POINTS_6581: [(u32, f32); 27] = [
    (0,    220.0), (128,  230.0), (256,  250.0), (384,  300.0),
    (512,  420.0), (640,  780.0), (768,  1600.0), (832,  2300.0),
    (896,  3200.0), (960,  4300.0), (1024, 5000.0), (1088, 5400.0),
    (1152, 5700.0), (1216, 6000.0), (1280, 6200.0), (1344, 6500.0),
    (1408, 6700.0), (1472, 6900.0), (1536, 7100.0), (1600, 7300.0),
    (1664, 7500.0), (1728, 7700.0), (1792, 7900.0), (1856, 8100.0),
    (1920, 8300.0), (1984, 8500.0), (2047, 8600.0),
];

/// Cutoff frequency in Hz for an 11-bit `fc` register value, per chip model.
///
/// The 8580 filter is very nearly linear from ~30 Hz to ~12.5 kHz; the 6581 is
/// strongly nonlinear and uses the interpolated [`F0_POINTS_6581`] breakpoints.
///
/// Crucially, both curves are then **scaled** so the register's full range maps
/// just under the *output* Nyquist (`CODEC_RATE/2`), not up to the chip's real
/// 8–12 kHz. The filter is oversampled (`FILTER_RATE` = 32 kHz) but the final
/// codec output is still 8 kHz, so any cutoff above ~4 kHz puts the filter's
/// resonant peak and rolloff into a band the codec cannot reproduce — which is
/// exactly what silences filter-swept percussion (e.g. this file's drum, whose
/// sweep sits at register `fc` 1024–1536: ~5–7 kHz on the raw 6581 curve, dead
/// air at 8 kHz out). Scaling (rather than clipping) preserves the curve's shape
/// and every sweep within the audible band; clipping would recollapse all high
/// cutoffs to one value, the very bug the ZDF rewrite set out to remove. When
/// `CODEC_RATE` rises (e.g. a future 16 kHz path, plan Phase 6) the ceiling
/// rises with it and more of the true curve becomes usable.
fn cutoff_curve_hz(fc: u32, is_8580: bool) -> f32 {
    let ceiling = 0.475 * CODEC_RATE as f32; // ~3800 Hz at 8 kHz output
    let fc = fc.min(2047);
    if is_8580 {
        let raw = 30.0 + (fc as f32 / 2047.0) * 12_470.0; // ~0..12.5 kHz
        return raw * (ceiling / 12_500.0);
    }
    let pts = &F0_POINTS_6581;
    let raw = if fc <= pts[0].0 {
        pts[0].1
    } else {
        let mut hz = pts[pts.len() - 1].1;
        for w in pts.windows(2) {
            let (x0, y0) = w[0];
            let (x1, y1) = w[1];
            if fc <= x1 {
                let t = (fc - x0) as f32 / (x1 - x0) as f32;
                hz = y0 + t * (y1 - y0);
                break;
            }
        }
        hz
    };
    raw * (ceiling / 8_600.0) // 6581 raw curve tops out ~8.6 kHz
}

pub struct Sid {
    voices: [Voice; 3],
    // filter / global
    fc: u32,       // 11-bit cutoff
    res: u8,       // 4-bit resonance
    filt_mask: u8, // which voices routed to filter (bits 0..2), bit3 = ext
    mode_vol: u8,  // bit0-3 volume, bit4 LP, bit5 BP, bit6 HP, bit7 3off

    // Zero-delay-feedback (topology-preserving) state-variable filter.
    // `ic1eq`/`ic2eq` are the two integrator memories, on the signal scale (not
    // Q-shifted). `a1`/`a2`/`a3`/`k` are the derived coefficients in Q16; they
    // depend only on `fc`/`res`, so they are recomputed lazily via `filter_dirty`
    // when a filter register is written rather than every sample.
    //
    // All of these fit comfortably in i32 (states peak in the low tens of
    // thousands even at max resonance; coefficients are <= ~92000). They are kept
    // i32 so the per-sample multiplies widen i32*i32 -> i64 (a `mul`/`mulh` pair
    // on RV32) instead of a full i64*i64; only the products and their sums are
    // taken in i64, which is where the range is actually needed.
    ic1eq: i32,
    ic2eq: i32,
    a1: i32,
    a2: i32,
    a3: i32,
    k: i32,
    filter_dirty: bool,
    // precomputed prewarped-cutoff table: fc(0..2047) -> g = tan(pi*fc_hz/FILTER_RATE), Q16
    g_table: [i32; 2048],
}

impl Sid {
    pub fn new(is_8580: bool) -> Self {
        // The filter runs at FILTER_RATE (32 kHz), so its usable range extends to
        // ~16 kHz — the whole SID cutoff range is now representable. `g` is the
        // prewarped ZDF coefficient tan(pi*fc_hz/fs); cap fc_hz at 0.45*fs so
        // tan() stays well-conditioned (it blows up toward Nyquist).
        let fs = FILTER_RATE as f32;
        let fc_cap = 0.45 * fs;
        let mut g_table = [0i32; 2048];
        for (i, slot) in g_table.iter_mut().enumerate() {
            let mut cutoff_hz = cutoff_curve_hz(i as u32, is_8580);
            if cutoff_hz > fc_cap {
                cutoff_hz = fc_cap;
            }
            let g = (core::f32::consts::PI * cutoff_hz / fs).tan();
            *slot = (g * 65536.0) as i32;
        }
        Sid {
            voices: [Voice::new(), Voice::new(), Voice::new()],
            fc: 0,
            res: 0,
            filt_mask: 0,
            mode_vol: 0x0f,
            ic1eq: 0,
            ic2eq: 0,
            a1: 0,
            a2: 0,
            a3: 0,
            k: 0,
            filter_dirty: true,
            g_table,
        }
    }

    /// Recompute the ZDF coefficients from the current `fc`/`res`. Called lazily
    /// from `output()` when a filter register has been written since the last
    /// evaluation — roughly once per frame in practice, so the one division here
    /// is free. See `output()` for the update equations.
    fn recompute_filter(&mut self) {
        let g = self.g_table[(self.fc & 0x7ff) as usize] as i64; // Q16
        let k = RES_K_Q16[(self.res & 0x0f) as usize] as i64; // Q16
        // denom = 1 + g*(g + k)   (all Q16); a1 = 1/denom, a2 = g*a1, a3 = g*a2.
        // Done in i64 here (this runs only when a filter register changes, ~once
        // per frame), then stored as i32 for the cheap per-sample path.
        let denom = (1i64 << 16) + ((g * (g + k)) >> 16);
        let a1 = (1i64 << 32) / denom; // Q16
        let a2 = (g * a1) >> 16; // Q16
        let a3 = (g * a2) >> 16; // Q16
        self.a1 = a1 as i32;
        self.a2 = a2 as i32;
        self.a3 = a3 as i32;
        self.k = k as i32;
        self.filter_dirty = false;
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
            0x15 => {
                self.fc = (self.fc & 0x7f8) | ((val as u32) & 0x07);
                self.filter_dirty = true;
            }
            0x16 => {
                self.fc = (self.fc & 0x007) | ((val as u32) << 3);
                self.filter_dirty = true;
            }
            0x17 => {
                self.filt_mask = val & 0x0f;
                self.res = val >> 4;
                self.filter_dirty = true;
            }
            0x18 => self.mode_vol = val,
            _ => {}
        }
    }

    /// Advance all voices by `n` chip cycles (n <= MAX_STEP).
    pub fn clock(&mut self, n: u32) {
        // Snapshot old accumulators / MSBs for sync + ring source calculation.
        let old_acc = [self.voices[0].acc, self.voices[1].acc, self.voices[2].acc];
        let old_msb = [old_acc[0] & 0x80_0000 != 0, old_acc[1] & 0x80_0000 != 0, old_acc[2] & 0x80_0000 != 0];

        // Advance oscillators and envelopes.
        let mut new_acc = old_acc;
        for v in 0..3 {
            let voice = &mut self.voices[v];
            voice.clock_env(n);
            if !voice.test() {
                let a0 = voice.acc;
                let adv = voice.freq * n; // < 2^23 for n <= MAX_STEP
                let na = (a0 + adv) & 0xff_ffff;
                // The noise LFSR clocks on every rising edge of accumulator bit 19,
                // i.e. each time the accumulator passes a value ≡ 0x80000 (mod
                // 0x100000). Count those edges over the step exactly (a0 + adv <
                // 2^25 fits in i32) and clock the LFSR that many times.
                let lo = a0 as i32 - 0x8_0000;
                let hi = (a0 + adv) as i32 - 0x8_0000;
                // edges = floor(hi/2^20) - floor(lo/2^20). The divisor is a
                // positive power of two, so floor-division is exactly an
                // arithmetic right shift (same rounding toward -inf for the
                // negative `lo` case) — and a shift avoids the soft-division
                // routine a `div_euclid` can compile to on RV32 (no HW divide).
                let edges = (hi >> 20) - (lo >> 20);
                for _ in 0..edges {
                    Self::clock_noise(voice);
                }
                voice.acc = na;
                new_acc[v] = na;
            }
        }

        // Hard sync: voice v is synced by voice (v+2)%3 (0<-2, 1<-0, 2<-1).
        for v in 0..3 {
            let src = SYNC_SRC[v];
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
        // The test bit's oscillator hold is already handled in `clock()`, which
        // skips advancing `acc` while test is set — so `acc` is naturally frozen
        // here and no special-casing is needed in the waveform read.
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
            let p = if voice.test() || (acc >> 12) >= voice.pw { 0xfff } else { 0x000 };
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
            let src = SYNC_SRC[v];
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

        // Zero-delay-feedback (topology-preserving) state-variable filter. Unlike
        // the old Chamberlin form this is unconditionally stable for any cutoff
        // and resonance, so it needs no runaway clamp and does not boost treble
        // above cutoff. Coefficients depend only on fc/res and are recomputed
        // lazily; the per-sample work is the update below.
        //
        //   v3 = x - ic2eq
        //   v1 = a1*ic1eq + a2*v3
        //   v2 = ic2eq + a2*ic1eq + a3*v3
        //   ic1eq = 2*v1 - ic1eq ;  ic2eq = 2*v2 - ic2eq
        //   lp = v2 ; bp = v1 ; hp = x - k*v1 - v2
        //
        // Products of a Q16 coefficient with a signal-scale state exceed i32 at
        // high resonance (state can reach ~Q * input), so each product/sum is
        // taken in i64 — but the operands are i32, so these widen i32*i32 -> i64
        // (a `mul`/`mulh` pair on RV32) rather than a full i64*i64. Results are
        // shifted back down and stored as i32. A handful of widening multiplies
        // per (32 kHz) filter sample, confined to this path — not the per-cycle
        // oscillator loop.
        if self.filter_dirty {
            self.recompute_filter();
        }
        let x = filt_in;
        let v3 = x - self.ic2eq;
        let v1 = ((self.a1 as i64 * self.ic1eq as i64 + self.a2 as i64 * v3 as i64) >> 16) as i32;
        let v2 = (self.ic2eq as i64
            + ((self.a2 as i64 * self.ic1eq as i64 + self.a3 as i64 * v3 as i64) >> 16)) as i32;
        self.ic1eq = 2 * v1 - self.ic1eq;
        self.ic2eq = 2 * v2 - self.ic2eq;
        // Tripwire (test builds only): the ZDF form keeps state bounded by
        // construction, so an integrator that grows past ~2^21 (well above the
        // ~10^5 a heavily resonant three-voice signal can reach) means a real
        // regression — an overflow, a bad coefficient, or a broken update.
        debug_assert!(
            self.ic1eq.abs() < (1 << 21) && self.ic2eq.abs() < (1 << 21),
            "SID ZDF filter state runaway"
        );
        let lp = v2;
        let bp = v1;
        let hp = x - ((self.k as i64 * v1 as i64) >> 16) as i32 - v2;

        let mut filt_out: i32 = 0;
        if self.mode_vol & 0x10 != 0 {
            filt_out += lp;
        }
        if self.mode_vol & 0x20 != 0 {
            filt_out += bp;
        }
        if self.mode_vol & 0x40 != 0 {
            filt_out += hp;
        }

        let total = direct + filt_out;
        // Apply master volume and scale into i16 territory; clamp. Peak `total`
        // (~5*10^4) * 15 * 4 stays well inside i32, so no widening is needed here.
        let s = (total * master_vol * 4) / 15;
        s.clamp(-32767, 32767)
    }
}
