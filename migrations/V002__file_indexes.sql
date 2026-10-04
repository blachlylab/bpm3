-- Lookups the file indexer makes on every new path and every digest query.
--
-- Ingest asks for files with the same size and the same fingerprint scheme and
-- value before it hashes a possible duplicate. `bpm query files --digest` asks
-- for the file that carries a digest.

CREATE INDEX files_fingerprint ON files (size_bytes, fingerprint_scheme, fingerprint);
CREATE INDEX file_digests_digest ON file_digests (algorithm, digest);
