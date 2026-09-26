#!/usr/bin/env bash
#
# test-check2-reaper.sh — CHECK 2's stale-lease reclaim loop and CHECK 2c's per-partition
#   orphan-claim sweep, both previously untested (dispatch test plan G9, G10).
#
#   ./test-check2-reaper.sh
#
# G9: parse_reclaimed (lib.sh) turns `bdq reclaim`'s success lines into ids and charges each
# one through bump_reclaim, driven by canned bd-output strings (cases 1-4). check2_reclaim_stale
# (lib.sh) is the loop itself — one bdq reclaim per partition named in $PARTITIONS, the
# --exclude-label $SPIRA_RECLAIM_SKIP_LABEL flag on every call, and the "no persona ...
# declares a partition" log when nothing is named — driven directly with a stubbed bdq
# (cases 5-6).
#
# G10: CHECK 2c's release_orphan_claims_partitions (lib.sh) sweeps every partition
# fayth_partitions names, not one hardcoded `${SPIRA_SCOPE_LABEL},plan`. Orphaned claims in
# an ops partition are released exactly like a plan one — the "one hardcoded partition"
# defect the fayth_partitions comment says was already removed from CHECK 2 and CHECK 5.
#
# defect: sp-9ce60 (dispatch test plan, gaps G9/G10)
# covers: spira/lib.sh spira/sentinel.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
has() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
log() { :; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-check2-reaper.sh"

# ======================================================================================
echo
echo "G9 case 1 — positive control: a reclaim line yields exactly one charge:"
# ======================================================================================
# Without this, an implementation that never charges bump_reclaim reads as correct.
BUMPED="$TMP/bumped"; : > "$BUMPED"
bump_reclaim() { printf '%s %s\n' "$1" "$2" >> "$BUMPED"; }
n="$(parse_reclaimed $'✓ Reclaimed sp-dead1 (was held by aeon-x, expired 42m ago)')"
is  "one reclaim line -> count 1"    "1" "$n"
has "bump_reclaim charged sp-dead1"  "sp-dead1 stale-lease" "$(cat "$BUMPED")"

echo
echo "G9 case 2 — multiple lines, both success shapes ('✓' and 'Reclaimed'), each charged:"
: > "$BUMPED"
n="$(parse_reclaimed $'Reclaimed sp-dead2 (aeon-y)\n✓ Reclaimed sp-dead3 (aeon-z)')"
is  "two lines -> count 2"   "2" "$n"
has "sp-dead2 charged" "sp-dead2 stale-lease" "$(cat "$BUMPED")"
has "sp-dead3 charged" "sp-dead3 stale-lease" "$(cat "$BUMPED")"

echo
echo "G9 case 3 — the idle message is not miscounted (the false-alert this exists to prevent):"
# ======================================================================================
# The first version of this code counted lines CONTAINING "reclaim", which also matches
# "No stale leases to reclaim in the filtered scope" — every idle pass then reported an
# action it had not taken. parse_reclaimed matches the SUCCESS SHAPE, not the word.
: > "$BUMPED"
n="$(parse_reclaimed 'No stale leases to reclaim in the filtered scope')"
is "idle message -> count 0"   "0"  "$n"
is "idle message -> no charge" "" "$(cat "$BUMPED")"

echo
echo "G9 case 4 — a matched line with no parseable id is counted but not charged:"
: > "$BUMPED"
n="$(parse_reclaimed 'Reclaimed 1 lease(s)')"
is "unparseable id -> count 1"   "1" "$n"
is "unparseable id -> no charge" "" "$(cat "$BUMPED")"

# ======================================================================================
echo
echo "G9 case 5 — the loop drives one bdq reclaim per partition, exclude-label on every call:"
# ======================================================================================
# check2_reclaim_stale must ask about EVERY partition $PARTITIONS names, not just one (the
# CHECK 2 half of the "one hardcoded partition" defect), and every ask must carry
# --exclude-label $SPIRA_RECLAIM_SKIP_LABEL — the flag that keeps this loop from re-reclaiming
# a bead check2_protect_waiting already protected.
CALLS="$TMP/bdq-calls"; : > "$CALLS"; : > "$BUMPED"
# Pinned to a non-default value: the shipped default (spira-waiting-operator) would pass
# just as well against an implementation with that string hardcoded instead of read from
# $SPIRA_RECLAIM_SKIP_LABEL.
export SPIRA_RECLAIM_SKIP_LABEL=test-skip-nondefault
_real_bdq="$(declare -f bdq)"
bdq() {
    printf '%s\n' "$*" >> "$CALLS"
    case " $* " in
        *"--label plan "*) printf '✓ Reclaimed sp-dead4 (was held by aeon-p, expired 42m ago)\n' ;;
        *"--label ops "*)  printf 'No stale leases to reclaim in the filtered scope\n' ;;
    esac
}
progressed=0; progress() { progressed=$((progressed+1)); }
check2_reclaim_stale $'plan\t\nops\t'
is  "two partitions -> two bdq reclaim calls" "2" "$(grep -c '^reclaim ' "$CALLS")"
has "plan call carries the exclude-label flag" "--exclude-label test-skip-nondefault" "$(grep 'label plan' "$CALLS")"
has "ops call carries the exclude-label flag"  "--exclude-label test-skip-nondefault" "$(grep 'label ops' "$CALLS")"
has "plan partition's reclaim charged"  "sp-dead4 stale-lease" "$(cat "$BUMPED")"
is  "ops partition's idle output charges nothing extra" "1" "$(wc -l < "$BUMPED")"
is  "progress reported once (only plan reclaimed)" "1" "$progressed"

echo
echo "G9 case 6 — no partitions declared: zero bdq calls, and the no-partition log fires:"
# ======================================================================================
# A REAPER WITH NOTHING TO REAP OVER SAYS SO. Without this, a loop that silently does
# nothing when $PARTITIONS is empty reads exactly like a harness with no dead leases.
: > "$CALLS"; : > "$BUMPED"; progressed=0
LOGGED="$TMP/logged"; : > "$LOGGED"
log() { printf '%s\n' "$1" >> "$LOGGED"; }
check2_reclaim_stale ''
is  "empty partitions -> zero bdq calls" "0" "$(wc -l < "$CALLS")"
has "empty partitions -> the no-partition log line" "no persona in the chamber declares a partition" "$(cat "$LOGGED")"
is  "empty partitions -> progress not reported" "0" "$progressed"
log() { :; }
eval "$_real_bdq"
unset -f progress bump_reclaim

# ======================================================================================
echo
echo "G10 case 0 — positive control: a single partition's orphan is released:"
# ======================================================================================
mkdir -p "$TMP/bin" "$TMP/state"
cat > "$TMP/bin/bd" <<'STUB'
#!/usr/bin/env bash
case " $* " in
    *" list "*"--label plan "*) cat "$BD_STATE/plan.json" ;;
    *" list "*"--label ops "*)  cat "$BD_STATE/ops.json" ;;
    *" assign "*) printf '%s\n' "$*" >> "$BD_STATE/assigns"; exit 0 ;;
    *) printf '[]' ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/bd"
export BD_STATE="$TMP/state"
printf '[{"id":"sp-plan1","assignee":"aeon-p","status":"open"}]' > "$BD_STATE/plan.json"
printf '[]' > "$BD_STATE/ops.json"
: > "$BD_STATE/assigns"

out="$(PATH="$TMP/bin:$PATH" SPIRA_BD=bd SPIRA_DB=fixture release_orphan_claims_partitions $'plan\t')"
has "plan-only sweep releases sp-plan1" "sp-plan1" "$out"
is  "plan-only sweep releases exactly one" "1" "$(grep -c '^RELEASED' <<< "$out")"

echo
echo "G10 case 1 — a second, non-plan partition (ops) is swept too, not just plan:"
# ======================================================================================
# Before the fix this called release_orphan_claims once, hardcoded to
# \${SPIRA_SCOPE_LABEL},plan — an orphaned claim in ops (or spike, or groom) was never
# released. Without this case, a fix that still sweeps only 'plan' reads as correct.
: > "$BD_STATE/assigns"
printf '[{"id":"sp-ops1","assignee":"aeon-o","status":"open"}]' > "$BD_STATE/ops.json"
out="$(PATH="$TMP/bin:$PATH" SPIRA_BD=bd SPIRA_DB=fixture release_orphan_claims_partitions $'plan\t\nops\t')"
has "plan partition still released"  "sp-plan1" "$out"
has "ops partition ALSO released"    "sp-ops1"  "$out"
is  "both partitions swept -> 2 released" "2" "$(grep -c '^RELEASED' <<< "$out")"

echo
echo "G10 case 2 — no partitions declared: nothing is swept, and nothing is released:"
: > "$BD_STATE/assigns"
out="$(PATH="$TMP/bin:$PATH" SPIRA_BD=bd SPIRA_DB=fixture release_orphan_claims_partitions '')"
is "empty partition list -> no output" "" "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
