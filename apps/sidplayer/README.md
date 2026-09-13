# SID Music Player

A Commodore 64 **SID** music player for Precursor / Xous. It emulates the 6502 CPU
and the MOS 6581/8580 SID chip in real time (a batched-clocking "tier-2" engine)
and plays PSID tunes through the device codec at 8 kHz.

Beyond the built-in demo tune, it can **download a folder of `.sid` files over
HTTPS**, store them locally in the PDDB, and browse/play them from a scrollable
list — including a **random shuffle** mode.

---

## Quick start

1. Launch **SID Music Player** from the app menu. Before you've downloaded
   anything it shows a built-in demo tune, Rob Hubbard's *Commando* (1985, Elite).
2. Use **↑/↓** to move the selection (the outline box) and **Enter** (or Space) to
   play/stop it.
3. To add your own tunes, press **d** and enter a directory URL (see
   [Downloading tunes](#downloading-tunes)).

---

## Controls

| Key | Action |
|---|---|
| **↑ / ↓** | Move the selection up/down the list (scrolls as needed). |
| **← / →** | Jump to the previous/next file. |
| **Enter / Space** | Play the selected subtune; press again to stop (toggle). While shuffle is running, this **stops shuffle** instead. |
| **1 – 9** | Start **random shuffle**; the digit is the minutes-per-tune. Pressing a number *while shuffling* jumps to a new random tune and changes the interval. |
| **x** | Stop playback. |
| **Backspace / Delete** | Delete the selected **downloaded** tune (asks to confirm). The built-in tune can't be deleted. |
| **a** | Toggle **show all** subtunes vs. music-only (see [Tracks](#tracks-music-vs-sound-effects)). |
| **d** | **Download** tunes from a directory URL. |
| **o** | Cycle audio **output**: Headphones → Speaker → Both. |
| **F1** / **+** | Volume up. |
| **F4** / **–** | Volume down. |

The bottom of the screen shows a status line, the current output/volume, and two
rows of key hints.

---

## The list

Everything you can play is shown as one scrollable list:

- The **built-in** *Commando* tune is shown only while nothing has been downloaded
  yet — a starter tune that steps aside once you have your own library. It can't be
  deleted. (A *downloaded* Commando is an ordinary tune and stays visible.)
- **Downloaded** tunes are listed sorted by name.
- Each file contributes one row **per subtune**. The first row of a file shows its
  name and author; the rest are indented `t2/19`, `t3/19`, etc.
- The **selected** row is drawn with an outline box. (Reverse-video highlighting
  isn't available to normal apps on this device, so a box is used instead — the
  text stays readable.)
- A **▶** marks the subtune that is currently playing.

### Tracks: music vs. sound effects

A single SID file often contains many "subtunes" that share one program — for
*Commando* there are **19**, but only a few are actual music; the rest are jingles
and sound effects. The SID format carries no per-subtune metadata, so at import the
player **probes** each subtune (it runs the tune's driver headlessly for a few
seconds and watches how the SID voices are gated) and classifies it as music or
SFX.

- By default the list shows only the **music** subtunes.
- Press **a** to **show all** subtunes (including SFX); press again to hide them.

The heuristic isn't perfect (a repeating sound effect or a slow-building intro can
be miscategorised), so **show all** is the escape hatch if a track you want is
hidden. If probing would hide *every* subtune of a file, all of them are shown
instead, so a file is never left unplayable.

---

## Downloading tunes

Press **d**, enter (or edit) an HTTPS directory URL such as
`https://example.org/sids/`, and the player will:

1. Fetch that page and find every `.sid` file **linked directly on it**
   (subdirectories are ignored — it downloads one directory, not a whole tree).
2. Download each file, parse and classify it, and store it in the PDDB.
3. Refresh the list.

The last URL you used is remembered and pre-filled next time.

### Append or replace

If you already have downloads, you'll be asked whether to:

- **Append new tunes** — keep what you have and add files you don't already have.
- **Replace all downloads** — delete existing downloads, then fetch the fresh set.

Replace only wipes the old tunes **after** the directory has been fetched
successfully and has `.sid` links, so a bad or unreachable URL can never leave you
with an empty library. The built-in tune is never affected.

### Prerequisites for HTTPS

Two things must be set up before a download can succeed:

1. **The clock must be set.** An unset Precursor clock defaults to the year 2000,
   which makes today's certificates look "not valid yet" and causes the download to
   fail. Set the time (NTP or manually) from the device's preferences first.
2. **The site's certificate must be trusted.** The device ships with no root CAs,
   so the **first** time you connect to a host the player probes it and shows a
   "trust this certificate?" list — pick the **root CA** and confirm. Your choice is
   saved in the PDDB, so subsequent downloads from that host are silent.

You also need Wi-Fi connected. If a download fails, the player shows the full URL
and reason in a popup — the most common cause is an unset clock or an untrusted
certificate.

---

## Shuffle

Press a number key **1 – 9** to start random shuffle. The digit sets how many
**minutes** each tune plays before the player jumps to another random subtune,
drawn from the tunes currently in the list (so it honours the **show all** filter
and avoids repeating the same track back-to-back).

- Press a **different number** while shuffling to jump immediately to a new random
  tune and change the interval.
- Press **Enter / Space** (or **x**) to stop shuffle.
- The title shows `[shuffle 3m]` while it's running.

Shuffle uses no background timer thread; it advances by counting the audio actually
played, so expect a brief blip at each track change but steady playback in between.

To play one specific subtune while a shuffle is running, press Enter once to stop
the shuffle, then Enter again to play the selected track.

---

## Audio output

The codec drives the headphones and the mono speaker in parallel with no hardware
auto-switching, and the speaker path is louder, so the player mutes the unused path
for you:

- **Headphones** — speaker muted; headphone gain follows the volume setting.
- **Speaker** — headphones muted; speaker at its default level.
- **Both** — both active.

Cycle these with **o**. Volume (**F1/F4** or **+/–**) adjusts the **headphone**
analog gain in 3 dB steps (0 dB loudest, down to −42 dB); it has no effect in
Speaker-only mode. Output is 8 kHz mono, duplicated to both channels.

---

## Where things are stored (PDDB)

All data lives in the default basis:

| Dict | Contents |
|---|---|
| `sidplayer.tunes` | one key per downloaded file (key = filename, value = raw `.sid` bytes). |
| `sidplayer.meta` | one key per file with cached metadata (name, author, subtune count, which subtunes are music) so tunes aren't re-probed on every launch. |
| `sidplayer.state` | app state; key `url` holds the last directory URL. |
| `tls.trusted` | trusted CA certificates (managed by the shared `tls` library). |

Deleting a tune (Backspace) removes it from `sidplayer.tunes` and `sidplayer.meta`.

---

## Supported tunes and limitations

- **PSID** tunes with a non-zero play address (the classic Rob Hubbard / Martin
  Galway style). **RSID** tunes and IRQ-vector-driven tunes (play address 0) are
  not supported and are skipped on import with a log message.
- Single-SID only; no 2SID/3SID, no digi/sample playback.
- Output is fixed at **8 kHz mono** (the device codec rate).
- Playback loops the current subtune indefinitely (SID tunes have no natural end);
  use shuffle for continuous, changing playback.
- There is no HVSC song-length database on the device, so shuffle uses a fixed
  minutes-per-tune rather than each tune's true length.

---

## Building

This app depends on service crates (`modals`, `pddb`, `tls`, `net`), so build it as
part of a full image rather than with `compile-apps`:

```
cargo xtask app-image sidplayer
```

This produces a signed `xous.img` under
`target/riscv32imac-unknown-xous-elf/release/` to flash to the device.

---

## Implementation notes

- `sidplayer.rs` — the app: UI, the combined list, playback/shuffle state, and the
  download flow.
- `psid.rs` — PSID v1/v2 header parser.
- `cpu6502.rs` — 6502 emulator with a SID-register write log.
- `sid.rs` — the batched-clocking SID engine.
- `player.rs` — drives the 6502 + SID to produce 8 kHz samples, and the headless
  music/SFX classifier.
- `catalog.rs` — PDDB-backed tune library (store/list/read/delete + last URL).
- `netfetch.rs` — HTTPS directory-index parsing, file download, and TLS trust setup.

To keep audio glitch-free on the single-core CPU, the app deliberately avoids
background timer threads: it repaints only on user actions (and once per shuffle
track change), and it feeds the codec from its refill callback.
