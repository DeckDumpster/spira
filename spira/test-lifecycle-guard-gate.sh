#!/usr/bin/env bash
#
# test-lifecycle-guard-gate.sh — the landing gate refuses a planted landstate/oracle use.
#
#   ./test-lifecycle-guard-gate.sh
#
# The lifecycle machine is the only route to a bead's state once the cutover lands (brain
# design bead-lifecycle-state-machine §3.6(3), §3.8(2)); what keeps it the only route is the
# tree's own lifecycle-guard running as a fence of the landing gate with no allow-list
# (sp-ts2qr). This suite reads the step out of the tree's gate.steps — the gate's own
# definition (gate/DESIGN.md "The tree owns its gate") — and runs it the way the gate does,
# from the root of a tree, with SPIRA_GUARD_BIN the tree's lifecycle-guard:
#
#   A  gate.steps builds lifecycle-guard and runs it in gate mode (seen red without the wiring)
#   B  a clean tree passes, printing the gate's fence line; a planted violation that lives
#      under tests/fixtures/ is data and does not fail it
#   C  each planted reintroduction — `landing-pass landed`, a read of the ledger's files, the
#      Rust `land_state` API, a shell `land_mark` — fails the step, and the refusal names
#      its exits (law-a-refusal-names-its-exit)
#   D  a landstate read in spira-lc outside its migration reader fails it too: the machine
#      boundary is not an allow-list
#
# defect: sp-ts2qr
# tier: T1
# covers: gate.steps lifecycle-guard/src/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
STEPS="$HERE/../gate.steps"

GUARD="$(command -v lifecycle-guard 2>/dev/null || true)"
[ -n "$GUARD" ] || skip "lifecycle-guard is not on PATH — the release under test ships it in bin/"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-lifecycle-guard-gate.sh — the gate's lifecycle-guard step refuses a planted violation"

# ---- A: the wiring ------------------------------------------------------------------------
grep -qxE 'bin[[:space:]]+SPIRA_GUARD_BIN[[:space:]]+lifecycle-guard' "$STEPS" \
    && ok "A: gate.steps builds the tree's lifecycle-guard as SPIRA_GUARD_BIN" \
    || bad "A: gate.steps builds the tree's lifecycle-guard as SPIRA_GUARD_BIN" "no 'bin SPIRA_GUARD_BIN lifecycle-guard' line"
STEP="$(sed -n 's/^step[[:space:]]\{1,\}\(.*SPIRA_GUARD_BIN.*\)$/\1/p' "$STEPS")"
is "A: gate.steps runs it once, in gate mode, over the tree root" '"$SPIRA_GUARD_BIN" --gate .' "$STEP"
[ -n "$STEP" ] || { tl_summary; exit 1; }

# run_step <tree> — the step's own text, run as the gate runs a step: from the tree's root,
# with the tool variable the gate's tools phase sets.
run_step() {
    ( cd "$1" && SPIRA_GUARD_BIN="$GUARD" bash -c "$STEP" ) > "$TMP/out" 2> "$TMP/err"
}

# ---- B: a clean tree ----------------------------------------------------------------------
T="$TMP/tree"
mkdir -p "$T/spira" "$T/queue/src" "$T/queue/tests/fixtures"
cat > "$T/spira/route.sh" <<'EOF'
#!/usr/bin/env bash
# landed() used to answer this; the machine does now.
set -uo pipefail
[ "$(spira-lc state "$1")" = LANDED ] && echo "$1 landed"
EOF
cat > "$T/queue/src/lib.rs" <<'EOF'
// land_mark(...) was the ledger's write; spira-lc's verbs replace it.
pub fn line(id: &str) -> String { format!("{id} never landed (gate-red)") }
EOF
cat > "$T/queue/tests/fixtures/planted.sh" <<'EOF'
#!/usr/bin/env bash
land_mark "$1" LANDED "$2"
EOF
run_step "$T"; rc=$?
is   "B: a clean tree passes the step" 0 "$rc"
want "B: and proves it checked (the gate's fence line)" "fence: lifecycle-guard checked 2 files" "$(cat "$TMP/out")"

# ---- C: each planted reintroduction fails it ------------------------------------------------
# plant <case> <name> <path> <content> — a fresh copy of the clean tree with one file added; the
# step must exit 1 and print a landstate finding at that file.
plant() {
    local c="$1" name="$2" rel="$3" content="$4" P="$TMP/plant-$2"
    rm -rf "$P"; cp -r "$T" "$P"
    mkdir -p "$(dirname "$P/$rel")"; printf '%s\n' "$content" > "$P/$rel"
    run_step "$P"; rc=$?
    is   "$c: $name — the step refuses" 1 "$rc"
    want "$c: $name — names the site" "$rel:" "$(cat "$TMP/out")"
    want "$c: $name — as a landstate finding" "[landstate-" "$(cat "$TMP/out")"
}
plant C "landing-pass landed" spira/oracle.sh \
    $'#!/usr/bin/env bash\nif env SPIRA_RUN="$RUN" landing-pass landed "$1" repo; then echo yes; fi'
plant C "a read of the ledger's files" spira/ledger.sh \
    $'#!/usr/bin/env bash\ncut -d" " -f1 < "$SPIRA_RUN/landstate/$1"'
plant C "the Rust land_state API" queue/src/state.rs \
    $'pub fn s(r: &std::path::Path, id: &str) -> Option<String> {\n    landing_pass::landstate::land_state(r, id)\n}'
plant C "a shell land_mark" spira/mark.sh \
    $'#!/usr/bin/env bash\nland_mark "$1" CERTIFIED "$2"'

err="$(cat "$TMP/err")"
for exit_named in "REFUSED" "spira-lc" "lifecycle-guard/" "SPIRA_LAND_UNGATED=<reason>" "no allow-list"; do
    want "C: the refusal names its exit: $exit_named" "$exit_named" "$err"
done
nowant "C: a refused run prints no fence line" "fence: lifecycle-guard" "$(cat "$TMP/out")"

# ---- D: spira-lc is held to the rule outside its migration reader ---------------------------
plant D "spira-lc beyond its migration reader" spira-lc/src/answer.rs \
    $'pub fn landed(r: &std::path::Path, id: &str) -> bool {\n    std::fs::read_to_string(r.join("landstate").join(id)).is_ok()\n}'

tl_summary
