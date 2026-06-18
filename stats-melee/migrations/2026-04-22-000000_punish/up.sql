-- Per-game punish rows — one row for each "combo opportunity" where an
-- attacker kept the victim in hitstun (with only brief interruptions) until
-- either a reset-to-neutral or a stock loss. Populated by the extractor in
-- `src/punish.rs`; consumed by openings-per-kill, average punish length, and
-- the punish-tree visualization in Track 4.

CREATE TABLE punish (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,

    game_id INTEGER NOT NULL,
    attacker_id INTEGER NOT NULL,
    victim_id INTEGER NOT NULL,

    -- Frame indices (0-based, into peppi's columnar frame data) that bracket
    -- the punish. `end_frame >= start_frame` always; single-hit punishes have
    -- end_frame == start_frame.
    start_frame INTEGER NOT NULL,
    end_frame INTEGER NOT NULL CHECK (end_frame >= start_frame),

    -- Number of discrete hits detected inside the punish. For a 1-hit
    -- conversion (a poke) this is 1; for a combo it's > 1.
    hit_count INTEGER NOT NULL CHECK (hit_count >= 1),

    -- Whether the punish ended with the victim losing a stock.
    did_kill INTEGER NOT NULL CHECK (did_kill IN (0, 1)),

    -- Slippi attack ID of the final hit, when `did_kill = 1`. NULL otherwise
    -- (including kill punishes where peppi couldn't read the attack id).
    kill_move INTEGER,

    FOREIGN KEY (game_id) REFERENCES game(id),
    FOREIGN KEY (attacker_id) REFERENCES gamePlayer(id),
    FOREIGN KEY (victim_id) REFERENCES gamePlayer(id)
);

CREATE INDEX idx_punish_game_id ON punish(game_id);
CREATE INDEX idx_punish_attacker_id ON punish(attacker_id);
CREATE INDEX idx_punish_victim_id ON punish(victim_id);
