-- One row per applied `bpm untrack`. It is an audit record, not an undo log:
-- the rows an untrack deletes are gone. `command` is the argument vector as a
-- JSON array, as in `link_runs`.

CREATE TABLE untrack_runs (
    id TEXT PRIMARY KEY,
    command TEXT NOT NULL,
    applied_at TEXT NOT NULL,
    locations_removed INTEGER NOT NULL,
    files_removed INTEGER NOT NULL,
    links_removed INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
