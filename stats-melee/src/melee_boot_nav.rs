// Melee NTSC 1.02 boot navigation prefix for DTM playback.
//
// Generates the DTM input sequence that navigates from game boot to match
// start: title screen → main menu → VS Mode → CSS → character select → SSS
// → stage select → match load.
//
// Each game frame (1/60 s) is represented as TWO DTM poll entries because
// Melee polls the Serial Interface twice per video frame. The caller must
// ensure the prefix is prepended before the replay's own inputs.
//
// Layout assumptions (Melee NTSC 1.02, fresh memory card, standard GCN ports):
//   - CSS default cursor: Captain Falcon (row 0, col 7) for all ports
//   - SSS default cursor: Battlefield (row 3, col 3)
//   - Main menu default cursor: 1P Mode (position 0)
//   - VS Mode is 1 DOWN from 1P Mode on the main menu

use crate::dtm::DtmControllerState;

// ── CSS grid ─────────────────────────────────────────────────────────────────
//
// 3 rows × 9 columns.  External (peppi) character IDs.
//
// Row 0: Dr.Mario(22) Mario(8) Luigi(7) Bowser(5) Peach(12) Yoshi(17)
//        DK(1) C.Falcon(0) Ganondorf(25)
// Row 1: Falco(20) Fox(2) Ness(11) ICs(14) Kirby(4) Samus(16)
//        Zelda(18) Link(6) Y.Link(21)
// Row 2: Pichu(24) Pikachu(13) Jigglypuff(15) Mewtwo(10) G&W(3)
//        Marth(9) Roy(23) [Random/empty]

const CSS_ROWS: usize = 3;
const CSS_COLS: usize = 9;
const CSS_DEFAULT_ROW: usize = 0;
const CSS_DEFAULT_COL: usize = 7; // Captain Falcon

pub fn css_position(char_id: u8) -> (usize, usize) {
    match char_id {
        22 => (0, 0), // Dr. Mario
        8  => (0, 1), // Mario
        7  => (0, 2), // Luigi
        5  => (0, 3), // Bowser
        12 => (0, 4), // Peach
        17 => (0, 5), // Yoshi
        1  => (0, 6), // Donkey Kong
        0  => (0, 7), // Captain Falcon (default)
        25 => (0, 8), // Ganondorf
        20 => (1, 0), // Falco
        2  => (1, 1), // Fox
        11 => (1, 2), // Ness
        14 => (1, 3), // Ice Climbers
        4  => (1, 4), // Kirby
        16 => (1, 5), // Samus
        18 => (1, 6), // Zelda/Sheik
        6  => (1, 7), // Link
        21 => (1, 8), // Young Link
        24 => (2, 0), // Pichu
        13 => (2, 1), // Pikachu
        15 => (2, 2), // Jigglypuff
        10 => (2, 3), // Mewtwo
        3  => (2, 4), // Mr. Game & Watch
        9  => (2, 5), // Marth
        23 => (2, 6), // Roy
        _  => (0, 7), // Unknown → Captain Falcon
    }
}

// ── SSS grid ─────────────────────────────────────────────────────────────────
//
// Approximate NTSC 1.02 VS Mode stage select grid, 5 rows × 8 columns.
// Default cursor at Battlefield (row 3, col 3).
// The layout is approximate; rows/cols may need empirical adjustment.

const SSS_ROWS: usize = 5;
const SSS_COLS: usize = 8;
const SSS_DEFAULT_ROW: usize = 3;
const SSS_DEFAULT_COL: usize = 3; // Battlefield

pub fn sss_position(stage_id: u16) -> (usize, usize) {
    match stage_id {
        4  => (0, 0), // Princess Peach's Castle
        5  => (0, 1), // Kongo Jungle
        6  => (0, 2), // Brinstar
        7  => (0, 3), // Corneria
        8  => (0, 4), // Yoshi's Story
        9  => (0, 5), // Onett
        10 => (0, 6), // Mute City
        11 => (1, 0), // Rainbow Cruise
        12 => (1, 1), // Jungle Japes
        13 => (1, 2), // Great Bay
        14 => (1, 3), // Hyrule Temple
        15 => (1, 4), // Brinstar Depths
        16 => (1, 5), // Yoshi's Island
        17 => (1, 6), // Green Greens
        18 => (1, 7), // Fourside
        19 => (2, 0), // Mushroom Kingdom I
        20 => (2, 1), // Mushroom Kingdom II
        22 => (2, 2), // Venom
        23 => (2, 3), // Poke Floats
        24 => (2, 4), // Big Blue
        25 => (2, 5), // Icicle Mountain
        27 => (2, 6), // Flat Zone
        28 => (3, 0), // Dream Land N64
        29 => (3, 1), // Yoshi's Island N64
        30 => (3, 2), // Kongo Jungle N64
        31 => (3, 3), // Battlefield (default cursor)
        32 => (3, 4), // Final Destination
        2  => (4, 0), // Fountain of Dreams
        3  => (4, 1), // Pokemon Stadium
        _  => (3, 3), // Unknown → Battlefield
    }
}

// ── Navigation helpers ────────────────────────────────────────────────────────

// Build a per-port input sequence (one DtmControllerState per game frame)
// to navigate from `start` to `target` in a wrapping grid.  Strategy:
// horizontal first (stay in starting row), then vertical.  This avoids
// landing on the empty slots at the far right of CSS row 2.
fn nav_sequence(
    start: (usize, usize),
    target: (usize, usize),
    rows: usize,
    cols: usize,
    hold_frames: usize,
    release_frames: usize,
) -> Vec<DtmControllerState> {
    let (sr, sc) = start;
    let (tr, tc) = target;
    let mut seq = Vec::new();

    // Horizontal: navigate to target column while staying in the starting row.
    if tc != sc {
        let right_dist = (tc as isize - sc as isize).rem_euclid(cols as isize) as usize;
        let left_dist  = cols - right_dist;
        let (is_right, count) = if right_dist <= left_dist {
            (true, right_dist)
        } else {
            (false, left_dist)
        };
        for _ in 0..count {
            let s = if is_right {
                DtmControllerState { dpad_right: true, ..DtmControllerState::neutral() }
            } else {
                DtmControllerState { dpad_left: true, ..DtmControllerState::neutral() }
            };
            for _ in 0..hold_frames    { seq.push(s.clone()); }
            for _ in 0..release_frames { seq.push(DtmControllerState::neutral()); }
        }
    }

    // Vertical: now at (sr, tc); move to target row.
    if tr != sr {
        let down_dist = (tr as isize - sr as isize).rem_euclid(rows as isize) as usize;
        let up_dist   = rows - down_dist;
        let (is_down, count) = if down_dist <= up_dist {
            (true, down_dist)
        } else {
            (false, up_dist)
        };
        for _ in 0..count {
            let s = if is_down {
                DtmControllerState { dpad_down: true, ..DtmControllerState::neutral() }
            } else {
                DtmControllerState { dpad_up: true, ..DtmControllerState::neutral() }
            };
            for _ in 0..hold_frames    { seq.push(s.clone()); }
            for _ in 0..release_frames { seq.push(DtmControllerState::neutral()); }
        }
    }

    seq
}

// Combine per-port game-frame sequences into the DTM frame format: each
// game frame = `Vec<DtmControllerState>` with one entry per port.  Shorter
// sequences are padded with neutral states.  Each game frame is emitted as
// two identical DTM poll entries (Melee polls SI twice per video frame).
fn combine_and_emit(port_seqs: &[Vec<DtmControllerState>]) -> Vec<Vec<DtmControllerState>> {
    let num_ports = port_seqs.len();
    let max_len = port_seqs.iter().map(|s| s.len()).max().unwrap_or(0);
    let mut out = Vec::with_capacity(max_len * 2);
    for i in 0..max_len {
        let row: Vec<DtmControllerState> = (0..num_ports)
            .map(|p| port_seqs[p].get(i).cloned().unwrap_or_else(DtmControllerState::neutral))
            .collect();
        out.push(row.clone());
        out.push(row); // 2 polls per game frame
    }
    out
}

// ── Builder ───────────────────────────────────────────────────────────────────

struct Builder {
    num_ports: usize,
    frames: Vec<Vec<DtmControllerState>>,
}

impl Builder {
    fn new(num_ports: usize) -> Self {
        Self { num_ports, frames: Vec::new() }
    }

    // Emit `count` neutral game frames (each = 2 DTM polls).
    fn neutral(&mut self, count: usize) {
        let row = vec![DtmControllerState::neutral(); self.num_ports];
        for _ in 0..(count * 2) {
            self.frames.push(row.clone());
        }
    }

    // Hold `states` for `hold` game frames, then emit `release` neutral frames.
    fn press(&mut self, states: &[DtmControllerState], hold: usize, release: usize) {
        assert_eq!(states.len(), self.num_ports);
        for _ in 0..(hold * 2) {
            self.frames.push(states.to_vec());
        }
        self.neutral(release);
    }

    // Build a state vec with `state` for port `idx`, neutral for all others.
    fn port_state(&self, idx: usize, state: DtmControllerState) -> Vec<DtmControllerState> {
        (0..self.num_ports)
            .map(|i| if i == idx { state.clone() } else { DtmControllerState::neutral() })
            .collect()
    }

    fn extend(&mut self, extra: Vec<Vec<DtmControllerState>>) {
        self.frames.extend(extra);
    }

    fn into_frames(self) -> Vec<Vec<DtmControllerState>> {
        self.frames
    }

    fn game_frames(&self) -> usize {
        self.frames.len() / 2
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build a DTM prefix that navigates Melee from cold boot to match start.
///
/// `char_ids` must be in ascending-port order (matching `SlpToDtmResult`).
/// For a standard 1v1, `char_ids = [p1_external_char_id, p2_external_char_id]`.
///
/// Returns `(prefix_frames, prefix_game_frame_count)`.
pub fn build_boot_prefix(char_ids: &[u8], stage_id: u16) -> (Vec<Vec<DtmControllerState>>, usize) {
    let num_ports = char_ids.len();
    let mut b = Builder::new(num_ports);

    // ── 1. Neutral boot ───────────────────────────────────────────────────────
    // Nintendo/HAL logos run for ~120 frames and ignore buttons.
    b.neutral(200);

    // ── 1b. Dismiss "There is no Memory Card in Slot A." ─────────────────────
    // This dialog appears after the logos (~frame 150) and requires an A press
    // to continue.  Pressing A here clears it; logos ignore A so early presses
    // are safe.
    let a = b.port_state(0, DtmControllerState { a: true, ..DtmControllerState::neutral() });
    b.press(&a, 2, 2);

    // ── 1c. Wait for "PRESS START" title screen ───────────────────────────────
    // Title screen appears at ~game frame 702.  (200 + 4 + 546 = 750 total.)
    b.neutral(546);

    // ── 2. Skip opening cinematic ─────────────────────────────────────────────
    // The first START press skips the cinematic (if it's still playing) and
    // lands on the "PRESS START" title screen.  It does NOT advance to the
    // main menu; that requires a second press.
    let start = b.port_state(0, DtmControllerState { start: true, ..DtmControllerState::neutral() });
    b.press(&start, 2, 2);

    // ── 3. Brief wait, then advance title screen → main menu ──────────────────
    // 120 frames (2 s) is enough for the title screen to settle after the
    // cinematic skip, and well under the ~600-frame attract-mode timeout that
    // re-launches the cinematic / demo match.
    b.neutral(120);
    b.press(&start, 2, 2);

    // ── 4. Wait for main menu ─────────────────────────────────────────────────
    b.neutral(180);

    // ── 5. Navigate to VS Mode (one DOWN) and open it ─────────────────────────
    let down = b.port_state(0, DtmControllerState { dpad_down: true, ..DtmControllerState::neutral() });
    b.press(&down, 2, 2);
    let a = b.port_state(0, DtmControllerState { a: true, ..DtmControllerState::neutral() });
    b.press(&a, 2, 2);

    // ── 6. Confirm "Melee" in the VS Mode submenu ─────────────────────────────
    // VS Mode opens a sub-menu with Melee/Tournament/Special Melee/Custom
    // Rules/Name Entry; "Melee" is highlighted by default, so a second A
    // press takes us to the CSS.  Without it, subsequent inputs navigate
    // the sub-menu and we end up on Name Entry.
    b.neutral(60);
    b.press(&a, 2, 2);

    // ── 7. Wait for CSS ───────────────────────────────────────────────────────
    b.neutral(700);

    // ── 6. Navigate all ports to their characters simultaneously ──────────────
    const NAV_HOLD: usize = 2;
    const NAV_RELEASE: usize = 2;
    let port_seqs: Vec<Vec<DtmControllerState>> = char_ids
        .iter()
        .map(|&cid| {
            let target = css_position(cid);
            nav_sequence(
                (CSS_DEFAULT_ROW, CSS_DEFAULT_COL),
                target,
                CSS_ROWS, CSS_COLS,
                NAV_HOLD, NAV_RELEASE,
            )
        })
        .collect();
    b.extend(combine_and_emit(&port_seqs));

    // ── 7. Select characters (all ports press A simultaneously) ───────────────
    let all_a: Vec<DtmControllerState> = (0..num_ports)
        .map(|_| DtmControllerState { a: true, ..DtmControllerState::neutral() })
        .collect();
    b.press(&all_a, 2, 2);

    // ── 8. Wait for Stage Select Screen ──────────────────────────────────────
    b.neutral(400);

    // ── 9. P1 navigates to target stage ──────────────────────────────────────
    let sss_target = sss_position(stage_id);
    let sss_seq = nav_sequence(
        (SSS_DEFAULT_ROW, SSS_DEFAULT_COL),
        sss_target,
        SSS_ROWS, SSS_COLS,
        NAV_HOLD, NAV_RELEASE,
    );
    let sss_port_seqs: Vec<Vec<DtmControllerState>> = (0..num_ports)
        .map(|i| if i == 0 { sss_seq.clone() } else { vec![] })
        .collect();
    b.extend(combine_and_emit(&sss_port_seqs));

    // ── 10. Confirm stage (P1 presses A) ─────────────────────────────────────
    let p1_a = b.port_state(0, DtmControllerState { a: true, ..DtmControllerState::neutral() });
    b.press(&p1_a, 2, 2);

    // ── 11. Wait for match to load (loading screen + character intros) ────────
    // Covers ~600 frames of loading + intro before Slippi frame -123 begins.
    b.neutral(700);

    let gf = b.game_frames();
    (b.into_frames(), gf)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_captain_falcon_is_default() {
        assert_eq!(css_position(0), (CSS_DEFAULT_ROW, CSS_DEFAULT_COL));
    }

    #[test]
    fn css_luigi_row0_col2() {
        assert_eq!(css_position(7), (0, 2));
    }

    #[test]
    fn css_marth_row2_col5() {
        assert_eq!(css_position(9), (2, 5));
    }

    #[test]
    fn css_unknown_falls_back_to_captain_falcon() {
        assert_eq!(css_position(200), (CSS_DEFAULT_ROW, CSS_DEFAULT_COL));
    }

    #[test]
    fn sss_battlefield_is_default() {
        assert_eq!(sss_position(31), (SSS_DEFAULT_ROW, SSS_DEFAULT_COL));
    }

    #[test]
    fn sss_final_destination_is_one_right_of_battlefield() {
        let bf = sss_position(31);
        let fd = sss_position(32);
        assert_eq!(fd.0, bf.0, "same row");
        assert_eq!(fd.1, bf.1 + 1, "one column right");
    }

    #[test]
    fn nav_sequence_no_move_when_already_at_target() {
        let seq = nav_sequence((0, 7), (0, 7), CSS_ROWS, CSS_COLS, 2, 2);
        assert!(seq.is_empty());
    }

    #[test]
    fn nav_sequence_luigi_from_falcon_uses_right_not_left() {
        // Luigi is at (0,2); from Falcon (0,7) going RIGHT 4 is shorter
        // than going LEFT 5.
        let seq = nav_sequence((0, 7), (0, 2), CSS_ROWS, CSS_COLS, 2, 2);
        // 4 presses × (2 hold + 2 release) = 16 frames
        assert_eq!(seq.len(), 16);
        // All presses should be dpad_right
        let presses: Vec<_> = seq.iter().filter(|s| s.dpad_right).collect();
        assert_eq!(presses.len(), 4 * 2, "4 presses × 2 hold-frames");
        let rights: Vec<_> = seq.iter().filter(|s| s.dpad_left).collect();
        assert!(rights.is_empty(), "no dpad_left expected");
    }

    #[test]
    fn nav_sequence_marth_horiz_then_vert() {
        // Marth (2,5) from Falcon (0,7): LEFT 2 (horiz) then UP 1 (vert,
        // since UP 1 < DOWN 2 in a 3-row grid).
        // 3 presses × (2 hold + 2 release) = 12 frames
        let seq = nav_sequence((0, 7), (2, 5), CSS_ROWS, CSS_COLS, 2, 2);
        assert_eq!(seq.len(), 12);
    }

    #[test]
    fn build_boot_prefix_1v1_luigi_marth_fd() {
        // Fixture: P1=Luigi(7), P2=Marth(9), stage=FD(32)
        let (frames, gf) = build_boot_prefix(&[7, 9], 32);
        // Each game frame = 2 DTM poll entries
        assert_eq!(frames.len(), gf * 2);
        // Each poll has 2 controller states (one per port)
        assert_eq!(frames[0].len(), 2);
        // Rough sanity: at least 2000 game frames (15s boot + menus + wait)
        assert!(gf >= 2000, "prefix too short: {gf} game frames");
    }

    #[test]
    fn build_boot_prefix_starts_neutral() {
        let (frames, _) = build_boot_prefix(&[0, 9], 31);
        // First poll frame must be all-neutral (boot phase)
        let f0 = &frames[0];
        for ctrl in f0 {
            assert!(!ctrl.start && !ctrl.a && !ctrl.dpad_down,
                "first frame should be neutral");
        }
    }

    #[test]
    fn build_boot_prefix_has_start_press() {
        // The sequence must contain at least one frame where P1 presses START
        let (frames, _) = build_boot_prefix(&[0, 9], 31);
        let has_start = frames.iter().any(|row| row[0].start);
        assert!(has_start, "no START press found in boot prefix");
    }
}
