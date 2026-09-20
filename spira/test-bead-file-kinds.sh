#!/usr/bin/env bash
#
# test-bead-file-kinds.sh — bead.sh file --kind routes non-work beads through the
#   correct bd type, carries no partition labels, and handles insight specially.
#
# covers: spira/bead.sh spira/schema.sh
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
# FAKE FAYTH. builder (plan lane) — only needed for the work-kind control.
# ---------------------------------------------------------------------------
cat > "$T/chamber/builder.fayth" <<'FAYTH'
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan}"
FAYTH_TOOLS="Bash,Edit,Write"
FAYTH_MODEL=claude-sonnet-5
FAYTH_MAX_CONCURRENT=4
FAYTH

# ---------------------------------------------------------------------------
# REPO MAP — for the --repo optional check.
# ---------------------------------------------------------------------------
cat > "$T/repo-map" <<'MAP'
testrepo | /tmp/test | push | origin/main | | true | plan
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
echo "test-bead-file-kinds.sh"

# ==========================================================================================
echo
echo "POSITIVE CONTROL: work kind via --for still works"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "work bead" --for builder --repo testrepo)"; rc=$?
is   "work: exits 0"          "0"       "$rc"
want "work: bd create called" "create"  "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "EVENT kind: filed with correct type, scope label, no partition label"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "something happened" --kind event)"; rc=$?
is   "event: exits 0"                    "0"          "$rc"
want "event: bd create called"           "create"     "$(cat "$BD_LOG")"
want "event: --type event passed"        "--type event" "$(cat "$BD_LOG")"
want "event: scope label present"        "testscope"  "$(cat "$BD_LOG")"
nowant "event: no partition label plan"  " plan"      "$(cat "$BD_LOG")"
nowant "event: no partition label incident" "incident" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "ESCALATION kind: filed with correct type"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "something is wrong" --kind escalation)"; rc=$?
is   "escalation: exits 0"               "0"               "$rc"
want "escalation: --type escalation"     "--type escalation" "$(cat "$BD_LOG")"
want "escalation: scope label"           "testscope"        "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "PROPOSAL kind: filed with correct type"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "an idea" --kind proposal)"; rc=$?
is   "proposal: exits 0"                 "0"                "$rc"
want "proposal: --type proposal"         "--type proposal"  "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "INSIGHT kind: created closed at P4 with insight label"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "a thing we learned" --kind insight)"; rc=$?
is   "insight: exits 0"                  "0"               "$rc"
want "insight: bd create called"         "create"          "$(cat "$BD_LOG")"
want "insight: --type chore"             "--type chore"    "$(cat "$BD_LOG")"
want "insight: --status closed"          "--status closed" "$(cat "$BD_LOG")"
want "insight: -p 4 (default priority)"  "-p 4"            "$(cat "$BD_LOG")"
want "insight: insight label present"    "insight"         "$(cat "$BD_LOG")"
nowant "insight: no partition label plan" " plan"          "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "INSIGHT kind with explicit priority: priority is not overridden"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "a thing we learned" --kind insight -p 2)"; rc=$?
is   "insight p2: exits 0"               "0"    "$rc"
want "insight p2: -p 2 passed"           "-p 2" "$(cat "$BD_LOG")"
nowant "insight p2: not -p 4"            "-p 4" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "NON-WORK with --repo: repo: label is added"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "an event" --kind event --repo testrepo)"; rc=$?
is   "event+repo: exits 0"               "0"            "$rc"
want "event+repo: repo:testrepo present" "repo:testrepo" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "NON-WORK without --repo: no repo: label required"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "an event" --kind event)"; rc=$?
is   "event no-repo: exits 0"            "0"      "$rc"
nowant "event no-repo: no repo: label"   "repo:"  "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "GATE kind: refused (bd-internal type)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "await something" --kind gate)"; rc=$?
is   "gate: exits non-zero"              "2"        "$rc"
want "gate: names bd gate"               "bd gate"  "$out"
nowant "gate: bd create NOT called"      "create"   "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "UNKNOWN kind: refused with error"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "a thing" --kind nosuchkind)"; rc=$?
is   "unknown kind: exits non-zero"      "2"           "$rc"
want "unknown kind: names the kind"      "nosuchkind"  "$out"
nowant "unknown kind: bd create NOT called" "create"   "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "MUTUAL EXCLUSION: --kind and --for together are refused"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "a thing" --kind event --for builder)"; rc=$?
is   "kind+for: exits non-zero"          "2"              "$rc"
want "kind+for: names exclusion"         "mutually exclusive" "$out"
nowant "kind+for: bd create NOT called"  "create"         "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "MISSING --for with no --kind: still required (work default)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "work bead" --repo testrepo)"; rc=$?
is   "no-for: exits non-zero"            "2"        "$rc"
want "no-for: mentions --for"            "--for"    "$out"

# ==========================================================================================
echo
echo "SUMMARY"
# ==========================================================================================
printf '\n  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
