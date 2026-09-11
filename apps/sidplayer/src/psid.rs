//! Minimal PSID (v1/v2) header parser.
//!
//! All multi-byte header fields are big-endian. We only support PSID (not RSID)
//! tunes with a non-zero play address (i.e. not IRQ-vector driven). That covers
//! the classic Rob Hubbard / Martin Galway style tunes such as Commando.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidModel {
    Mos6581,
    Mos8580,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    Pal,
    Ntsc,
}

#[derive(Debug)]
pub struct PsidError(pub &'static str);

pub struct Psid<'a> {
    pub version: u16,
    pub data_offset: u16,
    pub load_address: u16,
    pub init_address: u16,
    pub play_address: u16,
    pub songs: u16,
    pub start_song: u16,
    /// speed bits: bit n set => song n+1 uses CIA timer, else VBI (raster)
    pub speed: u32,
    pub name: heapless_str,
    pub author: heapless_str,
    pub released: heapless_str,
    pub clock: Clock,
    pub model: SidModel,
    /// The tune program body (everything after the header), including the two
    /// little-endian load-address bytes when load_address in the header was 0.
    pub data: &'a [u8],
}

/// A tiny fixed-capacity ASCII string wrapper for the 32-byte PSID text fields,
/// avoiding a heap allocation and any std String dependency in the parser.
#[derive(Clone, Copy)]
pub struct heapless_str {
    buf: [u8; 32],
    len: usize,
}

impl heapless_str {
    fn from_field(field: &[u8]) -> Self {
        let mut buf = [0u8; 32];
        let mut len = 0;
        for &b in field.iter().take(32) {
            if b == 0 {
                break;
            }
            buf[len] = b;
            len += 1;
        }
        heapless_str { buf, len }
    }

    pub fn as_str(&self) -> &str {
        // PSID text fields are ISO-8859-1; treat as ASCII, replacing anything
        // non-ASCII lazily is overkill here — just take the valid ASCII prefix.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Debug for heapless_str {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { write!(f, "{:?}", self.as_str()) }
}

fn be16(b: &[u8], off: usize) -> u16 { ((b[off] as u16) << 8) | (b[off + 1] as u16) }

fn be32(b: &[u8], off: usize) -> u32 {
    ((b[off] as u32) << 24) | ((b[off + 1] as u32) << 16) | ((b[off + 2] as u32) << 8) | (b[off + 3] as u32)
}

impl<'a> Psid<'a> {
    pub fn parse(raw: &'a [u8]) -> Result<Psid<'a>, PsidError> {
        if raw.len() < 0x7c {
            return Err(PsidError("file too small"));
        }
        match &raw[0..4] {
            b"PSID" => {}
            b"RSID" => return Err(PsidError("RSID tunes unsupported")),
            _ => return Err(PsidError("not a PSID file")),
        }
        let version = be16(raw, 0x04);
        let data_offset = be16(raw, 0x06);
        let load_address = be16(raw, 0x08);
        let init_address = be16(raw, 0x0a);
        let play_address = be16(raw, 0x0c);
        let songs = be16(raw, 0x0e);
        let start_song = be16(raw, 0x10);
        let speed = be32(raw, 0x12);

        let name = heapless_str::from_field(&raw[0x16..0x36]);
        let author = heapless_str::from_field(&raw[0x36..0x56]);
        let released = heapless_str::from_field(&raw[0x56..0x76]);

        // flags (v2+) at 0x76
        let (clock, model) = if version >= 2 && (data_offset as usize) >= 0x7c {
            let flags = be16(raw, 0x76);
            let clock_bits = (flags >> 2) & 0x3;
            let model_bits = (flags >> 4) & 0x3;
            let clock = match clock_bits {
                0b10 => Clock::Ntsc,
                _ => Clock::Pal, // 01 PAL, 11 either, 00 unknown -> default PAL
            };
            let model = match model_bits {
                0b10 => SidModel::Mos8580,
                _ => SidModel::Mos6581, // 01 6581, 11 either, 00 unknown -> default 6581
            };
            (clock, model)
        } else {
            (Clock::Pal, SidModel::Mos6581)
        };

        if play_address == 0 {
            return Err(PsidError("IRQ-driven tunes (playAddr=0) unsupported"));
        }

        let doff = data_offset as usize;
        if doff > raw.len() {
            return Err(PsidError("bad data offset"));
        }
        let data = &raw[doff..];

        Ok(Psid {
            version,
            data_offset,
            load_address,
            init_address,
            play_address,
            songs,
            start_song,
            speed,
            name,
            author,
            released,
            clock,
            model,
            data,
        })
    }

    /// Returns (effective_load_address, program_bytes). Handles the load_address==0
    /// convention where the first two data bytes (little-endian) are the load address.
    pub fn program(&self) -> (u16, &'a [u8]) {
        if self.load_address == 0 {
            if self.data.len() < 2 {
                return (0, &self.data[0..0]);
            }
            let lo = self.data[0] as u16;
            let hi = self.data[1] as u16;
            let addr = (hi << 8) | lo;
            (addr, &self.data[2..])
        } else {
            (self.load_address, self.data)
        }
    }

    /// True if the given (0-based) song index uses the CIA timer for its play rate.
    pub fn song_uses_cia(&self, song0: u16) -> bool {
        let bit = if song0 >= 32 { 31 } else { song0 as u32 };
        (self.speed >> bit) & 1 != 0
    }
}
