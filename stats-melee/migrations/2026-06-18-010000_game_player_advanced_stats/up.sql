-- Advanced per-game combat metrics, one set per (game, player), computed in a
-- single frame walk at ingest (see `src/advanced.rs`). These are raw counters;
-- the UI / aggregate queries turn them into ratios (damage per opening,
-- neutral-win %, stage control %, edge-guard %, first-blood win %, comeback
-- rate, average death %).
--
-- All nullable: legacy rows ingested before this column existed stay NULL
-- until re-ingested, and non-1v1 games (the advanced extractor is 1v1-only)
-- store NULL. Plain nullable ADD COLUMNs (constant NULL default), so no table
-- rebuild needed.
ALTER TABLE game_player_stat ADD COLUMN damage_dealt REAL;
ALTER TABLE game_player_stat ADD COLUMN openings INTEGER;
ALTER TABLE game_player_stat ADD COLUMN neutral_wins INTEGER;
ALTER TABLE game_player_stat ADD COLUMN adv_frames INTEGER;
ALTER TABLE game_player_stat ADD COLUMN edgeguard_attempts INTEGER;
ALTER TABLE game_player_stat ADD COLUMN edgeguard_kills INTEGER;
ALTER TABLE game_player_stat ADD COLUMN first_blood INTEGER;
ALTER TABLE game_player_stat ADD COLUMN deaths INTEGER;
ALTER TABLE game_player_stat ADD COLUMN death_percent_sum REAL;
ALTER TABLE game_player_stat ADD COLUMN comeback_win INTEGER;
