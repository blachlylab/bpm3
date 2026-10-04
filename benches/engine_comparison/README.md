# Engine comparison benchmark

Compares DuckDB and SQLite on the BPM catalog shape (design B: one `nodes`
table, real foreign keys, `(key, value)` indexes). Results and analysis are in
[`docs/benchmarks/engine-comparison.md`](../../docs/benchmarks/engine-comparison.md).

    pip install duckdb          # SQLite is in the Python standard library
    python3 bench.py duckdb 50  # ENGINE K: K million file-metadata pairs
    python3 bench.py sqlite 50
    ./runall.sh                 # 50M and 100M on both engines, sequentially

The database (`bench_<engine>_<K>.db`) and `result_<engine>_<K>.json` are
written next to the script and are git-ignored. At K=100 the SQLite file is
about 14 GB and a full run takes roughly 15 minutes on an M1 Pro. Add `--nofk`
to build without foreign keys.
