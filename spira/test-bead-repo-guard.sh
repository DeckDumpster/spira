#!/usr/bin/env bash
#
# test-bead-repo-guard.sh — bead.sh file refuses a --repo that the repo map does not
#   resolve, before bd create, and lists every mapped repo name in the refusal.
#
# WHAT THIS SUITE IS GUARDING
# ---------------------------
# bead.sh filed a bead with any repo: label; nothing checked it against the map before
# the bead entered the graph, so a typo or a repository that was never added surfaced only
# later, in aeon.sh, as a parked bead nobody could work. The check belongs at the point of
# filing, reading the map through the same function (repo_names) the rest of the harness
# uses, so this guard and the rest of the harness cannot disagree about what a valid repo is.
#
# Properties guarded:
#   1. An unmapped --repo exits non-zero.
#   2. bd create is never called for it.
#   3. The refusal names every mapped repo.
#   4. A mapped --repo passes through unaffected (positive control, both routing paths).
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
# ---------------------------------------------------------
# Each refusal case is preceded by a success case on the same path so a broken check
# that always passes or always refuses is distinguishable.
#
# covers: spira/bead.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"
mkdir -p "$T/chamber" "$T/run"

# ---------------------------------------------------------------------------
# FAKE FAYTH. builder (plan lane) — needed for the --for work-kind path.
# ---------------------------------------------------------------------------
cat > "$T/chamber/builder.fayth" <<'FAYTH'
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan}"
FAYTH_TOOLS="Bash,Edit,Write"
FAYTH_MODEL=claude-sonnet-5
FAYTH_MAX_CONCURRENT=4
FAYTH

# ---------------------------------------------------------------------------
# REPO MAP. Two mapped repos, neither named "ghost-repo".
# ---------------------------------------------------------------------------
cat > "$T/repo-map" <<'MAP'
mapped-one | /tmp/mapped-one | push | origin/main | | true | plan
mapped-two | /tmp/mapped-two | push | origin/main | | true | plan
MAP

# ---------------------------------------------------------------------------
# STUB BD. Records argv for create calls; exits 0.
# ---------------------------------------------------------------------------
STUB_BD="$T/stub-bd"
BD_LOG="$T/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    create) printf '%s\n' "$*" >> "$BD_LOG_PATH"; printf 'sp-test\n'; exit 0 ;;
    *)      exit 0 ;;
esac
STUB
chmod +x "$STUB_BD"

run_bead() {
    : > "$BD_LOG"
    env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_DB="$T/db" \
        SPIRA_HOME="$T" \
        SPIRA_REPO="$T" \
        SPIRA_REPO_MAP="$T/repo-map" \
        SPIRA_MAECHEN_LABEL="maechen-sweep" \
        SPIRA_GROOMER_LABEL="groom" \
        SPIRA_SPIKE_LABEL="spike" \
        SPIRA_CZAR_LABEL="czar-trigger" \
        SPIRA_INCIDENT_LABEL="incident" \
        SPIRA_PLAN_LABEL="plan" \
        SPIRA_SCOPE_LABEL="testscope" \
        bash "$HERE/bead.sh" file "$@" 2>&1
}

echo
echo "test-bead-repo-guard.sh"

# ==========================================================================================
echo
echo "POSITIVE CONTROL: mapped repo passes through (work kind, --for)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "implement feature X" --for builder --repo mapped-one)"; rc=$?
is   "mapped work: exits 0"          "0"      "$rc"
want "mapped work: bd create called" "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "POSITIVE CONTROL: mapped repo passes through (non-work kind)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "an event" --kind event --repo mapped-two)"; rc=$?
is   "mapped event: exits 0"          "0"             "$rc"
want "mapped event: bd create called" "create"        "$(cat "$BD_LOG")"
want "mapped event: repo label present" "repo:mapped-two" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "REFUSE: unmapped repo (work kind, --for)"
# ==========================================================================================
# POSITIVE CONTROL above proved the same path can pass. Now prove it refuses.
: > "$BD_LOG"
out="$(run_bead "implement feature X" --for builder --repo ghost-repo)"; rc=$?
is     "ghost work: exits non-zero"        "2"           "$rc"
want   "ghost work: names repo"            "ghost-repo"  "$out"
want   "ghost work: names repo map"        "repo map"    "$out"
want   "ghost work: lists mapped-one"      "mapped-one"  "$out"
want   "ghost work: lists mapped-two"      "mapped-two"  "$out"
nowant "ghost work: bd create NOT called"  "create"      "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "REFUSE: unmapped repo (non-work kind)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "an event" --kind event --repo ghost-repo)"; rc=$?
is     "ghost event: exits non-zero"       "2"           "$rc"
want   "ghost event: names repo"           "ghost-repo"  "$out"
want   "ghost event: lists mapped repos"   "mapped-one"  "$out"
nowant "ghost event: bd create NOT called" "create"      "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "NON-WORK without --repo: guard does not fire (repo optional)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "an event" --kind event)"; rc=$?
is   "no-repo event: exits 0"           "0"      "$rc"
want "no-repo event: bd create called"  "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "SUMMARY"
# ==========================================================================================
printf '\n  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
