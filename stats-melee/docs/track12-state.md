# Track 12 — current state (2026-05-05)

End-to-end pipeline `slp → DTM → vanilla Dolphin → AVI+WAVs → ffmpeg-mux → MP4`
is **working** with synced audio. This doc captures everything that was
discovered the hard way during the 12d/12e spike so the next session can
pick up cold.

For the design rationale and the original phased plan, see
[`slp-to-dtm-render.md`](slp-to-dtm-render.md).

## What's working right now

- 110 unit tests pass (`cargo test`).
- [`stats-melee/src/dtm.rs`](../src/dtm.rs) — DtmHeader (256 bytes,
  byte-matched against a Dolphin-recorded reference) + DtmControllerState +
  `write_dtm`.
- [`stats-melee/src/slp_to_dtm.rs`](../src/slp_to_dtm.rs) — emits **2 DTM
  polls per Slippi frame** (Melee polls SI 2× per game frame).
- [`stats-melee/src/bin/slp_to_dtm_bin.rs`](../src/bin/slp_to_dtm_bin.rs) —
  CLI: `cargo run --bin slp_to_dtm_bin -- [--single-port] [--pad-prefix=N] <slp> [out.dtm]`
- A custom Dolphin build at `/Users/matthewlafrance/Dev/dolphin/` (master
  branch, commit `e22551e` of 2026-05-03), built RelWithDebInfo, with one
  local patch + re-signed for lldb.

## The reproducible recipe

```sh
# 1. .slp → .dtm
cd stats-melee
cargo run --bin slp_to_dtm_bin -- --single-port --pad-prefix=7200 \
    test_slps/SOME_REPLAY.slp /tmp/run.dtm

# 2. clear previous Dolphin dump output
rm -rf ~/Library/Application\ Support/Dolphin/Dump/Frames \
       ~/Library/Application\ Support/Dolphin/Dump/Audio

# 3. emulate (interpreter is slow — ~5–10% real-time on M4 Pro)
"/Users/matthewlafrance/Dev/dolphin/Build/Binaries/Dolphin.app/Contents/MacOS/Dolphin" \
  --exec="<MELEE_ISO>" \
  --movie=/tmp/run.dtm

# Quit with Cmd+Q after enough game time has elapsed.

# 4. mux (the AVI already has correct timestamps; do NOT pass -framerate).
#    Audio dump starts before video dump by audio_dur - video_dur seconds of
#    game time. Skip that prefix from the audio inputs to align.
VIDEO=$(ls ~/Library/Application\ Support/Dolphin/Dump/Frames/*.avi | head -1)
DSP=$(ls ~/Library/Application\ Support/Dolphin/Dump/Audio/*_dspdump.wav | head -1)
DTK=$(ls ~/Library/Application\ Support/Dolphin/Dump/Audio/*_dtkdump.wav | head -1)
VIDEO_DUR=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$VIDEO")
AUDIO_DUR=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$DSP")
OFFSET=$(echo "$AUDIO_DUR - $VIDEO_DUR" | bc -l)
ffmpeg -y -i "$VIDEO" -ss "$OFFSET" -i "$DSP" -ss "$OFFSET" -i "$DTK" \
    -filter_complex "[1:a][2:a]amix=inputs=2:duration=shortest" \
    -c:v libx264 -preset veryfast -crf 23 -c:a aac -b:a 192k -shortest \
    /tmp/run.mp4
```

## Things that were hard / non-obvious

### Header was 256 bytes, not 272

The plan doc had `video_backend = 32 bytes`; real Dolphin uses **16**. All
offsets from `0x051` onward are 16 less than the plan doc claims. The doc
itself contains a typo: "0x110 == 256". Real header ends at `0x100`.

### saved_config_valid must be FALSE

We tried `true` with byte-for-byte reference values. It triggers a
`JitArm64::ResetFreeMemoryRanges` heap corruption bug (see below) on the
mid-boot config-mutation path. With `false`, Dolphin uses its local
config and the bug is dodged for that path.

But that wasn't enough — the JIT bug also fires on *every* boot via
`CBoot::RunApploader → JitArm64::ClearCache`. The only reliable workaround
is to disable the JIT entirely.

### Dolphin's ARM64 JIT has a real bug we can't fix from outside

Symptom: `__tree::destroy` walks a corrupt node during `RangeSizeSet::clear()`,
crashing in `JitArm64::ResetFreeMemoryRanges`. Two flavors:
1. `routines_far_size` wraps to a giant unsigned because `m_far_code` ends
   pointing at a different region than where it started during
   `GenerateAsm()`. We patched this in [our local Dolphin](../../dolphin/Source/Core/Core/PowerPC/JitArm64/Jit.cpp)
   `GenerateAsmAndResetFreeMemoryRanges` (clamp `end - start` to 0 if end < start).
2. Even with the clamp, the Sizes multimap of `m_free_ranges_far_0` is corrupt
   when `clear()` is later called. Unknown root cause without ASan.

**Workaround:** `[Core] CPUCore = 0` in `Dolphin.ini` forces the Interpreter,
which never touches `JitArm64`. Slow (~5–10% real-time on M4 Pro) but works.
For batch rendering this is acceptable for V1.

### macOS frame-dump output

Dolphin master writes:
- `[Settings] DumpFramesAsImages = True` (GFX.ini) → PNG sequence
  (`framedump_N.png`). **Numbering is sequential but loses per-frame
  timing**, so ffmpeg can't reconstruct correct A/V sync from PNGs alone.
- `[Settings] DumpFramesAsImages = False` → single AVI with proper
  timestamps embedded. **Use this.** The plan doc said macOS ignored this
  on Dolphin 2603a; on master it works.

### Audio dump runs longer than video dump

The DSP/DTK audio dumps start before the video dump (by some seconds of
game time). Skip the prefix with `ffmpeg -ss $OFFSET` where
`OFFSET = audio_duration - video_duration`. This was confirmed correct
by listening to the muxed result.

### Active Dolphin user-config edits

These live in `~/Library/Application Support/Dolphin/Config/` and are
required for the recipe to work. Cleanup if reverting:

**Dolphin.ini** — add to `[Core]`:
```ini
CPUCore = 0
SIDevice0 = 6
SIDevice1 = 6
```
And new sections at the end:
```ini
[Movie]
DumpFrames = True
DumpFramesSilent = True
[DSP]
DumpAudio = True
DumpAudioSilent = True
```

**GFX.ini** — add to `[Settings]`:
```ini
DumpFrames = True
DumpFramesAsImages = False
```

### macOS doesn't ship `timeout`

Stock macOS doesn't have GNU `timeout` (or `gtimeout`). The first version
of `track12_validate.sh` used `timeout` and instantly failed with exit 127
("command not found") on every test. The current script uses a Bash
background-watchdog pattern that's portable. If we ever shell out from
Rust to do this, use `tokio::time::timeout` or similar — don't rely on
the system having `timeout(1)`.

### lldb debugging Dolphin

`/Applications/Dolphin.app` is signed without `get-task-allow`, so
lldb can't attach. We work around by copying to `/tmp/Dolphin_debug.app`
and re-signing with `/tmp/debug_entitlements.plist`. Same is required
for the source-built Dolphin (CMake's signing step uses ad-hoc but
without debug entitlements). The plist already exists at
`/tmp/debug_entitlements.plist` from the spike session.

```sh
codesign -s - -f --deep --entitlements /tmp/debug_entitlements.plist \
  <path-to-Dolphin.app>
```

## What's left

1. ~~Three quick validations~~ **DONE** (`/tmp/track12_validations.log`,
   2026-05-05). Results:
   - V1 `--batch` auto-exit: **TIMED OUT** — Dolphin never auto-exits in
     interpreter mode. render_worker time-kills via `child.kill()`.
   - V2 multi-port: **works** — AVI produced; no `--single-port` needed.
   - V3 `--user=<tempdir>`: **works** — output isolated to tempdir; global
     config state has no effect.
2. ~~**Phase 12f**~~ **DONE** (2026-05-05).
   [`render_worker.rs`](../../stats-melee-app/src/render_worker.rs) rewritten
   for the DTM pipeline:
   - `slp_file_to_dtm` helper added to `stats-melee/src/slp_to_dtm.rs`.
   - Per-render isolated temp dir (`--user=<tempdir>`); no backup/restore.
   - Time-based kill: `(pad_game_frames + slp_game_frames) / 60 / 0.07 + 120s`.
   - Audio offset via ffprobe + `amix` (DSP + DTK) mux.
   - All 201 tests pass.
3. ~~**Phase 12g**~~ **DONE** (2026-05-05). `RENDER_VIDEO_FEATURE_ENABLED = true`
   flipped in [`stats-melee-app/src/app.rs:40`](../../stats-melee-app/src/app.rs).
   App starts without crash; "🎞 Render video" button now visible in replay viewer.
4. **Phase 12h** — trigger a render in the live app and verify MP4 is written to
   the video cache.  Debug log: `/tmp/stats-melee-render-debug.log`.
   Expected duration: ~15 min for `Game_20250402T140144.slp` (132 frames).

## Known-good test fixture

`test_slps/Game_20250402T140144.slp` (~132 Slippi frames, smallest
fixture in the corpus). Used as the canonical DTM test input.

## Open questions for future work

- Real-time playback would need the JIT bug fixed. Best path: build
  Dolphin with AddressSanitizer to track the heap corruption in
  `RangeSizeSet`. Not blocking V1.
- Distribution: where does the patched custom Dolphin binary live?
  Currently `/Users/matthewlafrance/Dev/dolphin/Build/Binaries/Dolphin.app`,
  user-specific. For shipping we'd either bundle it, document a build
  step, or upstream the fix.
- The boot-to-input-frame alignment problem from the original plan is
  still unresolved. Currently we use a large `--pad-prefix` to give
  Melee enough boot-time input budget; eventually we'd want an exact
  count or a savestate-based DTM.
