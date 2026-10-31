# epic-claim-fixture.sh — shared by test-epic-claim-*.sh: a real bd on a throwaway database,
# the spira-lc stand-in, lib.sh, the seed helpers (epic, closed_child, bead) and the rank
# readers (ranked_ids, rank_row). ecl_dwarf_env adds what a suite that runs the dwarf needs:
# an origin and clone, a fixture home with a builder rune, and the model stand-in.
# Sourced after testlib.sh, with HERE and ECL_SUITE set.
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require "$ECL_SUITE"
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up epicclaimdwarf || { echo "$ECL_SUITE: could not build a fixture database"; exit 1; }
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

ecl_dwarf_env() {
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
}
