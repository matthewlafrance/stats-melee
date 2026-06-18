-- Requires SQLite 3.35+ for `ALTER TABLE ... DROP COLUMN`. That's been
-- the baseline since stats-melee started, and the other migrations in
-- this tree already rely on features introduced around 3.35 (generated
-- columns, returning clauses, etc.), so no reason to emulate via
-- table-rebuild here.

DROP INDEX IF EXISTS idx_game_replay_path;

ALTER TABLE game DROP COLUMN replay_path;
