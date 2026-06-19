-- DROP COLUMN is supported on the SQLite bundled by libsqlite3-sys
-- (>= 3.35). No index references this column, so the drop is clean.
ALTER TABLE game DROP COLUMN started_at;
