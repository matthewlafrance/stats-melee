-- Track 2 expansion: derived per-game stats extracted from peppi frame/start data.
--
-- All columns are nullable. Frame-derived metrics occasionally come up empty
-- (corrupt replays, pre-game disconnects, or peppi versions that dropped a
-- field); we'd rather persist the row with NULLs than refuse to ingest.

ALTER TABLE game_player_stat
    ADD COLUMN starting_stocks INTEGER
        CHECK (starting_stocks IS NULL OR (starting_stocks >= 0 AND starting_stocks <= 99));

ALTER TABLE game_player_stat
    ADD COLUMN inputs INTEGER
        CHECK (inputs IS NULL OR inputs >= 0);

ALTER TABLE game_player_stat
    ADD COLUMN l_cancel_attempts INTEGER
        CHECK (l_cancel_attempts IS NULL OR l_cancel_attempts >= 0);

ALTER TABLE game_player_stat
    ADD COLUMN l_cancel_success INTEGER
        CHECK (
            l_cancel_success IS NULL OR
            (l_cancel_success >= 0 AND
                (l_cancel_attempts IS NULL OR l_cancel_success <= l_cancel_attempts))
        );
