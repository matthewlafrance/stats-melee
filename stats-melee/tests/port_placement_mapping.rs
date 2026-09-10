//! Port/placement mapping in `post_game_full`, exercised without fixtures.
//!
//! `GameData` is port-indexed while the `game` table is placement-indexed
//! (`first`/`second`/... columns) — `post_game_full` is the single place
//! that crossing happens. The `.slp` corpus is local-only, so the
//! fixture-driven integration tests skip on a clean checkout and leave that
//! translation uncovered. These tests synthesize `GameData` directly so the
//! mapping is verified everywhere, and they deliberately use a game where
//! **port order and placement order disagree** — the case where an
//! off-by-one or a swapped index would otherwise go unnoticed.

use stats_melee::gamedata::{GameData, PortIndex, SlippiPlayer};
use stats_melee::testing::TestDb;
use stats_melee::{get_game_player_code, post_game};

/// Port 2 wins, port 0 loses. Placement order is therefore the reverse of
/// port order, so any code that conflates the two produces visibly wrong
/// results rather than accidentally-correct ones.
fn winner_on_higher_port() -> GameData {
    let mut players: [Option<SlippiPlayer>; 4] = [None, None, None, None];
    players[0] = Some(SlippiPlayer {
        netplay: "Loser".to_string(),
        code: "LOSE#001".to_string(),
        character: 2,
        port: PortIndex::P0,
    });
    players[2] = Some(SlippiPlayer {
        netplay: "Winner".to_string(),
        code: "WIN#002".to_string(),
        character: 9,
        port: PortIndex::P2,
    });

    let mut stocks_remaining = [None; 4];
    stocks_remaining[0] = Some(0); // port 0 was 4-stocked
    stocks_remaining[2] = Some(3);

    let mut starting_stocks = [None; 4];
    starting_stocks[0] = Some(4);
    starting_stocks[2] = Some(4);

    let mut inputs = [None; 4];
    inputs[0] = Some(1200);
    inputs[2] = Some(1800);

    GameData {
        players,
        // rank -> port: winner is on port 2, runner-up on port 0.
        placements: [Some(2), Some(0), None, None],
        stocks_remaining,
        starting_stocks,
        inputs,
        l_cancel_attempts: [None; 4],
        l_cancel_success: [None; 4],
        punishes: Vec::new(),
        advanced: None,
        stage: 8,
        time: 240,
        started_at: None,
    }
}

#[test]
fn game_row_orders_players_by_placement_not_port() {
    let mut db = TestDb::new().expect("test db");
    let gd = winner_on_higher_port();
    let game = post_game(&mut db.conn, &gd).expect("post_game");

    let first = game.first.expect("first place populated");
    let second = game.second.expect("second place populated");

    assert_eq!(
        get_game_player_code(&mut db.conn, first).unwrap(),
        "WIN#002",
        "game.first must hold the winner (port 2), not the lowest port"
    );
    assert_eq!(
        get_game_player_code(&mut db.conn, second).unwrap(),
        "LOSE#001"
    );
    assert!(game.third.is_none());
    assert!(game.fourth.is_none());
}

#[test]
fn stat_rows_pair_each_players_own_stats_with_their_placement() {
    use diesel::prelude::*;
    use stats_melee::schema::{gamePlayer, game_player_stat};

    let mut db = TestDb::new().expect("test db");
    let gd = winner_on_higher_port();
    let game = post_game(&mut db.conn, &gd).expect("post_game");

    // (code, placement, stocks_remaining, inputs) for every stat row.
    let rows: Vec<(String, i32, Option<i32>, Option<i32>)> = game_player_stat::table
        .inner_join(gamePlayer::table.on(gamePlayer::id.eq(game_player_stat::game_player_id)))
        .filter(game_player_stat::game_id.eq(game.id))
        .select((
            gamePlayer::code,
            game_player_stat::placement,
            game_player_stat::stocks_remaining,
            game_player_stat::inputs,
        ))
        .load(&mut db.conn)
        .expect("load stat rows");

    assert_eq!(rows.len(), 2, "one stat row per populated port");

    let winner = rows.iter().find(|r| r.0 == "WIN#002").expect("winner row");
    assert_eq!(winner.1, 0, "winner's placement is 0");
    assert_eq!(winner.2, Some(3), "winner's stocks came from port 2");
    assert_eq!(winner.3, Some(1800), "winner's inputs came from port 2");

    let loser = rows.iter().find(|r| r.0 == "LOSE#001").expect("loser row");
    assert_eq!(loser.1, 1);
    assert_eq!(loser.2, Some(0), "loser's stocks came from port 0");
    assert_eq!(loser.3, Some(1200));
}

#[test]
fn port_is_stored_zero_indexed_on_game_player() {
    use diesel::prelude::*;
    use stats_melee::schema::gamePlayer;

    let mut db = TestDb::new().expect("test db");
    let gd = winner_on_higher_port();
    post_game(&mut db.conn, &gd).expect("post_game");

    let port_for_winner: i32 = gamePlayer::table
        .filter(gamePlayer::code.eq("WIN#002"))
        .select(gamePlayer::port)
        .first(&mut db.conn)
        .expect("winner gamePlayer row");

    assert_eq!(
        port_for_winner, 2,
        "gamePlayer.port is the 0-based index, so peppi's P3 stores as 2"
    );
}

#[test]
fn accessors_agree_on_the_port_placement_relationship() {
    let gd = winner_on_higher_port();

    assert_eq!(gd.winner().map(|p| p.code.as_str()), Some("WIN#002"));
    assert_eq!(
        gd.player_at_placement(1).map(|p| p.code.as_str()),
        Some("LOSE#001")
    );
    assert!(gd.player_at_placement(2).is_none());

    assert_eq!(gd.placement_of_port(2), Some(0));
    assert_eq!(gd.placement_of_port(0), Some(1));
    assert_eq!(gd.placement_of_port(1), None, "empty port has no placement");
}
