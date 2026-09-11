//! A compact but complete NMOS 6502 core with a flat 64 KiB address space.
//!
//! Writes to the SID register range ($D400..=$D418) are captured into a
//! cycle-stamped log (`writes`) instead of being applied directly, so the SID
//! synthesis engine can consume them while it batch-clocks. Reads of OSC3
//! ($D41B) and ENV3 ($D41C) return values fed back from the engine.
//!
//! Only the pieces a PSID player needs are implemented; there is no interrupt
//! hardware, no banking of $01, and decimal mode follows the documented BCD
//! behaviour (good enough for the rare tune that touches it).

extern crate alloc;
use alloc::vec::Vec;

#[derive(Clone, Copy)]
pub struct RegWrite {
    /// Cycle offset from the start of the current run() call.
    pub cycle: u32,
    pub reg: u8,
    pub val: u8,
}

// Status flag bit masks.
const FLAG_C: u8 = 0x01;
const FLAG_Z: u8 = 0x02;
const FLAG_I: u8 = 0x04;
const FLAG_D: u8 = 0x08;
const FLAG_B: u8 = 0x10;
const FLAG_U: u8 = 0x20; // unused, always set
const FLAG_V: u8 = 0x40;
const FLAG_N: u8 = 0x80;

pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub p: u8,
    pub mem: Vec<u8>, // 65536 bytes
    /// running cycle count within the current run() call
    pub cycle: u32,
    pub writes: Vec<RegWrite>,
    /// OSC3 / ENV3 readback values, updated by the player from engine state.
    pub osc3: u8,
    pub env3: u8,
}

impl Cpu {
    pub fn new() -> Self {
        let mut mem = Vec::new();
        mem.resize(65536, 0);
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xff,
            pc: 0,
            p: FLAG_U | FLAG_I,
            mem,
            cycle: 0,
            writes: Vec::with_capacity(512),
            osc3: 0,
            env3: 0,
        }
    }

    #[inline]
    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xd41b => self.osc3,
            0xd41c => self.env3,
            _ => self.mem[addr as usize],
        }
    }

    #[inline]
    pub fn write(&mut self, addr: u16, val: u8) {
        if (0xd400..=0xd418).contains(&addr) {
            self.writes.push(RegWrite {
                cycle: self.cycle,
                reg: (addr - 0xd400) as u8,
                val,
            });
        }
        // Mirror the whole SID page write into RAM too; harmless and lets tunes
        // that read back their own shadow copies work if they live elsewhere.
        self.mem[addr as usize] = val;
    }

    #[inline]
    fn read16(&self, addr: u16) -> u16 {
        (self.read(addr) as u16) | ((self.read(addr.wrapping_add(1)) as u16) << 8)
    }

    // ---- stack ----
    #[inline]
    fn push(&mut self, v: u8) {
        self.mem[0x0100 + self.sp as usize] = v;
        self.sp = self.sp.wrapping_sub(1);
    }
    #[inline]
    fn pull(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.mem[0x0100 + self.sp as usize]
    }

    // ---- flag helpers ----
    #[inline]
    fn set_zn(&mut self, v: u8) {
        self.p &= !(FLAG_Z | FLAG_N);
        if v == 0 {
            self.p |= FLAG_Z;
        }
        self.p |= v & FLAG_N;
    }
    #[inline]
    fn set_flag(&mut self, mask: u8, cond: bool) {
        if cond {
            self.p |= mask;
        } else {
            self.p &= !mask;
        }
    }
    #[inline]
    fn flag(&self, mask: u8) -> bool {
        self.p & mask != 0
    }

    // ---- fetch ----
    #[inline]
    fn fetch(&mut self) -> u8 {
        let v = self.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    #[inline]
    fn fetch16(&mut self) -> u16 {
        let lo = self.fetch() as u16;
        let hi = self.fetch() as u16;
        lo | (hi << 8)
    }

    // ================= addressing modes =================
    // Each returns the effective address; page-cross penalty is accounted where
    // relevant by the caller via `pc_crossed`.
    #[inline]
    fn a_zp(&mut self) -> u16 {
        self.fetch() as u16
    }
    #[inline]
    fn a_zpx(&mut self) -> u16 {
        (self.fetch().wrapping_add(self.x)) as u16
    }
    #[inline]
    fn a_zpy(&mut self) -> u16 {
        (self.fetch().wrapping_add(self.y)) as u16
    }
    #[inline]
    fn a_abs(&mut self) -> u16 {
        self.fetch16()
    }
    #[inline]
    fn a_absx(&mut self, extra: &mut u32) -> u16 {
        let base = self.fetch16();
        let addr = base.wrapping_add(self.x as u16);
        if (base & 0xff00) != (addr & 0xff00) {
            *extra += 1;
        }
        addr
    }
    #[inline]
    fn a_absy(&mut self, extra: &mut u32) -> u16 {
        let base = self.fetch16();
        let addr = base.wrapping_add(self.y as u16);
        if (base & 0xff00) != (addr & 0xff00) {
            *extra += 1;
        }
        addr
    }
    #[inline]
    fn a_izx(&mut self) -> u16 {
        let zp = self.fetch().wrapping_add(self.x);
        let lo = self.read(zp as u16) as u16;
        let hi = self.read(zp.wrapping_add(1) as u16) as u16;
        lo | (hi << 8)
    }
    #[inline]
    fn a_izy(&mut self, extra: &mut u32) -> u16 {
        let zp = self.fetch();
        let lo = self.read(zp as u16) as u16;
        let hi = self.read(zp.wrapping_add(1) as u16) as u16;
        let base = lo | (hi << 8);
        let addr = base.wrapping_add(self.y as u16);
        if (base & 0xff00) != (addr & 0xff00) {
            *extra += 1;
        }
        addr
    }

    // ================= ALU ops =================
    fn adc(&mut self, val: u8) {
        if self.flag(FLAG_D) {
            // BCD add
            let a = self.a;
            let mut lo = (a & 0x0f) + (val & 0x0f) + (if self.flag(FLAG_C) { 1 } else { 0 });
            let mut hi = (a >> 4) + (val >> 4);
            if lo > 9 {
                lo += 6;
                hi += 1;
            }
            // N/V computed from the binary-ish intermediate (approx NMOS behaviour)
            let bin = (a as u16).wrapping_add(val as u16).wrapping_add(if self.flag(FLAG_C) { 1 } else { 0 });
            self.set_flag(FLAG_Z, (bin & 0xff) == 0);
            self.set_flag(FLAG_N, (hi & 0x08) != 0);
            self.set_flag(
                FLAG_V,
                (!(a ^ val) & (a ^ ((hi << 4) | (lo & 0x0f))) & 0x80) != 0,
            );
            if hi > 9 {
                hi += 6;
            }
            self.set_flag(FLAG_C, hi > 0x0f);
            self.a = ((hi << 4) | (lo & 0x0f)) & 0xff;
        } else {
            let sum = (self.a as u16) + (val as u16) + (if self.flag(FLAG_C) { 1 } else { 0 });
            let res = sum as u8;
            self.set_flag(FLAG_C, sum > 0xff);
            self.set_flag(FLAG_V, (!(self.a ^ val) & (self.a ^ res) & 0x80) != 0);
            self.a = res;
            self.set_zn(res);
        }
    }

    fn sbc(&mut self, val: u8) {
        if self.flag(FLAG_D) {
            let a = self.a;
            let carry = if self.flag(FLAG_C) { 0 } else { 1 };
            let diff = (a as i16) - (val as i16) - carry as i16;
            let mut lo = (a & 0x0f) as i16 - (val & 0x0f) as i16 - carry as i16;
            let mut hi = (a >> 4) as i16 - (val >> 4) as i16;
            if lo < 0 {
                lo -= 6;
                hi -= 1;
            }
            if hi < 0 {
                hi -= 6;
            }
            let bin = (a as u16)
                .wrapping_sub(val as u16)
                .wrapping_sub(carry as u16);
            let res = bin as u8;
            self.set_flag(FLAG_C, diff >= 0);
            self.set_flag(FLAG_V, ((a ^ val) & (a ^ res) & 0x80) != 0);
            self.set_zn(res);
            self.a = (((hi << 4) | (lo & 0x0f)) & 0xff) as u8;
        } else {
            let val = val ^ 0xff;
            let sum = (self.a as u16) + (val as u16) + (if self.flag(FLAG_C) { 1 } else { 0 });
            let res = sum as u8;
            self.set_flag(FLAG_C, sum > 0xff);
            self.set_flag(FLAG_V, (!(self.a ^ val) & (self.a ^ res) & 0x80) != 0);
            self.a = res;
            self.set_zn(res);
        }
    }

    fn cmp_reg(&mut self, reg: u8, val: u8) {
        let r = (reg as u16).wrapping_sub(val as u16);
        self.set_flag(FLAG_C, reg >= val);
        self.set_zn(r as u8);
    }

    fn asl(&mut self, v: u8) -> u8 {
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = v << 1;
        self.set_zn(r);
        r
    }
    fn lsr(&mut self, v: u8) -> u8 {
        self.set_flag(FLAG_C, v & 0x01 != 0);
        let r = v >> 1;
        self.set_zn(r);
        r
    }
    fn rol(&mut self, v: u8) -> u8 {
        let c = if self.flag(FLAG_C) { 1 } else { 0 };
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = (v << 1) | c;
        self.set_zn(r);
        r
    }
    fn ror(&mut self, v: u8) -> u8 {
        let c = if self.flag(FLAG_C) { 0x80 } else { 0 };
        self.set_flag(FLAG_C, v & 0x01 != 0);
        let r = (v >> 1) | c;
        self.set_zn(r);
        r
    }

    fn branch(&mut self, cond: bool, extra: &mut u32) {
        let off = self.fetch() as i8 as i16;
        if cond {
            let old = self.pc;
            let new = (self.pc as i16).wrapping_add(off) as u16;
            *extra += 1;
            if (old & 0xff00) != (new & 0xff00) {
                *extra += 1;
            }
            self.pc = new;
        }
    }

    // Read-modify-write on memory helper.
    fn rmw<F: Fn(&mut Cpu, u8) -> u8>(&mut self, addr: u16, f: F) {
        let v = self.read(addr);
        let r = f(self, v);
        self.write(addr, r);
    }

    /// Execute a single instruction, updating `self.cycle`. Returns the number
    /// of cycles the instruction took.
    pub fn step(&mut self) -> u32 {
        let op = self.fetch();
        let mut extra: u32 = 0;
        let base: u32 = CYCLES[op as usize] as u32;

        match op {
            // ---- ORA ----
            0x09 => { let a = self.a_zp_imm(); self.a |= a; let v = self.a; self.set_zn(v); }
            0x05 => { let ad = self.a_zp(); let m = self.read(ad); self.a |= m; let v=self.a; self.set_zn(v); }
            0x15 => { let ad = self.a_zpx(); let m = self.read(ad); self.a |= m; let v=self.a; self.set_zn(v); }
            0x0d => { let ad = self.a_abs(); let m = self.read(ad); self.a |= m; let v=self.a; self.set_zn(v); }
            0x1d => { let ad = self.a_absx(&mut extra); let m = self.read(ad); self.a |= m; let v=self.a; self.set_zn(v); }
            0x19 => { let ad = self.a_absy(&mut extra); let m = self.read(ad); self.a |= m; let v=self.a; self.set_zn(v); }
            0x01 => { let ad = self.a_izx(); let m = self.read(ad); self.a |= m; let v=self.a; self.set_zn(v); }
            0x11 => { let ad = self.a_izy(&mut extra); let m = self.read(ad); self.a |= m; let v=self.a; self.set_zn(v); }

            // ---- AND ----
            0x29 => { let m = self.a_zp_imm(); self.a &= m; let v=self.a; self.set_zn(v); }
            0x25 => { let ad = self.a_zp(); let m = self.read(ad); self.a &= m; let v=self.a; self.set_zn(v); }
            0x35 => { let ad = self.a_zpx(); let m = self.read(ad); self.a &= m; let v=self.a; self.set_zn(v); }
            0x2d => { let ad = self.a_abs(); let m = self.read(ad); self.a &= m; let v=self.a; self.set_zn(v); }
            0x3d => { let ad = self.a_absx(&mut extra); let m = self.read(ad); self.a &= m; let v=self.a; self.set_zn(v); }
            0x39 => { let ad = self.a_absy(&mut extra); let m = self.read(ad); self.a &= m; let v=self.a; self.set_zn(v); }
            0x21 => { let ad = self.a_izx(); let m = self.read(ad); self.a &= m; let v=self.a; self.set_zn(v); }
            0x31 => { let ad = self.a_izy(&mut extra); let m = self.read(ad); self.a &= m; let v=self.a; self.set_zn(v); }

            // ---- EOR ----
            0x49 => { let m = self.a_zp_imm(); self.a ^= m; let v=self.a; self.set_zn(v); }
            0x45 => { let ad = self.a_zp(); let m = self.read(ad); self.a ^= m; let v=self.a; self.set_zn(v); }
            0x55 => { let ad = self.a_zpx(); let m = self.read(ad); self.a ^= m; let v=self.a; self.set_zn(v); }
            0x4d => { let ad = self.a_abs(); let m = self.read(ad); self.a ^= m; let v=self.a; self.set_zn(v); }
            0x5d => { let ad = self.a_absx(&mut extra); let m = self.read(ad); self.a ^= m; let v=self.a; self.set_zn(v); }
            0x59 => { let ad = self.a_absy(&mut extra); let m = self.read(ad); self.a ^= m; let v=self.a; self.set_zn(v); }
            0x41 => { let ad = self.a_izx(); let m = self.read(ad); self.a ^= m; let v=self.a; self.set_zn(v); }
            0x51 => { let ad = self.a_izy(&mut extra); let m = self.read(ad); self.a ^= m; let v=self.a; self.set_zn(v); }

            // ---- ADC ----
            0x69 => { let m = self.a_zp_imm(); self.adc(m); }
            0x65 => { let ad = self.a_zp(); let m=self.read(ad); self.adc(m); }
            0x75 => { let ad = self.a_zpx(); let m=self.read(ad); self.adc(m); }
            0x6d => { let ad = self.a_abs(); let m=self.read(ad); self.adc(m); }
            0x7d => { let ad = self.a_absx(&mut extra); let m=self.read(ad); self.adc(m); }
            0x79 => { let ad = self.a_absy(&mut extra); let m=self.read(ad); self.adc(m); }
            0x61 => { let ad = self.a_izx(); let m=self.read(ad); self.adc(m); }
            0x71 => { let ad = self.a_izy(&mut extra); let m=self.read(ad); self.adc(m); }

            // ---- SBC ----
            0xe9 | 0xeb => { let m = self.a_zp_imm(); self.sbc(m); }
            0xe5 => { let ad = self.a_zp(); let m=self.read(ad); self.sbc(m); }
            0xf5 => { let ad = self.a_zpx(); let m=self.read(ad); self.sbc(m); }
            0xed => { let ad = self.a_abs(); let m=self.read(ad); self.sbc(m); }
            0xfd => { let ad = self.a_absx(&mut extra); let m=self.read(ad); self.sbc(m); }
            0xf9 => { let ad = self.a_absy(&mut extra); let m=self.read(ad); self.sbc(m); }
            0xe1 => { let ad = self.a_izx(); let m=self.read(ad); self.sbc(m); }
            0xf1 => { let ad = self.a_izy(&mut extra); let m=self.read(ad); self.sbc(m); }

            // ---- CMP ----
            0xc9 => { let m = self.a_zp_imm(); let a=self.a; self.cmp_reg(a,m); }
            0xc5 => { let ad=self.a_zp(); let m=self.read(ad); let a=self.a; self.cmp_reg(a,m); }
            0xd5 => { let ad=self.a_zpx(); let m=self.read(ad); let a=self.a; self.cmp_reg(a,m); }
            0xcd => { let ad=self.a_abs(); let m=self.read(ad); let a=self.a; self.cmp_reg(a,m); }
            0xdd => { let ad=self.a_absx(&mut extra); let m=self.read(ad); let a=self.a; self.cmp_reg(a,m); }
            0xd9 => { let ad=self.a_absy(&mut extra); let m=self.read(ad); let a=self.a; self.cmp_reg(a,m); }
            0xc1 => { let ad=self.a_izx(); let m=self.read(ad); let a=self.a; self.cmp_reg(a,m); }
            0xd1 => { let ad=self.a_izy(&mut extra); let m=self.read(ad); let a=self.a; self.cmp_reg(a,m); }

            // ---- CPX / CPY ----
            0xe0 => { let m=self.a_zp_imm(); let x=self.x; self.cmp_reg(x,m); }
            0xe4 => { let ad=self.a_zp(); let m=self.read(ad); let x=self.x; self.cmp_reg(x,m); }
            0xec => { let ad=self.a_abs(); let m=self.read(ad); let x=self.x; self.cmp_reg(x,m); }
            0xc0 => { let m=self.a_zp_imm(); let y=self.y; self.cmp_reg(y,m); }
            0xc4 => { let ad=self.a_zp(); let m=self.read(ad); let y=self.y; self.cmp_reg(y,m); }
            0xcc => { let ad=self.a_abs(); let m=self.read(ad); let y=self.y; self.cmp_reg(y,m); }

            // ---- LDA ----
            0xa9 => { let m=self.a_zp_imm(); self.a=m; self.set_zn(m); }
            0xa5 => { let ad=self.a_zp(); let m=self.read(ad); self.a=m; self.set_zn(m); }
            0xb5 => { let ad=self.a_zpx(); let m=self.read(ad); self.a=m; self.set_zn(m); }
            0xad => { let ad=self.a_abs(); let m=self.read(ad); self.a=m; self.set_zn(m); }
            0xbd => { let ad=self.a_absx(&mut extra); let m=self.read(ad); self.a=m; self.set_zn(m); }
            0xb9 => { let ad=self.a_absy(&mut extra); let m=self.read(ad); self.a=m; self.set_zn(m); }
            0xa1 => { let ad=self.a_izx(); let m=self.read(ad); self.a=m; self.set_zn(m); }
            0xb1 => { let ad=self.a_izy(&mut extra); let m=self.read(ad); self.a=m; self.set_zn(m); }

            // ---- LDX ----
            0xa2 => { let m=self.a_zp_imm(); self.x=m; self.set_zn(m); }
            0xa6 => { let ad=self.a_zp(); let m=self.read(ad); self.x=m; self.set_zn(m); }
            0xb6 => { let ad=self.a_zpy(); let m=self.read(ad); self.x=m; self.set_zn(m); }
            0xae => { let ad=self.a_abs(); let m=self.read(ad); self.x=m; self.set_zn(m); }
            0xbe => { let ad=self.a_absy(&mut extra); let m=self.read(ad); self.x=m; self.set_zn(m); }

            // ---- LDY ----
            0xa0 => { let m=self.a_zp_imm(); self.y=m; self.set_zn(m); }
            0xa4 => { let ad=self.a_zp(); let m=self.read(ad); self.y=m; self.set_zn(m); }
            0xb4 => { let ad=self.a_zpx(); let m=self.read(ad); self.y=m; self.set_zn(m); }
            0xac => { let ad=self.a_abs(); let m=self.read(ad); self.y=m; self.set_zn(m); }
            0xbc => { let ad=self.a_absx(&mut extra); let m=self.read(ad); self.y=m; self.set_zn(m); }

            // ---- STA ----
            0x85 => { let ad=self.a_zp(); let a=self.a; self.write(ad,a); }
            0x95 => { let ad=self.a_zpx(); let a=self.a; self.write(ad,a); }
            0x8d => { let ad=self.a_abs(); let a=self.a; self.write(ad,a); }
            0x9d => { let ad=self.a_absx(&mut extra); let a=self.a; self.write(ad,a); }
            0x99 => { let ad=self.a_absy(&mut extra); let a=self.a; self.write(ad,a); }
            0x81 => { let ad=self.a_izx(); let a=self.a; self.write(ad,a); }
            0x91 => { let ad=self.a_izy(&mut extra); let a=self.a; self.write(ad,a); }

            // ---- STX / STY ----
            0x86 => { let ad=self.a_zp(); let x=self.x; self.write(ad,x); }
            0x96 => { let ad=self.a_zpy(); let x=self.x; self.write(ad,x); }
            0x8e => { let ad=self.a_abs(); let x=self.x; self.write(ad,x); }
            0x84 => { let ad=self.a_zp(); let y=self.y; self.write(ad,y); }
            0x94 => { let ad=self.a_zpx(); let y=self.y; self.write(ad,y); }
            0x8c => { let ad=self.a_abs(); let y=self.y; self.write(ad,y); }

            // ---- transfers ----
            0xaa => { let v=self.a; self.x=v; self.set_zn(v); }
            0xa8 => { let v=self.a; self.y=v; self.set_zn(v); }
            0x8a => { let v=self.x; self.a=v; self.set_zn(v); }
            0x98 => { let v=self.y; self.a=v; self.set_zn(v); }
            0xba => { let v=self.sp; self.x=v; self.set_zn(v); }
            0x9a => { self.sp=self.x; }

            // ---- inc/dec regs ----
            0xe8 => { self.x=self.x.wrapping_add(1); let v=self.x; self.set_zn(v); }
            0xca => { self.x=self.x.wrapping_sub(1); let v=self.x; self.set_zn(v); }
            0xc8 => { self.y=self.y.wrapping_add(1); let v=self.y; self.set_zn(v); }
            0x88 => { self.y=self.y.wrapping_sub(1); let v=self.y; self.set_zn(v); }

            // ---- inc/dec mem ----
            0xe6 => { let ad=self.a_zp(); self.rmw(ad,|c,v|{let r=v.wrapping_add(1); c.set_zn(r); r}); }
            0xf6 => { let ad=self.a_zpx(); self.rmw(ad,|c,v|{let r=v.wrapping_add(1); c.set_zn(r); r}); }
            0xee => { let ad=self.a_abs(); self.rmw(ad,|c,v|{let r=v.wrapping_add(1); c.set_zn(r); r}); }
            0xfe => { let ad=self.a_absx(&mut extra); self.rmw(ad,|c,v|{let r=v.wrapping_add(1); c.set_zn(r); r}); }
            0xc6 => { let ad=self.a_zp(); self.rmw(ad,|c,v|{let r=v.wrapping_sub(1); c.set_zn(r); r}); }
            0xd6 => { let ad=self.a_zpx(); self.rmw(ad,|c,v|{let r=v.wrapping_sub(1); c.set_zn(r); r}); }
            0xce => { let ad=self.a_abs(); self.rmw(ad,|c,v|{let r=v.wrapping_sub(1); c.set_zn(r); r}); }
            0xde => { let ad=self.a_absx(&mut extra); self.rmw(ad,|c,v|{let r=v.wrapping_sub(1); c.set_zn(r); r}); }

            // ---- shifts on A ----
            0x0a => { let v=self.a; let r=self.asl(v); self.a=r; }
            0x4a => { let v=self.a; let r=self.lsr(v); self.a=r; }
            0x2a => { let v=self.a; let r=self.rol(v); self.a=r; }
            0x6a => { let v=self.a; let r=self.ror(v); self.a=r; }

            // ---- shifts on mem ----
            0x06 => { let ad=self.a_zp(); self.rmw(ad,|c,v|c.asl(v)); }
            0x16 => { let ad=self.a_zpx(); self.rmw(ad,|c,v|c.asl(v)); }
            0x0e => { let ad=self.a_abs(); self.rmw(ad,|c,v|c.asl(v)); }
            0x1e => { let ad=self.a_absx(&mut extra); self.rmw(ad,|c,v|c.asl(v)); }
            0x46 => { let ad=self.a_zp(); self.rmw(ad,|c,v|c.lsr(v)); }
            0x56 => { let ad=self.a_zpx(); self.rmw(ad,|c,v|c.lsr(v)); }
            0x4e => { let ad=self.a_abs(); self.rmw(ad,|c,v|c.lsr(v)); }
            0x5e => { let ad=self.a_absx(&mut extra); self.rmw(ad,|c,v|c.lsr(v)); }
            0x26 => { let ad=self.a_zp(); self.rmw(ad,|c,v|c.rol(v)); }
            0x36 => { let ad=self.a_zpx(); self.rmw(ad,|c,v|c.rol(v)); }
            0x2e => { let ad=self.a_abs(); self.rmw(ad,|c,v|c.rol(v)); }
            0x3e => { let ad=self.a_absx(&mut extra); self.rmw(ad,|c,v|c.rol(v)); }
            0x66 => { let ad=self.a_zp(); self.rmw(ad,|c,v|c.ror(v)); }
            0x76 => { let ad=self.a_zpx(); self.rmw(ad,|c,v|c.ror(v)); }
            0x6e => { let ad=self.a_abs(); self.rmw(ad,|c,v|c.ror(v)); }
            0x7e => { let ad=self.a_absx(&mut extra); self.rmw(ad,|c,v|c.ror(v)); }

            // ---- BIT ----
            0x24 => { let ad=self.a_zp(); let m=self.read(ad); self.bit(m); }
            0x2c => { let ad=self.a_abs(); let m=self.read(ad); self.bit(m); }

            // ---- flags ----
            0x18 => self.set_flag(FLAG_C, false),
            0x38 => self.set_flag(FLAG_C, true),
            0x58 => self.set_flag(FLAG_I, false),
            0x78 => self.set_flag(FLAG_I, true),
            0xb8 => self.set_flag(FLAG_V, false),
            0xd8 => self.set_flag(FLAG_D, false),
            0xf8 => self.set_flag(FLAG_D, true),

            // ---- branches ----
            0x10 => { let c=!self.flag(FLAG_N); self.branch(c,&mut extra); }
            0x30 => { let c=self.flag(FLAG_N); self.branch(c,&mut extra); }
            0x50 => { let c=!self.flag(FLAG_V); self.branch(c,&mut extra); }
            0x70 => { let c=self.flag(FLAG_V); self.branch(c,&mut extra); }
            0x90 => { let c=!self.flag(FLAG_C); self.branch(c,&mut extra); }
            0xb0 => { let c=self.flag(FLAG_C); self.branch(c,&mut extra); }
            0xd0 => { let c=!self.flag(FLAG_Z); self.branch(c,&mut extra); }
            0xf0 => { let c=self.flag(FLAG_Z); self.branch(c,&mut extra); }

            // ---- jumps / subroutines ----
            0x4c => { let ad=self.a_abs(); self.pc=ad; }
            0x6c => {
                // JMP (indirect) with the NMOS page-wrap bug
                let ptr=self.fetch16();
                let lo=self.read(ptr) as u16;
                let hi=self.read((ptr & 0xff00) | ((ptr.wrapping_add(1)) & 0x00ff)) as u16;
                self.pc = lo | (hi<<8);
            }
            0x20 => {
                let ad=self.a_abs();
                let ret=self.pc.wrapping_sub(1);
                self.push((ret>>8) as u8);
                self.push((ret & 0xff) as u8);
                self.pc=ad;
            }
            0x60 => {
                let lo=self.pull() as u16;
                let hi=self.pull() as u16;
                self.pc=((lo|(hi<<8)).wrapping_add(1));
            }
            0x40 => {
                // RTI
                self.p = (self.pull() & !FLAG_B) | FLAG_U;
                let lo=self.pull() as u16;
                let hi=self.pull() as u16;
                self.pc=lo|(hi<<8);
            }
            0x00 => {
                // BRK: treat as a soft interrupt; push and set I. Rarely used in tunes.
                let ret=self.pc.wrapping_add(1);
                self.push((ret>>8) as u8);
                self.push((ret & 0xff) as u8);
                self.push(self.p | FLAG_B | FLAG_U);
                self.set_flag(FLAG_I, true);
                self.pc=self.read16(0xfffe);
            }

            // ---- stack ops ----
            0x48 => { let a=self.a; self.push(a); }
            0x68 => { let v=self.pull(); self.a=v; self.set_zn(v); }
            0x08 => { let p=self.p | FLAG_B | FLAG_U; self.push(p); }
            0x28 => { let v=self.pull(); self.p=(v & !FLAG_B)|FLAG_U; }

            // ---- NOP ----
            0xea => {}

            // ================= common undocumented opcodes =================
            // multi-byte NOPs (must consume operand bytes to stay in sync)
            0x1a | 0x3a | 0x5a | 0x7a | 0xda | 0xfa => {}
            0x80 | 0x82 | 0x89 | 0xc2 | 0xe2 => { let _=self.a_zp_imm(); }
            0x04 | 0x44 | 0x64 => { let _=self.a_zp(); }
            0x14 | 0x34 | 0x54 | 0x74 | 0xd4 | 0xf4 => { let _=self.a_zpx(); }
            0x0c => { let _=self.a_abs(); }
            0x1c | 0x3c | 0x5c | 0x7c | 0xdc | 0xfc => { let _=self.a_absx(&mut extra); }

            // LAX
            0xa7 => { let ad=self.a_zp(); let m=self.read(ad); self.a=m; self.x=m; self.set_zn(m); }
            0xb7 => { let ad=self.a_zpy(); let m=self.read(ad); self.a=m; self.x=m; self.set_zn(m); }
            0xaf => { let ad=self.a_abs(); let m=self.read(ad); self.a=m; self.x=m; self.set_zn(m); }
            0xbf => { let ad=self.a_absy(&mut extra); let m=self.read(ad); self.a=m; self.x=m; self.set_zn(m); }
            0xa3 => { let ad=self.a_izx(); let m=self.read(ad); self.a=m; self.x=m; self.set_zn(m); }
            0xb3 => { let ad=self.a_izy(&mut extra); let m=self.read(ad); self.a=m; self.x=m; self.set_zn(m); }

            // SAX
            0x87 => { let ad=self.a_zp(); let v=self.a & self.x; self.write(ad,v); }
            0x97 => { let ad=self.a_zpy(); let v=self.a & self.x; self.write(ad,v); }
            0x8f => { let ad=self.a_abs(); let v=self.a & self.x; self.write(ad,v); }
            0x83 => { let ad=self.a_izx(); let v=self.a & self.x; self.write(ad,v); }

            // DCP (DEC + CMP)
            0xc7 => { let ad=self.a_zp(); self.dcp(ad); }
            0xd7 => { let ad=self.a_zpx(); self.dcp(ad); }
            0xcf => { let ad=self.a_abs(); self.dcp(ad); }
            0xdf => { let ad=self.a_absx(&mut extra); self.dcp(ad); }
            0xdb => { let ad=self.a_absy(&mut extra); self.dcp(ad); }
            0xc3 => { let ad=self.a_izx(); self.dcp(ad); }
            0xd3 => { let ad=self.a_izy(&mut extra); self.dcp(ad); }

            // ISC (INC + SBC)
            0xe7 => { let ad=self.a_zp(); self.isc(ad); }
            0xf7 => { let ad=self.a_zpx(); self.isc(ad); }
            0xef => { let ad=self.a_abs(); self.isc(ad); }
            0xff => { let ad=self.a_absx(&mut extra); self.isc(ad); }
            0xfb => { let ad=self.a_absy(&mut extra); self.isc(ad); }
            0xe3 => { let ad=self.a_izx(); self.isc(ad); }
            0xf3 => { let ad=self.a_izy(&mut extra); self.isc(ad); }

            // SLO (ASL + ORA)
            0x07 => { let ad=self.a_zp(); self.slo(ad); }
            0x17 => { let ad=self.a_zpx(); self.slo(ad); }
            0x0f => { let ad=self.a_abs(); self.slo(ad); }
            0x1f => { let ad=self.a_absx(&mut extra); self.slo(ad); }
            0x1b => { let ad=self.a_absy(&mut extra); self.slo(ad); }
            0x03 => { let ad=self.a_izx(); self.slo(ad); }
            0x13 => { let ad=self.a_izy(&mut extra); self.slo(ad); }

            // RLA (ROL + AND)
            0x27 => { let ad=self.a_zp(); self.rla(ad); }
            0x37 => { let ad=self.a_zpx(); self.rla(ad); }
            0x2f => { let ad=self.a_abs(); self.rla(ad); }
            0x3f => { let ad=self.a_absx(&mut extra); self.rla(ad); }
            0x3b => { let ad=self.a_absy(&mut extra); self.rla(ad); }
            0x23 => { let ad=self.a_izx(); self.rla(ad); }
            0x33 => { let ad=self.a_izy(&mut extra); self.rla(ad); }

            // SRE (LSR + EOR)
            0x47 => { let ad=self.a_zp(); self.sre(ad); }
            0x57 => { let ad=self.a_zpx(); self.sre(ad); }
            0x4f => { let ad=self.a_abs(); self.sre(ad); }
            0x5f => { let ad=self.a_absx(&mut extra); self.sre(ad); }
            0x5b => { let ad=self.a_absy(&mut extra); self.sre(ad); }
            0x43 => { let ad=self.a_izx(); self.sre(ad); }
            0x53 => { let ad=self.a_izy(&mut extra); self.sre(ad); }

            // RRA (ROR + ADC)
            0x67 => { let ad=self.a_zp(); self.rra(ad); }
            0x77 => { let ad=self.a_zpx(); self.rra(ad); }
            0x6f => { let ad=self.a_abs(); self.rra(ad); }
            0x7f => { let ad=self.a_absx(&mut extra); self.rra(ad); }
            0x7b => { let ad=self.a_absy(&mut extra); self.rra(ad); }
            0x63 => { let ad=self.a_izx(); self.rra(ad); }
            0x73 => { let ad=self.a_izy(&mut extra); self.rra(ad); }

            // Anything else: consume as a 1-byte NOP (KIL/JAM etc. treated leniently).
            _ => {}
        }

        let cyc = base + extra;
        self.cycle = self.cycle.wrapping_add(cyc);
        cyc
    }

    // immediate operand fetch (named oddly to reuse in many arms)
    #[inline]
    fn a_zp_imm(&mut self) -> u8 {
        self.fetch()
    }

    fn bit(&mut self, m: u8) {
        self.set_flag(FLAG_Z, (self.a & m) == 0);
        self.set_flag(FLAG_N, m & 0x80 != 0);
        self.set_flag(FLAG_V, m & 0x40 != 0);
    }

    fn dcp(&mut self, ad: u16) {
        let v = self.read(ad).wrapping_sub(1);
        self.write(ad, v);
        let a = self.a;
        self.cmp_reg(a, v);
    }
    fn isc(&mut self, ad: u16) {
        let v = self.read(ad).wrapping_add(1);
        self.write(ad, v);
        self.sbc(v);
    }
    fn slo(&mut self, ad: u16) {
        let v = self.read(ad);
        let r = self.asl(v);
        self.write(ad, r);
        self.a |= r;
        let a = self.a;
        self.set_zn(a);
    }
    fn rla(&mut self, ad: u16) {
        let v = self.read(ad);
        let r = self.rol(v);
        self.write(ad, r);
        self.a &= r;
        let a = self.a;
        self.set_zn(a);
    }
    fn sre(&mut self, ad: u16) {
        let v = self.read(ad);
        let r = self.lsr(v);
        self.write(ad, r);
        self.a ^= r;
        let a = self.a;
        self.set_zn(a);
    }
    fn rra(&mut self, ad: u16) {
        let v = self.read(ad);
        let r = self.ror(v);
        self.write(ad, r);
        self.adc(r);
    }
}

/// Base cycle counts per opcode (page-cross and branch penalties added at runtime).
#[rustfmt::skip]
static CYCLES: [u8; 256] = [
    /*      0  1  2  3  4  5  6  7  8  9  A  B  C  D  E  F */
    /*0*/   7, 6, 2, 8, 3, 3, 5, 5, 3, 2, 2, 2, 4, 4, 6, 6,
    /*1*/   2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7,
    /*2*/   6, 6, 2, 8, 3, 3, 5, 5, 4, 2, 2, 2, 4, 4, 6, 6,
    /*3*/   2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7,
    /*4*/   6, 6, 2, 8, 3, 3, 5, 5, 3, 2, 2, 2, 3, 4, 6, 6,
    /*5*/   2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7,
    /*6*/   6, 6, 2, 8, 3, 3, 5, 5, 4, 2, 2, 2, 5, 4, 6, 6,
    /*7*/   2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7,
    /*8*/   2, 6, 2, 6, 3, 3, 3, 3, 2, 2, 2, 2, 4, 4, 4, 4,
    /*9*/   2, 6, 2, 6, 4, 4, 4, 4, 2, 5, 2, 5, 5, 5, 5, 5,
    /*A*/   2, 6, 2, 6, 3, 3, 3, 3, 2, 2, 2, 2, 4, 4, 4, 4,
    /*B*/   2, 5, 2, 5, 4, 4, 4, 4, 2, 4, 2, 4, 4, 4, 4, 4,
    /*C*/   2, 6, 2, 8, 3, 3, 5, 5, 2, 2, 2, 2, 4, 4, 6, 6,
    /*D*/   2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7,
    /*E*/   2, 6, 2, 8, 3, 3, 5, 5, 2, 2, 2, 2, 4, 4, 6, 6,
    /*F*/   2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7,
];
