-- Indexes for the two columns every stats query joins through.
--
-- `game_player_stat` is the hub of the read path: essentially every
-- filterable statistic joins gamePlayer -> game_player_stat -> game, and the
-- per-game lookup (`get_stats_for_game`) filters on game_id directly.
-- Without these, both are full table scans.
CREATE INDEX IF NOT EXISTS idx_game_player_stat_game_id
    ON game_player_stat(game_id);

CREATE INDEX IF NOT EXISTS idx_game_player_stat_game_player_id
    ON game_player_stat(game_player_id);
