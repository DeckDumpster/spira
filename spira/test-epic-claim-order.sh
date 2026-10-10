#!/usr/bin/env bash
#
# test-epic-claim-order.sh — the epic-first claim rank (sp-ns46j, per Ryan 2026-09-28):
# rank ready beads by min(epic priority, bead priority), then a started epic before an unstarted
# one at equal priority, then the bead's own priority, then resumable-before-fresh, then
# oldest. Covers epic_parent_lookup/epic_rank_rows (lib.sh) directly and aeon.sh's wiring
# of them into the actual claim.
#
# THE DEFECT THIS FIXES. `bd ready --claim` orders by bead priority alone, so a bead ejected
# back to ready from the epic being delivered competes on its own P-number with every
# unrelated P0 — narrow the fleet to a few aeons and the epic stalls behind work that has
# nothing to do with it. T3 below reproduces exactly that shape: a started P0 epic's
# lower-priority child against a fresh, unrelated P0 bead.
#
# T1/T2 CANNOT FAIL FOR WANT OF A TARGET on today's code: epic_parent_lookup and
# epic_rank_rows do not exist before this bead, so they error out (command not found) rather
# than merely disagreeing with the expectation — the strongest form of "seen to fail".
#
# A REAL bd ON A THROWAWAY DATABASE (testdb.sh): the rank is a claim about what `bd list`,
# `bd children` and the labels/status/parent fields they return actually contain, which a
# stub would be a second, driftable opinion of (law-prefer-the-real-dependency).
#
# T7/T8 (sp-o4trx): a ready set past MAX_ARG_STRLEN (128 KiB) still ranks every bead, and a
# forced rank failure surfaces as a claim-error rather than an empty/idle result.
#
# defect: sp-ns46j sp-o4trx
# tier: T3
# covers: spira/lib.sh aeon/src/* spira/epic-rank.sh spira-claim/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-epic-claim-order
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up epicclaimorder || { echo "test-epic-claim-order.sh: could not build a fixture database"; exit 1; }
# sp-mve9i: an epic is "started" by its children's lifecycle rows, never bd status. With no
# lifecycle store here, a stand-in spira-lc on PATH (spira-claim runs it by name) tells the
# fixture's bd story in lifecycle terms: closed → LANDED, in_progress → WORKING, open → READY.
lc_mirror_bd "$TMP/lc-mirror"
export PATH="$TMP/lc-mirror:$PATH" SPIRA_LC_BIN

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
tl_config SPIRA_RUN="$SPIRA_RUN"
# round 2 fix: the complete fixture declares scope_label="spira" as its base value, so
# builder.fayth's FAYTH_LABELS (resolved against the real config, not this shell's unset
# $SPIRA_SCOPE_LABEL) would require a "spira" label bead()'s seeded beads never carry —
# nothing would ever be ready. Declare the empty scope this suite has always meant.
tl_config SPIRA_SCOPE_LABEL=""
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-epic-claim-order.sh"

seed() { testdb_reset; testdb_seed; }   # seed <<JSONL ... JSONL, after a clean slate

# epic <id> <priority>            -> one open epic bead
# closed_child <id> <parent>      -> one CLOSED child, makes <parent> "started"
# bead <id> <parent-or-""> <prio> -> one ready (open, plan-labelled, unparented-or-childed) task
epic()         { printf '{"id":"%s","title":"epic","status":"open","issue_type":"epic","priority":%s,"labels":[],"updated_at":"2026-09-04T00:00:00Z"}\n' "$1" "$2"; }
closed_child() { printf '{"id":"%s","title":"done child","status":"closed","issue_type":"task","priority":2,"labels":[],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"%s","type":"parent-child"}]}\n' "$1" "$1" "$2"; }
bead() {
    local id="$1" parent="$2" prio="$3" deps="" labels="\"plan\",\"repo:fixture\""
    [ -n "$parent" ] && deps=",\"dependencies\":[{\"issue_id\":\"$id\",\"depends_on_id\":\"$parent\",\"type\":\"parent-child\"}]"
    [ -n "${SPIRA_SCOPE_LABEL:-}" ] && labels="\"$SPIRA_SCOPE_LABEL\",$labels"
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","priority":%s,"labels":[%s],"updated_at":"2026-09-04T00:00:00Z"%s}\n' "$id" "$prio" "$labels" "$deps"
}

# ranked_ids -> the ready set's own ids (READY_ARGS shape, plan label), ranked best-first.
ranked_ids() {
    local ready_json epic_lookup
    ready_json="$(bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
    [ -n "$ready_json" ] || ready_json="[]"
    epic_lookup="$(epic_parent_lookup "$ready_json")"
    epic_rank_rows "$ready_json" "$epic_lookup" "" | cut -f6
}
# rank_row <id> -> that bead's own TSV row (eprio estarted bprio resumable age id epic_id)
rank_row() {
    local id="$1" ready_json epic_lookup
    ready_json="$(bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
    [ -n "$ready_json" ] || ready_json="[]"
    epic_lookup="$(epic_parent_lookup "$ready_json")"
    epic_rank_rows "$ready_json" "$epic_lookup" "" | awk -F'\t' -v id="$id" '$6 == id'
}

# ==========================================================================================
echo
echo "T1: epic priority outranks bead priority"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-p2 sp-e1 2)
$(epic sp-e2 0)
$(bead sp-e2-p0 sp-e2 0)
JSONL
order="$(ranked_ids)"
is "E2's P0 child (its epic is P0) ranks before E1's P2 child (its epic is P1)" \
    "sp-e2-p0
sp-e1-p2" "$order"

# ==========================================================================================
echo
echo "T2: the first rank component is min(epic priority, bead priority); at equal effective"
echo "    priority a STARTED epic outranks an unstarted one"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-p2 sp-e1 2)
$(epic sp-e2 1)
$(bead sp-e2-p0 sp-e2 0)
JSONL
order="$(ranked_ids)"
is "a P0 child of an unstarted P1 epic outranks a P2 child of a started P1 epic" \
    "sp-e2-p0
sp-e1-p2" "$order"
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-p1 sp-e1 1)
$(epic sp-e2 1)
$(bead sp-e2-p1 sp-e2 1)
JSONL
order="$(ranked_ids)"
is "equal effective priority keeps started-epic-first order" \
    "sp-e1-p1
sp-e2-p1" "$order"

# ==========================================================================================
echo
echo "T3: a reworked bead keeps its epic's rank — the starvation this bead fixes"
echo "    (Ryan, 2026-09-28: narrowing the fleet let a reworked bead lose to unrelated P0s)"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 0)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-rework sp-e1 3)
$(bead sp-unrelated-p0 "" 0)
JSONL
order="$(ranked_ids)"
is "E1's reworked P3 child (E1 is P0, started) outranks a fresh, unrelated P0 bead" \
    "sp-e1-rework
sp-unrelated-p0" "$order"
epic_id="$(rank_row sp-e1-rework | cut -f7)"
is "sp-e1-rework's rank still names E1 as its epic" "sp-e1" "$epic_id"

# ==========================================================================================
echo
echo "T4: a bead with no epic sorts by its own priority, and ranks as its own epic"
# ==========================================================================================
seed <<JSONL
$(bead sp-none-p0 "" 0)
$(bead sp-none-p2 "" 2)
JSONL
order="$(ranked_ids)"
is "no-epic beads: P0 before P2" "sp-none-p0
sp-none-p2" "$order"
epic_id="$(rank_row sp-none-p0 | cut -f7)"
is "a no-epic bead's epic_id is its own id" "sp-none-p0" "$epic_id"

# ==========================================================================================
echo
echo "T5: the parent lookup is ONE query, whatever the ready-set size — not one per bead"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(epic sp-e2 2)
$(bead sp-e1-a sp-e1 1)
$(bead sp-e1-b sp-e1 2)
$(bead sp-e1-c sp-e1 3)
$(bead sp-e2-a sp-e2 1)
$(bead sp-e2-b sp-e2 2)
JSONL
REAL_BD="${SPIRA_BD:-$(command -v "${TESTDB_BD:-bd-embedded}")}"
BIN_BD="$TMP/bin"; mkdir -p "$BIN_BD"
CALL_LOG="$TMP/bd-calls.log"; : > "$CALL_LOG"
cat > "$BIN_BD/bd-count" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$CALL_LOG"
exec "$REAL_BD" "\$@"
STUB
chmod +x "$BIN_BD/bd-count"
# SPIRA_BD is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare via
# tl_config, not the env prefixes below, which no process reads it from any more.
tl_config SPIRA_BD="$BIN_BD/bd-count"
ready_json="$(bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
: > "$CALL_LOG"
epic_parent_lookup "$ready_json" >/dev/null
prio_calls="$(grep -c '^-C .*list --id ' "$CALL_LOG" 2>/dev/null || echo 0)"
is "exactly one 'bd list --id ...' call resolves every epic's own priority (5 beads, 2 epics)" \
    "1" "$prio_calls"
children_calls="$(grep -c '^-C .*children ' "$CALL_LOG" 2>/dev/null || echo 0)"
is "the 'started' check is one 'bd children' call per DISTINCT epic (2), not per bead (5)" \
    "2" "$children_calls"

# ==========================================================================================
echo
echo "T7: a ready set past MAX_ARG_STRLEN (128 KiB) still ranks every bead (sp-o4trx)"
# ==========================================================================================
# THE CLIFF THIS REPRODUCES. epic_parent_lookup/epic_rank_rows used to pass the whole
# ready-set JSON to python3 -c as a single argv element. At 142 real ready beads that
# argument exceeded the kernel's per-argument limit (MAX_ARG_STRLEN, 128 KiB): the exec died
# "Argument list too long" (rc 126), both functions printed nothing, and every builder aeon
# read the empty ranked list as "nothing ready to claim" for an hour (572 idle summons
# against 142 ready beads). Every other fixture in this suite uses a handful of beads, so
# none of them ever reached the threshold — this one must.
big_desc="$(python3 -c 'print("x" * 900)')"
N=300
{
    epic sp-ebig 0
    closed_child sp-ebig-done sp-ebig
    for i in $(seq 1 "$N"); do
        printf '{"id":"sp-big%03d","title":"t","status":"open","issue_type":"task","priority":%d,"labels":["plan","repo:fixture"],"updated_at":"2026-09-04T00:00:00Z","description":"%s","dependencies":[{"issue_id":"sp-big%03d","depends_on_id":"sp-ebig","type":"parent-child"}]}\n' \
            "$i" "$((i % 5))" "$big_desc" "$i"
    done
} > "$TMP/t7.jsonl"
seed < "$TMP/t7.jsonl"

t7_ready_json="$(bdjson ready --limit 0 --exclude-type epic,event -u --label plan)"
t7_size=${#t7_ready_json}
# 0. POSITIVE CONTROL. The case below is only meaningful if the payload is actually past
#    the limit; a fixture that shrank under it would pass against the broken code.
[ "$t7_size" -gt 131072 ] && ok "fixture ready-set JSON is past the 128 KiB limit ($t7_size bytes)" \
                          || bad "fixture ready-set JSON is only $t7_size bytes — not testing the cliff"

t7_order="$(ranked_ids)"
t7_count="$(printf '%s\n' "$t7_order" | grep -c .)"
is "every one of $N beads past the argv limit is ranked (none dropped by an E2BIG exec)" \
    "$N" "$t7_count"

t7_lookup_rc=0
epic_lookup="$(epic_parent_lookup "$t7_ready_json")" || t7_lookup_rc=$?
is "epic_parent_lookup succeeds (rc=0) against the oversized ready set" "0" "$t7_lookup_rc"

t7_rank_rc=0
epic_rank_rows "$t7_ready_json" "$epic_lookup" "" >/dev/null || t7_rank_rc=$?
is "epic_rank_rows succeeds (rc=0) against the oversized ready set" "0" "$t7_rank_rc"

# ==========================================================================================
echo
echo "T6a: epic-rank.sh (the round cutter) groups beads by epic, groups in rank order,"
echo "     members within a group by their own priority"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 1)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-y sp-e1 1)
$(bead sp-e1-x sp-e1 3)
$(epic sp-e2 1)
$(bead sp-e2-z sp-e2 2)
JSONL
cut_out="$(epic-rank.sh --label plan)"
is "grouped, group order by epic rank, members by their own priority within a group" \
"== sp-e1 (P1, started) ==
  sp-e1-y
  sp-e1-x
== sp-e2 (P1, unstarted) ==
  sp-e2-z" "$cut_out"

# ==========================================================================================
echo
echo "T6: aeon.sh's own claim, wired end to end, claims the epic-first candidate — not"
echo "    whatever 'bd ready --claim' would have taken on bead priority alone"
# ==========================================================================================
seed <<JSONL
$(epic sp-e1 0)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-rework sp-e1 3)
$(bead sp-unrelated-p0 "" 0)
JSONL
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; timeout 5 git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; timeout 5 git -C "$REPO" push -q origin main 2>/dev/null

AEON_HOME="$TMP/aeonhome"; mkdir -p "$AEON_HOME/chamber"
# round 2 fix (pattern 6): SPIRA_CHAMBER no longer derives from SPIRA_HOME — the complete
# fixture declares its own /fixture/userhome/.../chamber. Declare this suite's real one.
tl_config SPIRA_CHAMBER="$AEON_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$AEON_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$AEON_HOME/"
cp -r "$HERE/actors" "$AEON_HOME/" 2>/dev/null || true
AEON_RUN="$TMP/aeonrun"; mkdir -p "$AEON_RUN"
AEON_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$AEON_REPO_MAP"
cat > "$AEON_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}plan"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$AEON_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
tl_config SPIRA_AGENT="$SPIRA_AGENT"
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib aeon_fixture_agent.
aeon_fixture_agent "$BIN/claude"
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'claimed\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id - claimed"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

# THE AEON CLAIMS THROUGH THE MACHINE (sp-v62vn: no off mode): its ready set is spira-lc
# `list` and its claim a Claim event, so its runs get the stateful stand-in (testlib
# lc_aeon_mirror) ahead of the suite's read-only lc_mirror_bd. It logs every applied claim
# in $SPIRA_RUN/lc-claims.log, and reads the shim's bd close as the builder's submit.
lc_aeon_mirror "$TMP/lc-aeon"
# SPIRA_RUN/SPIRA_DB/SPIRA_REPO_MAP are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefix below, which no process reads any more.
tl_config SPIRA_RUN="$AEON_RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$AEON_REPO_MAP"
( PATH="$TMP/lc-aeon:$PATH" SPIRA_HOME="$AEON_HOME" \
  SPIRA_CONF="$TMP/no-such2.conf" \
  aeon --home "$AEON_HOME" builder > "$TMP/aeon-out" 2>&1 )

# WHICH BEAD THE AEON TOOK is the claim the machine recorded: the holder of sp-e1-rework's
# Claim, and nobody's on the unrelated P0 head. (The old reading — bd status reopened to
# open plus spira-submitted by the teardown's bd-close conversion, sp-qsona — is gone with
# sp-v62vn: every session runs restricted, and a restricted session's bd close is inert.)
is "aeon.sh claimed the started epic's reworked child, not the unrelated P0 head" \
    "yes" "$(grep -q '^sp-e1-rework ' "$AEON_RUN/lc-claims.log" 2>/dev/null && echo yes || echo no)"
want "and its session was handed that bead" "work sp-e1-rework " "$(cat "$TMP/prompt" 2>/dev/null)"
is "the unrelated P0 bead was left alone, never claimed" \
    "no" "$(grep -q '^sp-unrelated-p0 ' "$AEON_RUN/lc-claims.log" 2>/dev/null && echo yes || echo no)"
want "the log names the epic-first rank as the reason" "epic-first rank" "$(cat "$TMP/aeon-out")"

# ==========================================================================================
echo
echo "T8: a forced epic_rank_rows failure surfaces as a claim-error, never as idle (sp-o4trx)"
# ==========================================================================================
# THE SECOND HALF OF THE OUTAGE. Fixing the argv size alone is not enough: any OTHER reason
# epic_rank_rows might fail (a python crash, a corrupted temp file) must not read as "nothing
# ready to claim" either — the ledger line and the exit code must say claim-error.
seed <<JSONL
$(epic sp-e1 0)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-rework sp-e1 3)
$(bead sp-unrelated-p0 "" 0)
JSONL

# A spira-claim SHIM that fails only the final rank — the `select --resumable` call, the
# aeon binary's epic_rank_rows (aeon/src/claim.rs Selector::select) — and hands every other
# call (the epic lookup, the --top-tier band) to the real spira-claim, so the lookup and the
# resumability pass still run for real and only the rank itself is forced to fail. Ranking
# moved out of python (aeon.sh's epic_rank_rows) into spira-claim with the Rust cutover, so
# the old python3 shim no longer reached the call under test.
REAL_CLAIM="$(command -v spira-claim)" \
    || { echo "test-epic-claim-order: spira-claim is not on PATH — T8 cannot force the rank" >&2; exit 1; }
BIN2="$TMP/bin2"; mkdir -p "$BIN2"
cat > "$BIN2/spira-claim" <<STUB
#!/usr/bin/env bash
for a in "\$@"; do
    if [ "\$a" = "--resumable" ]; then
        cat >/dev/null
        echo "T8 shim: forced epic_rank_rows failure" >&2
        exit 1
    fi
done
exec "$REAL_CLAIM" "\$@"
STUB
chmod +x "$BIN2/spira-claim"

AEON_RUN2="$TMP/aeonrun2"; mkdir -p "$AEON_RUN2"
# SPIRA_RUN/SPIRA_DB/SPIRA_REPO_MAP are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefix below, which no process reads any more.
tl_config SPIRA_RUN="$AEON_RUN2" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$AEON_REPO_MAP"
( PATH="$BIN2:$TMP/lc-aeon:$PATH" SPIRA_HOME="$AEON_HOME" \
  SPIRA_CONF="$TMP/no-such3.conf" \
  aeon --home "$AEON_HOME" builder > "$TMP/aeon-out2" 2>&1 )
t8_rc=$?

[ "$t8_rc" -ne 0 ] && ok "aeon.sh exits non-zero on a forced rank failure (never the 'idle' success exit)" \
                   || bad "aeon.sh exits non-zero on a forced rank failure (never the 'idle' success exit)" "rc=0"
want "the log names it a claim-error, not idle" "claim-error" "$(cat "$TMP/aeon-out2")"
t8_ledger="$(cat "$AEON_RUN2/aeon-ledger.log" 2>/dev/null)"
want "the ledger records claim-error" "claim-error" "$t8_ledger"
is "the ledger never records this as an idle summons" "0" \
    "$(printf '%s\n' "$t8_ledger" | grep -c ' idle$')"

tl_summary
