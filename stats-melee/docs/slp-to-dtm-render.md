# Track 10 (revised) — Render via vanilla Dolphin + DTM

> **Status:** research complete; implementation not started. Replaces the
> Slippi-Dolphin frame-dump approach (see `dolphin-frame-dump-notes.md`)
> which is parked indefinitely on macOS — Slippi's playback Dolphin's
> AVI muxer is structurally broken there. Vanilla Dolphin on the same
> Mac dumps frames fine, but it can't read `.slp` files. This doc
> bridges the gap.

## Why this approach

Slippi's playback Dolphin and vanilla Dolphin are two different binaries
with two non-overlapping capability gaps:

|                     | Slippi Dolphin  | Vanilla Dolphin |
|---------------------|-----------------|-----------------|
| `.slp` playback     | Yes (`-i comm.json`) | No |
| Frame dump (macOS)  | **Broken**      | **Works**       |

Neither alone can render a `.slp` to MP4 on macOS. But both speak a
common language: GameCube controller inputs at 60 Hz. Slippi `.slp`
files contain those inputs (it's one of the things Slippi records).
Dolphin's native input-replay format is `.dtm`. Convert the inputs
from one to the other, hand the `.dtm` to vanilla Dolphin, and the
emulator runs Melee with the captured inputs as if a controller were
sending them — same game state, same visuals, same audio. Dump frames,
mux, done.

```
                  ┌──────────────────────────────┐
                  │   .slp (peppi-parsed)        │
                  │   per-frame controller input │
                  └──────────────┬───────────────┘
                                 │ slp_to_dtm()
                                 ▼
                  ┌──────────────────────────────┐
                  │   movie.dtm                  │
                  │   256-byte header + 8 B/frame│
                  └──────────────┬───────────────┘
                                 │
                                 ▼
                  ┌──────────────────────────────┐
                  │   Vanilla Dolphin            │
                  │   -e melee.iso -m movie.dtm  │
                  │   -b --output-directory <d>  │
                  │   (+ UCF Gecko codes via     │
                  │    GALE01.ini)               │
                  └──────────────┬───────────────┘
                                 │ framedump_N.png + dspdump.wav + dtkdump.wav
                                 ▼
                  ┌──────────────────────────────┐
                  │   ffmpeg                     │
                  │   -framerate 60              │
                  │   -i framedump_%d.png ...    │
                  └──────────────┬───────────────┘
                                 ▼
                              out.mp4
```

## DTM format specification

Source: [TASVideos DTM spec](https://tasvideos.org/EmulatorResources/Dolphin/DTM)
cross-referenced with `dolphin-emu/dolphin` `Source/Core/Core/Movie.h` and
`Movie.cpp`. **All multi-byte integers are little-endian. Bits within a
byte are LSB = bit 0. Strings are NUL-padded to fill the field.**

### Header (256 bytes, offsets in hex)

| Offset | Size | Field                  | Type     | Notes |
|--------|------|------------------------|----------|-------|
| `0x000`| 4    | Signature              | bytes    | Must be `44 54 4D 1A` ("DTM\x1A") |
| `0x004`| 6    | Game ID                | char[6]  | "GALE01" for NTSC Melee 1.02 |
| `0x00A`| 1    | Is Wii                 | bool     | `0` |
| `0x00B`| 1    | Controllers Plugged    | u8       | Bit set per port. For 1v1: `0b00000011` (ports 1+2) |
| `0x00C`| 1    | Starts From Savestate  | bool     | `0` |
| `0x00D`| 8    | VI Count               | u64      | Total VBlank frames; we set = Input Count |
| `0x015`| 8    | Input Count            | u64      | Total controller-poll frames |
| `0x01D`| 8    | Lag Counter            | u64      | `0` (best-effort) |
| `0x025`| 8    | Reserved               | —        | Must be 0 |
| `0x02D`| 4    | Rerecord Count         | u32      | `0` |
| `0x031`| 32   | Author                 | char[32] | "stats-melee" |
| `0x051`| 32   | Video Backend          | char[32] | Empty / "OGL" |
| `0x071`| 16   | Audio Emulator         | bytes    | `0` (HLE flag is in config block instead) |
| `0x081`| 16   | Game MD5               | u8[16]   | MD5 of the user's Melee ISO. Computed value for the project's NTSC v1.02 ISO: `0e63d4223b01d9aba596259dc155a174` |
| `0x091`| 8    | Recording Start Time   | u64      | Unix timestamp |
| `0x099`| 1    | Saved Config Valid     | bool     | `1` so the config block below is honored |
| `0x09A`| 1    | Idle Skipping          | bool     | `1` (default) |
| `0x09B`| 1    | Dual Core              | bool     | `1` (default) |
| `0x09C`| 1    | Progressive Scan       | bool     | `0` |
| `0x09D`| 1    | DSP HLE                | bool     | `1` (matches Slippi) |
| `0x09E`| 1    | Fast Disc Speed        | bool     | `0` |
| `0x09F`| 1    | CPU Core               | u8       | `1` (JIT) |
| `0x0A0`| 1    | EFB Access             | bool     | `0` |
| `0x0A1`| 1    | EFB Copy               | bool     | `1` |
| `0x0A2`| 1    | Copy EFB To Texture    | bool     | `1` |
| `0x0A3`| 1    | EFB Copy Cache         | bool     | `0` |
| `0x0A4`| 1    | Emulate Format Changes | bool     | `0` |
| `0x0A5`| 1    | Use XFB                | bool     | `0` |
| `0x0A6`| 1    | Use Real XFB           | bool     | `0` |
| `0x0A7`| 1    | Memory Cards Present   | u8       | `0` |
| `0x0A8`| 1    | Memory Card Blank      | bool     | `0` |
| `0x0A9`| 1    | Bongos Plugged         | u8       | `0` |
| `0x0AA`| 1    | Sync GPU Thread        | bool     | `0` |
| `0x0AB`| 1    | Netplay Session        | bool     | `0` |
| `0x0AC`| 1    | SYSCONF PAL60          | bool     | `0` |
| `0x0AD`| 1    | Language               | u8       | `0` (English) |
| `0x0AE`| 1    | Reserved               | —        | `0` |
| `0x0AF`| 1    | JIT Branch Following   | bool     | `1` |
| `0x0B0`| 1    | Accurate FMA           | bool     | `0` |
| `0x0B1`| 1    | GBAs Plugged           | u8       | `0` |
| `0x0B2`| 1    | SYSCONF Widescreen     | bool     | `0` |
| `0x0B3`| 1    | SYSCONF Country        | u8       | `0x31` (US) |
| `0x0B4`| 5    | Reserved               | —        | `0` |
| `0x0B9`| 40   | Second Disc ISO        | char[40] | Empty |
| `0x0E1`| 20   | Dolphin Git SHA-1      | u8[20]   | `0` (informational only) |
| `0x0F5`| 4    | DSP IROM Hash          | u32      | `0` (informational only) |
| `0x0F9`| 4    | DSP COEF Hash          | u32      | `0` (informational only) |
| `0x0FD`| 8    | Tick Count             | u64      | `0` (informational only) |
| `0x105`| 11   | Reserved               | —        | `0` |
| `0x110`| —    | **end of header**      |          | `0x110 == 256` |

### Per-frame controller state (8 bytes)

For each frame, for each controller in port-order (skipping disconnected
ports), 8 bytes in this layout. **Bit 0 = LSB. Bytes are little-endian
in file order.**

| Byte | Bits | Field            | Notes |
|------|------|------------------|-------|
| 0    | 0    | Start            | |
| 0    | 1    | A                | |
| 0    | 2    | B                | |
| 0    | 3    | X                | |
| 0    | 4    | Y                | |
| 0    | 5    | Z                | |
| 0    | 6    | D-Pad Up         | |
| 0    | 7    | D-Pad Down       | |
| 1    | 0    | D-Pad Left       | |
| 1    | 1    | D-Pad Right      | |
| 1    | 2    | L (digital)      | |
| 1    | 3    | R (digital)      | |
| 1    | 4    | Change Disc      | `0` |
| 1    | 5    | Reset            | `0` |
| 1    | 6    | Connected        | `1` for active ports (added v5.0-5911) |
| 1    | 7    | Reset Analog     | `0` (added v5.0-10479) |
| 2    | —    | L Pressure       | u8 0–255 |
| 3    | —    | R Pressure       | u8 0–255 |
| 4    | —    | Stick X          | u8 0–255, center 128 |
| 5    | —    | Stick Y          | u8 0–255, center 128 |
| 6    | —    | C-Stick X        | u8 0–255, center 128 |
| 7    | —    | C-Stick Y        | u8 0–255, center 128 |

### Body layout

After the header, GameCube controller records appear **interleaved by
frame**, in port order:

```
[hdr:256][frame 0 ctrl 1][frame 0 ctrl 2][frame 1 ctrl 1][frame 1 ctrl 2]...
```

For a 1v1 with ports 1+2 enabled and `frameCount == N`:
file size = 256 + 2 × 8 × N bytes.

Disconnected ports contribute zero bytes; they're not padded with
no-input records.

### Critical playback constraints

- Signature `44 54 4D 1A` — refusal to play if wrong.
- All reserved bytes must be `0`.
- VI Count = Input Count = number of frames in the body. Mismatch
  causes desync.
- `Saved Config Valid = 1` if any of the config bools below it differ
  from Dolphin's current settings. Easier to just always set `1` and
  fill in values that match a known-good Slippi-equivalent config.
- Game MD5 mismatch: Dolphin warns but plays anyway in newer builds.

## Slippi pre-frame → DTM controller-state mapping

Source: [Slippi SPEC.md (project-slippi/slippi-wiki)](https://github.com/project-slippi/slippi-wiki/blob/master/SPEC.md),
event `0x37` (pre-frame update).

Each Slippi pre-frame contains both **physical** (raw hardware) and
**processed** (post-UCF / post-deadzone) input fields:

| Slippi field             | Type    | Source         | DTM target |
|--------------------------|---------|----------------|------------|
| Physical Buttons         | u16     | Hardware bits  | DTM bytes 0+1 button bits (Start/A/B/X/Y/Z/DUp/DDown/DLeft/DRight/L/R) |
| Physical L Trigger       | f32     | Hardware       | DTM byte 2: `(t * 255).round() as u8` |
| Physical R Trigger       | f32     | Hardware       | DTM byte 3: `(t * 255).round() as u8` |
| Joystick X (processed)   | f32     | Post-UCF       | *(see "UCF strategy" below)* |
| Joystick Y (processed)   | f32     | Post-UCF       | *(see "UCF strategy")* |
| C-Stick X (processed)    | f32     | Post-UCF       | *(see "UCF strategy")* |
| C-Stick Y (processed)    | f32     | Post-UCF       | *(see "UCF strategy")* |
| X analog for UCF         | i8      | Hardware raw   | *(see "UCF strategy")* |
| Y analog for UCF         | i8      | Hardware raw   | *(see "UCF strategy")* |
| X c-stick for UCF        | i8      | Hardware raw   | *(see "UCF strategy")* |
| Y c-stick for UCF        | i8      | Hardware raw   | *(see "UCF strategy")* |

### Slippi physical-button bit map (u16 at SLP offset 0x31)

| Bit | Button     |
|-----|------------|
| 0   | D-Pad Left |
| 1   | D-Pad Right|
| 2   | D-Pad Down |
| 3   | D-Pad Up   |
| 4   | Z          |
| 5   | R          |
| 6   | L          |
| 7   | (unused)   |
| 8   | A          |
| 9   | B          |
| 10  | X          |
| 11  | Y          |
| 12  | Start      |
| 13–15 | unused   |

These need re-packing into DTM's distinct bit layout (button → bit-in-byte
map in the per-frame table above). A static bit-shuffle table is the
cleanest implementation.

## UCF strategy: which inputs to use

This is the key decision. Slippi records both the player's raw analog
input and the UCF-modified processed value. We have two paths:

**Option A — emit raw, let Dolphin apply UCF.**
Use `X/Y analog for UCF` and `X/Y c-stick for UCF` (i8) as the DTM
analog values, converting `dtm_byte = (i8_value as i16 + 128) as u8`.
Pair this with **Slippi's UCF Gecko codes loaded into vanilla Dolphin's
`GALE01.ini`** so the same UCF transformation is applied at playback.

**Option B — emit processed, no UCF in Dolphin.**
Use the post-UCF processed `Joystick X/Y` (f32, range [-1, 1]) and
`C-Stick X/Y`, converting `dtm_byte = ((v + 1.0) * 127.5).round() as u8`.
Don't load UCF Gecko codes in vanilla Dolphin. The values fed to the
game are already UCF-corrected.

**Recommendation: Option B for V1.** Simpler — no Gecko code juggling,
no GALE01.ini editing, no risk of double-applying UCF. The processed
inputs are exactly what the game saw during the original recording.
Option A becomes the right answer only if we discover the game's
input-poll path applies further transformations *after* the
processed-input read that depend on raw values (unlikely but
worth checking during the first integration test).

The UCF code lists are still on disk if we need them for Option A —
`~/Library/Application Support/Slippi Launcher/playback/Slippi Dolphin.app/Contents/Resources/Slippi/InjectionLists/list_console_UCF.json`.

## Per-frame frame count and replay framing

Slippi frames start at `-123` (the Melee bootloader / "Ready... GO!"
countdown). DTM frame 0 is the first frame after Dolphin starts the
game from boot, so we have to:

1. Pad the DTM with `123 + N` neutral input frames at the start
   (where `N` is the number of additional frames from boot to when
   Slippi's frame counter reaches `-123`). Empirically this needs
   characterization — start with 0 padding and see how desynced
   the result is.

2. After the Slippi frame range, optionally append a few seconds of
   neutral input so the recording captures the kill animation /
   "GAME!" screen. Otherwise the dump cuts off the moment inputs run
   out and Dolphin behavior past EOF is unspecified.

The "boot to first input frame" delay is the single biggest source of
playback risk; it depends on Dolphin's emulation of Melee's load
sequence and may differ between Dolphin versions. Capture-time spike
work in V1 will measure this on a known fixture (one of the test_slps).

## Vanilla Dolphin invocation

```sh
"/Applications/Dolphin.app/Contents/MacOS/Dolphin" \
  --exec="/path/to/Super Smash Bros. Melee (USA) (En,Ja) (v1.02).iso" \
  --movie="/tmp/stats-melee/movie.dtm" \
  --batch \
  --output-directory="/tmp/stats-melee/dump" \
  --output-filename-base="render"
```

Flag notes (all confirmed in vanilla Dolphin 2603a's `--help`):

- `-e, --exec` — boots the ISO directly. Required: vanilla Dolphin
  doesn't have Slippi's launch-from-Launcher-config behavior.
- `-m, --movie` — plays back the DTM. Movie ends → emulation stops.
- `-b, --batch` — exits Dolphin when emulation stops. Combined with
  `-m`, this gives clean auto-exit at end-of-movie.
- `--output-directory` — where dumps go. **Critical:** without this we'd
  be modifying the user's vanilla Dolphin user dir. With it, each render
  gets its own scratch dir.
- `--output-filename-base` — prefix for `framedump_N.png` /
  `audio.wav` / `dspdump.wav`. Lets us distinguish concurrent renders.

Frame-dump output on macOS is **PNG sequence** (not AVI — `DumpFormat`
is ignored on macOS in current builds). The mux step uses
`-framerate 60 -i framedump_%d.png` rather than the AVI input the old
render_worker used.

### ffmpeg mux

```sh
ffmpeg \
  -hide_banner -loglevel error -y \
  -framerate 60 -i "$DUMP/framedump_%d.png" \
  -i "$DUMP/dspdump.wav" \
  -i "$DUMP/dtkdump.wav" \
  -filter_complex "[1:a][2:a]amix=inputs=2:duration=shortest" \
  -c:v libx264 -preset veryfast -crf 23 \
  -c:a aac -b:a 192k \
  -shortest \
  "$OUT/render.mp4"
```

The two audio streams (`dspdump.wav` = sound effects, `dtkdump.wav` =
disc-streamed BGM) are mixed in ffmpeg. Slippi's render_worker only
mixed `dspdump`; this version captures the music too.

## Determinism risks

### Will the playback match the original game?

The game state during DTM playback is determined by:

1. **Initial RAM state** — set by the ISO + boot sequence.
2. **Per-frame inputs** — what we're providing.
3. **RNG state** — derived from inputs + game logic; the game seeds
   itself.

If (1) and (2) match the original, (3) follows. Slippi-recorded inputs
are sufficient to recreate the game state because Melee is fully
deterministic given the same inputs from the same boot state.

The risks are at the boundaries:

- **Boot-to-first-input frame alignment.** Off-by-N at the start
  cascades into desync. See "Per-frame frame count" above.
- **Different Dolphin version.** Vanilla Dolphin and Slippi's Dolphin
  fork share a Dolphin codebase but diverge over time. Subtle CPU/GPU
  emulation differences could cause divergent state.
- **DSP HLE vs LLE.** Audio engine choice changes timing slightly.
  Match Slippi's default (HLE) — set in DTM header config block.
- **UCF vs no-UCF.** Covered above.

### How we detect desync

The `.slp` file already contains *post-frame state* per frame: position,
action state, percent. Render the DTM, parse the resulting frame log
that Dolphin can optionally produce (`-d`/`--debugger` mode dumps too
much; better path: render to PNGs, then we can spot-check by visually
diffing one frame near the end against what the player actually did).

A more rigorous check: instrument vanilla Dolphin to dump per-frame
position/action state to a sidecar file, then compare that against
what's in the `.slp`. Out of scope for V1 but a good "is this thing
working?" test.

## Module structure in stats-melee

```
stats-melee/src/
  dtm.rs              ← NEW. DTM header struct + serialize.
                        Pure encoder, no IO. Unit-testable.

  slp_to_dtm.rs       ← NEW. Reads peppi Game, walks
                        frames.ports[i].leader.pre, emits a
                        Vec<DtmControllerState> per port.
                        Pure transform.

stats-melee-app/src/
  render_worker.rs    ← MODIFY. Currently shells out to Slippi
                        Dolphin + Slippi-specific config dance.
                        Replace with:
                          1. slp_to_dtm() to a temp file
                          2. spawn vanilla Dolphin with -e/-m/-b
                          3. ffmpeg mux PNGs+WAVs to MP4
                          4. write into video_cache
                        Most of the worker plumbing
                        (channels, progress, retries) stays.

  config.rs           ← MODIFY. Drop slippi_user_dir setting.
                        Add vanilla_dolphin_path and melee_iso_path
                        (already present), plus
                        ffmpeg_path (already present).
```

Settings go from {Slippi user dir, ISO path, ffmpeg path} to
{vanilla Dolphin path, ISO path, ffmpeg path}. The Slippi
playback Dolphin is no longer involved in rendering.

The `video_cache` and `file_cache` modules don't change — they're
storage policy, format-agnostic.

## Phased delivery

Each phase produces something verifiable on its own. Stop and verify
between phases; don't bundle.

### V1a — DTM header encoder + golden-file test

`stats-melee/src/dtm.rs`. Implement the header struct and serialize
it. Build a golden test: hand-craft a known-good DTM header (use
`xxd` on a DTM produced by vanilla Dolphin's "Movie → Start
Recording" feature for one frame), encode our struct with the same
inputs, byte-compare. Verifies the bit-level layout before we touch
controller data.

### V1b — Controller state encoder + body assembly

Add `DtmControllerState` to dtm.rs. Encode + serialize. Compose:
`fn write_dtm(header, frames: &[Vec<DtmControllerState>]) -> Vec<u8>`.
Tests cover button bit packing, analog stick conversion, multi-port
interleaving.

### V1c — Slippi pre-frame → DtmControllerState transform

`stats-melee/src/slp_to_dtm.rs`. Read peppi's `pre.buttons` (u32),
`pre.joystick_x/y`, `pre.cstick_x/y`, `pre.trigger`. Map to
`DtmControllerState` per port per frame. Tests on a fixture
.slp from `test_slps/`.

### V1d — Vanilla Dolphin invocation spike

Manual test: pick the shortest fixture, generate a DTM with V1c,
run vanilla Dolphin headless against it. Verify:

- Dolphin exits cleanly via `-b`.
- `framedump_N.png` files appear under `--output-directory`.
- `dspdump.wav` and `dtkdump.wav` appear.
- Visually compare frame ~60 (one second in) to a screenshot of the
  same point in Slippi playback. Identical = pipeline works.

This is the single largest risk in the plan — if game state
diverges, we discover it here. Document findings in this file.

### V1e — ffmpeg mux

Wire the mux step. Validate the output MP4 has both video and
audio tracks, plays end-to-end in QuickTime / mpv.

### V1f — render_worker rewrite

Replace `prepare_dolphin_config_for_dump` and the Slippi-specific
plumbing with the vanilla-Dolphin pipeline. Keep the worker
interface (`spawn_render` -> `RenderMsg::Progress` / `::Done`)
identical so the eframe app side doesn't change.

### V1g — Flip the feature gate

`RENDER_VIDEO_FEATURE_ENABLED = true` in `app.rs:40`. Run the full
"Render video" / "Open video" V1 flow end-to-end on the eframe app.

### V1h — `cargo check` + `cargo test`

The customary cargo-verify pass that's been pending across several
tracks. This time we mean it.

## Open questions

1. **Boot-to-first-input frame count.** How many neutral input frames
   does the DTM need at the start before Slippi's `-123` frame begins?
   Measure during V1d.

2. **Match-end framing.** When Slippi's last input frame ends, does
   vanilla Dolphin auto-exit on movie EOF, or does it sit at black?
   Adjust by appending neutral input frames if needed.

3. **Stage hazards / Pokémon Stadium transforms.** RNG-driven events
   should be deterministic given matching input streams, but worth
   verifying on a Pokémon Stadium fixture specifically.

4. **Multi-disc / multi-region ISOs.** We're targeting NTSC GALE01
   only for V1; PAL (GALPP) and any other revisions are out of scope.
   Add a precondition check that `metadata.game_id == "GALE01"`.

5. **Doubles / 3+ player FFA.** Slippi supports up to 4 players.
   `controllers_plugged` bitmask + multi-port DTM body assembly
   should handle this, but only 1v1 fixtures will be tested in V1.

6. **macOS Gatekeeper / quarantine.** Spawning vanilla Dolphin from
   our app may hit Gatekeeper if Dolphin was downloaded recently.
   Document the user-facing fix (right-click → Open) in the eframe
   error message.

## What this does NOT change

- `video_cache.rs`, `analysis_cache.rs`, `file_cache.rs` — storage
  policy stays.
- The viewer's combat-state scrub bar, key-moment markers, etc. —
  visual layer is independent of how the underlying MP4 was made.
- Tracks 11 (sidecar cache), 4 (input overlay), 9 (combat v2) —
  these all run against `.slp` data, which is unchanged.

## Why we kept the broken Slippi-Dolphin path documented

`dolphin-frame-dump-notes.md` and the parked render_worker code aren't
deleted because:

1. If Slippi fixes macOS frame dumping upstream, that path becomes
   strictly cheaper (one binary instead of two) and we'd flip back.
2. The diagnostics history is useful context for future investigations
   into emulator-related problems.

`RENDER_VIDEO_FEATURE_ENABLED` and the existing `render_worker.rs` get
replaced when V1f lands; the old code goes away in that PR rather than
sitting as dead code indefinitely.
