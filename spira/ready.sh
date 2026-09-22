#!/usr/bin/env bash
#
# ready.sh — the postflight. Answers exactly one question: can this installation
# receive work right now?
#
# doctor.sh is the preflight and stays read-only; this is the armed check it does not
# make. Run it after install, or after any state change that could prevent the loop
# from receiving work.
#
#   ready.sh     run all checks; exit 1 if any check FAILs or is unknown (?)
#
# A check that renders ? means its input could not be read — never a pass
# (law-detection-outranks-rejection).
#
# CHECKS (each names what it read):
#
#   sentinel    — timer active
#   world       — not halted; stamp file named with its mtime when present
#   database    — readable (bead count); statutes in force
#   ready work  — sentinel.sh --report sees open work
#   loom        — answers 200 at SPIRA_LOOM_ADDR inside SPIRA_LOOM_BUDGET_MS
#   agent       — configured agent binary is present  [WARNING BY DESIGN]
#   cockpit     — panel and health panes present
#
# AGENT ROW — WARNING by design. The loop is armed and will summon nothing when the
# agent is absent or uncredentialed. Under an ephemeral profile the agent is absent and
# the installation still answers; that is expected output, not a fault.
#
# WORLD ROW — two kinds of halt. The stamp file at $SPIRA_RUN/world.halted survives
# an uninstall: a reinstall keeps $SPIRA_RUN, so a brand-new install can carry a halt
# that predates it. The check compares the stamp's mtime to the sentinel unit's mtime
# to distinguish "you halted this" from "this halt predates the install".
#
# SEAMS (for test fixtures):
#   SPIRA_SYSTEMCTL    — replaces systemctl (from conf.sh)
#   SPIRA_LOOM_PROBE   — if set, called instead of the Python HTTP probe;
#                        must print "200 Nms", "ERR Nms reason", or "NNN Nms"
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

SC="${SPIRA_SYSTEMCTL:-systemctl}"

pass=0; warn=0; fail=0; unkn=0; skip=0
PASS() { printf '  pass  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; pass=$((pass+1)); }
WARN() { printf '  WARN  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; warn=$((warn+1)); }
FAIL() { printf '  FAIL  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; fail=$((fail+1)); }
UNKN() { printf '  ?     %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; unkn=$((unkn+1)); }
# SKIP: component deliberately not installed; counts as neither pass nor fail.
SKIP() { printf '  skip  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; skip=$((skip+1)); }

echo "spira ready"
echo

# =============================================================================
echo "sentinel"
# =============================================================================
# Resolve the timer name for this instance. spira_unit returns '?' when neither the
# instance-qualified nor the plain form is known to systemd — treat that as unknown
# state; the harness must not raise or clear an alarm from a unit it cannot identify
# (law-a-pattern-match-is-not-an-identity-check).
_sent_timer="$(spira_unit sentinel timer)"
if [ "$_sent_timer" = "?" ]; then
    UNKN "sentinel timer — unit not found (SPIRA_INSTANCE=${SPIRA_INSTANCE:-prod})" \
         "install.sh may not have run, or SPIRA_INSTANCE does not match what was installed."
elif "$SC" --user is-active --quiet "$_sent_timer" 2>/dev/null; then
    PASS "sentinel timer active — $_sent_timer"
else
    _sent_state="$("$SC" --user is-active "$_sent_timer" 2>/dev/null || true)"
    FAIL "sentinel timer not active — $_sent_timer is ${_sent_state:-inactive}" \
         "Start it: systemctl --user start $_sent_timer
    Or re-run install.sh to enable and start all timers."
fi

# =============================================================================
echo ""
echo "world"
# =============================================================================
# $SPIRA_RUN/world.halted is the stamp file world.sh stop writes. uninstall KEEPS
# $SPIRA_RUN, so a reinstall can carry a halt that predates it: compare the stamp's
# mtime to the sentinel unit file's to distinguish the two cases.
STAMP="$SPIRA_RUN/world.halted"
if [ ! -f "$STAMP" ]; then
    PASS "world not halted — no stamp at $STAMP"
else
    _halt_h="$(stat --format='%y' "$STAMP" 2>/dev/null | cut -d'.' -f1 || echo '?')"
    _halt_e="$(stat --format='%Y' "$STAMP" 2>/dev/null || echo 0)"
    _predates=""
    if [ "${_sent_timer:-?}" != "?" ]; then
        _unit_f="$HOME/.config/systemd/user/$_sent_timer"
        if [ -f "$_unit_f" ]; then
            _unit_e="$(stat --format='%Y' "$_unit_f" 2>/dev/null || echo 0)"
            [ "$_halt_e" -lt "$_unit_e" ] 2>/dev/null && _predates=1
        fi
    fi
    if [ -n "$_predates" ]; then
        FAIL "world halted — $STAMP (mtime $_halt_h) PREDATES this install" \
             "The halt in $SPIRA_RUN survived uninstall/reinstall; the loop will not summon.
    Clear it: $SPIRA_HOME/world.sh start"
    else
        FAIL "world halted — $STAMP (mtime $_halt_h)" \
             "The loop was stopped. Resume: $SPIRA_HOME/world.sh start"
    fi
fi

# =============================================================================
echo ""
echo "database"
# =============================================================================
if ! [ -d "$SPIRA_DB/.beads" ]; then
    FAIL "database absent — no .beads at $SPIRA_DB" \
         "Create it: bd -C $SPIRA_DB init"
elif ! _db_out="$(timeout 30 "$SPIRA_BD" -C "$SPIRA_DB" list --limit 0 --json 2>&1)"; then
    _rdy_dp=""
    if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
        _rdy_dp=3307
        if [ -f "$SPIRA_DOLT_DATA/dolt-server.yaml" ]; then
            _p="$(grep -E '^\s*port\s*:' "$SPIRA_DOLT_DATA/dolt-server.yaml" 2>/dev/null \
                | head -1 | sed 's/.*:\s*//' | tr -d ' ')"
            [ -n "$_p" ] && [ "$_p" -gt 0 ] 2>/dev/null && _rdy_dp="$_p"
            unset _p
        fi
    fi
    if [ -n "$_rdy_dp" ] && \
           "$SC" --user is-active --quiet dolt-beads.service 2>/dev/null && \
           ! (echo -n "" >/dev/tcp/127.0.0.1/"$_rdy_dp") 2>/dev/null; then
        UNKN "database: dolt-beads.service active, port $_rdy_dp not yet open — server is starting"
    else
        FAIL "database unreadable — $SPIRA_DB" \
             "$(printf '%s' "$_db_out" | head -2)"
    fi
    unset _rdy_dp
else
    _bead_count="$(printf '%s\n' "$_db_out" \
        | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null \
        || echo '?')"
    PASS "database readable — $SPIRA_DB ($_bead_count bead(s))"
    if _miss_out="$("$SPIRA_HOME/seed.sh" --list 2>/dev/null)"; then
        _missing="$(printf '%s\n' "$_miss_out" | grep -c ' -$' || true)"
        if [ "${_missing:-0}" -gt 0 ]; then
            WARN "$_missing shipped statute(s) not in this database" \
                 "Write them in: $SPIRA_HOME/seed.sh"
        else
            PASS "all shipped statutes in force"
        fi
    else
        UNKN "statutes — seed.sh --list failed; cannot verify statute coverage"
    fi
fi

# =============================================================================
echo ""
echo "ready work"
# =============================================================================
# sentinel.sh --report lists open beads under SPIRA_GOAL. Each line of open work is
# indented with two spaces. A timeout guards against a slow or stuck database.
if ! _rep="$(timeout 20 "$SPIRA_HOME/sentinel.sh" --report 2>&1)"; then
    UNKN "ready work — sentinel.sh --report failed or timed out" \
         "$(printf '%s' "$_rep" | head -2)"
else
    _n_open="$(printf '%s\n' "$_rep" | grep -c '^  ' || true)"
    if [ "${_n_open:-0}" -gt 0 ]; then
        PASS "sentinel sees $_n_open open bead(s) under $SPIRA_GOAL"
        printf '%s\n' "$_rep" | head -7 | sed 's/^/        /'
    else
        WARN "sentinel sees no open work under $SPIRA_GOAL" \
             "No beads are currently open under this goal — nothing to summon."
    fi
fi

# =============================================================================
echo ""
echo "loom"
# =============================================================================
# Probe Loom at $SPIRA_LOOM_ADDR/api/beads. python3 is a fatal doctor.sh requirement
# so it is always available. SPIRA_LOOM_PROBE overrides the HTTP call for test fixtures.
_loom_url="http://$SPIRA_LOOM_ADDR/api/beads"
if ! [ -x "${SPIRA_LOOM_BIN:-}" ]; then
    _loom_unit="$(spira_unit loom service)"
    if [ "$_loom_unit" = "?" ]; then
        # Binary absent and no unit found: installer deliberately skipped loom (cargo absent).
        # The loop is unaffected; this is not a fault.
        SKIP "loom not installed — binary not built at ${SPIRA_LOOM_BIN:-<unset>}" \
             "Build it: cd $SPIRA_REPO/loom && cargo build --release"
    else
        # Binary absent but unit is installed: inconsistent state; probe should have worked.
        UNKN "loom — binary not built at ${SPIRA_LOOM_BIN:-<unset>}; cannot probe $_loom_url" \
             "Build it: cd $SPIRA_REPO/loom && cargo build --release"
    fi
else
    _loom_probe_cmd="${SPIRA_LOOM_PROBE:-}"
    if [ -n "$_loom_probe_cmd" ]; then
        _loom_result="$($_loom_probe_cmd "$_loom_url" "$SPIRA_LOOM_BUDGET_MS" 2>/dev/null)"
    else
        # Python probe: measures wall-clock ms inside the budget ceiling.
        # Outputs "200 Nms" on success, "ERR Nms <reason>" otherwise.
        _loom_result="$(python3 - "$_loom_url" "$SPIRA_LOOM_BUDGET_MS" 2>/dev/null <<'PYEOF'
import sys, urllib.request, time
url = sys.argv[1]
budget_ms = float(sys.argv[2])
t0 = time.monotonic()
try:
    r = urllib.request.urlopen(url, timeout=budget_ms / 1000)
    ms = int((time.monotonic() - t0) * 1000)
    sys.stdout.write("%d %dms\n" % (r.status, ms))
except Exception as e:
    ms = int((time.monotonic() - t0) * 1000)
    sys.stdout.write("ERR %dms %s\n" % (ms, str(e)[:100]))
PYEOF
        )"
    fi
    case "$_loom_result" in
        "200 "*)
            _loom_ms="${_loom_result#200 }"; _loom_ms="${_loom_ms%ms}"
            if [ "${_loom_ms:-0}" -le "${SPIRA_LOOM_BUDGET_MS:-1500}" ] 2>/dev/null; then
                PASS "loom answers 200 at $_loom_url (${_loom_ms}ms, budget ${SPIRA_LOOM_BUDGET_MS}ms)"
            else
                WARN "loom answers 200 but over budget at $_loom_url (${_loom_ms}ms > ${SPIRA_LOOM_BUDGET_MS}ms)" \
                     "Requests may be refused. Raise SPIRA_LOOM_BUDGET_MS or investigate Loom latency."
            fi ;;
        "ERR "*)
            _loom_detail="${_loom_result#ERR }"; _loom_detail="${_loom_detail#*ms }"
            _loom_svc="$(spira_unit loom service)"
            FAIL "loom does not answer at $_loom_url" \
                 "${_loom_detail:-connection refused}
    Is the loom service active? systemctl --user status ${_loom_svc:-spira-loom.service}"
            ;;
        "")
            UNKN "loom — probe returned no output for $_loom_url" \
                 "Python3 HTTP probe failed to run."
            ;;
        *)
            _loom_code="${_loom_result%% *}"
            _loom_svc="$(spira_unit loom service)"
            FAIL "loom returned HTTP $_loom_code at $_loom_url (expected 200)" \
                 "${_loom_svc:-spira-loom.service} may be degraded."
            ;;
    esac
fi

# =============================================================================
echo ""
echo "agent"
# =============================================================================
# WARNING BY DESIGN. The loop is armed and will summon nothing when the agent is absent
# or uncredentialed. Under an ephemeral profile the binary is absent and the installation
# still answers — that is expected output, not a fault. The row names whichever agent
# SPIRA_AGENT configures (default: claude) and distinguishes absent from present so the
# operator can tell the two apart without running the binary.
AGENT="${SPIRA_AGENT:-claude}"
if _agent_path="$(command -v "$AGENT" 2>/dev/null)"; then
    WARN "agent present — SPIRA_AGENT=$AGENT at $_agent_path" \
         "WARNING by design: present but may lack credentials. Under an ephemeral profile this is expected."
else
    WARN "agent absent — SPIRA_AGENT=$AGENT not on PATH; no summons will succeed" \
         "Install the agent or set SPIRA_AGENT and SPIRA_PATH in spira.conf.
    Under an ephemeral profile this is expected output, not a fault."
fi

# =============================================================================
echo ""
echo "cockpit"
# =============================================================================
# SNAPSHOT FRESHNESS. A missing or stale cockpit.env means the collector is not
# writing — the exact condition that went undetected for 71 minutes while every
# other health surface reported OK (sp-itsy). Check before the pane checks because
# a pane that cannot read any snapshot is useless even if the pane itself exists.
_rdy_snap="$SPIRA_RUN/cockpit.env"
_rdy_stale_s="${SPIRA_COCKPIT_STALE_S:-120}"
if [ ! -f "$_rdy_snap" ]; then
    FAIL "no cockpit snapshot at $_rdy_snap — collector has not written yet" \
         "Check: systemctl --user status $(spira_unit cockpit service)"
else
    _rdy_snap_age=$(( $(date +%s) - $(stat --format='%Y' "$_rdy_snap" 2>/dev/null || echo 0) ))
    if [ "$_rdy_snap_age" -gt "$_rdy_stale_s" ]; then
        FAIL "cockpit snapshot stale — last written ${_rdy_snap_age}s ago (limit ${_rdy_stale_s}s)" \
             "The collector is not writing. Check: systemctl --user status $(spira_unit cockpit service)"
    else
        PASS "cockpit snapshot fresh — $_rdy_snap (${_rdy_snap_age}s old)"
    fi
fi
unset _rdy_snap _rdy_snap_age _rdy_stale_s

# Check for the two tagged panes (panel, health) that layout.sh creates. Each carries
# the tmux pane-scoped option @cockpit set to its role.
if ! command -v tmux >/dev/null 2>&1; then
    WARN "tmux not on PATH — cannot check cockpit panes" \
         "Install tmux or set SPIRA_PATH in spira.conf."
elif ! tmux list-panes -a >/dev/null 2>&1; then
    WARN "no tmux server running — cockpit panes absent" \
         "Build the cockpit: $SPIRA_COCKPIT/layout.sh up"
else
    for _ctag in panel health; do
        _cpane="$(tmux list-panes -a -F '#{@cockpit} #{pane_id}' 2>/dev/null \
            | awk -v t="$_ctag" '$1==t {print $2; exit}')"
        if [ -n "$_cpane" ]; then
            PASS "cockpit $_ctag pane present — $_cpane"
        else
            WARN "cockpit $_ctag pane absent" \
                 "Rebuild the cockpit: $SPIRA_COCKPIT/layout.sh up"
        fi
    done
fi

# =============================================================================
echo ""
if [ "$fail" -gt 0 ] || [ "$unkn" -gt 0 ]; then
    printf '%d pass, %d WARN, %d FAIL, %d unknown, %d skip — NOT READY: loop may not receive work.\n' \
        "$pass" "$warn" "$fail" "$unkn" "$skip"
    exit 1
fi
printf '%d pass, %d WARN, %d skip — armed: loop is ready to receive work.\n' "$pass" "$warn" "$skip"
exit 0
