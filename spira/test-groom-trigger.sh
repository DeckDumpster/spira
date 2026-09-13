#!/usr/bin/env bash
#
# test-groom-trigger.sh — groom-trigger.sh: files the groomer's trigger bead and
# deduplicates when an open trigger already exists.
#
#   ./test-groom-trigger.sh
#
# WHAT THIS SUITE IS GUARDING
# ---------------------------
# groom-trigger.sh is the missing scanner whose absence kept the groomer from ever
# running. The suite guards three properties:
#
#   1. When no trigger is open, it files a bead with SPIRA_SCOPE_LABEL and
#      SPIRA_GROOMER_LABEL (the groomer's FAYTH_LABELS partition).
#
#   2. When an open trigger already exists, it exits 0 WITHOUT filing another bead
#      (dedup). A second trigger would queue a redundant pass; with FAYTH_MAX_CONCURRENT=1
#      the second trigger would wait forever for the first, growing unboundedly.
#
#   3. The labels used are the same ones FAYTH_LABELS expands to, so the trigger
#      lands in exactly the partition the groomer queries. Tested by asserting both
#      SPIRA_SCOPE_LABEL and SPIRA_GROOMER_LABEL appear in the bd create call.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# -------------------------------------------------------
# The dedup check requires a stub that returns a non-empty JSON array for bd list.
# The filing check requires a stub that returns '[]'. Both are tested explicitly so
# a stub bug cannot make dedup and filing look identical.
#
# STUB BD (law-gates-run-in-a-clean-environment)
# ----------------------------------------------
# groom-trigger.sh calls bd for list (dedup query) and create (filing). A real bd
# call requires a live Dolt server and a seeded database. The stub records argv to
# a log and returns configurable JSON for list and 0 for create. This proves
# groom-trigger.sh sends the right arguments to bd; bd's correctness is tested in
# suites that use testdb.sh.
#
# covers: spira/groom-trigger.sh spira/conf.sh
# scar: groom-trigger.sh was absent, so the groomer never ran; without a trigger bead the groomer's partition was always empty.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

TRIGSH="$HERE/groom-trigger.sh"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"

# Build a stub bd. The stub checks $BD_LIST_OUTPUT to decide what to return for
# 'list' subcommands and records all argv to BD_LOG. For all other subcommands it
# exits 0 with nothing on stdout.
#
# STRIP -C <db>. Every bd call from groom-trigger.sh begins with `-C <path>`. The stub
# records the full argv (including -C) then strips the prefix before the case so that
# the subcommand is always $1 at dispatch time. A case on $1 without stripping would
# always see `-C` and fall through to `*`, making all list calls return empty and
# defeating the dedup test.
STUB_BD="$T/stub-bd"
BD_LOG="$T/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG_PATH"
# Strip -C <path> so the subcommand is always in $1 for the case.
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    list) printf '%s\n' "${BD_LIST_OUTPUT:-[]}"; exit 0 ;;
    *)    exit 0 ;;
esac
STUB
chmod +x "$STUB_BD"

# Run groom-trigger.sh in a clean environment.
# SPIRA_CONF points to a nonexistent file so no real config is read; conf.sh defaults
# still apply. SPIRA_BD is the stub. BD_LOG_PATH is the argv capture file.
run_trigger() {
    env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        BD_LIST_OUTPUT="${BD_LIST_OUTPUT:-[]}" \
        SPIRA_DB="$T/fixture.db" \
        bash "$TRIGSH" "$@" 2>&1
}

# ==========================================================================================
echo
echo "FILING: no open trigger — bd create is called with the right labels"
# ==========================================================================================
# POSITIVE CONTROL: BD_LIST_OUTPUT=[] means no open trigger; we expect a create call.
: > "$BD_LOG"
BD_LIST_OUTPUT="[]" out="$(BD_LIST_OUTPUT="[]" run_trigger)"; rc=$?
is   "filing exits 0"                  0          "$rc"
want "bd create is called"             "create"   "$(cat "$BD_LOG")"
# The trigger bead must carry the scope label so the groomer's FAYTH_LABELS predicate
# finds it. The default scope label is "spira" and the groomer label is "groom".
want "create args include scope label" "spira"    "$(cat "$BD_LOG")"
want "create args include groom label" "groom"    "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "DEDUP: an open trigger exists — bd create is NOT called"
# ==========================================================================================
# POSITIVE CONTROL FOR THE DEDUP PATH. BD_LIST_OUTPUT holds a non-empty JSON array so
# the stub's list response looks like an already-open trigger bead. The create call must
# NOT appear in the log — if it does, the dedup check is broken.
: > "$BD_LOG"
BD_LIST_OUTPUT='[{"id":"sp-test","title":"Groomer pass"}]'
out="$(BD_LIST_OUTPUT='[{"id":"sp-test","title":"Groomer pass"}]' run_trigger)"; rc=$?
is     "dedup exits 0"              0           "$rc"
nowant "bd create NOT called"       "create"    "$(cat "$BD_LOG")"
want   "list IS called for dedup"   "list"      "$(cat "$BD_LOG")"
want   "dedup log mentions skipping" "skipping"     "$out"

# ==========================================================================================
echo
echo "ERROR: bd create fails — exit code is 1"
# ==========================================================================================
# If filing fails (bd returns non-zero for create), groom-trigger.sh must exit 1 so the
# service records a failure and the timer does not silently mark success for a broken pass.
FAIL_BD="$T/fail-bd"
cat > "$FAIL_BD" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
    -C) shift; shift ;;&
    list) printf '[]'; exit 0 ;;
    *)   exit 1 ;;
esac
STUB
chmod +x "$FAIL_BD"
: > "$BD_LOG"
out="$(env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$FAIL_BD" \
        BD_LOG_PATH="$BD_LOG" \
        BD_LIST_OUTPUT="[]" \
        SPIRA_DB="$T/fixture.db" \
        bash "$TRIGSH" 2>&1)"; rc=$?
is   "create failure exits 1" 1 "$rc"
want "failure log mentions ERROR" "ERROR" "$out"

# ==========================================================================================
echo
echo "LABELS: custom SPIRA_SCOPE_LABEL and SPIRA_GROOMER_LABEL are used"
# ==========================================================================================
# Pin to non-defaults (law-gates-run-in-a-clean-environment). A test asserting the
# default passes even if the code has the literal written in; a non-default is the
# thing the config key exists to stop.
: > "$BD_LOG"
BD_LIST_OUTPUT="[]"
out="$(BD_LIST_OUTPUT="[]" \
    env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_SCOPE_LABEL="myproject" \
        SPIRA_GROOMER_LABEL="hygiene" \
    bash "$TRIGSH" 2>&1)"; rc=$?
is   "custom labels exits 0"                        0           "$rc"
want "custom scope label in create args"            "myproject" "$(cat "$BD_LOG")"
want "custom groomer label in create args"          "hygiene"   "$(cat "$BD_LOG")"
# The --label argument must not include the old defaults when overridden. Extract the
# partition labels only (strip delivers:* labels whose path may contain the default
# scope name as a directory component — e.g. /path/to/.runtime/spira/groom.log).
label_arg="$(grep 'create' "$BD_LOG" | grep -oP '(?<=--label )\S+')"
partition_labels="$(printf '%s' "${label_arg:-}" | tr ',' '\n' | grep -v '^delivers:' | paste -sd, -)"
nowant "default scope label not in --label" "spira"  "${partition_labels:-}"
nowant "default groom label not in --label" ",groom" "${partition_labels:-}"

# ==========================================================================================
echo
echo "EMPTY SCOPE: SPIRA_SCOPE_LABEL='' produces only the groomer label (no leading comma)"
# ==========================================================================================
# SPIRA_SCOPE_LABEL='' is a valid and meaningful value (no scope restriction). A leading
# comma in the label string would produce a malformed bd --label argument and could match
# nothing or everything. Verify no leading comma appears in the bd create call.
: > "$BD_LOG"
out="$(env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_SCOPE_LABEL="" \
        SPIRA_GROOMER_LABEL="groom" \
    bash "$TRIGSH" 2>&1)"; rc=$?
is     "empty scope exits 0"               0     "$rc"
nowant "no leading comma in labels"        ",groom" "$(grep 'create' "$BD_LOG")"
want   "groomer label present without scope" "groom" "$(cat "$BD_LOG")"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
