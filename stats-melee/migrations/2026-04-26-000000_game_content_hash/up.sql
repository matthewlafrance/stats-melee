-- Cache key for the analysis sidecar (Track 11). Hex-encoded SHA-256
-- of the .slp file's bytes — content-addressed so renaming or moving
-- the replay folder doesn't invalidate the cache.
--
-- Nullable for two reasons:
--   1. Rows ingested before this migration ran exist with no hash.
--      The viewer's load path checks for None and falls through to
--      the slow re-parse-from-disk pathway, which is just a UX
--      regression for those rows, not a correctness issue.
--   2. Tests that bypass the file-walking ingestion path (calling
--      `post_game` directly with a synthesized `GameData`) have no
--      .slp file to hash.
--
-- Adding a nullable column with no default expression is the one
-- ALTER TABLE ADD COLUMN form SQLite allows without the rebuild
-- dance, so we use it here instead of the table-recreate workaround
-- the ingested_at migration needed.
ALTER TABLE game ADD COLUMN content_hash TEXT;

-- Non-unique on purpose. Two ingests of the same .slp at different
-- paths (user copied the file) would collide on content_hash; the
-- UNIQUE-on-replay_path index already handles within-corpus dedup,
-- and the cache is content-addressed so duplicate hashes are cheap
-- (both paths share the same sidecar). Promote to UNIQUE if we ever
-- want to teach ingestion to merge same-content rows.
CREATE INDEX idx_game_content_hash ON game(content_hash);
