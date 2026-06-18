-- SQLite only got DROP COLUMN in 3.35, but we want this down-migration to work
-- on older installs as well. Rebuild the table from scratch.

CREATE TABLE game_player_stat_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,

    game_id INTEGER NOT NULL,
    game_player_id INTEGER NOT NULL,

    placement INTEGER NOT NULL CHECK (placement >= 0 AND placement <= 3),

    stocks_remaining INTEGER CHECK (
        stocks_remaining IS NULL OR
        (stocks_remaining >= 0 AND stocks_remaining <= 255)
    ),

    UNIQUE (game_id, game_player_id),

    FOREIGN KEY (game_id) REFERENCES game(id),
    FOREIGN KEY (game_player_id) REFERENCES gamePlayer(id)
);

INSERT INTO game_player_stat_new (id, game_id, game_player_id, placement, stocks_remaining)
    SELECT id, game_id, game_player_id, placement, stocks_remaining
    FROM game_player_stat;

DROP TABLE game_player_stat;
ALTER TABLE game_player_stat_new RENAME TO game_player_stat;
