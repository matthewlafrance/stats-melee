//! Pure value-to-string helpers: stat formatting, id labels, and the civil
//! date arithmetic behind the library's date filters.
//!
//! Nothing here touches egui or the app state, which is exactly why the date
//! conversions are directly unit-testable — see the tests at the bottom.

use stats_melee::gamedata::{spaced_name, CHARACTERS, STAGES};

use crate::replay_list::SortKey;

/// Render `Option<f64>` with `decimals` precision, falling back to "—".
pub(super) fn fmt_opt_f64(v: Option<f64>, decimals: usize) -> String {
    match v {
        // Named-arg precision (`.prec$`) plays nicely with Rust's captured
        // format args — `.*` would require a positional `decimals` arg and
        // conflict with the captured `x`.
        Some(x) => format!("{x:.decimals$}"),
        None => "—".to_string(),
    }
}

/// Render `Option<f64>` in `[0.0, 1.0]` as `NN.N%`, or "—" when `None`.
pub(super) fn fmt_opt_percent(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{:.1}%", x * 100.0),
        None => "—".to_string(),
    }
}

/// Render an `Option<f64>` that's already a melee damage percent (0..200+),
/// e.g. an average death percent, as `NN%` — *not* scaled by 100 like a
/// `[0,1]` ratio.
pub(super) fn fmt_opt_death_percent(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.0}%"),
        None => "—".to_string(),
    }
}

/// Format a total duration in seconds as a compact `Hh Mm` read-out
/// ("12h 34m"), dropping the hours below an hour ("45m") and the minutes
/// when exactly zero ("3h"). Sub-minute totals round up to "<1m" so a
/// non-empty history never reads "0m".
pub(super) fn fmt_playtime(total_seconds: i64) -> String {
    let total = total_seconds.max(0);
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    if hours == 0 && minutes == 0 {
        return if total > 0 { "<1m".to_string() } else { "0m".to_string() };
    }
    match (hours, minutes) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// Display label for the Analytics character selector. `None` → "Any";
/// otherwise looks up the id in [`CHARACTERS`] and falls back to a literal
/// "char #N" so an out-of-range id never panics the UI.
pub(super) fn character_label(id: Option<i32>) -> String {
    match id {
        None => "Any".to_string(),
        Some(c) => CHARACTERS
            .get(c as usize)
            .map(|s| spaced_name(s))
            .unwrap_or_else(|| format!("char #{c}")),
    }
}

/// Display label for the Analytics stage selector. Mirrors
/// [`character_label`] for the stage table.
pub(super) fn stage_label(id: Option<i32>) -> String {
    match id {
        None => "Any".to_string(),
        Some(s) => STAGES
            .get(s as usize)
            .map(|name| spaced_name(name))
            .unwrap_or_else(|| format!("stage #{s}")),
    }
}

/// Display label for a [`SortKey`] in the Replay Library sort dropdown.
/// "Newest" reads better than "Ingested at" for the default chronological
/// ordering.
pub(super) fn sort_key_label(key: SortKey) -> &'static str {
    match key {
        SortKey::IngestedAt => "Newest (added)",
        SortKey::PlayedAt => "Date played",
        SortKey::GameId => "Game ID",
        SortKey::Stage => "Stage",
        SortKey::Duration => "Duration",
        SortKey::Outcome => "Outcome",
    }
}

/// Parse the leading `YYYY-MM-DD` of an ISO date/time string into `(y, m, d)`.
pub(super) fn parse_iso_ymd(s: &str) -> Option<(i64, i64, i64)> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    if (1..=12).contains(&m) && (1..=31).contains(&d) {
        Some((y, m, d))
    } else {
        None
    }
}

/// Days since 1970-01-01 for a proleptic-Gregorian `(y, m, d)`.
pub(super) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: day number → `(y, m, d)`.
pub(super) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DD` prefix → day ordinal, or `None` if it doesn't parse.
pub(super) fn date_to_ordinal(s: &str) -> Option<i64> {
    let (y, m, d) = parse_iso_ymd(s)?;
    Some(days_from_civil(y, m, d))
}

/// Day ordinal → `YYYY-MM-DD`.
pub(super) fn ordinal_to_date(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Inclusive min/max of a list of day ordinals, or `None` when empty. Used to
/// derive a date filter's slider domain from its per-game ordinal list.
pub(super) fn ordinal_domain(ordinals: &[i64]) -> Option<(i64, i64)> {
    let mut it = ordinals.iter().copied();
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), v| (lo.min(v), hi.max(v))))
}

/// Reformat free-form date input to a `YYYY-MM-DD` mask: keep up to 8 leading
/// digits and group them year(4)-month(2)-day(2). Forgiving of any separator
/// style or partial entry — `"20250401"`, `"2025/4/1"`, or mid-typing — so
/// the user never has to type the dashes themselves.
pub(super) fn autoformat_ymd(s: &str) -> String {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
    let mut out = String::with_capacity(10);
    for (i, c) in digits.chars().enumerate() {
        if i == 4 || i == 6 {
            out.push('-');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn parse_iso_ymd_accepts_date_and_datetime() {
        assert_eq!(parse_iso_ymd("2025-04-01"), Some((2025, 4, 1)));
        assert_eq!(parse_iso_ymd("2025-04-01T14:39:10Z"), Some((2025, 4, 1)));
        assert_eq!(parse_iso_ymd("2025-12-31 23:59:59"), Some((2025, 12, 31)));
        // Bad shapes / out-of-range.
        assert_eq!(parse_iso_ymd("2025/04/01"), None);
        assert_eq!(parse_iso_ymd("2025-13-01"), None);
        assert_eq!(parse_iso_ymd("2025-04-00"), None);
        assert_eq!(parse_iso_ymd("nope"), None);
    }

    #[test]
    fn ordinal_round_trips_through_date_string() {
        for s in ["1970-01-01", "2000-02-29", "2024-02-29", "2025-04-01", "2099-12-31"] {
            let ord = date_to_ordinal(s).expect("parse");
            assert_eq!(ordinal_to_date(ord), s, "round-trip failed for {s}");
        }
    }

    #[test]
    fn ordinal_is_monotonic_and_day_steps_are_one() {
        let a = date_to_ordinal("2025-04-01").unwrap();
        let b = date_to_ordinal("2025-04-02").unwrap();
        let c = date_to_ordinal("2025-05-01").unwrap();
        assert_eq!(b - a, 1, "consecutive days differ by 1");
        assert_eq!(c - a, 30, "April has 30 days");
        assert!(a < c, "later dates have larger ordinals");
    }

    #[test]
    fn epoch_is_zero() {
        assert_eq!(date_to_ordinal("1970-01-01"), Some(0));
    }

    #[test]
    fn fmt_playtime_reads_compactly() {
        assert_eq!(fmt_playtime(0), "0m");
        assert_eq!(fmt_playtime(30), "<1m"); // non-empty but sub-minute
        assert_eq!(fmt_playtime(60), "1m");
        assert_eq!(fmt_playtime(45 * 60), "45m");
        assert_eq!(fmt_playtime(3600), "1h");
        assert_eq!(fmt_playtime(3 * 3600), "3h"); // exact hours drop minutes
        assert_eq!(fmt_playtime(12 * 3600 + 34 * 60), "12h 34m");
        assert_eq!(fmt_playtime(-5), "0m"); // negative clamps
    }

    #[test]
    fn autoformat_ymd_masks_progressively_and_forgives_separators() {
        // Progressive masking as the user types digits.
        assert_eq!(autoformat_ymd(""), "");
        assert_eq!(autoformat_ymd("2025"), "2025");
        assert_eq!(autoformat_ymd("20250"), "2025-0");
        assert_eq!(autoformat_ymd("202504"), "2025-04");
        assert_eq!(autoformat_ymd("2025040"), "2025-04-0");
        assert_eq!(autoformat_ymd("20250401"), "2025-04-01");
        // Any separator style collapses to the canonical mask (the mask
        // packs digits positionally, so months/days must be zero-padded).
        assert_eq!(autoformat_ymd("2025/04/01"), "2025-04-01");
        assert_eq!(autoformat_ymd("2025.04.01"), "2025-04-01");
        assert_eq!(autoformat_ymd("2025-04-01"), "2025-04-01");
        // Excess digits past the day are dropped, junk is ignored.
        assert_eq!(autoformat_ymd("2025040199"), "2025-04-01");
        assert_eq!(autoformat_ymd("abc2025"), "2025");
        // The masked output is idempotent (re-running doesn't drift).
        let once = autoformat_ymd("2025-04-01");
        assert_eq!(autoformat_ymd(&once), once);
    }

    #[test]
    fn ordinal_domain_spans_min_to_max() {
        assert_eq!(ordinal_domain(&[]), None);
        assert_eq!(ordinal_domain(&[5]), Some((5, 5)));
        assert_eq!(ordinal_domain(&[5, 1, 9, 3]), Some((1, 9)));
        assert_eq!(ordinal_domain(&[-3, -10, 0]), Some((-10, 0)));
    }
}

