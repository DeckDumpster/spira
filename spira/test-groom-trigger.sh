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
# tier: T1
# covers: spira/groom-trigger.sh spira/conf.sh UC-ops-detection-remediation-33
# scar: groom-trigger.sh was absent, so the groomer never ran; without a trigger bead the groomer's partition was always empty.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# SPIRA_CONFIG_DIR: the directory holding spira-config on THIS invocation's own PATH, so
# every stripped-down PATH built below can still find it — conf.sh now refuses outright
# without it (sp-ubcgo: "wave 4.5: conf.sh becomes an eval of resolve"). Resolved once,
# from testenv's/the gate's own PATH (where the staged release's spira-config lives),
# never guessed.
SPIRA_CONFIG_DIR="$(command -v spira-config >/dev/null 2>&1 && dirname "$(command -v spira-config)" || true)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"

# GROOM_MAP: one repo in develop mode so the lane check admits the groom lane (and any
# custom SPIRA_GROOMER_LABEL, since _spira_expand_lanes reads env vars at expansion time).
GROOM_MAP="$T/groom-map"
printf 'test-repo | /tmp/test-repo | push | origin/main | | true | develop\n' > "$GROOM_MAP"

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

# STUB SPIRA-LC. Whether a trigger bead is still open is its lifecycle row (sp-mve9i, design
# §3.4), never bd status, and so is the backlog the score counts (its READY/WORKING/REWORK
# rows): `list` answers LC_LIST_OUTPUT (default: no rows).
STUB_LC="$T/stub-lc"
cat > "$STUB_LC" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
    list) printf '%s\n' "${LC_LIST_OUTPUT:-[]}"; exit 0 ;;
    *)    exit 2 ;;
esac
STUB
chmod +x "$STUB_LC"

# FIVE_OPEN: five unfinished lifecycle rows, satisfying SPIRA_GROOM_THRESHOLD=5 (default).
# Used as LC_LIST_OUTPUT wherever the short-circuit predicate must not suppress filing. The
# landed row is not backlog and must not count.
FIVE_OPEN='[{"bead_id":"x1","state":"READY"},{"bead_id":"x2","state":"WORKING"},{"bead_id":"x3","state":"REWORK"},{"bead_id":"x4","state":"READY"},{"bead_id":"x5","state":"READY"},{"bead_id":"x6","state":"LANDED"}]'

# Run groom-trigger.sh in a clean environment.
# SPIRA_CONF points to a nonexistent file so no real config is read; conf.sh defaults
# still apply. SPIRA_BD is the stub. BD_LOG_PATH is the argv capture file.
# LC_LIST_OUTPUT feeds the lifecycle rows (the backlog count and the dedup's open check);
# BD_LIST_OUTPUT feeds the dedup's bd list response. SPIRA_RUN=$T/run gives tests a writable, predictable lastpass dir.
# SPIRA_HOME="$HERE": groom-trigger.sh's own conf.sh derives SPIRA_HOME from $_spira_conf_here
# (where conf.sh sits) when unset, but that derivation is a local, unexported shell var —
# `maechen-trigger --home "$SPIRA_HOME" open-trigger-count ...` (lib.sh's
# spira_open_trigger_count) only feeds its own `--home` flag from it, which maechen-trigger
# uses for repo-registry lookups, NEVER for locating $SPIRA_TOML: that is
# spira_config::process::cfg's own locate_home_for_process, which reads SPIRA_HOME/
# SPIRA_RELEASE straight out of the environment and refuses when neither is an actual env
# var. Production has SPIRA_RELEASE ambient from the unit; this suite's env -i has neither,
# so maechen-trigger refused before ever calling bd, and groom-trigger.sh's dedup silently
# read that failure as "0 open" and fell through to the short-circuit instead (one source of
# config, per Ryan 2026-10-05).
run_trigger() {
    mkdir -p "$T/run"
    tl_config SPIRA_DB="$T/fixture.db" SPIRA_RUN="$T/run" SPIRA_REPO_MAP="$GROOM_MAP" SPIRA_BD="$STUB_BD"
    env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
        SPIRA_HOME="$HERE" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_CONF="$NONE" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_LC_BIN="$STUB_LC" LC_LIST_OUTPUT="${LC_LIST_OUTPUT:-[]}" \
        BD_LIST_OUTPUT="${BD_LIST_OUTPUT:-[]}" \
        groom-trigger.sh "$@" 2>&1
}

# ==========================================================================================
echo
echo "FILING: no open trigger — bd create is called with the right labels"
# ==========================================================================================
# POSITIVE CONTROL: BD_LIST_OUTPUT=[] means no open trigger; LC_LIST_OUTPUT satisfies
# the short-circuit threshold; we expect a create call.
: > "$BD_LOG"
out="$(BD_LIST_OUTPUT="[]" LC_LIST_OUTPUT="$FIVE_OPEN" run_trigger)"; rc=$?
is   "filing exits 0"                  0          "$rc"
want "bd create is called"             "create"   "$(cat "$BD_LOG")"
# The trigger bead must carry the scope label so the groomer's FAYTH_LABELS predicate
# finds it. The default scope label is "spira" and the groomer label is "groom".
want "create args include scope label" "spira"    "$(cat "$BD_LOG")"
want "create args include groom label" "groom"    "$(cat "$BD_LOG")"
# sp-mve9i: the backlog count reads the lifecycle rows; bd is never asked by status.
nowant "bd is never queried by status"  "--status" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "DEDUP: an open trigger exists — bd create is NOT called"
# ==========================================================================================
# POSITIVE CONTROL FOR THE DEDUP PATH. BD_LIST_OUTPUT holds a non-empty JSON array so
# the stub's list response looks like an already-open trigger bead. The create call must
# NOT appear in the log — if it does, the dedup check is broken.
: > "$BD_LOG"
BD_LIST_OUTPUT='[{"id":"sp-test","title":"Groomer pass"}]'
out="$(BD_LIST_OUTPUT='[{"id":"sp-test","title":"Groomer pass"}]' LC_LIST_OUTPUT='[{"bead_id":"sp-test","state":"READY"}]' run_trigger)"; rc=$?
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
# FAIL_BD returns [] for the dedup list query and exit 1 for create; the lifecycle rows
# (FIVE_OPEN) keep the short-circuit predicate from firing.
cat > "$FAIL_BD" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    list) printf '[]'; exit 0 ;;
    *) exit 1 ;;
esac
STUB
chmod +x "$FAIL_BD"
: > "$BD_LOG"
tl_config SPIRA_RUN="$T/run" SPIRA_DB="$T/fixture.db" SPIRA_REPO_MAP="$GROOM_MAP" SPIRA_BD="$FAIL_BD"
out="$(env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_CONF="$NONE" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_LC_BIN="$STUB_LC" LC_LIST_OUTPUT="$FIVE_OPEN" \
        BD_LIST_OUTPUT="[]" \
        groom-trigger.sh 2>&1)"; rc=$?
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
tl_config SPIRA_DB="$T/fixture.db" SPIRA_RUN="$T/run" SPIRA_REPO_MAP="$GROOM_MAP" \
    SPIRA_SCOPE_LABEL="myproject" SPIRA_GROOMER_LABEL="hygiene" SPIRA_BD="$STUB_BD"
out="$(BD_LIST_OUTPUT="[]" \
    env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_CONF="$NONE" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_LC_BIN="$STUB_LC" LC_LIST_OUTPUT="$FIVE_OPEN" \
    groom-trigger.sh 2>&1)"; rc=$?
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
tl_config SPIRA_DB="$T/fixture.db" SPIRA_RUN="$T/run" SPIRA_REPO_MAP="$GROOM_MAP" \
    SPIRA_SCOPE_LABEL="" SPIRA_GROOMER_LABEL="groom" SPIRA_BD="$STUB_BD"
out="$(env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_CONF="$NONE" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_LC_BIN="$STUB_LC" LC_LIST_OUTPUT="$FIVE_OPEN" \
    groom-trigger.sh 2>&1)"; rc=$?
is     "empty scope exits 0"               0     "$rc"
nowant "no leading comma in labels"        ",groom" "$(grep 'create' "$BD_LOG")"
want   "groomer label present without scope" "groom" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "LANE GUARD: no repository admits groom — trigger skips with one log line"
# ==========================================================================================
# POSITIVE CONTROL (law-absence-needs-a-positive-control): the next test proves the skip
# is lifted when a repo does admit the lane; if the trigger always skipped, both tests
# would exit 0 but the positive control would lack a bd create call.
CONSUME_MAP="$T/consume-map"
printf 'home-tg | /tmp/home-tg | push | origin/main | | | consume\n' > "$CONSUME_MAP"
printf 'plan-only | /tmp/plan-only | push | origin/main | | | consume\n' >> "$CONSUME_MAP"
: > "$BD_LOG"
tl_config SPIRA_DB="$T/fixture.db" SPIRA_REPO_MAP="$CONSUME_MAP" \
    SPIRA_HOME_REPO="home-tg" SPIRA_GROOMER_LABEL="groom" SPIRA_BD="$STUB_BD"
out_ng="$(env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_CONF="$NONE" \
    BD_LOG_PATH="$BD_LOG" \
    BD_LIST_OUTPUT="[]" \
    groom-trigger.sh 2>&1)"; rc_ng=$?
is     "no-groom-map: trigger exits 0"        0 "$rc_ng"
nowant "no-groom-map: no bd create call"      "create" "$(cat "$BD_LOG")"
want   "no-groom-map: logs skipping trigger"  "skipping trigger" "$out_ng"

# POSITIVE CONTROL: develop mode admits groom — trigger must fire.
: > "$BD_LOG"
tl_config SPIRA_DB="$T/fixture.db" SPIRA_RUN="$T/run" SPIRA_REPO_MAP="$GROOM_MAP" \
    SPIRA_HOME_REPO="spira" SPIRA_GROOMER_LABEL="groom" SPIRA_BD="$STUB_BD"
out_gp="$(env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_CONF="$NONE" \
    BD_LOG_PATH="$BD_LOG" \
    BD_LIST_OUTPUT="[]" \
    SPIRA_LC_BIN="$STUB_LC" LC_LIST_OUTPUT="$FIVE_OPEN" \
    groom-trigger.sh 2>&1)"; rc_gp=$?
is   "groom-admitted map: trigger exits 0"      0        "$rc_gp"
want "groom-admitted map: bd create is called"  "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "SHORT-CIRCUIT: low score — trigger skips without filing"
# ==========================================================================================
# POSITIVE CONTROL: the next test proves the skip is lifted when the score is at the
# threshold; if the trigger always skipped, both tests would exit 0 but the positive
# control would lack a bd create call.
: > "$BD_LOG"
out_sc="$(BD_LIST_OUTPUT="[]" LC_LIST_OUTPUT="[]" run_trigger)"; rc_sc=$?
is     "short-circuit exits 0"          0         "$rc_sc"
nowant "short-circuit: no create call"  "create"  "$(cat "$BD_LOG")"
want   "short-circuit: logs no-pass"    "no-pass" "$out_sc"

# ==========================================================================================
echo
echo "SHORT-CIRCUIT LIFTED: score at threshold — trigger fires"
# ==========================================================================================
# FIVE_OPEN delivers score=5 (its LANDED row does not count), matching the default threshold=5. Trigger must file.
: > "$BD_LOG"
out_sf="$(BD_LIST_OUTPUT="[]" LC_LIST_OUTPUT="$FIVE_OPEN" run_trigger)"; rc_sf=$?
is   "score-at-threshold exits 0"       0        "$rc_sf"
want "score-at-threshold: create called" "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "SHORT-CIRCUIT: custom SPIRA_GROOM_THRESHOLD respected"
# ==========================================================================================
# Two unfinished rows (score=2; the SUBMITTED one is past the builder) with threshold=2 must fire; with threshold=3 must not.
TWO_OPEN='[{"bead_id":"y1","state":"READY"},{"bead_id":"y2","state":"WORKING"},{"bead_id":"y3","state":"SUBMITTED"}]'
: > "$BD_LOG"
tl_config SPIRA_DB="$T/fixture.db" SPIRA_RUN="$T/run" SPIRA_REPO_MAP="$GROOM_MAP" SPIRA_GROOM_THRESHOLD="2" SPIRA_BD="$STUB_BD"
out_t2="$(env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_CONF="$NONE" \
    BD_LOG_PATH="$BD_LOG" \
    BD_LIST_OUTPUT="[]" \
    SPIRA_LC_BIN="$STUB_LC" LC_LIST_OUTPUT="$TWO_OPEN" \
    groom-trigger.sh 2>&1)"; rc_t2=$?
is   "threshold=2, score=2: exits 0"        0        "$rc_t2"
want "threshold=2, score=2: create called"  "create" "$(cat "$BD_LOG")"
: > "$BD_LOG"
tl_config SPIRA_GROOM_THRESHOLD="3" SPIRA_BD="$STUB_BD"
out_t3="$(env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_CONF="$NONE" \
    BD_LOG_PATH="$BD_LOG" \
    BD_LIST_OUTPUT="[]" \
    SPIRA_LC_BIN="$STUB_LC" LC_LIST_OUTPUT="$TWO_OPEN" \
    groom-trigger.sh 2>&1)"; rc_t3=$?
is     "threshold=3, score=2: exits 0"          0         "$rc_t3"
nowant "threshold=3, score=2: no create call"   "create"  "$(cat "$BD_LOG")"
want   "threshold=3, score=2: logs no-pass"     "no-pass" "$out_t3"

# ==========================================================================================
echo
echo "CONCURRENCY (gap G10): a second instance declines while the lock is held:"
# ==========================================================================================
# Deterministic, not timing-based: the test holds the lock file directly before running
# the script, so the script's own flock --nonblock is guaranteed to find it taken.
mkdir -p "$T/run"
exec 8>"$T/run/groom-trigger.lock"
flock -x 8

: > "$BD_LOG"
out_locked="$(BD_LIST_OUTPUT="[]" LC_LIST_OUTPUT="$FIVE_OPEN" run_trigger)"; rc_locked=$?
exec 8>&-

is     "locked: exits 0 (a skipped tick is not an error)" 0             "$rc_locked"
nowant "locked: bd create NOT called"                     "create"      "$(cat "$BD_LOG")"
want   "locked: logs the reason"                           "skipping"   "$out_locked"

echo
tl_summary
