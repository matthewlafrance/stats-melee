//! Replays that predate the modern GameEnd block.
//!
//! Slippi replay spec 3.13 added player placements to GameEnd. Anything
//! recorded before it — essentially every public tournament archive — has a
//! GameEnd carrying only the end method, and console recordings additionally
//! write `"players": {}` into the metadata, naming nobody at all. Parsing
//! those means reconstructing both the finishing order and the players from
//! frame data and the Game Start block, which is what these tests exercise.
//!
//! The corpus test is opt-in, since the replays are large and local:
//!
//! ```sh
//! STATS_MELEE_LEGACY_CORPUS=~/melee-corpus \
//!   cargo test --release -p stats-melee --test legacy_replays -- --nocapture
//! ```

use std::collections::HashMap;


use stats_melee::gamedata::{
    external_to_internal_character, internal_to_external_character, CHARACTERS,
    INTERNAL_TO_EXTERNAL_CHARACTER,
};
use stats_melee::parse_single_replay;
use stats_melee::testing::{corpus_from_env, slps_recursive};

/// The two id spaces must be exact inverses over the playable roster.
///
/// Ice Climbers is the one asymmetry: internal 10 (Popo) and 11 (Nana) both
/// map out to external 14, so external 14 comes back as Popo. Game Start
/// names the character the player controls, which is Popo, so that is the
/// correct direction to collapse.
#[test]
fn character_id_spaces_round_trip() {
    for internal in 0..INTERNAL_TO_EXTERNAL_CHARACTER.len() as i32 {
        let external = internal_to_external_character(internal)
            .unwrap_or_else(|| panic!("internal {internal} has no external id"));
        let back = external_to_internal_character(external)
            .unwrap_or_else(|| panic!("external {external} has no internal id"));
        if internal == 11 {
            assert_eq!(back, 10, "external 14 should resolve to Popo, not Nana");
        } else {
            assert_eq!(
                back, internal,
                "{} (internal {internal}) round-tripped to {}",
                CHARACTERS[internal as usize], CHARACTERS[back as usize]
            );
        }
    }
}

/// Names as they appear in the public dataset's filenames, mapped to this
/// crate's internal character ids.
fn name_to_internal(name: &str) -> Option<i32> {
    let canonical = match name.trim() {
        "Bowser" => "Bowser",
        "Captain Falcon" | "Falcon" | "CaptainFalcon" => "CaptainFalcon",
        "Donkey Kong" | "DK" => "DonkeyKong",
        "Dr. Mario" | "Dr Mario" | "Doc" => "DrMario",
        "Falco" => "Falco",
        "Fox" => "Fox",
        "Mr. Game & Watch" | "Game & Watch" | "GameAndWatch" | "G&W" => "GameAndWatch",
        "Ganondorf" | "Ganon" => "Ganondorf",
        "Ice Climbers" | "IceClimbers" | "ICs" => "Popo",
        "Jigglypuff" | "Puff" => "Jigglypuff",
        "Kirby" => "Kirby",
        "Link" => "Link",
        "Luigi" => "Luigi",
        "Mario" => "Mario",
        "Marth" => "Marth",
        "Mewtwo" => "Mewtwo",
        "Ness" => "Ness",
        "Peach" => "Peach",
        "Pichu" => "Pichu",
        "Pikachu" => "Pikachu",
        "Roy" => "Roy",
        "Samus" => "Samus",
        "Sheik" => "Sheik",
        "Young Link" | "YoungLink" | "YLink" => "YoungLink",
        "Yoshi" => "Yoshi",
        "Zelda" => "Zelda",
        _ => return None,
    };
    CHARACTERS.iter().position(|c| *c == canonical).map(|i| i as i32)
}

/// Pull the declared matchup out of a filename, for the conventions that
/// carry one. Returns `None` for default-named `Game_*.slp` files.
fn matchup_from_name(filename: &str) -> Option<(i32, i32)> {
    let stem = filename.strip_suffix(".slp")?;

    // "10_23_35 Bowser + Peach (BF)"
    if let Some(rest) = stem.get(9..) {
        if stem.as_bytes().get(2) == Some(&b'_') && stem.as_bytes().get(5) == Some(&b'_') {
            let body = rest.rsplit_once(" (")?.0;
            if let Some((a, b)) = body.split_once(" + ") {
                // Tags like "[THRN] Kirby" prefix the character name.
                let strip = |s: &str| s.rsplit(']').next().unwrap_or(s).trim().to_string();
                return Some((name_to_internal(&strip(a))?, name_to_internal(&strip(b))?));
            }
        }
    }

    // "20200101 - HNC 4 - PM 0737 - Marth (Default) vs Bowser (Red) - Battlefield"
    if let Some((_, tail)) = stem.split_once(" - PM ").or_else(|| stem.split_once(" - AM ")) {
        let body = tail.split_once(" - ")?.1;
        let body = body.rsplit_once(" - ")?.0;
        if let Some((a, b)) = body.split_once(" vs ") {
            let strip = |s: &str| s.split_once(" (").map(|x| x.0).unwrap_or(s).trim().to_string();
            return Some((name_to_internal(&strip(a))?, name_to_internal(&strip(b))?));
        }
    }

    // "Peach vs Falcon [FoD] Game_001DBC7CDA54_20200307T172646"
    if let Some((body, _)) = stem.split_once(" [") {
        if let Some((a, b)) = body.split_once(" vs ") {
            return Some((name_to_internal(a.trim())?, name_to_internal(b.trim())?));
        }
    }

    None
}

/// Parse a slice of the legacy corpus and check what came back.
///
/// Two independent things are under test. That the replays parse at all —
/// they exercise the Game-Start fallbacks for both placements and player
/// identity. And that the characters are *right*: for every filename that
/// declares its matchup, the parsed pair must match it. That second check is
/// what would catch an internal/external id mix-up, which otherwise produces
/// a database full of confidently mislabelled fighters.
#[test]
fn legacy_corpus_parses_with_correct_characters() {
    let Some(root) = corpus_from_env("STATS_MELEE_LEGACY_CORPUS") else {
        eprintln!("legacy_corpus: skipped — set STATS_MELEE_LEGACY_CORPUS to run");
        return;
    };
    let mut files = slps_recursive(&root);
    files.truncate(400);
    assert!(!files.is_empty(), "no .slp files under {}", root.display());

    let (mut parsed, mut failed, mut checked, mut anonymous) = (0, 0, 0, 0);
    let mut with_placements = 0;
    let (mut ports_with_player, mut ports_with_stocks) = (0, 0);
    let mut errors: HashMap<String, usize> = HashMap::new();

    for path in &files {
        let gd = match parse_single_replay(path) {
            Ok(gd) => gd,
            Err(e) => {
                failed += 1;
                *errors.entry(e.to_string()).or_default() += 1;
                continue;
            }
        };
        parsed += 1;

        if gd.placements().iter().any(|p| p.is_some()) {
            with_placements += 1;
        }
        for p in gd.players().iter().flatten() {
            if p.is_anonymous() {
                anonymous += 1;
            }
        }

        // Frame-derived stats must land for every port that has a player.
        // `frames.ports` is dense over occupied ports, so indexing it with a
        // port *number* silently yields nothing whenever the players sit
        // anywhere but the lowest ports — common on console, never on
        // netplay. The failure surfaces here as missing stocks, and
        // downstream as missing placements.
        for (idx, player) in gd.players().iter().enumerate() {
            if player.is_some() {
                ports_with_player += 1;
                if gd.stocks_remaining[idx].is_some() {
                    ports_with_stocks += 1;
                }
            }
        }

        let name = path.file_name().unwrap().to_string_lossy();
        let Some((want_a, want_b)) = matchup_from_name(&name) else {
            continue;
        };
        let mut got: Vec<i32> = gd.players().iter().flatten().map(|p| p.character).collect();
        got.sort_unstable();
        let mut want = vec![want_a, want_b];
        want.sort_unstable();
        assert_eq!(
            got,
            want,
            "{name}: parsed {:?} but the filename says {:?}",
            got.iter().map(|c| CHARACTERS[*c as usize]).collect::<Vec<_>>(),
            want.iter().map(|c| CHARACTERS[*c as usize]).collect::<Vec<_>>(),
        );
        checked += 1;
    }

    eprintln!(
        "legacy_corpus: {parsed}/{} parsed, {failed} failed, {with_placements} with placements, \
         {anonymous} anonymous players, {checked} matchups verified against filenames",
        files.len()
    );
    for (err, n) in &errors {
        eprintln!("    {n:>4}x {err}");
    }

    eprintln!(
        "legacy_corpus: {ports_with_stocks}/{ports_with_player} occupied ports have \
         frame-derived stocks"
    );

    assert!(parsed > 0, "no legacy replay parsed");
    // Allow a small tail of genuinely truncated replays, but a port-slot
    // regression would drop this to roughly half.
    assert!(
        ports_with_stocks * 100 >= ports_with_player * 95,
        "only {ports_with_stocks}/{ports_with_player} occupied ports had stocks — \
         frames.ports is probably being indexed by port number again"
    );
    assert!(
        checked >= 20,
        "only {checked} filenames declared a matchup — too few to trust the id mapping"
    );
}

