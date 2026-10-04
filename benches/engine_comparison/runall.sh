#!/bin/bash
cd "$(dirname "$0")"
for K in 50 100; do
 for E in duckdb sqlite; do
  echo "=== $E $K ===" 
  python3 bench.py $E $K 2>&1
  rm -f bench_${E}_${K}.db*
 done
done
echo ALLDONE
