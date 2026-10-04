-- Every table is STRICT. Tables keyed by text, or by more than one column, are
-- WITHOUT ROWID so the primary key is the table's own B-tree. UUIDs are canonical
-- lowercase text. Timestamps are ISO 8601 UTC text with a Z suffix. Each node
-- table's parent column references the table one step up, ON DELETE RESTRICT.
-- (node_type, node_id) names one of six tables, so it is not a foreign key; the
-- library checks it.

CREATE TABLE catalog_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE programs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    program_id TEXT NOT NULL REFERENCES programs (id) ON DELETE RESTRICT,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (program_id, name)
) STRICT, WITHOUT ROWID;

CREATE TABLE cases (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE samples (
    id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL REFERENCES cases (id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE raw_data (
    id TEXT PRIMARY KEY,
    sample_id TEXT NOT NULL REFERENCES samples (id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE analyses (
    id TEXT PRIMARY KEY,
    raw_data_id TEXT NOT NULL REFERENCES raw_data (id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE INDEX projects_program ON projects (program_id);
CREATE INDEX cases_project ON cases (project_id);
CREATE INDEX samples_case ON samples (case_id);
CREATE INDEX raw_data_sample ON raw_data (sample_id);
CREATE INDEX analyses_raw_data ON analyses (raw_data_id);

CREATE VIEW entities AS
SELECT 'program' AS node_type, id, CAST(NULL AS TEXT) AS parent_id, name, created_at, updated_at FROM programs
UNION ALL
SELECT 'project', id, program_id, name, created_at, updated_at FROM projects
UNION ALL
SELECT 'case', id, project_id, CAST(NULL AS TEXT), created_at, updated_at FROM cases
UNION ALL
SELECT 'sample', id, case_id, CAST(NULL AS TEXT), created_at, updated_at FROM samples
UNION ALL
SELECT 'raw_data', id, sample_id, CAST(NULL AS TEXT), created_at, updated_at FROM raw_data
UNION ALL
SELECT 'analysis', id, raw_data_id, CAST(NULL AS TEXT), created_at, updated_at FROM analyses;

CREATE TABLE entity_metadata (
    node_type TEXT NOT NULL,
    node_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (node_type, node_id, key),
    CHECK (node_type IN ('program', 'project', 'case', 'sample', 'raw_data', 'analysis')),
    CHECK (key <> ''),
    CHECK (value <> '')
) STRICT, WITHOUT ROWID;

CREATE INDEX entity_metadata_kv ON entity_metadata (key, value);

CREATE TABLE files (
    id TEXT PRIMARY KEY,
    size_bytes INTEGER,
    mtime TEXT,
    fingerprint TEXT,
    fingerprint_scheme TEXT,
    created_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE file_metadata (
    file_id TEXT NOT NULL REFERENCES files (id) ON DELETE RESTRICT,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (file_id, key),
    CHECK (key <> ''),
    CHECK (value <> '')
) STRICT, WITHOUT ROWID;

CREATE INDEX file_metadata_kv ON file_metadata (key, value);

CREATE TABLE file_digests (
    file_id TEXT NOT NULL REFERENCES files (id) ON DELETE RESTRICT,
    algorithm TEXT NOT NULL,
    digest TEXT NOT NULL,
    source TEXT NOT NULL,
    generation INTEGER NOT NULL,
    current INTEGER NOT NULL,
    observed_at TEXT NOT NULL,
    PRIMARY KEY (file_id, algorithm, generation),
    CHECK (source IN ('computed', 'backend')),
    CHECK (current IN (0, 1))
) STRICT, WITHOUT ROWID;

CREATE TABLE file_locations (
    file_id TEXT NOT NULL REFERENCES files (id) ON DELETE RESTRICT,
    backend TEXT NOT NULL,
    uri TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    last_size INTEGER,
    last_mtime TEXT,
    presence TEXT NOT NULL,
    stat_state TEXT NOT NULL,
    digest_state TEXT NOT NULL,
    PRIMARY KEY (backend, uri),
    CHECK (presence IN ('present', 'missing')),
    CHECK (stat_state IN ('unchanged', 'changed', 'unknown')),
    CHECK (digest_state IN ('unverified', 'match', 'mismatch'))
) STRICT, WITHOUT ROWID;

CREATE INDEX file_locations_file ON file_locations (file_id);

CREATE TABLE file_links (
    file_id TEXT NOT NULL REFERENCES files (id) ON DELETE RESTRICT,
    node_type TEXT NOT NULL,
    node_id TEXT NOT NULL,
    role TEXT NOT NULL,
    PRIMARY KEY (file_id, node_type, node_id),
    CHECK (node_type IN ('program', 'project', 'case', 'sample', 'raw_data', 'analysis'))
) STRICT, WITHOUT ROWID;

CREATE INDEX file_links_node ON file_links (node_type, node_id);

CREATE TABLE ingest_runs (
    id TEXT PRIMARY KEY,
    backend TEXT NOT NULL,
    root_uri TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL,
    files_seen INTEGER NOT NULL,
    files_created INTEGER NOT NULL,
    CHECK (status IN ('running', 'complete', 'incomplete'))
) STRICT, WITHOUT ROWID;

CREATE TABLE ingest_errors (
    run_id TEXT NOT NULL REFERENCES ingest_runs (id) ON DELETE RESTRICT,
    uri TEXT NOT NULL,
    message TEXT NOT NULL
) STRICT;

CREATE INDEX ingest_errors_run ON ingest_errors (run_id);

CREATE TABLE scan_runs (
    id TEXT PRIMARY KEY,
    backend TEXT NOT NULL,
    root_uri TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL,
    files_seen INTEGER NOT NULL,
    CHECK (status IN ('running', 'complete', 'incomplete'))
) STRICT, WITHOUT ROWID;

CREATE TABLE scan_errors (
    run_id TEXT NOT NULL REFERENCES scan_runs (id) ON DELETE RESTRICT,
    uri TEXT NOT NULL,
    message TEXT NOT NULL
) STRICT;

CREATE INDEX scan_errors_run ON scan_errors (run_id);
