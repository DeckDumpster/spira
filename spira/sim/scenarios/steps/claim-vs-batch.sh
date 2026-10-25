#!/bin/sh
# claim-vs-batch.sh <bead> <control-bead> — a batch's landing racing a crowd of claims on a delivering row.
# Exit 3 until <bead> is CERTIFIED, 0 once it is LANDED, 9 on the first assertion that does not hold.
set -u
bead="$1"; ctl="$2"
fail() { echo "claim-vs-batch: $1" >&2; exit 9; }
state() { spira-lc state "$1" 2>/dev/null; }
field() { spira-lc show "$1" | python3 -c 'import json,sys; v=json.load(sys.stdin)["bead"].get(sys.argv[1]); print("" if v is None else v)' "$2"; }
ev() {  # ev <bead> <expect> <version> <actor> <kind-json>
    spira-lc event bead "$1" --expect "$2" --version "$3" --actor "$4" --kind "$5" >/dev/null 2>&1
}
claim() { ev "$1" "$2" "$3" "$4" "{\"Claim\":{\"holder\":\"$4\",\"lease_until\":9}}"; }

[ "$(state "$bead")" != LANDED ] || exit 0
[ "$(state "$bead")" = CERTIFIED ] || exit 3
[ "$(state "$ctl")" = READY ] || fail "the control bead $ctl is not READY"

claim "$ctl" READY "$(field "$ctl" version)" aeon-control || fail "control: a claim on a READY row was refused"
[ "$(state "$ctl")" = WORKING ] || fail "control: the claimed READY row is not WORKING"
ev "$ctl" WORKING "$(field "$ctl" version)" aeon-control '"Release"' || fail "control: the control claim could not be released"

v=$(field "$bead" version)
ev "$bead" CERTIFIED "$v" batch '"Deliver"' || fail "the batch could not take $bead into delivery"
v=$((v + 1))
[ "$(state "$bead")" = IN_DELIVERY ] || fail "$bead is not IN_DELIVERY"

h0=$(field "$bead" holder)
claim "$bead" IN_DELIVERY "$v" aeon-alone && fail "a claim on an IN_DELIVERY row was applied"
[ "$(field "$bead" holder)" = "$h0" ] || fail "the refused claim changed the holder"

tip=$(git rev-parse "refs/heads/spira/$bead") || fail "no branch for $bead"
git branch -f local/main "$tip" || fail "local/main could not be moved to $tip"

: > "$SIM_WORLD/claims.$bead"
for i in 1 2 3 4 5 6 7 8; do
    ( claim "$bead" IN_DELIVERY "$v" "aeon-$i" && echo applied >> "$SIM_WORLD/claims.$bead" ) &
done
ev "$bead" IN_DELIVERY "$v" batch "{\"Delivered\":{\"merge_sha\":\"$tip\",\"proof\":\"batch\"}}" || { wait; fail "the landing was refused"; }
wait
[ ! -s "$SIM_WORLD/claims.$bead" ] || fail "a racing claim was applied"
[ "$(state "$bead")" = LANDED ] || fail "$bead did not end LANDED"
case "$(field "$bead" holder)" in aeon-[0-9]*|aeon-late) fail "a racing claimant holds the landed row" ;; esac
[ "$(field "$bead" version)" = "$((v + 1))" ] || fail "the version did not advance by exactly one"

claim "$bead" LANDED "$((v + 1))" aeon-late && fail "a claim after the landing was applied"
case "$(field "$bead" holder)" in aeon-late) fail "the late claim left a holder" ;; esac
