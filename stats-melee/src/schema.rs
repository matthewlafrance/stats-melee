// @generated automatically by Diesel CLI.

diesel::table! {
    character (id) {
        id -> Integer,
        name -> Text,
    }
}

diesel::table! {
    game (id) {
        id -> Integer,
        first -> Nullable<Integer>,
        second -> Nullable<Integer>,
        third -> Nullable<Integer>,
        fourth -> Nullable<Integer>,
        stage -> Integer,
        time -> Integer,
        // Canonical filesystem path of the .slp this game was ingested
        // from. UNIQUE-indexed so ingestion is idempotent.
        replay_path -> Nullable<Text>,
        // ISO-8601 UTC timestamp of when the row was inserted. SQLite
        // fills this via DEFAULT CURRENT_TIMESTAMP — Rust code never
        // writes this column explicitly.
        ingested_at -> Text,
        // Hex-encoded SHA-256 of the .slp file's bytes. Cache key for
        // the analysis sidecar (Track 11). Nullable for backward compat
        // with rows ingested before the column existed and for tests
        // that synthesize GameData without a backing file.
        content_hash -> Nullable<Text>,
    }
}

diesel::table! {
    gamePlayer (id) {
        id -> Integer,
        code -> Text,
        character -> Integer,
        port -> Integer,
    }
}

diesel::table! {
    game_player_stat (id) {
        id -> Integer,
        game_id -> Integer,
        game_player_id -> Integer,
        placement -> Integer,
        stocks_remaining -> Nullable<Integer>,
        starting_stocks -> Nullable<Integer>,
        inputs -> Nullable<Integer>,
        l_cancel_attempts -> Nullable<Integer>,
        l_cancel_success -> Nullable<Integer>,
    }
}

diesel::table! {
    player (code) {
        code -> Text,
        netplay -> Text,
    }
}

diesel::table! {
    stage (id) {
        id -> Integer,
        name -> Text,
    }
}

diesel::table! {
    punish (id) {
        id -> Integer,
        game_id -> Integer,
        attacker_id -> Integer,
        victim_id -> Integer,
        start_frame -> Integer,
        end_frame -> Integer,
        hit_count -> Integer,
        did_kill -> Integer,
        kill_move -> Nullable<Integer>,
    }
}

diesel::joinable!(gamePlayer -> player (code));
diesel::joinable!(game_player_stat -> game (game_id));
diesel::joinable!(game_player_stat -> gamePlayer (game_player_id));
diesel::joinable!(punish -> game (game_id));

diesel::allow_tables_to_appear_in_same_query!(
    character,
    game,
    gamePlayer,
    game_player_stat,
    player,
    punish,
    stage,
);
