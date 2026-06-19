//! Data-loading + row shape for the Replay Library page.
//!
//! Keeping the DB → `ReplayRow` translation in its own module so the UI
//! code in `app.rs` stays focused on rendering.

use std::collections::HashMap;

use anyhow::Result;
use diesel::prelude::*;

use stats_melee::gamedata::{CHARACTERS, STAGES};
use stats_melee::models::{Game, GamePlayer};

/// One slot in a game (1st / 2nd / 3rd / 4th place).
#[derive(Debug, Clone)]
pub struct PlayerSlot {
    /// Slippi connect code, e.g. "MATT#123".
    pub code: String,
    /// Character id as per peppi's character table (0 = Mario, 1 = Fox, ...).
    pub character_id: i32,
}

impl PlayerSlot {
    /// Pretty-printed character name. Returns "Unknown" for ids we don't
    /// know about (peppi spec is pre-DLC so the table is fixed at 33).
    pub fn character_name(&self) -> &'static str {
        usize::try_from(self.character_id)
            .ok()
            .and_then(|i| CHARACTERS.get(i).copied())
            .unwrap_or("Unknown")
    }
}

/// One row in the Replay Library table.
///
/// Denormalized so rendering doesn't have to poke the DB — build it once
/// via [`load_rows`] and reuse across frames.
#[derive(Debug, Clone)]
pub struct ReplayRow {
    pub game_id: i32,
    /// Indexed by placement slot (0 = 1st, 1 = 2nd, ...). Entries are
    /// `None` for unpopulated slots (common on 1v1 games).
    pub slots: [Option<PlayerSlot>; 4],
    /// Stage id from peppi's stage table.
    pub stage_id: i32,
    /// Match duration in seconds (already adjusted for the 123-frame
    /// pre-game by the ingestion path).
    pub duration_seconds: i32,
    /// True when the given `user_player_code` placed first in this game.
    /// `None` means either the user wasn't in this match or no code was
    /// provided to the loader.
    pub user_won: Option<bool>,
    /// ISO-8601 UTC timestamp recorded by SQLite when this game row was
    /// inserted. Sorting uses string comparison, which is correct because
    /// the format is fixed-width `"YYYY-MM-DD HH:MM:SS"`.
    pub ingested_at: String,
    /// ISO-8601 timestamp the game was PLAYED, from the Slippi metadata
    /// `startAt`. `None` for legacy rows ingested before the column
    /// existed (re-ingest to populate). String-comparable for sort/filter
    /// because ISO-8601 is lexicographically ordered.
    pub played_at: Option<String>,
}

impl ReplayRow {
    /// Pretty-printed stage name.
    pub fn stage_name(&self) -> &'static str {
        usize::try_from(self.stage_id)
            .ok()
            .and_then(|i| STAGES.get(i).copied())
            .unwrap_or("Unknown")
    }

    /// Duration formatted as `M:SS`.
    pub fn duration_display(&self) -> String {
        let total = self.duration_seconds.max(0);
        let minutes = total / 60;
        let seconds = total % 60;
        format!("{minutes}:{seconds:02}")
    }

    /// The played date as `YYYY-MM-DD`, or `None` when not recorded. Both
    /// `2025-04-01T14:39:10Z` and `2025-04-01 14:39:10` slice to the same
    /// 10-char prefix.
    pub fn played_date(&self) -> Option<&str> {
        self.played_at.as_deref().and_then(|s| {
            if s.len() >= 10 {
                Some(&s[..10])
            } else {
                None
            }
        })
    }

    /// The ingested ("date added") date as `YYYY-MM-DD`, sliced from the
    /// fixed-width `"YYYY-MM-DD HH:MM:SS"` SQLite timestamp. `None` only for
    /// the (shouldn't-happen) case of a malformed/short timestamp.
    pub fn ingested_date(&self) -> Option<&str> {
        if self.ingested_at.len() >= 10 {
            Some(&self.ingested_at[..10])
        } else {
            None
        }
    }
}

/// Load every game in the DB, optionally filtering to games in which
/// `user_player_code` appeared, and enrich each with its player slots +
/// outcome-for-user flag.
///
/// Uses two queries total (games + game_players) then a single in-memory
/// denormalization pass — avoids an N+1 round-trip per game.
pub fn load_rows(
    conn: &mut SqliteConnection,
    user_player_code: Option<&str>,
) -> Result<Vec<ReplayRow>> {
    use stats_melee::schema::game;
    use stats_melee::schema::gamePlayer;

    // Pull every game. Ordered by id desc so the most recently ingested
    // games land at the top of the table.
    let games: Vec<Game> = game::table
        .order(game::id.desc())
        .select(Game::as_select())
        .load(conn)?;

    // Index every gamePlayer row by id for O(1) slot lookup.
    let gps: Vec<GamePlayer> = gamePlayer::table.select(GamePlayer::as_select()).load(conn)?;
    let gp_by_id: HashMap<i32, GamePlayer> = gps.into_iter().map(|g| (g.id, g)).collect();

    let mut rows = Vec::with_capacity(games.len());
    for g in games {
        let slot_ids = [g.first, g.second, g.third, g.fourth];
        let slots: [Option<PlayerSlot>; 4] = std::array::from_fn(|i| {
            slot_ids[i]
                .and_then(|id| gp_by_id.get(&id))
                .map(|gp| PlayerSlot {
                    code: gp.code.clone(),
                    character_id: gp.character,
                })
        });

        // Filter out games where the user didn't appear, if a code was given.
        let user_present = match user_player_code {
            Some(code) if !code.is_empty() => slots
                .iter()
                .flatten()
                .any(|s| s.code == code),
            _ => true,
        };
        if !user_present {
            continue;
        }

        // Outcome relative to the user: did their code land in slot 0?
        let user_won = match user_player_code {
            Some(code) if !code.is_empty() => slots
                .first()
                .and_then(|s| s.as_ref())
                .map(|s| s.code == code),
            _ => None,
        };

        rows.push(ReplayRow {
            game_id: g.id,
            slots,
            stage_id: g.stage,
            duration_seconds: g.time,
            user_won,
            ingested_at: g.ingested_at,
            played_at: g.started_at,
        });
    }

    Ok(rows)
}

/// Column the replay table is sorted by.
///
/// The enum doubles as the header's "which column did the user click"
/// identity — flipping sort direction is handled at the UI layer by
/// comparing against the current key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Default: most recently ingested game at the top.
    IngestedAt,
    /// By when the game was played (Slippi `startAt`). Rows with no
    /// recorded play date sink to the bottom regardless of direction.
    PlayedAt,
    /// Insert order — stable even if the system clock is wrong. Useful
    /// as a tiebreaker and as a fallback when timestamps all collide
    /// (e.g. a single bulk import).
    GameId,
    /// Alphabetical by stage name.
    Stage,
    /// Shorter games sort first ascending, longer first descending.
    Duration,
    /// Wins before losses ascending; losses before wins descending.
    /// Rows where the user wasn't present always sink to the bottom.
    Outcome,
}

impl SortKey {
    /// Default direction when the user first clicks this column — chosen
    /// so the "most useful ordering" shows up immediately. Clicking a
    /// column that's already active flips direction.
    pub fn default_direction(self) -> SortDirection {
        match self {
            // Most recent first / largest game_id first feels right by
            // default; the alphabetical and numeric columns start small.
            SortKey::IngestedAt | SortKey::PlayedAt | SortKey::GameId | SortKey::Duration => {
                SortDirection::Desc
            }
            SortKey::Stage | SortKey::Outcome => SortDirection::Asc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// In-place stable sort of `rows` by `(key, direction)`. Stable so that
/// ties on the primary key preserve the previous order — clicking
/// "Stage" after "Duration" gives a Stage-primary, Duration-secondary
/// ordering for free.
///
/// Outcome sort is special-cased: rows where the user wasn't present
/// (`user_won == None`) are always pinned to the bottom, regardless of
/// `dir`. The user clicked "Outcome" to compare wins to losses; floating
/// "not applicable" rows to the top of a desc sort is just noise.
pub fn sort_rows(rows: &mut [ReplayRow], key: SortKey, dir: SortDirection) {
    use std::cmp::Ordering;

    rows.sort_by(|a, b| {
        if matches!(key, SortKey::Outcome) {
            // Partition first: unknowns are always last.
            let a_known = a.user_won.is_some();
            let b_known = b.user_won.is_some();
            if a_known != b_known {
                return if a_known { Ordering::Less } else { Ordering::Greater };
            }
            // Both known (or both unknown) — rank wins-before-losses,
            // then apply direction.
            let ord = outcome_rank(a.user_won).cmp(&outcome_rank(b.user_won));
            return match dir {
                SortDirection::Asc => ord,
                SortDirection::Desc => ord.reverse(),
            };
        }

        if matches!(key, SortKey::PlayedAt) {
            // Rows with no recorded play date sink to the bottom regardless
            // of direction (same rationale as the Outcome unknowns).
            let a_known = a.played_at.is_some();
            let b_known = b.played_at.is_some();
            if a_known != b_known {
                return if a_known { Ordering::Less } else { Ordering::Greater };
            }
            let ord = a.played_at.cmp(&b.played_at);
            return match dir {
                SortDirection::Asc => ord,
                SortDirection::Desc => ord.reverse(),
            };
        }

        let ord = match key {
            SortKey::IngestedAt => a.ingested_at.cmp(&b.ingested_at),
            SortKey::GameId => a.game_id.cmp(&b.game_id),
            SortKey::Stage => a.stage_name().cmp(b.stage_name()),
            SortKey::Duration => a.duration_seconds.cmp(&b.duration_seconds),
            // Unreachable — Outcome / PlayedAt are handled above. Written
            // out long-hand so this stays `rustc`-exhaustive on a new key.
            SortKey::Outcome | SortKey::PlayedAt => Ordering::Equal,
        };
        match dir {
            SortDirection::Asc => ord,
            SortDirection::Desc => ord.reverse(),
        }
    });
}

/// Integer rank for known outcomes only. Wins before losses; used after
/// the Some/None partition in `sort_rows`.
fn outcome_rank(user_won: Option<bool>) -> u8 {
    match user_won {
        Some(true) => 0,
        Some(false) => 1,
        // Not actually hit — partitioned out upstream — but the match
        // needs to be total.
        None => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(code: &str, char_id: i32) -> Option<PlayerSlot> {
        Some(PlayerSlot {
            code: code.to_string(),
            character_id: char_id,
        })
    }

    #[test]
    fn character_name_lookup_handles_valid_and_invalid_ids() {
        let s = slot("A", 1).unwrap(); // Fox
        assert_eq!(s.character_name(), "Fox");

        let s = PlayerSlot {
            code: "A".into(),
            character_id: 999,
        };
        assert_eq!(s.character_name(), "Unknown");

        let s = PlayerSlot {
            code: "A".into(),
            character_id: -1,
        };
        assert_eq!(s.character_name(), "Unknown");
    }

    #[test]
    fn stage_name_lookup_handles_valid_and_invalid_ids() {
        let row = ReplayRow {
            game_id: 0,
            slots: [None, None, None, None],
            stage_id: 2, // FountainOfDreams
            duration_seconds: 0,
            user_won: None,
            ingested_at: String::new(),
            played_at: None,
        };
        assert_eq!(row.stage_name(), "FountainOfDreams");

        let row = ReplayRow {
            game_id: 0,
            slots: [None, None, None, None],
            stage_id: 999,
            duration_seconds: 0,
            user_won: None,
            ingested_at: String::new(),
            played_at: None,
        };
        assert_eq!(row.stage_name(), "Unknown");
    }

    #[test]
    fn duration_display_formats_as_m_ss() {
        let mk = |secs| ReplayRow {
            game_id: 0,
            slots: [None, None, None, None],
            stage_id: 0,
            duration_seconds: secs,
            user_won: None,
            ingested_at: String::new(),
            played_at: None,
        };
        assert_eq!(mk(0).duration_display(), "0:00");
        assert_eq!(mk(5).duration_display(), "0:05");
        assert_eq!(mk(65).duration_display(), "1:05");
        assert_eq!(mk(180).duration_display(), "3:00");
        // Negative (shouldn't happen, but defensive).
        assert_eq!(mk(-10).duration_display(), "0:00");
    }

    #[test]
    fn slot_with_empty_code_is_still_a_slot() {
        let s = slot("", 0).unwrap();
        assert_eq!(s.code, "");
        assert_eq!(s.character_name(), "Mario");
    }

    fn row(id: i32, ingested: &str, stage: i32, dur: i32, won: Option<bool>) -> ReplayRow {
        ReplayRow {
            game_id: id,
            slots: [None, None, None, None],
            stage_id: stage,
            duration_seconds: dur,
            user_won: won,
            ingested_at: ingested.to_string(),
            played_at: None,
        }
    }

    fn row_played(id: i32, played: Option<&str>) -> ReplayRow {
        ReplayRow {
            game_id: id,
            slots: [None, None, None, None],
            stage_id: 0,
            duration_seconds: 0,
            user_won: None,
            ingested_at: String::new(),
            played_at: played.map(|s| s.to_string()),
        }
    }

    #[test]
    fn sort_by_played_at_desc_is_newest_first_with_undated_last() {
        let mut rows = vec![
            row_played(1, Some("2025-04-01T10:00:00Z")),
            row_played(2, None),
            row_played(3, Some("2025-04-22T08:00:00Z")),
            row_played(4, Some("2025-04-10T12:00:00Z")),
        ];
        sort_rows(&mut rows, SortKey::PlayedAt, SortDirection::Desc);
        let ids: Vec<_> = rows.iter().map(|r| r.game_id).collect();
        // Newest played first; the undated row (2) is pinned last.
        assert_eq!(ids, vec![3, 4, 1, 2]);
    }

    #[test]
    fn sort_by_played_at_asc_keeps_undated_last() {
        let mut rows = vec![
            row_played(1, Some("2025-04-01")),
            row_played(2, None),
            row_played(3, Some("2025-04-22")),
        ];
        sort_rows(&mut rows, SortKey::PlayedAt, SortDirection::Asc);
        let ids: Vec<_> = rows.iter().map(|r| r.game_id).collect();
        assert_eq!(ids, vec![1, 3, 2], "asc: oldest first, undated still last");
    }

    #[test]
    fn played_date_slices_iso_prefix() {
        assert_eq!(
            row_played(1, Some("2025-04-01T14:39:10Z")).played_date(),
            Some("2025-04-01")
        );
        assert_eq!(row_played(1, None).played_date(), None);
        assert_eq!(row_played(1, Some("short")).played_date(), None);
    }

    #[test]
    fn sort_by_ingested_at_desc_is_newest_first() {
        let mut rows = vec![
            row(1, "2026-04-20 10:00:00", 0, 120, Some(true)),
            row(2, "2026-04-22 08:00:00", 0, 200, Some(false)),
            row(3, "2026-04-21 12:00:00", 0, 180, None),
        ];
        sort_rows(&mut rows, SortKey::IngestedAt, SortDirection::Desc);
        let ids: Vec<_> = rows.iter().map(|r| r.game_id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    #[test]
    fn sort_by_game_id_asc_respects_direction() {
        let mut rows = vec![row(3, "", 0, 0, None), row(1, "", 0, 0, None), row(2, "", 0, 0, None)];
        sort_rows(&mut rows, SortKey::GameId, SortDirection::Asc);
        let ids: Vec<_> = rows.iter().map(|r| r.game_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn sort_by_duration_desc_places_longest_first() {
        let mut rows = vec![
            row(1, "", 0, 30, None),
            row(2, "", 0, 400, None),
            row(3, "", 0, 180, None),
        ];
        sort_rows(&mut rows, SortKey::Duration, SortDirection::Desc);
        let ids: Vec<_> = rows.iter().map(|r| r.game_id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    #[test]
    fn sort_by_stage_is_alphabetical() {
        // 0 = Fod-ish (actual id mapping irrelevant, we just need distinct
        // names). Using real stage ids so stage_name() resolves.
        let mut rows = vec![
            row(1, "", 2, 0, None),  // FountainOfDreams
            row(2, "", 8, 0, None),  // YoshisStory
            row(3, "", 28, 0, None), // DreamLandN64
        ];
        sort_rows(&mut rows, SortKey::Stage, SortDirection::Asc);
        let names: Vec<_> = rows.iter().map(|r| r.stage_name()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "stage names should come out alphabetical");
    }

    #[test]
    fn sort_outcome_asc_puts_wins_first_and_unknowns_last() {
        let mut rows = vec![
            row(1, "", 0, 0, Some(false)),
            row(2, "", 0, 0, None),
            row(3, "", 0, 0, Some(true)),
            row(4, "", 0, 0, Some(false)),
        ];
        sort_rows(&mut rows, SortKey::Outcome, SortDirection::Asc);
        let outcomes: Vec<_> = rows.iter().map(|r| r.user_won).collect();
        assert_eq!(
            outcomes,
            vec![Some(true), Some(false), Some(false), None],
            "asc: wins, losses, then unknowns pinned to bottom"
        );
    }

    #[test]
    fn sort_outcome_desc_flips_known_outcomes_but_keeps_unknowns_last() {
        let mut rows = vec![
            row(1, "", 0, 0, Some(false)),
            row(2, "", 0, 0, None),
            row(3, "", 0, 0, Some(true)),
        ];
        sort_rows(&mut rows, SortKey::Outcome, SortDirection::Desc);
        let outcomes: Vec<_> = rows.iter().map(|r| r.user_won).collect();
        assert_eq!(
            outcomes,
            vec![Some(false), Some(true), None],
            "desc: losses before wins, unknowns still last"
        );
    }

    #[test]
    fn sort_is_stable() {
        // Two rows with identical duration — the one that came first in
        // the input should stay first after sorting.
        let mut rows = vec![
            row(10, "", 0, 120, Some(true)),
            row(20, "", 0, 120, Some(false)),
            row(30, "", 0, 60, Some(true)),
        ];
        sort_rows(&mut rows, SortKey::Duration, SortDirection::Asc);
        let ids: Vec<_> = rows.iter().map(|r| r.game_id).collect();
        assert_eq!(ids, vec![30, 10, 20]);
    }

    #[test]
    fn default_direction_favors_useful_ordering() {
        // Changing these is fine, but it affects UX — lock them in a test
        // so a refactor has to update the expectation deliberately.
        assert_eq!(SortKey::IngestedAt.default_direction(), SortDirection::Desc);
        assert_eq!(SortKey::GameId.default_direction(), SortDirection::Desc);
        assert_eq!(SortKey::Duration.default_direction(), SortDirection::Desc);
        assert_eq!(SortKey::Stage.default_direction(), SortDirection::Asc);
        assert_eq!(SortKey::Outcome.default_direction(), SortDirection::Asc);
    }

}
