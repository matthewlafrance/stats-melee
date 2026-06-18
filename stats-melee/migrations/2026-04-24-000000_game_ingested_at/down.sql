-- Mirror image of up.sql: rebuild the table without `ingested_at`.
-- SQLite 3.35+ technically supports `ALTER TABLE ... DROP COLUMN`, but
-- the rebuild keeps the behavior identical across SQLite builds and
-- avoids any surprises with indexes referencing the dropped column
-- in older versions.

CREATE TABLE game_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,

    first INTEGER,
    second INTEGER,
    third INTEGER,
    fourth INTEGER,

    stage INTEGER NOT NULL CHECK(stage >= 0 AND stage <= 32),

    time INTEGER NOT NULL CHECK (time >= 0),

    replay_path TEXT,

    CHECK (
        first IS NOT NULL OR
        second IS NOT NULL OR
        third IS NOT NULL OR
        fourth IS NOT NULL
    ),

    FOREIGN KEY (first) REFERENCES gamePlayer(id),
    FOREIGN KEY (second) REFERENCES gamePlayer(id),
    FOREIGN KEY (third) REFERENCES gamePlayer(id),
    FOREIGN KEY (fourth) REFERENCES gamePlayer(id)
);

INSERT INTO game_new (id, first, second, third, fourth, stage, time, replay_path)
SELECT id, first, second, third, fourth, stage, time, replay_path
FROM game;

DROP TABLE game;
ALTER TABLE game_new RENAME TO game;

CREATE UNIQUE INDEX idx_game_replay_path ON game(replay_path);
