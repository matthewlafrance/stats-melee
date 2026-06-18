// Slippi pre-frame → DtmControllerState transform.
// Pure, no IO. See docs/slp-to-dtm-render.md §"Slippi pre-frame → DTM" and
// §"UCF strategy" (we use Option B: processed joystick/cstick, physical triggers).

use std::path::Path;

use anyhow::{anyhow, Result};
use peppi::game::immutable::Game;

use crate::dtm::{DtmControllerState, DtmHeader, write_dtm};
use crate::melee_boot_nav::build_boot_prefix;

// Melee 1.02 NTSC-U MD5 — the only ISO we support for DTM rendering.
const NTSC_102_MD5: [u8; 16] = [
    0x0e, 0x63, 0xd4, 0x22, 0x3b, 0x01, 0xd9, 0xab,
    0xa5, 0x96, 0x25, 0x9d, 0xc1, 0x55, 0xa1, 0x74,
];

/// Read `slp_path`, convert to DTM bytes, and prepend a Melee boot navigation
/// prefix that drives the game from cold boot through menus to match start.
///
/// Returns `(dtm_bytes, slp_game_frame_count, prefix_game_frames)`.
/// The caller uses both frame counts for the Dolphin kill timeout:
/// `(prefix_game_frames + slp_game_frame_count) / 60 / real_time_factor`.
pub fn slp_file_to_dtm(slp_path: &Path) -> Result<(Vec<u8>, usize, usize)> {
    use std::{fs, io, time};

    let game = {
        let mut r = io::BufReader::new(
            fs::File::open(slp_path)
                .map_err(|e| anyhow!("opening {}: {e}", slp_path.display()))?,
        );
        peppi::io::slippi::read(&mut r, None)
            .map_err(|e| anyhow!("parsing {}: {e}", slp_path.display()))?
    };

    let slp_game_frame_count = game.frames.id.len();
    let mut result = slp_to_dtm(&game)?;

    // Extract per-port character IDs (in ascending port order, matching
    // the DTM frame layout).
    let char_ids: Vec<u8> = {
        let mut ports: Vec<_> = game.start.players.iter().collect();
        ports.sort_by_key(|p| p.port as u8);
        ports.iter().map(|p| p.character).collect()
    };

    let stage_id = game.start.stage;
    let (prefix, prefix_game_frames) = build_boot_prefix(&char_ids, stage_id);

    let mut all_frames = Vec::with_capacity(prefix.len() + result.frames.len());
    all_frames.extend(prefix);
    all_frames.extend(result.frames);
    result.frames = all_frames;

    let input_count = result.frames.len() as u64;
    let recording_start_time = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut header = DtmHeader::new_for_melee(input_count, NTSC_102_MD5, recording_start_time);
    header.controllers_plugged = result.controllers_plugged;

    Ok((write_dtm(&header, &result.frames), slp_game_frame_count, prefix_game_frames))
}

/// Outcome of [`slp_to_dtm`]: the controllers bitmask and the per-frame states.
pub struct SlpToDtmResult {
    /// DTM `controllers_plugged` bitmask: bit N = GC port (N+1) is active.
    pub controllers_plugged: u8,
    /// `frames[i]` holds one `DtmControllerState` per active port for frame `i`,
    /// in ascending port order. Length == number of frames in the replay.
    pub frames: Vec<Vec<DtmControllerState>>,
}

/// Convert all pre-frame controller data from a peppi `Game` into a form
/// ready to pass to [`crate::dtm::write_dtm`].
///
/// Ports are sorted in ascending order (`P1 < P2 < P3 < P4`) to match the
/// DTM body's port-order requirement. `controllers_plugged` is derived from
/// which ports are present in the replay.
///
/// Uses Option B (processed joystick/cstick, physical triggers — see the
/// plan doc §"UCF strategy"). Invalid frame slots (ICs nana absent, etc.)
/// are represented as a neutral controller state.
pub fn slp_to_dtm(game: &Game) -> Result<SlpToDtmResult> {
    if game.frames.ports.is_empty() {
        return Err(anyhow!("replay has no port data"));
    }

    // Sort by port number so the DTM body is in the required port order.
    let mut port_data: Vec<_> = game.frames.ports.iter().collect();
    port_data.sort_by_key(|p| p.port as u8);

    let controllers_plugged: u8 = port_data
        .iter()
        .fold(0u8, |acc, p| acc | (1 << (p.port as u8)));

    // Melee polls the Serial Interface twice per game frame, so each Slippi
    // frame's input is emitted as TWO identical DTM controller records. This
    // matches the inputCount = 2 × frameCount ratio Dolphin expects.
    const POLLS_PER_FRAME: usize = 2;

    let frame_count = game.frames.id.len();
    let mut frames = Vec::with_capacity(frame_count * POLLS_PER_FRAME);

    for frame_idx in 0..frame_count {
        let mut row = Vec::with_capacity(port_data.len());
        for pd in &port_data {
            let pre = &pd.leader.pre;

            // Respect peppi's per-frame validity bitmap.
            let valid = pre
                .validity
                .as_ref()
                .map_or(true, |bm| bm.get_bit(frame_idx));

            if !valid {
                row.push(DtmControllerState::neutral());
                continue;
            }

            let btn = pre.buttons_physical.value(frame_idx);
            row.push(DtmControllerState {
                // ── Byte 0 buttons (Slippi physical u16 → DTM bit positions) ──
                start:      bit(btn, 12),
                a:          bit(btn,  8),
                b:          bit(btn,  9),
                x:          bit(btn, 10),
                y:          bit(btn, 11),
                z:          bit(btn,  4),
                dpad_up:    bit(btn,  3),
                dpad_down:  bit(btn,  2),
                // ── Byte 1 buttons ──────────────────────────────────────────
                dpad_left:  bit(btn,  0),
                dpad_right: bit(btn,  1),
                l_digital:  bit(btn,  6),
                r_digital:  bit(btn,  5),
                // ── Triggers (physical) ─────────────────────────────────────
                l_pressure: trigger_to_u8(pre.triggers_physical.l.value(frame_idx)),
                r_pressure: trigger_to_u8(pre.triggers_physical.r.value(frame_idx)),
                // ── Sticks (Option B: processed / post-UCF) ─────────────────
                stick_x:  axis_to_u8(pre.joystick.x.value(frame_idx)),
                stick_y:  axis_to_u8(pre.joystick.y.value(frame_idx)),
                cstick_x: axis_to_u8(pre.cstick.x.value(frame_idx)),
                cstick_y: axis_to_u8(pre.cstick.y.value(frame_idx)),
            });
        }
        // Push the same row POLLS_PER_FRAME times so the body length matches
        // Dolphin's expected `inputCount × controllers × 8` byte count.
        for _ in 0..POLLS_PER_FRAME {
            frames.push(row.clone());
        }
    }

    Ok(SlpToDtmResult {
        controllers_plugged,
        frames,
    })
}

#[inline]
fn bit(w: u16, n: u8) -> bool {
    w & (1 << n) != 0
}

/// Physical trigger [0.0, 1.0] → u8 [0, 255].
#[inline]
fn trigger_to_u8(t: f32) -> u8 {
    (t * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Processed axis [-1.0, 1.0] → u8 [0, 255], center 128.
#[inline]
fn axis_to_u8(v: f32) -> u8 {
    ((v + 1.0) * 127.5).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Conversion-function unit tests (no fixture needed) ──────────────────

    #[test]
    fn trigger_to_u8_endpoints() {
        assert_eq!(trigger_to_u8(0.0), 0);
        assert_eq!(trigger_to_u8(1.0), 255);
    }

    #[test]
    fn trigger_to_u8_half() {
        // 0.5 * 255 = 127.5 → rounds to 128
        assert_eq!(trigger_to_u8(0.5), 128);
    }

    #[test]
    fn trigger_to_u8_clamps_out_of_range() {
        assert_eq!(trigger_to_u8(-0.1), 0);
        assert_eq!(trigger_to_u8(1.1), 255);
    }

    #[test]
    fn axis_to_u8_center_zero() {
        // 0.0 → (0 + 1) * 127.5 = 127.5 → 128
        assert_eq!(axis_to_u8(0.0), 128);
    }

    #[test]
    fn axis_to_u8_endpoints() {
        assert_eq!(axis_to_u8(-1.0), 0);
        assert_eq!(axis_to_u8(1.0), 255);
    }

    #[test]
    fn axis_to_u8_clamps_out_of_range() {
        assert_eq!(axis_to_u8(-1.1), 0);
        assert_eq!(axis_to_u8(1.1), 255);
    }

    #[test]
    fn bit_helper() {
        assert!(bit(0b0001_0000_0000_0000u16, 12)); // Start bit
        assert!(!bit(0b0001_0000_0000_0000u16, 11));
        assert!(bit(0b1111_1111_1111_1111u16, 0));
    }

    // ── Fixture-based integration tests ─────────────────────────────────────

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test_slps/Game_20250402T140144.slp"
    );

    fn parse_fixture() -> peppi::game::immutable::Game {
        use std::{fs, io};
        let mut r = io::BufReader::new(fs::File::open(FIXTURE).expect("fixture not found"));
        peppi::io::slippi::read(&mut r, None).expect("parse failed")
    }

    #[test]
    fn slp_to_dtm_two_ports_for_1v1() {
        let game = parse_fixture();
        let result = slp_to_dtm(&game).expect("slp_to_dtm failed");
        // 1v1 replay → exactly 2 active ports
        assert_eq!(
            result.frames.first().map(|f| f.len()),
            Some(2),
            "expected 2 ports per frame"
        );
    }

    #[test]
    fn slp_to_dtm_controllers_plugged_ports_1_2() {
        let game = parse_fixture();
        let result = slp_to_dtm(&game).expect("slp_to_dtm failed");
        // Typical 1v1 uses P1+P2 → bitmask 0b00000011
        assert_eq!(result.controllers_plugged, 0b00000011);
    }

    #[test]
    fn slp_to_dtm_outputs_two_polls_per_slippi_frame() {
        let game = parse_fixture();
        let slippi_frames = game.frames.id.len();
        let result = slp_to_dtm(&game).expect("slp_to_dtm failed");
        assert_eq!(result.frames.len(), slippi_frames * 2);
    }

    #[test]
    fn slp_to_dtm_sticks_are_plausible() {
        // Every stick byte should be in [0, 255] — trivially true for u8 —
        // but we also check that at least some frames have a non-zero
        // stick value (i.e., the conversion isn't clamping everything away).
        let game = parse_fixture();
        let result = slp_to_dtm(&game).expect("slp_to_dtm failed");
        let any_nonzero_stick = result.frames.iter().any(|frame| {
            frame.iter().any(|c| c.stick_x != 0 || c.stick_y != 0)
        });
        assert!(any_nonzero_stick, "all stick values were zero — conversion bug?");
    }

    #[test]
    fn slp_to_dtm_first_few_frames_are_neutral_buttons() {
        // Slippi frames start at -123 (countdown). The first several frames
        // should have no buttons pressed (neutral pad while the game loads).
        let game = parse_fixture();
        let result = slp_to_dtm(&game).expect("slp_to_dtm failed");
        let first = &result.frames[0];
        for ctrl in first {
            assert!(!ctrl.start, "start pressed on first frame");
            assert!(!ctrl.a, "A pressed on first frame");
            assert!(!ctrl.b, "B pressed on first frame");
        }
    }
}
