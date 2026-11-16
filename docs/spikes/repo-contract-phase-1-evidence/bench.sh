#!/bin/bash
# Usage: bench.sh <flake-dir> <worktree>. Times two mechanisms, N runs each, for two rows.
set -u
FLAKE=$(readlink -f "$1"); WT=$2; N=${N:-20}
NIX=/nix/var/nix/profiles/default/bin/nix
NIXF=(--extra-experimental-features "nix-command flakes")
CACHE=$(mktemp -d)
cd "$WT" || exit 1
echo "cgroup: $(cat /proc/self/cgroup)"
now() { date +%s%N; }
stats() { sort -n | awk '{a[NR]=$1} END{printf "n=%d median=%.1fms max=%.1fms min=%.1fms\n",NR,(NR%2?a[(NR+1)/2]:(a[NR/2]+a[NR/2+1])/2),a[NR],a[1]}'; }
# mechanism 2: cache key = hash of flake.nix+flake.lock
key=$(cat "$FLAKE/flake.nix" "$FLAKE/flake.lock" | sha256sum | cut -c1-16)
envjson() {
  [ -s "$CACHE/$key.json" ] || $NIX "${NIXF[@]}" print-dev-env --json "$FLAKE" > "$CACHE/$key.json"
  echo "$CACHE/$key.json"
}
# exec argv in the shell's environment, nothing from the host but HOME
run_cached() {
  local j; j=$(envjson)
  local -a args
  mapfile -d '' -t args < <(python3 -I -c '
import json,sys
d=json.load(open(sys.argv[1]))["variables"]
for k,v in d.items():
    if v["type"]=="exported": sys.stdout.write(k+"="+v["value"]+"\0")
' "$j")
  env -i "${args[@]}" HOME="$HOME" TMPDIR=/tmp "$@"
}
mapfile -d '' -t X < /dev/null
declare -A ROWS=([true]="true" [ruff]="uv run ruff check mtg_collector/")
for row in true ruff; do
  read -ra argv <<< "${ROWS[$row]}"
  # warm both paths once (build/venv sync)
  $NIX "${NIXF[@]}" develop "$FLAKE" --command "${argv[@]}" >/dev/null 2>&1
  run_cached "${argv[@]}" >/dev/null 2>&1
  echo "== row: ${ROWS[$row]}"
  for mech in develop printenv; do
    : > "$CACHE/t.$mech"
    for i in $(seq "$N"); do
      s=$(now)
      if [ $mech = develop ]; then $NIX "${NIXF[@]}" develop "$FLAKE" --command "${argv[@]}" >/dev/null 2>&1; rc=$?
      else run_cached "${argv[@]}" >/dev/null 2>&1; rc=$?; fi
      e=$(now); echo "$(( (e-s)/1000 ))" | awk '{print $1/1000}' >> "$CACHE/t.$mech"; echo "rc=$rc" >> "$CACHE/rc.$mech.$row"
    done
    echo "$mech raw_ms: $(tr '\n' ' ' < "$CACHE/t.$mech")"
    echo "$mech: $(stats < "$CACHE/t.$mech")  rcs: $(sort "$CACHE/rc.$mech.$row" | uniq -c | tr '\n' ' ')"
  done
  # baseline: the row run directly with no shell (host PATH)
done
rm -rf "$CACHE"
