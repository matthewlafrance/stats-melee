-- Add a stable-identity column to `game` so ingestion can dedupe across
-- directory switches. Motivation:
--   - User ingests from /dir_A, then later switches replay_dir to /dir_B
--     containing older replays. The old file-mtime heuristic in
--     parse_new_replays would skip /dir_B entirely because its files are
--     older than the .db file. With this column we drop the mtime dance
--     and use the file path as the dedup key instead.
--   - The same replay should never land in the game table twice.
--
-- Nullable + UNIQUE is fine on SQLite: "all NULL values are considered
-- different from all other values" per the UNIQUE-index docs, so legacy
-- rows with NULL replay_path coexist happily after the ALTER.

ALTER TABLE game ADD COLUMN replay_path TEXT;

CREATE UNIQUE INDEX idx_game_replay_path ON game(replay_path);
