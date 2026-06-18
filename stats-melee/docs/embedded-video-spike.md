# Track 10e — Embedded video widget spike

> **Status:** four strategies surveyed; recommendation at the bottom.
> Implementation deferred until you pick a path — the dep weight delta
> across options is too big to silently commit one way.

Phase V1 (Track 10d) ships an "Open in OS player" button — clicks the
cached MP4 into QuickTime / mpv / whatever the system handler is. That
already satisfies the "we own the video" goal; what V2 buys is *putting
the video inside the eframe window* so:

1. The scrub bar (Track 10f) can sit directly on top of the video and
   share its time axis — clicking the bar should seek the video.
2. The combat-state coloring renders as an overlay on the video
   surface, not a sibling widget the user has to flip back and forth
   between.
3. The viewer-page UX matches what every native replay tool (Slippi
   itself, dolphin-replay-tools, etc.) does — eyes don't have to
   bounce between two windows.

(1) is the killer feature. (2) and (3) are nice-to-have.

## Constraints we're optimizing for

- **Single binary on macOS/Linux/Windows.** No bundled .so/.dylib
  install instructions for end users.
- **Reasonable dep weight.** stats-melee-app's release binary is
  currently in the low-megabyte range (eframe + diesel + peppi). The
  budget for video can be at most ~30 MB of additional binary;
  bigger than that is a smell.
- **Frame-stepping support.** The viewer should be able to step a
  single frame at a time — Melee tech is most readable that way.
- **Audio in sync.** Not a hard requirement at V2 (we could ship
  silent at first), but the path needs to be visible.

## Strategy A — ffmpeg-next (Rust bindings to libav*)

**Crate:** `ffmpeg-next = "7"` (or `ffmpeg-next-sys` for raw bindings).

**Shape:** Open the MP4 with `ffmpeg::format::input(&path)`. Iterate
packets, decode H.264 → YUV420 → convert to RGB with `sws_scale`,
upload an `egui::TextureHandle` per frame.

```rust
use ffmpeg_next as ffmpeg;
let mut input = ffmpeg::format::input(&path)?;
let video_stream_idx = input.streams().best(ffmpeg::media::Type::Video)?.index();
let mut decoder = stream.codec().decoder().video()?;
for (stream, packet) in input.packets() {
    if stream.index() != video_stream_idx { continue; }
    decoder.send_packet(&packet)?;
    let mut frame = ffmpeg::frame::Video::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        let rgb = scaler.run(&frame, &mut rgb_frame)?;
        // upload rgb_frame.data(0) into egui ColorImage
    }
}
```

**Pros:**

- Mature, battle-tested. Every native video player ever uses libav.
- Frame-accurate seek via `av_seek_frame`.
- Audio decode + sync is a known recipe — ffmpeg-next exposes audio
  streams the same way.
- Pure Rust at the binding layer; the heavy lifting is C.

**Cons:**

- **Build complexity is the showstopper.** `ffmpeg-next` *links
  against an existing libav install*. On macOS that means
  `brew install ffmpeg` + setting `PKG_CONFIG_PATH`. The user
  already has ffmpeg installed for Track 10d's render worker, so this
  isn't a new ask, but it does bake "ffmpeg must be present at
  compile time" into the build in a way that the subprocess approach
  doesn't.
- **License story is the *other* showstopper.** ffmpeg builds with
  H.264 support are GPL-by-default (libx264 is GPL); the LGPL build
  requires explicitly disabling x264. We can decode H.264 via the
  built-in `libavcodec` H.264 decoder which is LGPL, but the build
  config needs to be right.
- ~10–15 MB extra binary size from the linked libav stack.

## Strategy B — gstreamer-rs

**Crate:** `gstreamer = "0.24"` plus `gstreamer-app`, `gstreamer-video`.

**Shape:** Build a pipeline (`uridecodebin → videoconvert → appsink`),
pull RGB samples out of the appsink, upload to egui.

**Pros:**

- macOS first-class support via the native GStreamer install.
- Audio + video in one pipeline; sync handled by the framework.
- Mature, widely deployed.

**Cons:**

- Bigger user setup story than ffmpeg. macOS users typically
  `brew install gstreamer` + a half-dozen plugin packages. Linux is
  fine via `apt install`. Windows is *unpleasant*.
- More opinionated — the framework wants to drive the event loop,
  which fights with eframe's immediate-mode loop.
- Even bigger binary: the dynamic-link stack is ~50 MB.

This is the path I'd take for a desktop app shipping to a wide
audience, not for a self-built tool we control. Rejected.

## Strategy C — Pure-Rust decoder

**Crates:**

- `openh264 = "0.6"` — Cisco's H.264 decoder, MIT/BSD-licensed,
  native Rust bindings.
- `mp4 = "0.14"` — MP4 container parser, pure Rust.
- We'd implement audio out-of-scope for V2 (video-only is fine).

**Shape:** `mp4`'s `Mp4Reader` walks tracks, hands H.264 NAL units to
`openh264::Decoder`, which spits YUV. Convert YUV→RGB in Rust, upload.

**Pros:**

- Zero runtime dep. Single Rust binary, no `brew install` step.
- License-clean.
- Cross-platform identically — what works on macOS works on Windows.

**Cons:**

- **No audio.** `openh264` is video-only; audio decoding (AAC) needs
  another crate, and the pure-Rust AAC decoder space is not great.
- Performance: pure-Rust H.264 decode is ~3–5x slower than the C
  reference. Probably still fast enough for 1x playback at 60 fps
  (Melee renders are 1280x528 not 1080p), but the headroom for slo-mo
  playback is gone.
- Frame-accurate seek is doable via the MP4 sample table but adds
  bookkeeping the other strategies hide.
- Niche dep stack — fewer eyeballs on bugs.

## Strategy D — ffmpeg subprocess + raw RGB pipe

**Shape:** No new compile-time deps. At play-time, spawn ffmpeg as a
child process with an argv that decodes the MP4 and pipes raw RGB
frames to stdout:

```
ffmpeg -i video.mp4 -f rawvideo -pix_fmt rgba -an pipe:1
```

The Rust side reads `width * height * 4` bytes per frame off stdout,
uploads to an `egui::TextureHandle`, advances the playhead. Audio is a
parallel `ffmpeg ... -f s16le -ar 48000 pipe:1` piped to a tiny
audio-output crate (`cpal`).

For frame stepping / scrub: kill the current ffmpeg, respawn with
`-ss <seek-time> -frames:v 1` (decode one frame at the new position).
This is what `ffplay` does internally, just split across processes.

**Pros:**

- **Zero new compile-time deps.** ffmpeg is already a runtime dep
  from Track 10d's render worker.
- License: same as Track 10d (the `ffmpeg` binary is the user's
  responsibility to install, not bundled).
- Frame-accurate seek for free via `-ss`.
- Easy to debug: the same ffmpeg invocation runs at the shell.
- Trivially cross-platform if the user's `ffmpeg` install is sane.

**Cons:**

- Per-frame syscall + pipe overhead is real. Modern hardware can
  push tens of thousands of pipe writes per second so 60fps isn't
  the bottleneck, but a sustained 4k re-pipe stream at 60fps adds
  about 1 GB/min of pipe traffic. Manageable but worth measuring.
- Audio sync needs care — two parallel ffmpeg processes (one for
  video, one for audio) drift unless we sync them against a shared
  clock.
- Spawning ffmpeg + first-frame latency is in the 100–200ms range.
  Acceptable for a "click play" gesture; not great for tight
  scrubbing without a frame cache.
- We're encoding the seek-and-decode dance ourselves rather than
  letting libav do it.

## Recommendation

**Strategy D** for V2's first cut. Reasoning:

1. We already require ffmpeg at runtime. There is no new
   compile-time dep, no new install step, no new license story.
2. The build stays the small, clean cargo build we have today —
   nobody on the team has to learn `PKG_CONFIG_PATH` arcana to
   contribute.
3. Spike effort is low: ffmpeg's stdin/stdout protocol is stable and
   well-documented, the pipe-per-frame loop is a few hundred lines.
4. If perf turns out to matter (sustained 60fps with a 4k input,
   say), the migration path to Strategy A is straightforward — the
   widget API stays "give me the next frame as RGB", just the
   producer changes from `Child::stdout` to `ffmpeg-next::format`.
5. Audio is scoped out for V2; D's "dual ffmpeg processes" approach
   is the natural extension when we want it.

Strategy A is the right answer for V3 (or sooner) once we know the
shape of the audio + scrub interaction. Strategy C is the right answer
*only* if the "no runtime ffmpeg" axis becomes a real requirement, and
right now Track 10d already pinned that.

## Implementation sketch for Strategy D

Module: `stats-melee-app/src/video_widget.rs`. Owns:

- A child `ffmpeg` process with stdout piped to the app.
- A reader thread that pulls frames off the pipe into an
  `mpsc::Receiver<RgbFrame>` so the UI thread doesn't block.
- An `egui::TextureHandle` updated each repaint.
- Play / pause / step controls — paused state simply stops pulling
  off the receiver; step nudges one frame.
- Seek: kill the current ffmpeg, respawn with `-ss <time>`.

Skeleton:

```rust
pub struct VideoWidget {
    mp4_path: PathBuf,
    frame_rx: mpsc::Receiver<RgbFrame>,
    child: Child,                   // for kill on seek/drop
    texture: Option<egui::TextureHandle>,
    state: PlaybackState,           // Playing | Paused | AtFrame(usize)
    width: u32,
    height: u32,
    fps: f64,
    current_frame: usize,
}

impl VideoWidget {
    pub fn open(mp4_path: PathBuf, ffmpeg: &Path) -> Result<Self> { … }
    pub fn render(&mut self, ui: &mut egui::Ui) { … }
    pub fn play(&mut self) { … }
    pub fn pause(&mut self) { … }
    pub fn step(&mut self, frames: i64) { … }
    pub fn seek_seconds(&mut self, t: f64) -> Result<()> { … }
}

impl Drop for VideoWidget {
    fn drop(&mut self) { let _ = self.child.kill(); }
}
```

Tests:

- `argv` planner builds the right `ffmpeg -i ... -f rawvideo -pix_fmt
  rgba ...` invocation. Pure, easy.
- The frame reader correctly chunks `w*h*4`-byte frames from a
  fixture pipe (use `std::io::Cursor` over a synthesized buffer).
- State transitions (play/pause/step) drive the texture forward as
  expected.

## Open questions to answer during implementation

1. **Frame format**: rgba (4 bytes/px) is convenient but rgb (3) is
   25% less pipe traffic. egui wants rgba in `ColorImage`, so the
   convert-to-rgba either happens in ffmpeg (`-pix_fmt rgba`) or in
   the reader thread. Probably ffmpeg — it's vectorized.
2. **Frame buffering**: how many frames the reader thread buffers
   before back-pressuring the producer. Too few → stutter on slow
   draws; too many → lag on seek.
3. **Width/height/fps probe**: parse `ffprobe` output once at open
   time, or run an `ffmpeg -i` and read stderr. Latter is in the
   render worker already; could share that helper.
4. **macOS ffmpeg binary detection**: the render worker uses the
   same binary; share the resolution helper.

## What this unblocks

Track 10f (Phase V3 — scrub-bar + combat-state overlay) drops in on
top of `VideoWidget`'s `seek_seconds` API. The bar consumes
`(current_frame, total_frames)`, click → `widget.seek_seconds(...)`,
combat colors render as a bar background. Track 4's "clickable scrub
bar" (currently downgraded to `Sense::hover`) re-enables.
