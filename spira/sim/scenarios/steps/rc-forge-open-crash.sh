#!/bin/sh
# rc-forge-open-crash.sh seed|arm|check — the forge lane's crash between PR create and the phase move.
# `seed` takes the two filed beads to CERTIFIED and sifted at fresh branch tips; `arm` has the world's
# forge kill the `queue` process that creates the next PR, right after it has; `check` exits 0 only
# when exactly one batch PR exists, unmerged, and the batch record names it as open; 9 otherwise.
set -u
repo=fg
fail() { echo "rc-forge-open-crash: $1" >&2; exit 9; }

version() { spira-lc show "$1" | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d.get("bead", d) or {}
print(d.get("version", ""))'; }

event() {  # event <id> <expect> <kind JSON>
    spira-lc event bead "$1" --expect "$2" --version "$(version "$1")" --actor rc-forge-open-crash --kind "$3" >/dev/null || fail "$1: $2 event refused"
}

case "${1:-}" in
seed)
    work="$SIM_WORLD/repos/$repo/work"
    base=$(git -C "$work" rev-parse refs/remotes/origin/main) || fail "no origin/main in $work"
    for id in sp-fo01 sp-fo02; do
        idx="$SIM_WORLD/$id.index"
        blob=$(echo "$id" | git -C "$work" hash-object -w --stdin)
        GIT_INDEX_FILE=$idx git -C "$work" read-tree "$base"
        GIT_INDEX_FILE=$idx git -C "$work" update-index --add --cacheinfo "100644,$blob,$id.txt"
        tree=$(GIT_INDEX_FILE=$idx git -C "$work" write-tree)
        tip=$(git -C "$work" -c user.name=sim -c user.email=sim@sim commit-tree "$tree" -p "$base" -m "$id: member")
        timeout 5 git -C "$work" branch "spira/$id" "$tip" && timeout 5 git -C "$work" push -q origin "spira/$id" || fail "$id: branch not made"
        event "$id" READY '{"Claim":{"holder":"rc-forge-open-crash","lease_until":1}}'
        event "$id" WORKING "{\"Submit\":{\"tip\":\"$tip\"}}"
        event "$id" SUBMITTED "{\"GatePass\":{\"tip\":\"$tip\",\"gate_key\":\"rc-forge-open-crash\"}}"
        event "$id" CERTIFIED "{\"Sifted\":{\"tip\":\"$tip\"}}"
    done ;;
arm)
    sim ghctl "$SIM_GH_DIR" crash-after-create queue || fail "the crash was not armed" ;;
check)
    prs=$(timeout 5 gh pr list --state all --json number,headRefName,state,mergedAt | python3 -c '
import json, sys
for p in json.load(sys.stdin):
    if p["headRefName"].startswith("spira/queue/"):
        print(p["number"], p["state"], p["mergedAt"] or "-")') || fail "pr list failed"
    [ "$(printf '%s\n' "$prs" | grep -c .)" = 1 ] || fail "want exactly one batch PR, found: $prs"
    printf '%s' "$prs" | grep -q ' OPEN ' || fail "the batch PR is not open: $prs"
    n=${prs%% *}
    shown=$(queue batch show --repo "$repo" 2>&1) || fail "batch show: $shown"
    printf '%s\n' "$shown" | grep -qx "pr=$n" || fail "the batch record does not name PR $n: $shown"
    printf '%s\n' "$shown" | grep -qx 'forge.phase=open' || fail "the batch record is not open: $shown" ;;
*)
    fail "usage: rc-forge-open-crash.sh seed|arm|check" ;;
esac
exit 0
