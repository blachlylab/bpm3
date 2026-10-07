-- Bulk link runs, undo, and the known-role list.
--
-- A `bpm link` run tags what it creates with its run id: the link rows, and the
-- Case, Sample, Raw Data, and Analysis rows. Programs and projects are never
-- created by a link. `bpm undo RUN` removes exactly those rows. A run that
-- changes the role of an existing link records the old and new role, so undo
-- can put the old one back. `run_id` is a tag, not a foreign key: rows written
-- before this migration, or by `bpm create`, have none.

ALTER TABLE file_links ADD COLUMN run_id TEXT;
ALTER TABLE cases ADD COLUMN run_id TEXT;
ALTER TABLE samples ADD COLUMN run_id TEXT;
ALTER TABLE raw_data ADD COLUMN run_id TEXT;
ALTER TABLE analyses ADD COLUMN run_id TEXT;

CREATE INDEX file_links_run ON file_links (run_id) WHERE run_id IS NOT NULL;
CREATE INDEX cases_run ON cases (run_id) WHERE run_id IS NOT NULL;
CREATE INDEX samples_run ON samples (run_id) WHERE run_id IS NOT NULL;
CREATE INDEX raw_data_run ON raw_data (run_id) WHERE run_id IS NOT NULL;
CREATE INDEX analyses_run ON analyses (run_id) WHERE run_id IS NOT NULL;

-- `command` is the argument vector as a JSON array. Every row a run writes
-- carries `applied_at` as its timestamp, which is how undo tells the metadata
-- the run wrote from a later `bpm meta set`.
CREATE TABLE link_runs (
    id TEXT PRIMARY KEY,
    command TEXT NOT NULL,
    applied_at TEXT NOT NULL,
    entities_created INTEGER NOT NULL,
    links_created INTEGER NOT NULL,
    roles_changed INTEGER NOT NULL,
    undone_at TEXT
) STRICT, WITHOUT ROWID;

CREATE TABLE link_role_changes (
    run_id TEXT NOT NULL REFERENCES link_runs (id) ON DELETE RESTRICT,
    file_id TEXT NOT NULL,
    node_type TEXT NOT NULL,
    node_id TEXT NOT NULL,
    old_role TEXT NOT NULL,
    new_role TEXT NOT NULL,
    PRIMARY KEY (run_id, file_id, node_type, node_id)
) STRICT, WITHOUT ROWID;

-- Roles stay an open vocabulary. A role not on this list needs `--new-role`
-- once, which adds it, so a typo does not quietly start a second spelling.
CREATE TABLE link_roles (
    role TEXT PRIMARY KEY,
    added_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;

INSERT INTO link_roles (role, added_at)
SELECT role, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
FROM (
    SELECT 'data' AS role UNION ALL SELECT 'index' UNION ALL SELECT 'document'
    UNION ALL SELECT 'report' UNION ALL SELECT 'qc' UNION ALL SELECT 'checksum'
    UNION SELECT DISTINCT role FROM file_links
);
