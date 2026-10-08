#!/usr/bin/env bash
# Times lifecycle-shaped event-insert transactions against a Dolt sql-server, so commit
# latency can be read against event row count, journal size, writer concurrency and
# dolt_gc state. Point it at a throwaway server, never the serving one.
#   lc-commit-bench.sh HOST PORT DB [N_PER_WRITER] [WRITERS]
# Prints one "n= p50= p90= p99= max=" line (milliseconds) per writer.
set -uo pipefail
host=${1:?host} port=${2:?port} db=${3:?database}
n=${4:-150} writers=${5:-1}

gen() {
  local i
  for i in $(seq 1 "$n"); do
    printf "SET @a=NOW(6);\nSTART TRANSACTION;\n"
    printf "INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at) VALUES ('bead','bench-%s-%s','ev','x','a','b',1,NULL,'{\"pad\":\"xxxxxxxxxxxxxxxxxxxxxxxx\"}','bench',%s);\n" "$$" "$i" "$i"
    printf "COMMIT;\nSELECT TIMESTAMPDIFF(MICROSECOND,@a,NOW(6))/1000 AS ms;\n"
  done
}

one() {
  gen | dolt --host "$host" --port "$port" --no-tls -u root -p '' --use-db "$db" sql -r csv 2>&1 |
    grep -E '^[0-9.]+$' | sort -n |
    awk '{a[NR]=$1} END{if(!NR){print "n=0"; exit 1} print "n="NR" p50="a[int(NR/2)+1]" p90="a[int(NR*0.9)+1]" p99="a[int(NR*0.99)+1]" max="a[NR]}'
}

for _ in $(seq 1 "$writers"); do one & done
wait
