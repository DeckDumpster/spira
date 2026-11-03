#!/usr/bin/env bash
#
# test-incident-dedup.sh — a second filing is a recurrence; a closed incident is reopened, including through the BSD date fallback
#
# Split from the one test-incident.sh for the 30 s cap: every `incident file` is a few seconds of
# real bd round trips, so the scenarios run as test-incident.sh, test-incident-file.sh,
# test-incident-dedup.sh and test-incident-recur.sh, each on a fixture of its own. Driven through
# the REAL incident.sh against a REAL bd (law-prefer-the-real-dependency).
#
# defect: sp-5ll6
# tier: T2
# covers: spira/incident.sh watchtower/src/* incident/* UC-ops-detection-remediation-01 UC-ops-detection-remediation-02 UC-ops-detection-remediation-03
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=60

echo "test-incident-dedup.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-incident-dedup
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up incident_dedup || { echo "test-incident-dedup: could not build a fixture database"; exit 1; }
BD_REAL="${SPIRA_BD:-bd}"
# sp-jgjvh: incident beads are work beads, so the dedup reads each one's state from its
# lifecycle row, never bd status. With no lifecycle store here, a stand-in spira-lc tells
# this database's bd story in lifecycle terms (testlib.sh lc_mirror_bd: open → READY,
# closed → LANDED); a machine that does not answer would leave every filing spooled.
lc_mirror_bd "$TMP/lc"


RUN="$TMP/run"; mkdir -p "$RUN"
SPOOL="$RUN/spool"
LOCK="$RUN/incident.lock"
ILOG="$RUN/incident.log"

# Run incident.sh with an explicit, named environment, pointing at the fixture database and
# isolated scratch directories (law-gates-run-in-a-clean-environment). HOME is the real one
# because bd and dolt read their credentials from it; SPIRA_PATH is passed because conf.sh
# rebuilds PATH from it; SPIRA_CONF names a non-existent file so a real spira.conf on this
# box cannot override any key the suite sets explicitly.
inc() {
    tl_config SPIRA_PATH="${SPIRA_PATH:-}" SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_MAIL="$TMP/mail"
    local extra=() _a
    for _a in "$@"; do
        case "$_a" in
            SPIRA_BD=*) tl_config "$_a" ;;
            *) extra+=("$_a") ;;
        esac
    done
    env -i SPIRA_RELEASE="$(tl_release)" HOME="$HOME" PATH="$PATH" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_SPOOL="$SPOOL" \
        SPIRA_INCIDENT_LOG="$ILOG" \
        SPIRA_INCIDENT_LOCK="$LOCK" \
        SPIRA_HOME="$HERE" \
        SPIRA_LC_BIN="$SPIRA_LC_BIN" \
        "${extra[@]+"${extra[@]}"}" incident.sh file "the test sweep" -
}

# Count open beads carrying the given external ref on the fixture database.
# NOTE: bd-embedded does not support --external-ref server-side filtering, so filter
# client-side via JSON, exactly as incident.sh open_incident does.
count_open() {
    timeout 5 bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --label spira,incident --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
count = 0
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        count += 1
print(count)
' "$1"
}

# Count ALL beads (any status) carrying the given external ref on the fixture database.
# Used by the close-then-refile regression: after the fix the closed bead is reopened so
# the total count stays 1; under the unfixed code a new bead is created and the count is 2.
count_all() {
    timeout 5 bd -C "$SPIRA_DB" list --all --status open,in_progress,closed --limit 0 --label spira,incident --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
count = 0
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        count += 1
print(count)
' "$1"
}

# ======================================================================================
echo
echo "the sequential case — a second call on the same ref is a recurrence, not a new bead:"
# ======================================================================================
# The sequential dedupe evidently already passed before the incident. This case is kept as
# a sanity check: if it breaks, the lock (which is only needed for concurrency) is not the
# culprit and the external-ref filter is probably broken.
printf 'first\n' | inc >/dev/null
printf 'second\n' | inc >/dev/null
n="$(count_open 'incident:the-test-sweep')"
is "two sequential calls leave exactly one open bead" "1" "$n"
log_count="$(grep -c 'recurred' "$ILOG" 2>/dev/null || true)"
is "the second call was recorded as a recurrence, not a filing" "1" "$log_count"

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

# ======================================================================================
echo
echo "close-then-refile dedup — a handed-on (SUBMITTED) bead within the lookback is reopened (sp-srgr6):"
# ======================================================================================
# THE DEFECT REPRODUCED. open_incident formerly filtered --status open,in_progress only.
# Closing a bead for a still-failing ref made it invisible: the next invocation found no
# open bead and filed a fresh one — repeating until 38 duplicate beads accumulated in 2 days.
#
# SEEN TO FAIL against the unfixed tree (law-a-regression-test-must-be-seen-to-fail):
# Filing first bead; closing it; filing same ref again produced:
#   2026-09-10T14:25:47Z incident: filed sp-7ed for incident:the-test-sweep   ← second bead
# count_all returned 2 (one closed original + one fresh open bead).
#
# After the fix: recent_closed_incident finds the closed bead within SPIRA_INCIDENT_DEDUP_LOOKBACK
# days and file_one reopens it with a recurrence note — count_all stays 1, count_open stays 1.
printf 'first payload\n' | inc >/dev/null
# Extract and close the bead that was just filed.
_ctr_id="$(timeout 5 bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        print(i["id"]); break
' 'incident:the-test-sweep')"
# Its closed row is fixture state, declared as data (sp-voip5), not a bd close. Its lifecycle
# row is pinned SUBMITTED — handed on, not terminal — so the bead can still absorb the
# recurrence; a terminal (LANDED) one files a fresh bead instead (sp-nmlna, the next case).
testdb_restate "${_ctr_id:-}" closed || true
printf '%s SUBMITTED\n' "${_ctr_id:-}" > "$TMP/lc/states"
> "$ILOG"
# File the same ref again — should reopen, not create a second bead.
printf 'second payload\n' | inc >/dev/null
n_all="$(count_all 'incident:the-test-sweep')"
is "close-then-refile leaves exactly one bead total (original reopened)" "1" "$n_all"
n_open="$(count_open 'incident:the-test-sweep')"
is "the original bead was reopened (now open)" "1" "$n_open"
log_reopen="$(grep -c 'reopened from closed' "$ILOG" 2>/dev/null || true)"
is "reopen path was logged, not a new filing" "1" "$log_reopen"

rm -f "$TMP/lc/states"
testdb_reset
mkdir -p "$RUN"
> "$ILOG"

# ======================================================================================
echo
echo "BSD date -v fallback — the lookback boundary still resolves without GNU date -d:"
# ======================================================================================
# _dedup_incident computes its lookback boundary with GNU \`date -d\`, falling back to BSD's
# \`date -v\` when -d is unsupported. A PATH with no GNU date must not silently skip the
# closed-bead lookback (which would make every close-then-refile file a fresh bead) — it
# must still resolve a boundary date via the BSD form.
# THE HOST HAS NO REAL BSD date — GNU date does not implement -v at all, so a shim that
# merely forwarded -v to the real binary would fail identically to -d and prove nothing.
# It translates BSD's `-v -Nd` into GNU's `-d "-N days"` against the same underlying
# binary, so the shim is what emulates BSD, not what happens to work by accident.
_gnu_date="$(command -v date)"
DATEDIR="$TMP/no-gnu-date"; mkdir -p "$DATEDIR"
cat > "$DATEDIR/date" <<DATESHIM
#!/usr/bin/env bash
# Reject GNU-style -d, forcing the caller's own fallback branch.
for _a in "\$@"; do [ "\$_a" = "-d" ] && { echo "date: illegal option -- d" >&2; exit 1; }; done
out=()
while [ "\$#" -gt 0 ]; do
    if [ "\$1" = "-v" ]; then
        shift
        off="\$1"; shift
        sign="\${off%%[0-9]*}"; rest="\${off#"\$sign"}"
        num="\${rest%%[a-zA-Z]*}"; unit="\${rest##*[0-9]}"
        case "\$unit" in
            d) word=days ;; H) word=hours ;; M) word=minutes ;; S) word=seconds ;;
            m) word=months ;; y) word=years ;; w) word=weeks ;;
            *) word=days ;;
        esac
        [ "\$sign" = "+" ] && sign=""
        out+=(-d "\${sign}\${num} \${word}")
    else
        out+=("\$1"); shift
    fi
done
exec "$_gnu_date" "\${out[@]}"
DATESHIM
chmod +x "$DATEDIR/date"
_bsd_since="$(PATH="$DATEDIR:$PATH" bash -c 'date -u -v -7d "+%Y-%m-%d" 2>/dev/null')"
is "the BSD date -v shim itself resolves a boundary" "yes" "$([ -n "$_bsd_since" ] && echo yes || echo no)"

printf 'bsd-date first payload\n' | PATH="$DATEDIR:$PATH" inc >/dev/null
_bd_id="$(timeout 5 bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        print(i["id"]); break
' 'incident:the-test-sweep')"
testdb_restate "${_bd_id:-}" closed || true     # fixture data, not a bd close (sp-voip5)
printf '%s SUBMITTED\n' "${_bd_id:-}" > "$TMP/lc/states"   # handed on, not terminal
> "$ILOG"
printf 'bsd-date second payload\n' | PATH="$DATEDIR:$PATH" inc >/dev/null
n_bsd_all="$(count_all 'incident:the-test-sweep')"
is "with no GNU date -d, the BSD fallback still finds the closed bead within lookback" "1" "$n_bsd_all"
log_bsd_reopen="$(grep -c 'reopened from closed' "$ILOG" 2>/dev/null || true)"
is "and reopens it rather than filing fresh" "1" "$log_bsd_reopen"
rm -f "$TMP/lc/states"

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

tl_summary
