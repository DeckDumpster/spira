#!/usr/bin/env bash
# test-batch.sh — merge-queue batch builder.
#
# Ten cases:
#   1. 8 certified branches trigger a batch at once.
#   2. 3 certified branches trigger a batch only after the planted wait.
#   3. A suite-state transition branch is ordered before regular branches.
#   4. A branch that conflicts with a prior batch member is skipped (stays CERTIFIED).
#   5. An open batch record prevents a second batch from opening.
#   b. Local gate passes: gate called once, PR opened.
#   c. Local gate red, one member reproduces: ejected, rebuilt batch gated and PR opened.
#   e. Local gate red, no member reproduces: PR IS opened; CI adjudicates.
#   f. Local gate red, one member reproduces: rebuild happens (ejection path not bypassed).
#   d. Meter line written for (b) and (c); queue.sh stats reports local_red_rate.
#
# The forge seam is a local fixture that records pr-create calls and returns
# incrementing PR numbers; no network is reached.
# The gate is a stub (always pass for cases 1-5; controlled for b/c).
# SPIRA_QUEUE_REPRO_BATCH is a stub that returns red only for planted branches.
#
# covers: spira/batch.sh spira/forge.sh spira/conf.sh spira/landing.sh spira/queue.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batch || { echo "test-batch: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

# Gate stub: always pass, count calls. Tests that need a different gate overwrite this.
GATE_COUNT="$TMP/gate-count"
: > "$GATE_COUNT"
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"

# Repro stub: always green (no branch reproduces by default).
# Tests that need a red member write the branch name to REPRO_FAIL_FILE.
REPRO_FAIL_FILE="$TMP/repro-fail-file"
: > "$REPRO_FAIL_FILE"
export REPRO_FAIL_FILE
cat > "$SH/repro-stub.sh" <<'REPRO'
#!/usr/bin/env bash
br=""
while [ $# -gt 0 ]; do
    case "$1" in --mode|--suites) shift 2 ;; *) br="$1"; shift ;; esac
done
fail_list="$(cat "${REPRO_FAIL_FILE}" 2>/dev/null || true)"
for f in $fail_list; do [ "$f" = "$br" ] && exit 1; done
exit 0
REPRO
chmod +x "$SH/repro-stub.sh"

# Forge fixture: log every pr-create call, return incrementing PR numbers.
# A real forge is never reached in this suite.
FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
# Fixture forge seam.  Pinned to a non-default SPIRA_FORGE so that an assertion
# passing against "used gh" fails rather than passing vacuously.
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    pr-create)
        head="${1:-}" base="${2:-}" title="${3:-}"
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\t%s\n' "$head" "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    pr-number)
        head="${1:-}"
        grep "^${head}	" "$FORGE_LOG" 2>/dev/null | tail -1 | cut -f2
        ;;
    *) printf 'forge-fixture: unknown command: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"

# Write repo-map: queue mode, pinned to a non-default land value.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

B() { bd -C "$SPIRA_DB" "$@"; }

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_SUITE_STATE="spira/suite-state" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_QUEUE_REPRO_BATCH="$SH/repro-stub.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

seed() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

# plant_bead <id>  — seed a closed bead with no repo: label (defaults to home repo)
plant_bead() {
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$1" "$1" "$1" | testdb_seed
}

# branch <id> [epoch]  — create a spira/<id> branch on main with one commit
#                        and plant CERTIFIED landstate at <epoch> (default: now)
branch() {
    local id="$1" epoch="${2:-$(date +%s)}"
    local wt="$RUN/worktree/$id"
    git -C "$REPO" worktree add -q -b "spira/$id" "$wt" main 2>/dev/null || true
    printf '%s\n' "$id" > "$wt/$id.txt"
    git -C "$wt" add -A
    git -C "$wt" commit -q -m "$id: work"
    local tip; tip="$(git -C "$REPO" rev-parse "spira/$id")"
    printf 'CERTIFIED %s %s\n' "$tip" "$epoch" > "$LANDSTATE/$id"
    plant_bead "$id"
}

open_batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
batch_pr()        { grep '^pr=' "$(open_batch_file)" 2>/dev/null | cut -d= -f2; }
is_batched()      { grep -q '^BATCHED' "$LANDSTATE/${1:-}" 2>/dev/null; }
is_certified()    { awk '{print $1}' "$LANDSTATE/${1:-}" 2>/dev/null | grep -q '^CERTIFIED$'; }
is_ejected()      { awk '{print $1}' "$LANDSTATE/${1:-}" 2>/dev/null | grep -q '^EJECTED$'; }
gate_n()          { wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf '0'; }
landing_log()     { cat "$RUN/landing.log" 2>/dev/null || true; }

# clean_case — remove all landstate entries and spira/* branches between test cases
clean_case() {
    rm -f "$QUEUEDIR/$REPONAME/open"
    rm -f "$RUN/landing.log"
    : > "$FORGE_LOG"
    : > "$GATE_COUNT"
    : > "$REPRO_FAIL_FILE"
    find "$LANDSTATE" -maxdepth 1 -type f 2>/dev/null -delete
    # Remove the batch worktree cleanly first (unregisters AND deletes the dir).
    local wt="$RUN/worktree/.batch-$(basename "$REPO")"
    if [ -d "$wt" ]; then
        git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    fi
    # Remove ALL remaining worktree dirs before pruning — git branch -D only succeeds
    # once the worktree registration is gone, and worktree prune only prunes entries
    # whose directory no longer exists.
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    # Now all linked-worktree registrations are gone; branches are deletable.
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do
            git -C "$REPO" branch -D "$br" 2>/dev/null || true
        done
}

echo "test-batch.sh"

# =============================================================================
# POSITIVE CONTROL: the forge is reached. Without this, "no batch opened" passes
# just as well against a batch.sh that silently returns before calling the forge.
# =============================================================================
seed
for i in $(seq 1 8); do branch "sp-bt1-$i"; done
out="$(batch "$REPONAME")"
is   "8 certified: PR opened"      "1"  "$(batch_pr)"
is   "8 certified: all BATCHED"    "8"  \
     "$(for i in $(seq 1 8); do is_batched "sp-bt1-$i" && echo y; done | grep -c y)"
want "8 certified: batch reported" "PR 1 opened" "$out"
clean_case

# =============================================================================
# 2. FEWER THAN MAX: no batch when too new; batch when oldest is old enough.
# =============================================================================
seed
NOW="$(date +%s)"
NEW_EPOCH="$NOW"
OLD_EPOCH=$(( NOW - 1800 - 1 ))   # 30 min + 1 s — past the wait threshold

for i in 1 2 3; do branch "sp-bt2n-$i" "$NEW_EPOCH"; done
batch "$REPONAME" > /dev/null
is "3 new: no batch opens" "0" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"

# Age the landstate entries.
for i in 1 2 3; do
    tip="$(git -C "$REPO" rev-parse "spira/sp-bt2n-$i")"
    printf 'CERTIFIED %s %s\n' "$tip" "$OLD_EPOCH" > "$LANDSTATE/sp-bt2n-$i"
done
batch "$REPONAME" > /dev/null
is "3 old: batch opens" "1" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"
clean_case

# =============================================================================
# 3. TRANSITION ORDERING: a suite-state transition branch goes before a branch
#    certified earlier.
# =============================================================================
seed
NOW="$(date +%s)"
# Age both past the 30-min wait so the batch triggers with just 2 branches.
OLD3=$(( NOW - 1800 - 2 ))

# Regular branch: certified first (older epoch).
branch "sp-bt3-reg" $(( OLD3 - 1 ))

# Transition branch: certified second (newer but still old); modifies suite-state.
git -C "$REPO" worktree add -q -b "spira/sp-bt3-trans" \
    "$RUN/worktree/sp-bt3-trans" main 2>/dev/null || true
mkdir -p "$RUN/worktree/sp-bt3-trans/spira"
printf 'test-something.sh | quarantined | 2026-09-16T00:00:00Z | sp-xxx | flaky\n' \
    > "$RUN/worktree/sp-bt3-trans/spira/suite-state"
git -C "$RUN/worktree/sp-bt3-trans" add -A
git -C "$RUN/worktree/sp-bt3-trans" commit -q -m "sp-bt3-trans: quarantine suite"
trans_tip="$(git -C "$REPO" rev-parse "spira/sp-bt3-trans")"
printf 'CERTIFIED %s %s\n' "$trans_tip" "$OLD3" > "$LANDSTATE/sp-bt3-trans"
plant_bead "sp-bt3-trans"

batch "$REPONAME" > /dev/null
batch_br="$(git -C "$REPO" for-each-ref --format='%(refname:short)' \
    'refs/heads/spira/queue/*' 2>/dev/null | tail -1)"
is "transition: batch opened" "1" "$([ -n "$batch_br" ] && echo 1 || echo 0)"
# The first merge commit in the batch branch should be the transition branch.
first_landed="$(git -C "$REPO" log --format='%s' "origin/main..$batch_br" \
    | grep 'spira: land' | tail -1)"
want "transition goes first" "sp-bt3-trans" "$first_landed"
clean_case

# =============================================================================
# 4. CONFLICTING PAIR: the second branch (same file, different content) is skipped.
# =============================================================================
seed
NOW="$(date +%s)"
# Age both branches past the 30-minute wait threshold so the batch triggers.
OLD4=$(( NOW - 1800 - 2 ))

# Branch A: creates shared.txt = version-a (certified first).
git -C "$REPO" worktree add -q -b "spira/sp-bt4-a" \
    "$RUN/worktree/sp-bt4-a" main 2>/dev/null || true
printf 'version-a\n' > "$RUN/worktree/sp-bt4-a/shared.txt"
git -C "$RUN/worktree/sp-bt4-a" add -A
git -C "$RUN/worktree/sp-bt4-a" commit -q -m "sp-bt4-a: version a"
tip_a="$(git -C "$REPO" rev-parse "spira/sp-bt4-a")"
printf 'CERTIFIED %s %s\n' "$tip_a" $(( OLD4 - 1 )) > "$LANDSTATE/sp-bt4-a"
plant_bead "sp-bt4-a"

# Branch B: creates shared.txt = version-b (certified after A — goes second).
git -C "$REPO" worktree add -q -b "spira/sp-bt4-b" \
    "$RUN/worktree/sp-bt4-b" main 2>/dev/null || true
printf 'version-b\n' > "$RUN/worktree/sp-bt4-b/shared.txt"
git -C "$RUN/worktree/sp-bt4-b" add -A
git -C "$RUN/worktree/sp-bt4-b" commit -q -m "sp-bt4-b: version b"
tip_b="$(git -C "$REPO" rev-parse "spira/sp-bt4-b")"
printf 'CERTIFIED %s %s\n' "$tip_b" "$OLD4" > "$LANDSTATE/sp-bt4-b"
plant_bead "sp-bt4-b"

batch "$REPONAME" > /dev/null
is      "conflict: A is BATCHED"          "1" "$(is_batched "sp-bt4-a" && echo 1 || echo 0)"
is      "conflict: B stays CERTIFIED"     "1" "$(is_certified "sp-bt4-b" && echo 1 || echo 0)"
clean_case

# =============================================================================
# 5. OPEN BATCH BLOCKS SECOND: a planted batch record prevents another opening.
# =============================================================================
seed
for i in $(seq 1 8); do branch "sp-bt5-$i"; done
# Plant a fake open batch record.
printf 'pr=99\nhead=abc\nbase=%s\nmembers=\nopened=0\nbranch=spira/queue/fake\n' \
    "$(git -C "$REPO" rev-parse origin/main)" > "$(open_batch_file)"

batch "$REPONAME" > /dev/null
is "open batch blocks second" "99" "$(batch_pr)"
clean_case

# =============================================================================
# b. LOCAL GATE GREEN: gate runs exactly once on the combined batch; PR opens.
# =============================================================================
seed
NOW="$(date +%s)"; OLD_B=$(( NOW - 1800 - 1 ))
for i in 1 2 3; do branch "sp-btb-$i" "$OLD_B"; done
out="$(batch "$REPONAME")"
is   "b: gate called exactly once"   "1"  "$(gate_n)"
is   "b: PR opened"                  "1"  "$(batch_pr)"
want "b: QUEUE BATCH meter written"  "QUEUE BATCH"   "$(landing_log)"
want "b: meter verdict=green"        "verdict=green" "$(landing_log)"
want "b: meter members=3"            "members=3"     "$(landing_log)"
clean_case

# =============================================================================
# c. LOCAL GATE RED WITH ONE REPRODUCER: ejected, rebuilt batch PR opened.
#
# Gate fails on its first call (batch of 3), passes on its second (rebuilt batch
# of 2). The repro stub identifies sp-btc-red as the reproducing member.
# =============================================================================
seed
NOW="$(date +%s)"; OLD_C=$(( NOW - 1800 - 1 ))
for id in sp-btc-1 sp-btc-2 sp-btc-red; do branch "$id" "$OLD_C"; done

# Gate stub: fails on first call, passes thereafter.
cat > "$SH/gate.sh" <<GSTUB2
#!/usr/bin/env bash
n=\$(wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf 0)
printf '%s\n' "\$1" >> "$GATE_COUNT"
if [ "\${n:-0}" -eq 0 ]; then
    printf 'gate: VERDICT=FAIL reason=red test-btc.sh FAILED branch=%s repo=%s suite=test-btc.sh\n' "\$1" "\${2:-?}" >&2
    exit 1
fi
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB2
chmod +x "$SH/gate.sh"

# Repro stub: sp-btc-red reproduces; sp-btc-1 and sp-btc-2 do not.
printf 'spira/sp-btc-red\n' > "$REPRO_FAIL_FILE"

out="$(batch "$REPONAME")"
is   "c: gate called twice (full + rebuilt)"  "2"  "$(gate_n)"
is   "c: PR opened (rebuilt batch)"           "1"  "$(batch_pr)"
is   "c: sp-btc-red ejected"    "1" "$(is_ejected "sp-btc-red" && echo 1 || echo 0)"
is   "c: sp-btc-1 batched"      "1" "$(is_batched "sp-btc-1"   && echo 1 || echo 0)"
is   "c: sp-btc-2 batched"      "1" "$(is_batched "sp-btc-2"   && echo 1 || echo 0)"
want "c: CAUGHT line for ejected"  "QUEUE CAUGHT"    "$(landing_log)"
want "c: branch=sp-btc-red in CAUGHT" "branch=sp-btc-red" "$(landing_log)"
want "c: red BATCH meter line"   "verdict=red"      "$(landing_log)"
want "c: green BATCH meter line" "verdict=green"    "$(landing_log)"
want "c: ejected field in log"   "ejected=sp-btc-red" "$(landing_log)"

# Restore default always-pass gate stub.
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"
clean_case

# =============================================================================
# e. UNATTRIBUTABLE LOCAL-GATE RED: gate red, no member reproduces —
#    PR IS opened; CI adjudicates (law-local-gates-buy-latency-not-coverage).
#
# POSITIVE CONTROL: gate_n starts at 0 before this case (clean_case zeros it);
# batch_pr reads from the forge fixture, which is only written when pr-create is
# called — an empty return proves the forge was never reached.
#
# Against pre-fix batch.sh this case fails: no PR is opened and the batch
# rebuilds the same tree, running the gate a second time (gate_n would be 2).
# =============================================================================
seed
NOW="$(date +%s)"; OLD_E=$(( NOW - 1800 - 1 ))
for id in sp-bte-1 sp-bte-2 sp-bte-3; do branch "$id" "$OLD_E"; done

# Gate red on first call; would pass on a second call — but with the fix, the
# second call never happens when ejected_arr is empty.
cat > "$SH/gate.sh" <<GSTUB_E
#!/usr/bin/env bash
n=\$(wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf 0)
printf '%s\n' "\$1" >> "$GATE_COUNT"
if [ "\${n:-0}" -eq 0 ]; then
    printf 'gate: VERDICT=FAIL reason=red test-bte.sh FAILED branch=%s repo=%s suite=test-bte.sh\n' "\$1" "\${2:-?}" >&2
    exit 1
fi
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB_E
chmod +x "$SH/gate.sh"

# No member reproduces (repro stub default: all green).
: > "$REPRO_FAIL_FILE"

out_e="$(batch "$REPONAME")"
is   "e: gate called exactly once (no re-gate)"   "1"  "$(gate_n)"
is   "e: PR IS opened (unattributable red)"        "1"  "$(batch_pr)"
want "e: no-member-reproduced log line"            "no member reproduced" "$out_e"
want "e: ejected=- in landing log"                 "ejected=-"            "$(landing_log)"

# Restore default always-pass gate stub.
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"
clean_case

# =============================================================================
# f. GENUINE EJECTION STILL REBUILDS: gate red, one member reproduces —
#    ejected, rebuilt batch opened; the unattributable-red path is NOT taken.
#
# This guards against an over-broad fix that always falls through to open the
# PR regardless of ejections. With such a fix: gate_n would be 1 (re-gate
# skipped), sp-btf-red would not be ejected, and the batch would contain all
# three members — the three assertions below would all fail.
# =============================================================================
seed
NOW="$(date +%s)"; OLD_F=$(( NOW - 1800 - 1 ))
for id in sp-btf-1 sp-btf-2 sp-btf-red; do branch "$id" "$OLD_F"; done

# Gate fails on first call (batch of 3), passes on second (rebuilt batch of 2).
cat > "$SH/gate.sh" <<GSTUB_F
#!/usr/bin/env bash
n=\$(wc -l < "$GATE_COUNT" 2>/dev/null | tr -d ' ' || printf 0)
printf '%s\n' "\$1" >> "$GATE_COUNT"
if [ "\${n:-0}" -eq 0 ]; then
    printf 'gate: VERDICT=FAIL reason=red test-btf.sh FAILED branch=%s repo=%s suite=test-btf.sh\n' "\$1" "\${2:-?}" >&2
    exit 1
fi
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB_F
chmod +x "$SH/gate.sh"

# sp-btf-red reproduces; sp-btf-1 and sp-btf-2 do not.
printf 'spira/sp-btf-red\n' > "$REPRO_FAIL_FILE"

batch "$REPONAME" > /dev/null
is   "f: gate called twice (full + rebuilt)"   "2"  "$(gate_n)"
is   "f: sp-btf-red ejected"                   "1"  "$(is_ejected "sp-btf-red" && echo 1 || echo 0)"
is   "f: PR IS opened (rebuilt batch)"         "1"  "$(batch_pr)"
_f_members="$(grep '^members=' "$(open_batch_file)" 2>/dev/null | cut -d= -f2)"
nowant "f: sp-btf-red not in batch members"   "sp-btf-red" "$_f_members"

# Restore default always-pass gate stub.
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"
: > "$REPRO_FAIL_FILE"
clean_case

# =============================================================================
# d. METER STATS: queue.sh stats reads local_red_rate and cost from BATCH lines.
#    Plant one green BATCH line and one red BATCH line in landing.log.
# =============================================================================
seed
NOW="$(date +%s)"
# Green batch: members=3, gate_seconds=5, verdict=green.
printf 'QUEUE BATCH %s repo=%s members=3 gate_seconds=5 verdict=green\n' \
    "$NOW" "$REPONAME" >> "$RUN/landing.log"
# Red batch: members=3, gate_seconds=8, verdict=red, attr_seconds=2, ejected=sp-d1.
printf 'QUEUE BATCH %s repo=%s members=3 gate_seconds=8 verdict=red attr_seconds=2 ejected=sp-d1\n' \
    "$NOW" "$REPONAME" >> "$RUN/landing.log"

stats_out="$(SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$SH/repo-map" \
    bash "$SH/queue.sh" stats 2>&1)"
want "d: local_red_rate 1/2 in stats" "1/2"   "$stats_out"
want "d: batches=2 in stats"          "batches:         2" "$stats_out"
want "d: members=6 in stats"          "6 members"       "$stats_out"
# cost = (5+8)/6 = 2s avg
want "d: cost per branch in stats"    "cost:"            "$stats_out"
clean_case

# =============================================================================
# 6. LANDING'S RECORD SHAPE: landing.sh certifies with no trailing newline, and
# read returns non-zero at EOF even after filling its variables. Every branch
# landing certified was skipped while only queue.sh submit's lines were seen.
# =============================================================================
clean_case
seed
for i in $(seq 1 8); do
    branch "sp-bt6-$i"
    printf '%s %s %s %s' CERTIFIED "$(git -C "$REPO" rev-parse "spira/sp-bt6-$i")" "$(date +%s)" "" \
        > "$LANDSTATE/sp-bt6-$i"
done
batch "$REPONAME" > /dev/null
is "landing-shaped records: PR opened" "1" "$(batch_pr)"

# =============================================================================
# 7. ALREADY-IN-BASE: 8 certified branches whose tips are already in the land
#    ref are marked LANDED (already-in-base) and do not consume batch slots.
#    The 1 branch with a real commit is the only member.
#
#    POSITIVE CONTROL: without the already-in-base filter, the 8 no-ops (sorted
#    earlier by epoch) fill SPIRA_QUEUE_BATCH_MAX and the real branch is never
#    included in the batch.
# =============================================================================
clean_case
seed
NOW7="$(date +%s)"
OLD7=$(( NOW7 - 1800 - 2 ))
BASE_SHA7="$(git -C "$REPO" rev-parse origin/main)"

# 8 branches whose certified tips equal the base (already in the land ref).
for i in $(seq 1 8); do
    plant_bead "sp-bt7-noop-$i"
    git -C "$REPO" branch "spira/sp-bt7-noop-$i" main 2>/dev/null || true
    # Tip = base sha; epoch older than real branch so they sort first.
    printf 'CERTIFIED %s %s\n' "$BASE_SHA7" $(( OLD7 - 1 )) > "$LANDSTATE/sp-bt7-noop-$i"
done

# 1 real branch with a commit not in the base; old enough to trigger a batch.
branch "sp-bt7-real" "$OLD7"

out7="$(batch "$REPONAME")"
is "7. already-in-base: PR opened (1 real member)" "1" "$(batch_pr)"
# The only member in the batch should be the real branch.
_members7="$(grep '^members=' "$(open_batch_file)" 2>/dev/null | cut -d= -f2)"
is "7. already-in-base: batch member is sp-bt7-real" "1" \
    "$(printf '%s\n' "$_members7" | tr ' ' '\n' | grep -c 'sp-bt7-real:')"
# All 8 no-ops must be LANDED with reason already-in-base.
_all7=1
for i in $(seq 1 8); do
    _st7="" _rs7=""
    read -r _st7 _ _ _rs7 < "$LANDSTATE/sp-bt7-noop-$i" 2>/dev/null || true
    [ "$_st7" = "LANDED" ] && [ "$_rs7" = "already-in-base" ] || { _all7=0; break; }
done
is "7. already-in-base: 8 no-ops LANDED already-in-base" "1" "$_all7"
clean_case

# =============================================================================
# 8. BRANCH-GONE: a CERTIFIED landstate record with no branch anywhere is
#    marked LOST (branch-gone), NOT LANDED. It does not count toward the
#    stuck-queue age, and no stuck-queue mail is sent on its behalf.
#
#    POSITIVE CONTROL: without the branch-gone marking, the ghost record stays
#    CERTIFIED after batch runs; the is-LOST assertion below would fail.
# =============================================================================
clean_case
seed

# Ghost: a CERTIFIED record with no branch in any repo, old enough to trigger
# the stuck-queue threshold if it were counted.
printf 'CERTIFIED fakeshafakeshafakeshafakeshafakeshafakeshafakeshafakes %s\n' \
    $(( $(date +%s) - 7201 )) > "$LANDSTATE/sp-bt8-ghost"

# A live CERTIFIED branch (not yet old enough to trigger stuck-queue on its own).
branch "sp-bt8-live" "$(date +%s)"

MAIL_LOG8="$TMP/mail8"
out8="$(batch "$REPONAME" 2>&1)"
is "8. branch-gone: ghost is LOST (not LANDED)" "1" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-bt8-ghost" 2>/dev/null)" = "LOST" ] && echo 1 || echo 0)"
is "8. branch-gone: ghost reason is branch-gone" "branch-gone" \
    "$(awk '{print $4}' "$LANDSTATE/sp-bt8-ghost" 2>/dev/null)"
# The ghost's old epoch should not have triggered a stuck-queue mail:
# only the live branch is in _certified_list, and it is not old enough.
nowant "8. branch-gone: no stuck-queue mail" "mailed operator" "$out8"
clean_case

# =============================================================================
# 9. SPIRA_QUEUE_LOCAL_GATE=0: the PR opens without the local gate (sp-hrkwa).
#    The gate stub fails, so a PR can only open if the gate was never called.
# =============================================================================
seed
NOW="$(date +%s)"; OLD_9=$(( NOW - 1800 - 1 ))
branch "sp-bt9-a" "$OLD_9"
: > "$GATE_COUNT"
cat > "$SH/gate.sh" <<GSTUB9
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=FAIL reason=red test-x.sh FAILED branch=%s repo=%s suite=test-x.sh\n' "\$1" "\${2:-?}" >&2
exit 1
GSTUB9
chmod +x "$SH/gate.sh"
out9="$(SPIRA_QUEUE_LOCAL_GATE=0 batch "$REPONAME")"
is   "9. skip: gate never called"      "0" "$(gate_n)"
is   "9. skip: PR opened"              "1" "$(batch_pr)"
is   "9. skip: member batched"         "1" "$(is_batched "sp-bt9-a" && echo 1 || echo 0)"
want "9. skip: says so in its output"  "local gate skipped" "$out9"
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"
clean_case

# =============================================================================
# A. STALE-CERTIFICATION: a branch certified at the base tip then advanced
#    must NOT be marked LANDED (already-in-base). The new commit must be either
#    batched or re-certified.
#
#    POSITIVE CONTROL: without the fix the already-in-base filter compares the
#    certified tip (= base sha) against the base, finds it an ancestor, and
#    marks the bead LANDED — the "NOT LANDED" assertion below fails.
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_A=$(( NOW - 1800 - 1 ))

# Certify the branch at the base sha (tip equals the land ref).
plant_bead "sp-btA-stale"
BASE_SHA_A="$(git -C "$REPO" rev-parse origin/main)"
git -C "$REPO" branch "spira/sp-btA-stale" main 2>/dev/null || true
printf 'CERTIFIED %s %s\n' "$BASE_SHA_A" "$OLD_A" > "$LANDSTATE/sp-btA-stale"

# Advance the branch: add a commit the base does not have.
git -C "$REPO" worktree add -q "$RUN/worktree/sp-btA-stale" "spira/sp-btA-stale" 2>/dev/null || true
printf 'stale-cert test\n' > "$RUN/worktree/sp-btA-stale/stale.txt"
git -C "$RUN/worktree/sp-btA-stale" add -A
git -C "$RUN/worktree/sp-btA-stale" commit -q -m "sp-btA-stale: work after certification"
LIVE_TIP_A="$(git -C "$REPO" rev-parse "spira/sp-btA-stale")"

out_a="$(batch "$REPONAME")"
is "A. stale-cert: NOT marked LANDED" "0" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-btA-stale" 2>/dev/null)" = "LANDED" ] && echo 1 || echo 0)"
_tipA="$(awk '{print $2}' "$LANDSTATE/sp-btA-stale" 2>/dev/null)"
is "A. stale-cert: live tip in landstate" "$LIVE_TIP_A" "$_tipA"
want "A. stale-cert: stale-certification logged" "stale-certification" "$out_a"
want "A. stale-cert: certified sha in log" "${BASE_SHA_A:0:8}" "$out_a"
want "A. stale-cert: live sha in log"      "${LIVE_TIP_A:0:8}"  "$out_a"

# =============================================================================
# g. BASE-CONFLICT STAMP: a branch that conflicts with origin/main is reopened
#    AND stamped merge-conflict via bump_requeue, matching landing.sh:624/922.
#
#    POSITIVE CONTROL: on the pre-fix tree, bump_requeue is never called from
#    the base-conflict branch in batch.sh; the spy file stays empty and the
#    assertion reads "1" against "0".
# =============================================================================
clean_case
seed
NOW="$(date +%s)"; OLD_G=$(( NOW - 1800 - 1 ))

# Branch spira/sp-btg: edits conflict.txt = "branch-ver" on top of current main.
git -C "$REPO" worktree add -q -b "spira/sp-btg" \
    "$RUN/worktree/sp-btg" main 2>/dev/null || true
printf 'branch-ver\n' > "$RUN/worktree/sp-btg/conflict.txt"
git -C "$RUN/worktree/sp-btg" add -A
git -C "$RUN/worktree/sp-btg" commit -q -m "sp-btg: work"
tip_g="$(git -C "$REPO" rev-parse "spira/sp-btg")"
printf 'CERTIFIED %s %s\n' "$tip_g" "$OLD_G" > "$LANDSTATE/sp-btg"
plant_bead "sp-btg"

# Push conflict.txt = "main-ver" to origin/main after the branch exists;
# spira/sp-btg now conflicts with the new base.
printf 'main-ver\n' > "$REPO/conflict.txt"
git -C "$REPO" add conflict.txt
git -C "$REPO" commit -q -m "main: set conflict.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# Spy: append a bump_requeue override to the lib.sh copy; bash uses the last
# definition, so this replaces the original for this batch() call.
# The path to REQUEUE_SPY_G is expanded now; ${1:-} etc. are escaped for runtime.
REQUEUE_SPY_G="$TMP/rq-spy-g"
: > "$REQUEUE_SPY_G"
cat >> "$SH/lib.sh" << LIBSPY

bump_requeue() {
    printf '%s %s\n' "\${1:-}" "\${2:-}" >> "$REQUEUE_SPY_G"
    _bump_write_event "\${1:-}" requeued "\${2:-unrecorded}"
}
LIBSPY

out_g="$(batch "$REPONAME" 2>&1)"
want "g: base-conflict: reopened message"            "conflicts with"   "$out_g"
is   "g: base-conflict: bump_requeue merge-conflict" "1" \
    "$(grep -c "^sp-btg merge-conflict$" "$REQUEUE_SPY_G" 2>/dev/null || echo 0)"

cp "$HERE/lib.sh" "$SH/lib.sh"
clean_case

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
