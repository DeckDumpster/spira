#!/usr/bin/env bash
#
# test-cockpit-collect-probes.sh — the real queue probe's keys reach cockpit.env through
# the fragment/merge path.
#
# sp-kt4l3: the registry-integrity half ("every probe cockpit.sh's probe() calls is
# registered in collect.sh's PROBES") is retired here. It extracted the `PROBES=()` array
# literal from bash source at runtime by regex, which cockpit-collect has nothing
# equivalent to parse — PROBES is a real Rust `const` array now, checked directly and far
# more completely by `cockpit-collect`'s own unit test `probes_registry_is_well_formed`
# (18 entries, no duplicate names, every interval a declared tier, no malformed rows, at
# least one timeout shorter than its interval) plus `due_probes_caps_slow_tier_concurrency_
# cumulatively_within_one_tick`. What remains here — a real probe's keys surviving the
# fragment write and the merge into cockpit.env — has no Rust-unit equivalent (it is this
# binary's own subprocess/file-IO boundary) and stays a black-box suite over the compiled
# binary. The generic ok/never fragment cases are test-cockpit-tiered-collector.sh's job
# (cluster 6, docs/test-plan/cockpit-observability.md).
#
# tier: T1
# covers: cockpit-collect/src/supervisor.rs UC-cockpit-observability-02 UC-cockpit-observability-03 UC-cockpit-observability-04
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
# host-reason: cockpit-collect dispatch; no shared state or timed services.

TMP="$(mktemp -d)"
trap 'chmod -R +w "$TMP" 2>/dev/null || true; rm -rf "$TMP"' EXIT
FRAG_DIR="$TMP/cockpit.d"
SNAP="$TMP/cockpit.env"
mkdir -p "$FRAG_DIR"
BASE_PATH="$PATH"
tl_config SPIRA_RUN="$TMP" SPIRA_REPO_MAP="$TMP/no-map" SPIRA_FAYTHS=t SPIRA_COCKPIT="$TMP"

# Merge fixture fragments through the real cockpit-collect, not a copy of its logic. The
# generic ok/never fragment cases live in test-cockpit-tiered-collector.sh (cluster 6,
# docs/test-plan/cockpit-observability.md); this suite only needs the merge to prove the
# real `queue` probe's keys reach cockpit.env.
run_merge() {
    env -i SPIRA_TOML="$SPIRA_TOML" PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_DB="$TMP/nodb" \
        FRAG_DIR="$FRAG_DIR" \
        cockpit-collect merge 2>/dev/null
}

# ============================================================
echo "queue keys reach cockpit.env via _probe_body_test:"

# Build a mock cockpit-collect probe that emits known SP_QUEUE_* keys.
MOCK_COCK="$TMP/mock-cockpit.sh"
cat > "$MOCK_COCK" <<'MOCK'
#!/usr/bin/env bash
case "${1:-}" in
    queue) echo "SP_QUEUE_DEPTH=2"; echo "SP_QUEUE_EJECTED=0"; echo "SP_QUEUE_RED=0" ;;
    *) exit 1 ;;
esac
MOCK
chmod +x "$MOCK_COCK"

rm -f "$FRAG_DIR"/*.env
printf '_PROBE_AT=0\n_PROBE_STATUS=never\n_PROBE_KILLED=0\n' > "$FRAG_DIR/queue.env"
env -i SPIRA_TOML="$SPIRA_TOML" PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_DB="$TMP/nodb" \
    FRAG_DIR="$FRAG_DIR" COCK="$MOCK_COCK" \
    cockpit-collect _probe_body_test queue 10 queue 2>/dev/null || true

frag="$(cat "$FRAG_DIR/queue.env" 2>/dev/null)"
want "_probe_body_test: fragment has _PROBE_STATUS=ok"  "_PROBE_STATUS=ok"  "$frag"
want "_probe_body_test: fragment has SP_QUEUE_DEPTH=2" "SP_QUEUE_DEPTH=2" "$frag"

run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want "after merge: SP_QUEUE_DEPTH=2 in cockpit.env" "SP_QUEUE_DEPTH='2'" "$snap"
want "after merge: SP_QUEUE_EJECTED in cockpit.env"  "SP_QUEUE_EJECTED="   "$snap"
want "after merge: SP_COLLECTOR_REV is always emitted" "SP_COLLECTOR_REV=" "$snap"

# ============================================================
echo
echo "fragment statuses decide which keys reach cockpit.env:"

rm -f "$FRAG_DIR"/*.env
frag() {   # frag <name> <status> [KEY=value ...]
    local n="$1" st="$2"; shift 2
    { printf '_PROBE_AT=100\n_PROBE_STATUS=%s\n_PROBE_KILLED=0\n' "$st"; printf '%s\n' "$@"; } > "$FRAG_DIR/$n.env"
}
frag aok ok SP_A_OK=1 SP_SHARED=first
frag bnever never SP_B_NEVER=9
frag ctimeout timeout SP_C_TIMEOUT=9
frag derror error SP_D_ERROR=9
frag estale stale SP_E_STALE=5
frag zok ok SP_SHARED=last
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want   "ok: contributes its keys"                  "SP_A_OK='1'"                  "$snap"
want   "ok: and its probe status"                  "_PROBE_STATUS_aok='ok'"       "$snap"
want   "never: records only its status"            "_PROBE_STATUS_bnever='never'" "$snap"
nowant "never: contributes no value key"           "SP_B_NEVER"                   "$snap"
want   "timeout: records only its status"          "_PROBE_STATUS_ctimeout='timeout'" "$snap"
nowant "timeout: contributes no value key"         "SP_C_TIMEOUT"                 "$snap"
want   "error: records only its status"            "_PROBE_STATUS_derror='error'" "$snap"
nowant "error: contributes no value key"           "SP_D_ERROR"                   "$snap"
want   "stale: keeps its last-known-good value"    "SP_E_STALE='5'"               "$snap"
want   "a key clash picks the alphabetically first" "SP_SHARED='first'"           "$snap"
nowant "and not the later fragment"                "SP_SHARED='last'"             "$snap"

# ============================================================
echo
tl_summary
