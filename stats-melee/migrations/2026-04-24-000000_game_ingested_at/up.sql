-- Record when each game row was inserted. Used by the app's replay
-- list to power sort-by-date, "ingested today" digests, and anything
-- else that cares about chronology.
--
-- SQLite forbids `CURRENT_TIMESTAMP` (and any expression default) on
-- ALTER TABLE ADD COLUMN — "Cannot add a column with non-constant
-- default". Only NULL / literal constants are permitted there. So we
-- use the canonical "rebuild the table" workaround documented at
-- https://www.sqlite.org/lang_altertable.html#otheralter : CREATE a
-- new table with the desired schema, copy rows across, DROP the old
-- one, rename, recreate indexes.
--
-- The project doesn't enable FK enforcement anywhere (`PRAGMA
-- foreign_keys` is never set), so the DROP + rebuild is safe without
-- any FK-pragma gymnastics. If enforcement ever gets turned on, add
-- `PRAGMA defer_foreign_keys = ON;` at the top of this migration —
-- that one *does* work inside a transaction, unlike `foreign_keys`.

CREATE TABLE game_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,

    first INTEGER,
    second INTEGER,
    third INTEGER,
    fourth INTEGER,

    stage INTEGER NOT NULL CHECK(stage >= 0 AND stage <= 32),

    time INTEGER NOT NULL CHECK (time >= 0),

    replay_path TEXT,

    -- ISO-8601 'YYYY-MM-DD HH:MM:SS' UTC timestamp. DEFAULT fires for
    -- inserts that omit the column (the common path through diesel's
    -- NewGame insertable, which deliberately doesn't name it).
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

-- Backfill: every existing row gets "now" as its ingested_at. Using
-- datetime('now') directly rather than relying on the DEFAULT because
-- this INSERT names the column, so the DEFAULT wouldn't fire.
INSERT INTO game_new (id, first, second, third, fourth, stage, time, replay_path, ingested_at)
SELECT id, first, second, third, fourth, stage, time, replay_path, datetime('now')
FROM game;

DROP TABLE game;
ALTER TABLE game_new RENAME TO game;

-- Indexes that lived on the old table are dropped by `DROP TABLE`;
-- recreate them against the renamed table.
CREATE UNIQUE INDEX idx_game_replay_path ON game(replay_path);
CREATE INDEX idx_game_ingested_at ON game(ingested_at);
