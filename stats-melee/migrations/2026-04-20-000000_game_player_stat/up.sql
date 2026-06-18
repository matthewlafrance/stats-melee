-- Per-game, per-player derived stats (placement, stocks remaining, etc).
-- One row per (game, gamePlayer) pair.
CREATE TABLE game_player_stat (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,

    game_id INTEGER NOT NULL,
    game_player_id INTEGER NOT NULL,

    placement INTEGER NOT NULL CHECK (placement >= 0 AND placement <= 3),

    -- Stocks remaining at the last recorded frame. Nullable because frame data
    -- may be incomplete (corrupt file, very short DC) — we'd rather record the
    -- row with stats_remaining=NULL than drop the stat entirely.
    stocks_remaining INTEGER CHECK (
        stocks_remaining IS NULL OR
        (stocks_remaining >= 0 AND stocks_remaining <= 255)
    ),

    UNIQUE (game_id, game_player_id),

    FOREIGN KEY (game_id) REFERENCES game(id),
    FOREIGN KEY (game_player_id) REFERENCES gamePlayer(id)
);
