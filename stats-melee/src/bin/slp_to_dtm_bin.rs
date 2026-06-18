// CLI tool: .slp → .dtm
//
// Usage:
//   cargo run --bin slp_to_dtm_bin -- [--single-port] <replay.slp> [output.dtm]
//
// If no output path is given, writes <replay>.dtm alongside the input.
//
// `--single-port` keeps only port 1's records and forces controllers_plugged
// to 0x01. Used to isolate Dolphin port-2 controller-init crashes during the
// 12d spike — see memory project_track12_dtm_status.
//
// `--pad-prefix=N` prepends N neutral controller polls to every active port
// before the replay's own inputs. Used to give Melee enough boot-time input
// budget so the movie doesn't end before the title screen renders.

use anyhow::{anyhow, Result};
use std::{env, fs, io, path::PathBuf, time};
use stats_melee::{
    dtm::{DtmControllerState, DtmHeader, write_dtm},
    slp_to_dtm::slp_to_dtm,
};

const NTSC_102_MD5: [u8; 16] = [
    0x0e, 0x63, 0xd4, 0x22, 0x3b, 0x01, 0xd9, 0xab,
    0xa5, 0x96, 0x25, 0x9d, 0xc1, 0x55, 0xa1, 0x74,
];

fn main() -> Result<()> {
    let mut positional: Vec<String> = Vec::new();
    let mut single_port = false;
    let mut pad_prefix: usize = 0;
    for arg in env::args().skip(1) {
        if arg == "--single-port" {
            single_port = true;
        } else if let Some(n) = arg.strip_prefix("--pad-prefix=") {
            pad_prefix = n.parse().map_err(|e| anyhow!("--pad-prefix needs a positive integer: {e}"))?;
        } else {
            positional.push(arg);
        }
    }

    let slp_path = positional
        .get(0)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("usage: slp_to_dtm_bin [--single-port] <replay.slp> [output.dtm]"))?;

    let dtm_path = positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| slp_path.with_extension("dtm"));

    // Parse the replay.
    let game = {
        let mut r = io::BufReader::new(fs::File::open(&slp_path)?);
        peppi::io::slippi::read(&mut r, None)?
    };

    let mut result = slp_to_dtm(&game)?;

    if single_port {
        // Drop everything but port 1 from each frame, and reset the bitmask
        // to "port 1 only". The replay's actual P2 inputs are simply not
        // emitted to the DTM.
        for row in result.frames.iter_mut() {
            row.truncate(1);
        }
        result.controllers_plugged = 0b00000001;
    }

    if pad_prefix > 0 {
        let ports = result.frames.first().map(|f| f.len()).unwrap_or(1);
        let pad_row = vec![DtmControllerState::neutral(); ports];
        let mut padded = Vec::with_capacity(pad_prefix + result.frames.len());
        padded.extend(std::iter::repeat_with(|| pad_row.clone()).take(pad_prefix));
        padded.extend(result.frames.into_iter());
        result.frames = padded;
    }

    let input_count = result.frames.len() as u64;
    let recording_start_time = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut header = DtmHeader::new_for_melee(input_count, NTSC_102_MD5, recording_start_time);
    header.controllers_plugged = result.controllers_plugged;

    let dtm_bytes = write_dtm(&header, &result.frames);
    fs::write(&dtm_path, &dtm_bytes)?;

    println!("wrote {} bytes ({} frames, {} ports) → {}",
        dtm_bytes.len(),
        input_count,
        result.frames.first().map(|f| f.len()).unwrap_or(0),
        dtm_path.display(),
    );

    // Print the Dolphin command for phase 12d.
    let dtm_abs = fs::canonicalize(&dtm_path)
        .unwrap_or_else(|_| dtm_path.clone());

    let dump_dir = dtm_abs
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("dtm_dump");

    println!();
    println!("── Phase 12d: run vanilla Dolphin ────────────────────────────────────");
    println!(
        r#"mkdir -p {dump}
"/Applications/Dolphin.app/Contents/MacOS/Dolphin" \
  --exec="<PATH_TO_MELEE_ISO>" \
  --movie="{dtm}" \
  --batch \
  --output-directory="{dump}""#,
        dump = dump_dir.display(),
        dtm  = dtm_abs.display(),
    );
    println!("──────────────────────────────────────────────────────────────────────");
    println!("Then check {}/framedump_0.png exists.", dump_dir.display());

    Ok(())
}
