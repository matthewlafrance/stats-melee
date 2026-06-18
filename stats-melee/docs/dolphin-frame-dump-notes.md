# Track 10a — Dolphin frame-dump spike (macOS)

> **Status:** recipe drafted from public docs / reference tools; needs to
> be executed against your local Slippi install before we trust the
> numbers and commit to the pipeline shape.

This is a plain spike, not production code. The deliverable is a single
known-good `.slp → .mp4` round-trip on your machine, with timings + any
gotchas captured here so Tracks 10b–10g can build on a real recipe
instead of guessing.

## Goal

Take one fixture replay (let's use the shortest fixture in
`/sessions/vigilant-great-hypatia/mnt/slippi/test_slps/` —
short = fast iteration on the spike) and produce an `.mp4` on disk
without any GUI clicks or screen recording. Validate end-to-end before
we sink time into the cache module (10b), the worker (10c), or the
embedded widget (10e).

## How the toolchain composes

```
.slp  ──▶  Slippi Playback Dolphin (headless)  ──▶  framedump.avi  +  dspdump.wav
                          │                                       │
                          │  (driven by a "comm.json" file         │
                          │   passed via `-i`, --batch flag,        │
                          │   isRealTimeMode=false to render        │
                          │   as fast as the host can go)           │
                          └─────────────────────┬───────────────────┘
                                                ▼
                                             ffmpeg
                                                │
                                                ▼
                                          out.mp4
```

Every meaningful prior-art tool (`slp2mp4`, `slp-to-mp4`, `slp-to-video`)
follows this same shape. We're not inventing a pipeline; we're picking
which prior tool to learn from and confirming it still works on
current Slippi Launcher builds.

## What you need on the machine before running the spike

1. **Slippi Launcher** installed normally — its bundled playback Dolphin
   is the binary we drive. macOS path is typically:
   `~/Library/Application Support/Slippi Launcher/playback/Slippi Dolphin.app`.
   Confirm with:
   ```sh
   ls "$HOME/Library/Application Support/Slippi Launcher/playback/"
   ```
   The actual binary inside the bundle is at
   `Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin`.

2. **Melee 1.02 NTSC ISO** at a known path. The playback Dolphin needs
   to know where it is — same setup as normal replay viewing. If you
   haven't done it yet: open Slippi Launcher → Settings → "Melee ISO
   Path" → point at the file. The path is persisted in Dolphin's user
   config so the headless run inherits it.

3. **ffmpeg** on PATH:
   ```sh
   brew install ffmpeg
   ffmpeg -version
   ```

4. **A throwaway output dir** to keep the spike's artifacts separate:
   ```sh
   mkdir -p /tmp/stats-melee-spike
   ```

## The recipe

### Step 1 — figure out where Dolphin's user dir is

The user dir is where Dolphin reads config (graphics settings, frame-dump
toggles) and writes dumps. For Slippi Launcher's bundled playback build
on macOS the canonical path is:

```
$HOME/Library/Application Support/Slippi Launcher/playback/User
```

Confirm by listing:

```sh
ls "$HOME/Library/Application Support/Slippi Launcher/playback/User/Config" 2>/dev/null \
  || echo "user dir not where expected — open Slippi Launcher once to populate it"
```

### Step 2 — toggle frame + audio dump in `GFX.ini`

Frame dumping isn't a CLI flag; it's a config knob in Dolphin's
graphics settings. Edit (or create the relevant lines in)
`User/Config/GFX.ini`. The properties we need:

```ini
[Settings]
DumpFrames = True
DumpFramesSilent = True   # don't pop a "saved a frame dump" dialog
DumpFormat = avi          # safer than mp4 for Dolphin's built-in muxer
BitrateKbps = 25000       # 25 Mbps — overkill for analysis, fine for spike
```

And in `User/Config/Dolphin.ini`:

```ini
[DSP]
DumpAudio = True
DumpAudioSilent = True
```

> ⚠ **Save the original two files first** (`cp GFX.ini GFX.ini.bak` etc).
> The spike modifies your normal Slippi Dolphin config. Restore when
> you're done so non-headless playback isn't dumping frames.

### Step 3 — write a comm-file

Slippi's playback Dolphin reads a JSON "comm file" via the `-i` flag.
Schema (from `project-slippi/slippi-wiki/COMM_SPEC.md`):

| field                | type           | default        | notes                                                              |
|----------------------|----------------|----------------|--------------------------------------------------------------------|
| `mode`               | string         | `"normal"`     | `"normal"` / `"queue"` / `"mirror"`                                |
| `replay`             | string         | —              | path to `.slp` (used in `normal` mode)                             |
| `startFrame`         | int            | `-123`         | first Slippi frame (negative = pre-game)                           |
| `endFrame`           | int            | `INT_MAX`      | last frame inclusive                                               |
| `commandId`          | string         | —              | optional client-side correlation id                                |
| `queue`              | QueueItem[]    | —              | for `queue` mode: replays played back-to-back                      |
| `isRealTimeMode`     | bool           | `false`        | **false = render as fast as possible** ← critical for the spike    |
| `outputOverlayFiles` | bool           | `false`        | writes `.txt` overlay sidecars; not what we need                   |

QueueItem fields: `path`, `startFrame`, `endFrame`, `gameStation`.

Spike comm file (`/tmp/stats-melee-spike/comm.json`):

```json
{
  "mode": "queue",
  "isRealTimeMode": false,
  "outputOverlayFiles": false,
  "queue": [
    {
      "path": "/absolute/path/to/test_slps/<pick-a-short-one>.slp",
      "startFrame": -123,
      "endFrame": 99999,
      "gameStation": "stats-melee-spike"
    }
  ]
}
```

`mode: "queue"` matters — when the queue ends, Dolphin can shut down
cleanly. `mode: "normal"` will play the replay then sit there idle.

### Step 4 — run Dolphin headless

```sh
"$HOME/Library/Application Support/Slippi Launcher/playback/Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin" \
  --batch \
  --exec="<absolute path to Melee 1.02 ISO>" \
  -i "/tmp/stats-melee-spike/comm.json" \
  2>&1 | tee /tmp/stats-melee-spike/dolphin.log
```

Flags:

- `--batch` — Dolphin will exit on emulation-stop instead of returning
  to its main menu. With `mode: "queue"` this means "play through the
  queue, then exit".
- `--exec=<path>` — game ISO. Some Slippi builds also accept `-e`.
- `-i <comm.json>` — points at the comm file. Slippi's playback Dolphin
  reads this on startup.

**Time the run.** With `isRealTimeMode = false` a 5-minute replay
should render in well under a minute on M1/M2 hardware — the speed
ceiling is mostly Dolphin's framelimit toggle plus host GPU. Capture
wall-clock time:

```sh
time <the command above>
```

### Step 5 — find the dumps

Dolphin writes frame + audio dumps under the user dir:

```
~/Library/Application Support/Slippi Launcher/playback/User/Dump/Frames/framedump0.avi
~/Library/Application Support/Slippi Launcher/playback/User/Dump/Audio/dspdump.wav
```

Confirm both exist and have non-zero size:

```sh
ls -lh "$HOME/Library/Application Support/Slippi Launcher/playback/User/Dump/Frames"
ls -lh "$HOME/Library/Application Support/Slippi Launcher/playback/User/Dump/Audio"
```

### Step 6 — mux with ffmpeg

```sh
ffmpeg \
  -i "$HOME/Library/Application Support/Slippi Launcher/playback/User/Dump/Frames/framedump0.avi" \
  -i "$HOME/Library/Application Support/Slippi Launcher/playback/User/Dump/Audio/dspdump.wav" \
  -c:v libx264 -preset veryfast -crf 23 \
  -c:a aac -b:a 192k \
  -shortest \
  /tmp/stats-melee-spike/out.mp4
```

`-shortest` truncates the audio if it's longer than the video (Dolphin
sometimes records a fraction of a second after the last frame).

### Step 7 — verify

```sh
open /tmp/stats-melee-spike/out.mp4
# or
ffprobe /tmp/stats-melee-spike/out.mp4
```

Things to confirm and write down here:

- [ ] Output file plays end-to-end with audio in sync.
- [ ] Output duration ≈ replay duration (use `parse_single_replay` on
      the .slp to get the expected seconds, compare to `ffprobe`'s
      duration).
- [ ] Wall-clock time the spike took. ← expected ratio: render-time /
      replay-time. Anything > 1.0 means we're running real-time, which
      is bad — recheck `isRealTimeMode: false`.
- [ ] Output file size for a 5-minute replay (sanity-check disk
      footprint for the cache eviction policy in 10b).
- [ ] What, if anything, Dolphin pops a GUI dialog for. The
      `*Silent = True` toggles should suppress everything; if a dialog
      appears, capture which one.

## Open questions to capture during the run

(These are the items most likely to bite us in 10b/10c — answering them
during the spike is cheaper than discovering them later.)

1. **Does `--batch` actually cause exit on queue completion?** Some
   Dolphin forks need an additional flag to terminate cleanly.
2. **Does the comm-file persist across runs?** I.e. can the worker
   reuse one comm.json by overwriting it, or does Dolphin lock it?
3. **What's the multi-instance story?** Can we run two headless
   Dolphins in parallel, each pointed at its own user dir, to get
   throughput on bulk renders? Slippi Launcher's user dir layout
   suggests yes (the path is configurable via `--user`), but worth
   confirming with a 2-process test.
4. **Does the audio dump get overwritten on each run, or appended?** If
   appended, the worker has to clean it before each render.
5. **Frame-dump format on macOS specifically.** Dolphin's "avi" writer
   uses a particular codec on macOS that ffmpeg should handle, but
   there's been historical breakage. If the AVI doesn't decode, switch
   `DumpFormat = png` (one PNG per frame) and adjust the ffmpeg input
   accordingly:
   `ffmpeg -framerate 60 -i frame_%07d.png ...`.

## What this unblocks

Once the spike is green:

- **10b: cache module** can use the wall-clock numbers + output size
  here to set defaults (LRU byte budget, parallel render count).
- **10c: render worker** has the exact `Command` invocation to shell
  out to. The worker just needs to template the comm-file path, the
  replay path, and an output-dir override.
- **10d: V1 OS-player** is a button on the viewer page that runs the
  worker, polls the cache, then `open`s the resulting `.mp4` in the
  default player. End-to-end demo without touching eframe video
  rendering yet.

## References

- [project-slippi/slippi-wiki — COMM_SPEC.md](https://github.com/project-slippi/slippi-wiki/blob/master/COMM_SPEC.md) — comm-file JSON schema, queue semantics.
- [jmlee337/slp2mp4](https://github.com/jmlee337/slp2mp4) — current-best reference implementation; cross-platform Python wrapper around Dolphin + ffmpeg.
- [NunoDasNeves/slp-to-mp4](https://github.com/NunoDasNeves/slp-to-mp4) — original Python tool, smaller surface area, easier to read.
- [project-slippi/Ishiiruka — Slippi-on-macOS](https://github.com/project-slippi/Ishiiruka/wiki/Slippi-on-macOS) — macOS-specific gotchas for the playback build.
- [Dolphin emulator docs — Controlling the Global User Directory](https://dolphin-emu.org/docs/guides/controlling-global-user-directory/) — `--user` flag, user-dir layout. Useful for the parallel-render question above.
