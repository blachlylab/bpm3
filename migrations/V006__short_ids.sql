-- Short ids: the last 12 hex digits of an id, `substr(id, 25)` on the
-- 36-character hyphenated form. A command may name a file, an entity, or a
-- link run by its short id. These indexes serve that lookup, and only a query
-- that spells the expression exactly this way uses them (catalog SHORT_ID_SQL).

CREATE INDEX files_short_id ON files (substr(id, 25));
CREATE INDEX programs_short_id ON programs (substr(id, 25));
CREATE INDEX projects_short_id ON projects (substr(id, 25));
CREATE INDEX cases_short_id ON cases (substr(id, 25));
CREATE INDEX samples_short_id ON samples (substr(id, 25));
CREATE INDEX raw_data_short_id ON raw_data (substr(id, 25));
CREATE INDEX analyses_short_id ON analyses (substr(id, 25));
CREATE INDEX link_runs_short_id ON link_runs (substr(id, 25));
