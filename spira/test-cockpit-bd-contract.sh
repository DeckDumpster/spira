#!/usr/bin/env bash
#
# test-cockpit-bd-contract.sh — one real-bd row per distinct query shape the cockpit *_keys
# functions issue, so the fixture-driven suites that replaced them can trust bdsim.py.
#
#   ./test-cockpit-bd-contract.sh
#
# WHAT THIS SUITE IS FOR
# -----------------------
# docs/test-plan/cockpit-observability.md section 5 demotes dup-refs, repo-labels, sphere
# and sop off a real testdb_up store and onto SPIRA_BDJSON_FIXTURE (spira/bdsim.py's pure
# list/show/memories functions). That is safe only if bdsim.py's filtering — label AND
# matching, the default exclude-closed, --all, and the query shapes bd reads for `ready` and
# `memories` — actually agrees with the real `bd` binary. This suite is the explicit
# exception to "never build a real store" (law-prefer-the-real-dependency): it keeps exactly
# one row per query shape against real bd on a throwaway database, so a drift between
# bdsim.py and bd surfaces here instead of as a silent wrong answer in every fixture suite.
#
# NOT A REPEAT OF EACH SUITE'S OWN CASES. Each row asserts only that the shape resolves
# correctly against a real store — the exhaustive positive/negative/lookback/closed-bead
# cases already live in the fixture-driven suite for that probe.
#
# tier: T2
# covers: spira/cockpit.sh spira/bdsim.py cockpit/panel/src/store.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# Resolved BEFORE testdb.sh, which sources conf.sh, which rebuilds PATH from SPIRA_PATH +
# $HOME/.local/bin + /usr/local/bin + /usr/bin + /bin — dropping wherever this box's cargo
# actually lives (gap #4's row below needs it after that rebuild has already happened).
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
. "$HERE/testdb.sh"
testdb_require test-cockpit-bd-contract
TMP="$(mktemp -d)"
testdb_up bdcontract || { echo "test-cockpit-bd-contract: could not build a fixture database"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

RUN="$TMP/run"; mkdir -p "$RUN"
run_probe() {    # run_probe <subcommand> [env KEY=val ...]
    local sub="$1"; shift
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd-embedded}" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-goal SPIRA_FAYTHS=builder \
        SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" SPIRA_ASK_LABEL=needs-ryan \
        SPIRA_CI_LABEL=awaiting-ci \
        "$@" \
        bash "$HERE/cockpit.sh" "$sub" 2>/dev/null
}
field() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1; }

echo "test-cockpit-bd-contract.sh"

# ======================================================================================
echo
echo "list --all --label <a>,<b> plus the lookback filter (dup_refs_keys' shape):"
# ======================================================================================
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-cbd1","title":"dup bead 1","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"contract-dup-ref"}
{"id":"sp-cbd2","title":"dup bead 2","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"contract-dup-ref"}
{"id":"sp-cbd3","title":"different ref, not scoped label","status":"open","issue_type":"task","labels":["incident"],"updated_at":"2026-09-10T00:00:00Z","external_ref":"contract-dup-ref"}
JSONL
out="$(run_probe dup_refs)"
is "real bd: --label spira,incident (AND) finds the 2-way dup, not the 3rd bead" \
   "1" "$(field "$out" SP_DUP_REFS)"
is "real bd: SP_DUP_BEADS=1 surplus"  "1" "$(field "$out" SP_DUP_BEADS)"

# ======================================================================================
echo
echo "list --limit 0 (default filter excludes closed) — repo_label_keys' shape:"
# ======================================================================================
testdb_reset
MAP="$TMP/repo-map"; printf 'validrepo | /opt/valid | push | origin/main | | \n' > "$MAP"
testdb_seed <<'JSONL'
{"id":"sp-cbrl1","title":"open, unmapped","status":"open","issue_type":"task","labels":["repo:bogusrepo"],"updated_at":"2026-09-09T00:00:00Z"}
{"id":"sp-cbrl2","title":"closed, unmapped — must not count","status":"closed","issue_type":"task","labels":["repo:bogusrepo"],"updated_at":"2026-09-09T00:00:00Z"}
JSONL
out="$(run_probe repo_labels SPIRA_REPO_MAP="$MAP")"
is "real bd: default list excludes the closed bead" "1" "$(field "$out" SP_REPO_UNMAPPED)"

# ======================================================================================
echo
echo "list --all --label <single> (sphere_keys' poison shape):"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-cbsph1","title":"poisoned, incident-only","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-cbsph2","title":"poisoned but closed","status":"closed","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL
out="$(run_probe sphere)"
is "real bd: --all sees the poison label past sphere-grid scoping, excludes closed" \
   "1" "$(field "$out" SP_POISON)"

# ======================================================================================
echo
echo "bare memories (sop_keys' shape) — the shelf is a JSON object, not an array:"
# ======================================================================================
testdb_reset
"${SPIRA_BD:-bd-embedded}" -C "$SPIRA_DB" remember --key sop-contract-row "MATCH: contract test" >/dev/null 2>&1
out="$(run_probe sops SPIRA_SOP_LEDGER="$TMP/no-ledger.jsonl")"
never_fired="$(field "$out" SP_SOP_NEVER_FIRED)"
if [ "$never_fired" != "?" ] && [ -n "$never_fired" ]; then
    ok "real bd: bare 'memories' returns a JSON object sop_keys can read (SP_SOP_NEVER_FIRED=$never_fired)"
else
    bad "real bd: bare 'memories' must be readable by sop_keys" "got SP_SOP_NEVER_FIRED=$never_fired"
fi

# ======================================================================================
echo
echo "SP_READY: empty db reads 0, two ready beads read 2 (core_detail_keys' ready shape)."
echo "Moved here from test-cockpit-ready-seeded.sh (coverage row 08: MERGE-INTO the bd-contract suite)."
# ======================================================================================
testdb_reset
out="$(run_probe core)"
is "real bd: SP_READY=0 on an empty database" "0" "$(field "$out" SP_READY)"

testdb_seed <<JSONL
{"id":"sp-cbready1","title":"ready bead one","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-cbready2","title":"ready bead two","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL
out="$(run_probe core)"
is "real bd: SP_READY=2 with two ready beads seeded" "2" "$(field "$out" SP_READY)"

# ======================================================================================
echo
echo "gap #4: the panel's own Snapshot parsing (store.rs::fetch_beads) against a real"
echo "bd list --all --json, not the hand-shaped literal store.rs's own unit test stubs:"
# ======================================================================================
PANEL_MANIFEST="$(dirname "$HERE")/cockpit/panel/Cargo.toml"
if [ -z "$CARGO_BIN" ]; then
    printf '  skip  gap #4: cargo not found on PATH or at ~/.cargo/bin — install Rust: https://rustup.rs/\n'
elif ! PATH="$(dirname "$CARGO_BIN"):$PATH" "$CARGO_BIN" build --manifest-path "$PANEL_MANIFEST" --quiet 2>"$TMP/panel-build.err"; then
    bad "panel binary builds for the real-bd contract row" "cargo build failed: $(tail -5 "$TMP/panel-build.err")"
else
    PANEL_TARGET="${CARGO_TARGET_DIR:-$(dirname "$PANEL_MANIFEST")/target}"
    PANEL_BIN="$PANEL_TARGET/debug/panel"
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-cbdump1","title":"real bd through the panel's own Snapshot parsing","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL
    # No PANEL_FIXTURE: the panel shells out to the real `bd` this run's SPIRA_PATH/SPIRA_DB
    # point at, exactly as store.rs::db()/bin() resolve it live, and --dump prints whatever
    # fetch_beads()'s parsing made of that real payload.
    out="$(env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_DB="$SPIRA_DB" SPIRA_PATH="$SPIRA_PATH" \
        "$PANEL_BIN" --dump 2>"$TMP/panel-dump.err")"
    title="$(printf '%s' "$out" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    beads = d.get("beads") or []
    print(next((b.get("title", "") for b in beads if b.get("id") == "sp-cbdump1"), ""))
except Exception:
    print("")
' 2>/dev/null)"
    is "real bd list --all --json parses through store.rs::fetch_beads into the panel's own Snapshot" \
       "real bd through the panel's own Snapshot parsing" "$title"
fi

echo
printf 'test-cockpit-bd-contract: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
