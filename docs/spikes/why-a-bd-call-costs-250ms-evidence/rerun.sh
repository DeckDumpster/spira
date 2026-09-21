#!/usr/bin/env bash
# rerun.sh — re-take the measurements in this directory.
#
# Reads only. Writes nothing to any store. Needs `bd` and `bd-embedded` on PATH, a
# server-mode store at $SPIRA_DB, and ~500 MB of scratch.
#
#   SPIRA_DB=<server-mode store> ./rerun.sh [scratch-dir]
set -uo pipefail
S="${1:-$(mktemp -d)}"; mkdir -p "$S"; cd "$S" || exit 1
: "${SPIRA_DB:?set SPIRA_DB to a server-mode store}"

cpub() { # cpub <n> <label> -- cmd...
    local n=$1 label=$2; shift 2; [ "$1" = -- ] && shift
    local out; out="$(mktemp)"
    for _ in $(seq "$n"); do /usr/bin/time -f '%U %S %e' -a -o "$out" "$@" >/dev/null 2>/dev/null; done
    awk -v L="$label" -v N="$n" '
      {c[NR]=($1+$2)*1000; w[NR]=$3*1000}
      END{ n=asort(c); m=asort(w)
           printf "%-42s n=%-3d cpu_ms: min=%-6.0f p50=%-6.0f max=%-6.0f | wall_ms: min=%-6.0f p50=%-6.0f max=%-6.0f\n",
                  L,N,c[1],c[int(n/2)+1],c[n],w[1],w[int(m/2)+1],w[m] }' "$out"
    rm -f "$out"
}

echo "== 01: process start =="
cpub 15 "bd --version (no store)"        -- bd --version
GODEBUG=inittrace=1 bd --version 2>&1 | grep '^init ' |
    awk '{s+=$5} END{printf "  package init: %.1f ms across %d packages\n", s, NR}'
cpub 12 "bd --version GOGC=off"          -- env GOGC=off bd --version

echo "== 02: server-mode store =="
cpub 10 "live: bd count"                 -- bd -C "$SPIRA_DB" count
cpub 10 "live: bd label list <a bead>"   -- bd -C "$SPIRA_DB" label list "$(bd -C "$SPIRA_DB" list --limit 1 --json 2>/dev/null | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p' | head -1)"
cpub  6 "live: bd list --limit 0 --json" -- bd -C "$SPIRA_DB" list --limit 0 --json

echo "== 03: wall = cpu / quota =="
for q in 100 70 40 20; do
    printf '  CPUQuota=%3s%%  ' "$q"
    systemd-run --user --quiet --pipe --wait --collect --property=CPUQuota=${q}% \
        --setenv=PATH="$PATH" --setenv=HOME="$HOME" \
        /bin/bash -c 'ts=$(date +%s%N); for i in $(seq 8); do bd -C '"$SPIRA_DB"' count >/dev/null 2>&1; done
                      te=$(date +%s%N); echo "wall_per_call_ms=$(( (te-ts)/8000000 ))"' 2>&1 | tail -1
done

echo "== 04: the N+1 vs one GROUP BY =="
IDS="$(bd -C "$SPIRA_DB" list --status open,in_progress,blocked --limit 0 --json 2>/dev/null |
       python3 -c 'import json,sys;print(",".join("\x27%s\x27"%i["id"] for i in json.load(sys.stdin) if i.get("issue_type") not in ("epic","event")))')"
LIST="$(printf '%s' "$IDS" | tr -d "'" | tr ',' ' ')"
N=$(printf '%s\n' $LIST | wc -l)
s=$(date +%s%N)
for id in $LIST; do
    bd -C "$SPIRA_DB" label list "$id" >/dev/null 2>&1
    bd -C "$SPIRA_DB" sql "select count(*) from events where issue_id='$id'" >/dev/null 2>&1
    bd -C "$SPIRA_DB" sql "select count(*) from events where issue_id='$id' and event_type='reopened'" >/dev/null 2>&1
done
e=$(date +%s%N); A=$(( (e-s)/1000000 ))
s=$(date +%s%N)
bd -C "$SPIRA_DB" sql "select issue_id,
      sum(case when event_type='claimed' or (event_type='status_changed' and new_value like '%in_progress%') then 1 else 0 end),
      sum(case when event_type='closed' then 1 else 0 end),
      sum(case when event_type='reopened' then 1 else 0 end)
    from events where issue_id in ($IDS) group by issue_id" > batched.out 2>&1
e=$(date +%s%N); B=$(( (e-s)/1000000 ))
echo "  N=$N beads: $((N*3)) calls = ${A}ms | 1 GROUP BY = ${B}ms | ratio $((A/(B>0?B:1)))x"

echo "== positive control: batched must find a NON-ZERO reopen count somewhere =="
awk -F'|' 'NR>2 {gsub(/ /,"",$4); if ($4+0 > 0) {print "  non-zero found:", $1, "reopens =", $4; f=1; exit}}
           END{ if (!f) print "  REFUSING: every reopen count is zero — cannot tell a working query from a broken one" }' batched.out
