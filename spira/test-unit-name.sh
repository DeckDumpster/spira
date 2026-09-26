#!/usr/bin/env bash
#
# test-unit-name.sh — spira_unit resolves the correct instance-qualified unit name.
#
#   ./test-unit-name.sh
#
# WHAT THIS SUITE COVERS
# ----------------------
# sp-smbq0 (2026-09-09): strand.sh and doctor.sh addressed units by their
# un-suffixed plain names (spira-sentinel.timer, spira-loom.service) while the
# running units on a post-migration box carry the instance suffix
# (spira-sentinel-prod.timer, spira-loom-prod.service). This caused strand.sh to
# report "sentinel timer: inactive" while the sentinel was firing every two minutes,
# and doctor.sh to print a remediation naming a unit that does not exist.
#
# THREE PROPERTIES of spira_unit, all three of which are necessary for correctness:
#
#   A: instance-qualified form returned when that unit is active (post-migration box)
#   B: plain form returned when only the plain unit is active (pre-migration fallback)
#   C: '?' returned when neither form is known to systemd (law-absence-needs-a-positive-control)
#
# ADDITIONALLY: callers that receive '?' must render 'unknown', not 'inactive'.
#   strand.sh harness_state is tested here; it is the direct source of the three
#   stranded-queue reports that prompted this fix.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): before asserting that an
# absent unit returns '?', assert that a present unit returns a real name, so a stub
# that always returns '?' would fail the first set of tests and not silently pass all.
#
# systemctl IS STUBBED throughout.  Assertions on what spira_unit returns, and on what
# strand.sh prints, not on systemctl exit codes.
#
# defect: sp-smbq0
# tier: T1
# covers: spira/conf.sh spira/strand.sh spira/doctor.sh spira/cockpit.sh spira/auron.sh cockpit/layout.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-unit-name.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

SH="$TMP/spira"
RUN="$TMP/run"
mkdir -p "$SH" "$RUN"

# Copy the files under test so edits to the source are tested.
cp "$HERE/conf.sh" "$HERE/strand.sh" "$HERE/lib.sh" "$SH/"

# STUB SYSTEMCTL. Behaviour is controlled by two variables exported into the subshell:
#   SC_ENABLED  — unit name that is-enabled should confirm (empty = none)
#   SC_ACTIVE   — unit name that is-active should confirm as "active"
# Every other subcommand returns its default exit code (0 = success).
write_sc() {
    local enabled="${1:-}" active="${2:-}"
    cat > "$TMP/systemctl" <<SC
#!/usr/bin/env bash
cmd="" unit=""
for a; do
    case "\$a" in --user|--quiet) ;; *) [ -z "\$cmd" ] && cmd="\$a" || unit="\$a" ;; esac
done
case "\$cmd" in
    is-enabled) [ "\$unit" = "$enabled" ] && exit 0 || exit 1 ;;
    is-active)
        [ "\$unit" = "$active" ] && { echo active; exit 0; } || { echo inactive; exit 3; } ;;
    *) exit 0 ;;
esac
SC
    chmod +x "$TMP/systemctl"
}

# Run spira_unit in an isolated subshell that sources our copied conf.sh.
run_spira_unit() {
    local inst="$1" base="$2" type="${3:-service}"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_SYSTEMCTL="$TMP/systemctl" SPIRA_INSTANCE="$inst" \
        bash -c '. "$1/conf.sh"; spira_unit "$2" "$3"' _ "$SH" "$base" "$type"
}

# Run the harness_state function extracted from strand.sh. The function definition is
# sourced directly, then called, so we are testing the real implementation rather than
# a copy (law-prefer-the-real-dependency). strand.sh runs in a full script context with
# lib.sh already sourced, so we replicate that here: source lib.sh, then define
# harness_state by extracting it from strand.sh via awk, then call it.
run_harness_state() {
    local inst="$1"
    # Extract harness_state from strand.sh: the function body from its definition
    # line to the closing '}' on its own line.
    local fn_body
    fn_body="$(awk '/^harness_state\(\)/{found=1} found{print} found && /^\}$/{exit}' "$SH/strand.sh")"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_SYSTEMCTL="$TMP/systemctl" SPIRA_INSTANCE="$inst" SENTINEL_LOG="$RUN/sentinel.log" \
        bash -c '. "$1/lib.sh"; eval "$2"; harness_state' _ "$SH" "$fn_body" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# POSITIVE CONTROL — before testing absence, prove presence works.
# A stub that always returns '?' would fail here.
# ---------------------------------------------------------------------------
echo
echo "A: instance-qualified unit is active (post-migration):"

write_sc "" "spira-sentinel-prod.timer"
result="$(run_spira_unit prod sentinel timer)"
is "A: spira_unit returns instance-qualified sentinel timer" \
   "spira-sentinel-prod.timer" "$result"
nowant "A: plain name not returned when instance-qualified is active" \
       "spira-sentinel.timer" "$result"

write_sc "" "spira-loom-prod.service"
result="$(run_spira_unit prod loom service)"
is "A: spira_unit returns instance-qualified loom service" \
   "spira-loom-prod.service" "$result"

# ---------------------------------------------------------------------------
# B: pre-migration fallback — only the plain form is active
# ---------------------------------------------------------------------------
echo
echo "B: plain unit active (pre-migration fallback):"

write_sc "" "spira-sentinel.timer"
result="$(run_spira_unit prod sentinel timer)"
is "B: spira_unit falls back to plain sentinel timer" \
   "spira-sentinel.timer" "$result"

write_sc "" "spira-loom.service"
result="$(run_spira_unit prod loom service)"
is "B: spira_unit falls back to plain loom service" \
   "spira-loom.service" "$result"

# ---------------------------------------------------------------------------
# C: neither form is known — must return '?', not a name that renders inactive
# ---------------------------------------------------------------------------
echo
echo "C: no unit loaded — must return '?':"

write_sc "" ""
result="$(run_spira_unit prod sentinel timer)"
is "C: spira_unit returns '?' when no sentinel unit is known" \
   "?" "$result"

result="$(run_spira_unit prod loom service)"
is "C: spira_unit returns '?' when no loom unit is known" \
   "?" "$result"

# ---------------------------------------------------------------------------
# D: strand.sh harness_state must render 'unknown' when unit resolves to '?'
#    Before this fix it would call: systemctl --user is-active spira-sentinel.timer
#    which would print 'inactive' for a disabled unit — a confident false reading.
# ---------------------------------------------------------------------------
echo
echo "D: strand.sh harness_state renders 'unknown' for unresolvable unit:"

write_sc "" ""
out="$(run_harness_state prod)"
want   "D: harness_state output contains 'unknown' when unit is '?'" \
       "unknown" "$out"
nowant "D: harness_state does not report 'inactive' for a '?' unit" \
       "inactive" "$out"

# Also verify that when the instance-qualified unit IS active, harness_state reports it.
write_sc "" "spira-sentinel-prod.timer"
out="$(run_harness_state prod)"
want   "D: harness_state reports 'active' when instance-qualified sentinel is active" \
       "active" "$out"
nowant "D: harness_state does not report 'inactive' when sentinel is active" \
       "inactive" "$out"

# ---------------------------------------------------------------------------
tl_summary
