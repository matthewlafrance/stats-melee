//! Background worker: .slp → DTM → vanilla Dolphin → AVI+WAVs → ffmpeg → MP4
//!
//! ## Pipeline
//!
//! ```text
//! .slp ──▶  slp_file_to_dtm (in-process)  ──▶  run.dtm
//!                                                  │
//!                                                  ▼
//!                       vanilla Dolphin (interpreter, --user=<tempdir>)
//!                                                  │
//!                                         <tempdir>/Dump/
//!                                         ├── Frames/GALE01_…_0.avi
//!                                         └── Audio/GALE01_…_dspdump.wav
//!                                                  ├── …_dtkdump.wav
//!                                                  │
//!                                                  ▼
//!                       ffmpeg amix (offset DSP+DTK, mux with AVI)
//!                                                  │
//!                                                  ▼
//!                                       <video_cache>/<hash>.mp4
//! ```
//!
//! ## Key design decisions (from track12_validate.sh results, 2026-05-05)
//!
//! - `--batch` does not auto-exit in interpreter mode → we time-kill Dolphin
//!   after `(prefix_game_frames + slp_game_frames) / 60 / real_time_factor + buffer`.
//! - Multi-port DTM works with our setup → no `--single-port` needed.
//! - `--user=<tempdir>` is fully honoured → one isolated dir per render, no
//!   backup/restore of the user's global Dolphin config.
//!
//! ## Audio offset
//!
//! Dolphin's DSP/DTK audio dump starts before the video dump by
//! `audio_duration − video_duration` seconds. We skip that prefix on both
//! audio inputs with `ffmpeg -ss $offset -i <audio>`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use eframe::egui;


// Conservative real-time factor for interpreter mode on Apple Silicon.
// 0.07 = 7% real-time.  Observed range: 5–10%.  Using the lower end means
// the kill timer fires late rather than early, ensuring the full replay is
// captured.
const INTERPRETER_REAL_TIME_FACTOR: f64 = 0.07;

// Extra seconds added to the computed kill timeout as a safety margin.
const KILL_BUFFER_SECS: u64 = 120;

// ── Public API ───────────────────────────────────────────────────────────────

/// Everything the worker needs to render one replay. All paths absolute.
#[derive(Debug, Clone)]
pub struct RenderRequest {
    /// `.slp` file to render.
    pub slp_path: PathBuf,
    /// Hex SHA-256 of the .slp content — used as the MP4 cache key.
    pub slp_hash: String,
    /// Melee 1.02 NTSC ISO.
    pub melee_iso: PathBuf,
    /// Vanilla Dolphin binary (our patched master build with interpreter
    /// mode; NOT Slippi Playback Dolphin).
    pub dolphin_binary: PathBuf,
    /// `ffmpeg` binary path.  `ffprobe` is derived by replacing the filename.
    pub ffmpeg_binary: PathBuf,
    /// Final destination for the rendered MP4.
    pub mp4_out: PathBuf,
}

/// Progress + completion messages from the render worker.
#[derive(Debug)]
pub enum RenderMsg {
    /// Human-readable status string for the UI progress widget.
    Progress(String),
    /// Final result: `Ok(path)` = the freshly-rendered MP4; `Err(msg)`.
    Done(Result<PathBuf, String>),
}

/// Spawn the render worker.  Returns the receiving half of the channel —
/// caller polls on the UI thread.
pub fn spawn_render(req: RenderRequest, ctx: Option<egui::Context>) -> mpsc::Receiver<RenderMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        run_pipeline(req, &tx, ctx.as_ref());
    });
    rx
}

/// Path of the diagnostic log written during each render.  Overwritten per
/// run so it always reflects the most-recent attempt.
pub const DEBUG_LOG_PATH: &str = "/tmp/stats-melee-render-debug.log";

// ── Worker pipeline ─────────────────────────────────────────────────────────

fn run_pipeline(req: RenderRequest, tx: &mpsc::Sender<RenderMsg>, ctx: Option<&egui::Context>) {
    debug_log_reset();
    debug_log(&format!("=== stats-melee render {} ===", chrono_like_now()));
    debug_log(&format!("slp_path:       {}", req.slp_path.display()));
    debug_log(&format!("melee_iso:      {}", req.melee_iso.display()));
    debug_log(&format!("dolphin_binary: {}", req.dolphin_binary.display()));
    debug_log(&format!("ffmpeg_binary:  {}", req.ffmpeg_binary.display()));
    debug_log(&format!("mp4_out:        {}", req.mp4_out.display()));

    macro_rules! progress {
        ($fmt:literal $($arg:tt)*) => {{
            let _ = tx.send(RenderMsg::Progress(format!($fmt $($arg)*)));
            if let Some(c) = ctx {
                c.request_repaint();
            }
        }};
    }

    let result: Result<PathBuf> = (|| {
        // ── Stage 1: .slp → DTM (with Melee boot navigation prefix) ──────────
        progress!("Generating DTM");
        let (dtm_bytes, slp_game_frame_count, prefix_game_frames) =
            stats_melee::slp_to_dtm::slp_file_to_dtm(&req.slp_path)
                .with_context(|| format!("generating DTM for {}", req.slp_path.display()))?;
        debug_log(&format!(
            "DTM: {} bytes, {slp_game_frame_count} Slippi frames, prefix={prefix_game_frames} game frames",
            dtm_bytes.len()
        ));

        // ── Stage 2: isolated render dir ───────────────────────────────────
        let render_dir = make_render_dir()?;
        debug_log(&format!("render_dir: {}", render_dir.display()));

        write_isolated_dolphin_config(&render_dir.join("Config"))?;

        // Seed the GCI memory card folder so Melee finds existing save data
        // and skips the "Create Game Data?" dialog on first boot.
        match seed_gci_folder(&render_dir) {
            Ok(true)  => debug_log("GCI folder seeded from user's Dolphin saves"),
            Ok(false) => debug_log("warn: no existing Melee GCI save found — dialog may block"),
            Err(e)    => debug_log(&format!("warn: GCI seed failed: {e}")),
        }

        let dtm_path = render_dir.join("run.dtm");
        std::fs::write(&dtm_path, &dtm_bytes)
            .with_context(|| format!("writing {}", dtm_path.display()))?;

        // ── Stage 3: run Dolphin ────────────────────────────────────────────
        // Compute kill timeout: boot/nav prefix frames + replay frames at
        // interpreter speed, plus a buffer.
        let total_game_frames = prefix_game_frames + slp_game_frame_count;
        let wall_secs =
            (total_game_frames as f64 / 60.0 / INTERPRETER_REAL_TIME_FACTOR).ceil() as u64;
        let kill_after = Duration::from_secs(wall_secs + KILL_BUFFER_SECS);
        debug_log(&format!(
            "kill_after: {}s ({total_game_frames} game frames at {INTERPRETER_REAL_TIME_FACTOR:.2}× real-time)",
            kill_after.as_secs()
        ));

        let eta_min = (kill_after.as_secs() as f64 / 60.0).ceil() as u64;
        progress!("Rendering frames — leave Dolphin open (~{eta_min} min)");
        let dolphin_argv = build_dolphin_argv(&req.melee_iso, &dtm_path, &render_dir);
        debug_log(&format!(
            "spawning: {} {:?}",
            req.dolphin_binary.display(),
            dolphin_argv
        ));
        run_dolphin_timed(&req.dolphin_binary, &dolphin_argv, kill_after)?;

        // ── Stage 4: locate dump output ────────────────────────────────────
        debug_log("Dolphin done — scanning dump dir");
        debug_log(&list_dir_recursive(&render_dir.join("Dump")));
        let (avi, dsp, dtk) = find_dump_files(&render_dir).with_context(|| {
            format!(
                "no dump files in {}\nDiagnostic log: {}",
                render_dir.display(),
                DEBUG_LOG_PATH
            )
        })?;
        debug_log(&format!("avi: {}", avi.display()));
        debug_log(&format!("dsp: {}", dsp.display()));
        debug_log(&format!("dtk: {}", dtk.display()));

        // ── Stage 5: mux with ffmpeg ───────────────────────────────────────
        progress!("Muxing video (ffmpeg)");
        let ffprobe = req.ffmpeg_binary.with_file_name("ffprobe");
        let video_dur = probe_duration(&ffprobe, &avi)?;
        let audio_dur = probe_duration(&ffprobe, &dsp)?;
        let offset = (audio_dur - video_dur).max(0.0);
        debug_log(&format!(
            "video_dur={video_dur:.3}s  audio_dur={audio_dur:.3}s  offset={offset:.3}s"
        ));

        let tmp = req.mp4_out.with_extension("tmp");
        let ffmpeg_argv = build_ffmpeg_argv(&avi, &dsp, &dtk, offset, &tmp);
        debug_log(&format!(
            "ffmpeg: {} {:?}",
            req.ffmpeg_binary.display(),
            ffmpeg_argv
        ));
        run_ffmpeg(&req.ffmpeg_binary, &ffmpeg_argv)?;

        // Atomic swap: write through .tmp then rename so the cache never
        // sees a partially-written entry.
        std::fs::rename(&tmp, &req.mp4_out)
            .or_else(|_| {
                let _ = std::fs::remove_file(&req.mp4_out);
                std::fs::rename(&tmp, &req.mp4_out)
            })
            .with_context(|| {
                format!("renaming {} → {}", tmp.display(), req.mp4_out.display())
            })?;

        // Clean up render dir (best-effort — don't fail the render if this
        // errors).
        let _ = std::fs::remove_dir_all(&render_dir);

        Ok(req.mp4_out.clone())
    })();

    let done = match result {
        Ok(p) => RenderMsg::Done(Ok(p)),
        Err(e) => RenderMsg::Done(Err(e.to_string())),
    };
    let _ = tx.send(done);
    if let Some(c) = ctx {
        c.request_repaint();
    }
}

// ── Pure builders ────────────────────────────────────────────────────────────

/// Build the argv for vanilla Dolphin movie playback.
pub fn build_dolphin_argv(iso: &Path, dtm: &Path, user_dir: &Path) -> Vec<String> {
    vec![
        format!("--exec={}", iso.display()),
        format!("--movie={}", dtm.display()),
        format!("--user={}", user_dir.display()),
    ]
}

/// Build the ffmpeg argv for the audio-offset + amix mux step.
///
/// Structure mirrors the working recipe from track12-state.md:
/// ```text
/// ffmpeg -y -i <video>
///           -ss <offset> -i <dsp>
///           -ss <offset> -i <dtk>
///           -filter_complex "[1:a][2:a]amix=inputs=2:duration=shortest"
///           -c:v libx264 -preset veryfast -crf 23
///           -c:a aac -b:a 192k -shortest <out>
/// ```
pub fn build_ffmpeg_argv(
    video: &Path,
    dsp: &Path,
    dtk: &Path,
    offset: f64,
    out: &Path,
) -> Vec<String> {
    let offset_str = format!("{offset:.6}");
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        video.display().to_string(),
        "-ss".into(),
        offset_str.clone(),
        "-i".into(),
        dsp.display().to_string(),
        "-ss".into(),
        offset_str,
        "-i".into(),
        dtk.display().to_string(),
        "-filter_complex".into(),
        "[1:a][2:a]amix=inputs=2:duration=shortest".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "23".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-shortest".into(),
        out.display().to_string(),
    ]
}

// ── IO helpers ───────────────────────────────────────────────────────────────

/// Copy .gci files from the user's default Dolphin "Card A" folder into the
/// render dir so Melee finds existing save data and skips the first-boot
/// "Create Game Data?" dialog.  Returns `Ok(true)` if at least one file was
/// copied, `Ok(false)` if the source folder doesn't exist or is empty.
fn seed_gci_folder(render_dir: &Path) -> Result<bool> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
    let src = PathBuf::from(home)
        .join("Library/Application Support/Dolphin/GC/USA/Card A");
    if !src.exists() {
        return Ok(false);
    }
    let dst = render_dir.join("GC/USA/Card A");
    std::fs::create_dir_all(&dst)
        .with_context(|| format!("creating {}", dst.display()))?;
    let mut copied = 0usize;
    for entry in std::fs::read_dir(&src)
        .with_context(|| format!("reading {}", src.display()))?
    {
        let entry = entry.with_context(|| "reading GCI dir entry")?;
        if entry.file_name().to_string_lossy().ends_with(".gci") {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))
                .with_context(|| format!("copying {}", entry.path().display()))?;
            copied += 1;
        }
    }
    Ok(copied > 0)
}

/// Create a unique temp dir for one render job.
fn make_render_dir() -> Result<PathBuf> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("stats-melee-render-{ts}"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating render dir {}", dir.display()))?;
    Ok(dir)
}

const DOLPHIN_INI_CONTENT: &str = "\
[Core]
CPUCore = 0
SIDevice0 = 6
SIDevice1 = 6
SlotA = 8
SlotB = 255

[Movie]
DumpFrames = True
DumpFramesSilent = True

[DSP]
DumpAudio = True
DumpAudioSilent = True
";

const GFX_INI_CONTENT: &str = "\
[Settings]
DumpFrames = True
DumpFramesAsImages = False
";

/// Write the minimal Dolphin.ini and GFX.ini needed for headless frame
/// dumping into `config_dir`.  Dolphin reads these at startup; writing them
/// before launch guarantees the required settings regardless of the user's
/// global Dolphin config state.
pub fn write_isolated_dolphin_config(config_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("creating {}", config_dir.display()))?;
    std::fs::write(config_dir.join("Dolphin.ini"), DOLPHIN_INI_CONTENT)
        .with_context(|| "writing Dolphin.ini")?;
    std::fs::write(config_dir.join("GFX.ini"), GFX_INI_CONTENT)
        .with_context(|| "writing GFX.ini")?;
    Ok(())
}

/// Spawn Dolphin and kill it after `kill_after` wall-clock time.
///
/// Dolphin does not auto-exit when movie playback ends in interpreter mode
/// (validated 2026-05-05 — `--batch` is ignored).  We poll every 30 s and
/// send SIGKILL once the timeout expires.  The AVI file is intact after a
/// SIGKILL (confirmed by the same validation run).
fn run_dolphin_timed(binary: &Path, argv: &[String], kill_after: Duration) -> Result<()> {
    let mut child = Command::new(binary)
        .args(argv)
        .spawn()
        .with_context(|| format!("spawning {}", binary.display()))?;

    let start = Instant::now();
    let poll = Duration::from_secs(30);

    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| "polling Dolphin")?
        {
            debug_log(&format!("Dolphin exited naturally: {status}"));
            return Ok(());
        }
        if start.elapsed() >= kill_after {
            debug_log(&format!(
                "kill timeout after {}s — sending SIGKILL",
                start.elapsed().as_secs()
            ));
            let _ = child.kill();
            child.wait().with_context(|| "waiting after kill")?;
            debug_log("Dolphin killed and reaped");
            return Ok(());
        }
        thread::sleep(poll);
    }
}

/// Locate the AVI, DSP wav, and DTK wav Dolphin wrote to
/// `<user_dir>/Dump/{Frames,Audio}/`.  Filenames contain timestamps so we
/// glob by suffix rather than expecting a fixed name.
fn find_dump_files(user_dir: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let frames_dir = user_dir.join("Dump").join("Frames");
    let audio_dir = user_dir.join("Dump").join("Audio");

    let avi = first_file_ending(&frames_dir, ".avi")?;
    let dsp = first_file_ending(&audio_dir, "_dspdump.wav")?;
    let dtk = first_file_ending(&audio_dir, "_dtkdump.wav")?;
    Ok((avi, dsp, dtk))
}

fn first_file_ending(dir: &Path, suffix: &str) -> Result<PathBuf> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        if entry.file_name().to_string_lossy().ends_with(suffix) {
            return Ok(entry.path());
        }
    }
    Err(anyhow!(
        "no file ending with {:?} in {}",
        suffix,
        dir.display()
    ))
}

/// Run ffprobe and return the container duration in seconds.
fn probe_duration(ffprobe: &Path, file: &Path) -> Result<f64> {
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
            &file.to_string_lossy(),
        ])
        .output()
        .with_context(|| format!("running ffprobe on {}", file.display()))?;
    let s = String::from_utf8_lossy(&output.stdout);
    s.trim()
        .parse::<f64>()
        .with_context(|| format!("parsing ffprobe output {:?}", s.trim()))
}

/// Run ffmpeg with the given argv, blocking until completion.
fn run_ffmpeg(ffmpeg: &Path, argv: &[String]) -> Result<()> {
    let status = Command::new(ffmpeg)
        .args(argv)
        .status()
        .with_context(|| format!("spawning {}", ffmpeg.display()))?;
    if !status.success() {
        return Err(anyhow!("ffmpeg exited with {status} (argv: {argv:?})"));
    }
    Ok(())
}

// ── Diagnostic log ───────────────────────────────────────────────────────────

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch+{secs}s")
}

fn debug_log(line: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(DEBUG_LOG_PATH)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn debug_log_reset() {
    let _ = std::fs::remove_file(DEBUG_LOG_PATH);
}

fn list_dir_recursive(root: &Path) -> String {
    fn walk(out: &mut String, p: &Path, depth: usize, max_depth: usize) {
        if depth > max_depth {
            return;
        }
        let entries = match std::fs::read_dir(p) {
            Ok(e) => e,
            Err(e) => {
                out.push_str(&format!("{:indent$}(read failed: {e})\n", "", indent = depth * 2));
                return;
            }
        };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let meta = entry.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            if meta.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
                out.push_str(&format!("{:indent$}{name}/\n", "", indent = depth * 2));
                walk(out, &path, depth + 1, max_depth);
            } else {
                out.push_str(&format!(
                    "{:indent$}{name} ({size} bytes)\n",
                    "",
                    indent = depth * 2
                ));
            }
        }
    }
    if !root.exists() {
        return format!("(dir does not exist: {})", root.display());
    }
    let mut out = String::new();
    walk(&mut out, root, 0, 4);
    if out.is_empty() {
        out.push_str("(empty)\n");
    }
    out
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dolphin_argv_has_exec_movie_user() {
        let iso = Path::new("/games/melee.iso");
        let dtm = Path::new("/tmp/render/run.dtm");
        let user = Path::new("/tmp/render");
        let argv = build_dolphin_argv(iso, dtm, user);
        assert!(
            argv.iter().any(|a| a.starts_with("--exec=")),
            "--exec missing: {argv:?}"
        );
        assert!(
            argv.iter().any(|a| a.starts_with("--movie=")),
            "--movie missing: {argv:?}"
        );
        assert!(
            argv.iter().any(|a| a.starts_with("--user=")),
            "--user missing: {argv:?}"
        );
        // Guard against regressing to the Slippi-style argv that confuses
        // the older Dolphin parser.
        for forbidden in &["-i", "-b", "--batch"] {
            assert!(
                !argv.iter().any(|a| a == *forbidden),
                "{forbidden} leaked into argv: {argv:?}"
            );
        }
    }

    #[test]
    fn ffmpeg_argv_has_three_inputs_and_amix() {
        let argv = build_ffmpeg_argv(
            Path::new("/d/Dump/Frames/GALE01_foo_0.avi"),
            Path::new("/d/Dump/Audio/GALE01_foo_dspdump.wav"),
            Path::new("/d/Dump/Audio/GALE01_foo_dtkdump.wav"),
            5.3,
            Path::new("/o/out.tmp"),
        );
        // Exactly three -i flags (video + dsp + dtk).
        assert_eq!(
            argv.iter().filter(|a| a.as_str() == "-i").count(),
            3,
            "expected 3 -i flags: {argv:?}"
        );
        // -filter_complex must contain amix.
        let has_amix = argv
            .windows(2)
            .any(|w| w[0] == "-filter_complex" && w[1].contains("amix"));
        assert!(has_amix, "amix filter missing: {argv:?}");
        // Exactly two -ss flags (one before DSP, one before DTK).
        assert_eq!(
            argv.iter().filter(|a| a.as_str() == "-ss").count(),
            2,
            "expected 2 -ss flags: {argv:?}"
        );
        // Output is the last element.
        assert_eq!(argv.last().unwrap(), "/o/out.tmp");
    }

    #[test]
    fn ffmpeg_argv_offset_zero_when_audio_not_longer() {
        // offset clamped to 0 means the -ss values are "0.000000", still
        // present (two of them).
        let argv = build_ffmpeg_argv(
            Path::new("/v.avi"),
            Path::new("/d.wav"),
            Path::new("/t.wav"),
            0.0,
            Path::new("/o.tmp"),
        );
        assert_eq!(argv.iter().filter(|a| a.as_str() == "-ss").count(), 2);
        let ss_val = argv
            .windows(2)
            .find_map(|w| if w[0] == "-ss" { Some(&w[1]) } else { None })
            .unwrap();
        assert!(ss_val.starts_with("0."), "got: {ss_val}");
    }

    #[test]
    fn write_isolated_dolphin_config_creates_both_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("Config");
        write_isolated_dolphin_config(&config_dir).expect("write config");

        let dolphin_ini =
            std::fs::read_to_string(config_dir.join("Dolphin.ini")).expect("Dolphin.ini");
        assert!(dolphin_ini.contains("CPUCore = 0"), "got: {dolphin_ini}");
        assert!(dolphin_ini.contains("SIDevice0 = 6"), "got: {dolphin_ini}");
        assert!(dolphin_ini.contains("DumpAudio = True"), "got: {dolphin_ini}");
        assert!(dolphin_ini.contains("DumpFrames = True"), "got: {dolphin_ini}");
        assert!(dolphin_ini.contains("SlotA = 8"), "got: {dolphin_ini}");
        assert!(dolphin_ini.contains("SlotB = 255"), "got: {dolphin_ini}");

        let gfx_ini = std::fs::read_to_string(config_dir.join("GFX.ini")).expect("GFX.ini");
        assert!(gfx_ini.contains("DumpFrames = True"), "got: {gfx_ini}");
        assert!(
            gfx_ini.contains("DumpFramesAsImages = False"),
            "got: {gfx_ini}"
        );
    }
}
