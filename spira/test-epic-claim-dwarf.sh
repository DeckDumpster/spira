#!/usr/bin/env bash
#
# test-epic-claim-dwarf.sh — dwarf's own claim wired end to end onto the epic-first rank
# (sp-ns46j, sp-o4trx), split off test-epic-claim-order.sh for wall time. T6: the dwarf claims
# a started epic's reworked child, not whatever `bd ready --claim` would take on bead priority
# alone. T8: a forced epic_rank_rows failure surfaces as a claim-error, never as idle.
#
# A REAL bd ON A THROWAWAY DATABASE (testdb.sh), as the rank suite does.

# defect: sp-ns46j sp-o4trx
# tier: T3
# covers: spira/lib.sh dwarf/src/* spira-claim/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-epic-claim-dwarf
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up epicclaimdwarf || { echo "test-epic-claim-dwarf.sh: could not build a fixture database"; exit 1; }
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
# builder.rune's RUNE_LABELS (resolved against the real config, not this shell's unset
# $SPIRA_SCOPE_LABEL) would require a "spira" label bead()'s seeded beads never carry —
# nothing would ever be ready. Declare the empty scope this suite has always meant.
tl_config SPIRA_SCOPE_LABEL=""
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-epic-claim-dwarf.sh"

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
echo "T6: dwarf.sh's own claim, wired end to end, claims the epic-first candidate — not"
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

DWARF_HOME="$TMP/dwarfhome"; mkdir -p "$DWARF_HOME/runes"
# round 2 fix (pattern 6): SPIRA_RUNES_DIR no longer derives from SPIRA_HOME — the complete
# fixture declares its own /fixture/userhome/.../runes. Declare this suite's real one.
tl_config SPIRA_RUNES_DIR="$DWARF_HOME/runes"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$DWARF_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$DWARF_HOME/"
cp -r "$HERE/actors" "$DWARF_HOME/" 2>/dev/null || true
DWARF_RUN="$TMP/dwarfrun"; mkdir -p "$DWARF_RUN"
DWARF_REPO_MAP="$TMP/repomap"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$DWARF_REPO_MAP"
cat > "$DWARF_HOME/runes/builder.rune" <<RUNE
RUNE_NAME=builder
RUNE_WORKING_EXITS="SUBMITTED ask park hold"
RUNE_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}plan"
RUNE_EXCLUDE_LABELS="spira-poison"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=600
RUNE
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$DWARF_HOME/runes/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
tl_config SPIRA_AGENT="$SPIRA_AGENT"
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib dwarf_fixture_agent.
dwarf_fixture_agent "$BIN/claude"
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'claimed\n' >> f
git add -A && git -c user.email=a@a -c user.name=dwarf commit -qm "$id - claimed"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

# THE DWARF CLAIMS THROUGH THE MACHINE (sp-v62vn: no off mode): its ready set is spira-lc
# `list` and its claim a Claim event, so its runs get the stateful stand-in (testlib
# lc_dwarf_mirror) ahead of the suite's read-only lc_mirror_bd. It logs every applied claim
# in $SPIRA_RUN/lc-claims.log, and reads the shim's bd close as the builder's submit.
lc_dwarf_mirror "$TMP/lc-dwarf"
# SPIRA_RUN/SPIRA_DB/SPIRA_REPO_MAP are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefix below, which no process reads any more.
tl_config SPIRA_RUN="$DWARF_RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$DWARF_REPO_MAP"
( PATH="$TMP/lc-dwarf:$PATH" SPIRA_HOME="$DWARF_HOME" \
  SPIRA_CONF="$TMP/no-such2.conf" \
  dwarf --home "$DWARF_HOME" builder > "$TMP/dwarf-out" 2>&1 )

# WHICH BEAD THE DWARF TOOK is the claim the machine recorded: the holder of sp-e1-rework's
# Claim, and nobody's on the unrelated P0 head. (The old reading — bd status reopened to
# open plus spira-submitted by the teardown's bd-close conversion, sp-qsona — is gone with
# sp-v62vn: every session runs restricted, and a restricted session's bd close is inert.)
is "dwarf.sh claimed the started epic's reworked child, not the unrelated P0 head" \
    "yes" "$(grep -q '^sp-e1-rework ' "$DWARF_RUN/lc-claims.log" 2>/dev/null && echo yes || echo no)"
want "and its session was handed that bead" "work sp-e1-rework " "$(cat "$TMP/prompt" 2>/dev/null)"
is "the unrelated P0 bead was left alone, never claimed" \
    "no" "$(grep -q '^sp-unrelated-p0 ' "$DWARF_RUN/lc-claims.log" 2>/dev/null && echo yes || echo no)"
want "the log names the epic-first rank as the reason" "epic-first rank" "$(cat "$TMP/dwarf-out")"

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
# dwarf binary's epic_rank_rows (dwarf/src/claim.rs Selector::select) — and hands every other
# call (the epic lookup, the --top-tier band) to the real spira-claim, so the lookup and the
# resumability pass still run for real and only the rank itself is forced to fail. Ranking
# moved out of python (dwarf.sh's epic_rank_rows) into spira-claim with the Rust cutover, so
# the old python3 shim no longer reached the call under test.
REAL_CLAIM="$(command -v spira-claim)" \
    || { echo "test-epic-claim-dwarf: spira-claim is not on PATH — T8 cannot force the rank" >&2; exit 1; }
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

DWARF_RUN2="$TMP/dwarfrun2"; mkdir -p "$DWARF_RUN2"
# SPIRA_RUN/SPIRA_DB/SPIRA_REPO_MAP are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefix below, which no process reads any more.
tl_config SPIRA_RUN="$DWARF_RUN2" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$DWARF_REPO_MAP"
( PATH="$BIN2:$TMP/lc-dwarf:$PATH" SPIRA_HOME="$DWARF_HOME" \
  SPIRA_CONF="$TMP/no-such3.conf" \
  dwarf --home "$DWARF_HOME" builder > "$TMP/dwarf-out2" 2>&1 )
t8_rc=$?

[ "$t8_rc" -ne 0 ] && ok "dwarf.sh exits non-zero on a forced rank failure (never the 'idle' success exit)" \
                   || bad "dwarf.sh exits non-zero on a forced rank failure (never the 'idle' success exit)" "rc=0"
want "the log names it a claim-error, not idle" "claim-error" "$(cat "$TMP/dwarf-out2")"
t8_ledger="$(cat "$DWARF_RUN2/dwarf-ledger.log" 2>/dev/null)"
want "the ledger records claim-error" "claim-error" "$t8_ledger"
is "the ledger never records this as an idle calls" "0" \
    "$(printf '%s\n' "$t8_ledger" | grep -c ' idle$')"

tl_summary
