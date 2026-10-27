-- `bpm ingest` is now `bpm track`. Its run log follows the command's name.
--
-- RENAME TO rewrites the foreign key in the errors table. It does not rename
-- the index, so the index is dropped and created under the new name.

ALTER TABLE ingest_runs RENAME TO track_runs;
ALTER TABLE ingest_errors RENAME TO track_errors;

DROP INDEX ingest_errors_run;
CREATE INDEX track_errors_run ON track_errors (run_id);
