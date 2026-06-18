//! Minimal stage-bounds table used by the combat-state classifier for
//! off-stage detection.
//!
//! This is what remains after we ripped out the embedded 2D viewer —
//! no platforms, no blast zones, no geometry-for-rendering. Just the
//! main-platform x-range and y-height per legal stage, because "is
//! this character off-stage right now" is a first-class signal for the
//! advantage / disadvantage heuristic.
//!
//! The main-platform y we expose is the *top surface* of the stage
//! floor — the value characters land on in peppi's world frame. A
//! player with `position.y < bounds.y` is airborne below the stage
//! plane and therefore off-stage.
//!
//! Stages outside the competitive pool (Peach's Castle, Kongo Jungle,
//! Hyrule Temple, etc.) return `None` — the classifier falls back to
//! hitstun + action-state signals alone when geometry is unavailable.

/// Main-platform bounds for off-stage detection.
///
/// `left`..`right` is the horizontal extent of the stage floor;
/// characters with `position.x` outside that range are off-stage in
/// the horizontal dimension. `y` is the top surface — characters
/// below it are off-stage vertically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageBounds {
    pub left: f32,
    pub right: f32,
    pub y: f32,
}

impl StageBounds {
    pub const fn new(left: f32, right: f32, y: f32) -> Self {
        Self { left, right, y }
    }

    /// True when `(x, y)` is outside the main stage. Off-stage by any
    /// dimension counts — a character well within horizontal range but
    /// below the main floor (falling through a side platform after
    /// getting hit) is still off-stage for classification purposes.
    pub fn is_offstage(&self, x: f32, y: f32) -> bool {
        x < self.left || x > self.right || y < self.y
    }
}

/// Look up main-platform bounds for `stage_id`. Stage ids line up with
/// peppi's `game.start.stage` — the raw Slippi stage id, not the peppi
/// enum ordinal. See the slippi-wiki SPEC.md for the canonical id
/// table.
pub fn stage_bounds(stage_id: i32) -> Option<StageBounds> {
    match stage_id {
        2 => Some(StageBounds::new(-63.35, 63.35, 0.0)),  // Fountain of Dreams
        3 => Some(StageBounds::new(-87.75, 87.75, 0.0)),  // Pokémon Stadium
        8 => Some(StageBounds::new(-56.0, 56.0, 0.0)),    // Yoshi's Story
        28 => Some(StageBounds::new(-77.27, 77.27, 0.0)), // Dream Land 64
        31 => Some(StageBounds::new(-68.4, 68.4, 0.0)),   // Battlefield
        32 => Some(StageBounds::new(-85.5, 85.5, 0.0)),   // Final Destination
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_legal_stages_have_bounds() {
        // The six-stage legal pool. If any of these returns None,
        // off-stage detection silently disables on that stage — which
        // would be a real regression, so guard against it explicitly.
        for id in [2, 3, 8, 28, 31, 32] {
            assert!(
                stage_bounds(id).is_some(),
                "stage id {id} missing bounds"
            );
        }
    }

    #[test]
    fn unknown_stage_returns_none() {
        for id in [0, 1, 4, 5, 6, 7, 9, 14, 999, -1] {
            assert!(
                stage_bounds(id).is_none(),
                "stage id {id} should have no bounds"
            );
        }
    }

    #[test]
    fn bounds_have_positive_width() {
        for id in [2, 3, 8, 28, 31, 32] {
            let b = stage_bounds(id).unwrap();
            assert!(b.right > b.left, "stage {id}: right {} <= left {}", b.right, b.left);
        }
    }

    #[test]
    fn is_offstage_detects_horizontal() {
        // Battlefield — left edge at -68.4, right at 68.4, y=0.
        let b = stage_bounds(31).unwrap();
        assert!(b.is_offstage(-100.0, 10.0), "far left airborne = offstage");
        assert!(b.is_offstage(100.0, 10.0), "far right airborne = offstage");
        assert!(!b.is_offstage(0.0, 10.0), "center airborne = onstage");
    }

    #[test]
    fn is_offstage_detects_vertical_below() {
        let b = stage_bounds(31).unwrap();
        // Below stage plane, even within horizontal range, = offstage
        // (character is mid-fall through a side platform or dropped).
        assert!(b.is_offstage(0.0, -5.0), "center below plane = offstage");
    }

    #[test]
    fn is_offstage_tolerates_on_floor() {
        // Standing on the main floor is explicitly *not* offstage.
        let b = stage_bounds(31).unwrap();
        assert!(!b.is_offstage(0.0, 0.0), "on-floor center = onstage");
        assert!(!b.is_offstage(-68.0, 0.0), "on-floor near-left = onstage");
    }
}
