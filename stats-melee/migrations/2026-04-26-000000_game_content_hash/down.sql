-- Reverse of up.sql. SQLite 3.35+ supports `DROP COLUMN`, but for
-- portability we use the documented "rebuild table" workaround so
-- callers on older sqlite (e.g. the system sqlite shipped with some
-- macOS versions) can roll back too.

DROP INDEX IF EXISTS idx_game_content_hash;

CREATE TABLE game_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,

    first INTEGER,
    second INTEGER,
    third INTEGER,
    fourth INTEGER,

    stage INTEGER NOT NULL CHECK(stage >= 0 AND stage <= 32),
    time INTEGER NOT NULL CHECK (time >= 0),
    replay_path TEXT,
    ingested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,

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

INSERT INTO game_new (id, first, second, third, fourth, stage, time, replay_path, ingested_at)
SELECT id, first, second, third, fourth, stage, time, replay_path, ingested_at
FROM game;

DROP TABLE game;
ALTER TABLE game_new RENAME TO game;

CREATE UNIQUE INDEX idx_game_replay_path ON game(replay_path);
CREATE INDEX idx_game_ingested_at ON game(ingested_at);
