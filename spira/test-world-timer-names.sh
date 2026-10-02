#!/usr/bin/env bash
#
# test-world-timer-names.sh — world.sh resolves timer unit names per SPIRA_INSTANCE.
#
#   ./test-world-timer-names.sh
#
# WHAT THIS SUITE COVERS
# ----------------------
# sp-4biz (2026-09-08): TIMERS held the plain unit names (spira-sentinel.timer, etc.)
# while the per-instance migration (sp-trn1) renamed the live units to
# spira-sentinel-prod.timer. systemctl stop exits 0 against a disabled unit, so
# world.sh stop printed "stopped spira-sentinel.timer" while the live timer kept firing.
# The loop re-summoned aeons within two minutes of a "successful" halt.
#
# TWO PROPERTIES, each verified under BOTH naming schemes:
#
#   1. stop targets the RESOLVED sentinel timer — the one that is enabled or active —
#      confirmed by reading the call log, not by stop's exit code. A test that asserts
#      on exit code passes while leaving the sentinel running (systemctl stop exits 0
#      for a disabled unit, which is exactly what hid the original defect).
#
#   2. status reports the state of the RESOLVED timer, not a hardcoded plain name.
#      Before the fix, status read spira-sentinel.timer (inactive) while
#      spira-sentinel-prod.timer was active.
#
# TWO SCENARIOS per property:
#   A: post-migration — instance-qualified unit is enabled/active (spira-sentinel-prod.timer)
#   B: pre-migration fallback — instance-qualified not enabled; plain name is active
#
# ADDITIONALLY: --hard stop under both naming schemes for watcher units.
#   The rename replaced spira-watch@<name>.service (template instance) with
#   spira-watch-<name>-<instance>.service (plain per-instance). world.sh --hard must
#   find and stop whichever form is running.
#
# systemctl IS STUBBED throughout. Assertions are on what world.sh ASKED systemd to do.
#
# defect: sp-4biz
# tier: T1
# covers: spira-world/src/bin/world.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-world-timer-names.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

SH="$TMP/spira"
RUN="$TMP/run"
CALLS="$TMP/sc-calls"
export CALLS
mkdir -p "$SH" "$RUN"

WORLD_BIN="$(command -v world || true)"
[ -n "$WORLD_BIN" ] && [ -x "$WORLD_BIN" ] || { echo "test-world-timer-names.sh: the world binary is not on PATH" >&2; exit 1; }
cp "$WORLD_BIN" "$SH/world.sh"; chmod +x "$SH/world.sh"
cp "$HERE/conf.sh" "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
printf '#!/usr/bin/env bash
exit 0
' > "$SH/slay"; chmod +x "$SH/slay"

# The six TIMER_PRIORITY bases this suite is NOT about — always resolvable by their
# plain (unqualified) name, so a scenario that only configures sentinel's state never
# makes world.sh refuse over an UNRELATED base it never meant to put in play
# (sp-ivfu3-2: resolution is now by EXISTENCE — list-unit-files/cat — not by
# enabled/active state, and a base nothing claims to exist is a REFUSAL, not a quiet
# plain-name default).
OTHER_BASES_PLAIN="spira-summon.timer spira-ops.timer spira-watchtower.timer spira-archivist.timer spira-archive.timer spira-skew.timer"

# write_sc ENABLED_TIMER ACTIVE_TIMER LEGACY_WATCH INSTANCE_WATCH [SENTINEL_EXISTING]
#   ENABLED_TIMER     — unit name that is-enabled should confirm (empty = none)
#   ACTIVE_TIMER      — unit name that is-active should confirm as "active"
#   LEGACY_WATCH      — unit to emit for list-units 'spira-watch@*' (empty = none)
#   INSTANCE_WATCH    — unit to emit for list-units 'spira-watch-*' (empty = none)
#   SENTINEL_EXISTING — space-separated sentinel unit name(s) list-unit-files/cat must
#                       agree exist (sp-ivfu3-2); default: ENABLED_TIMER and
#                       ACTIVE_TIMER, which is right for every scenario before that bead
#                       (a timer this suite marks enabled or active obviously exists).
write_sc() {
    local enabled="${1:-}" active="${2:-}" legacy_watch="${3:-}" inst_watch="${4:-}"
    local sentinel_existing="${5:-$enabled $active}"
    local existing="$sentinel_existing $OTHER_BASES_PLAIN"
    cat > "$TMP/systemctl" <<SC
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$CALLS"
cmd=""; unit=""
for a; do
    case "\$a" in --user|--state=*|--no-legend) ;; *) [ -z "\$cmd" ] && cmd="\$a" || unit="\$a" ;; esac
done
case "\$cmd" in
is-enabled)  [ "\$unit" = "$enabled" ] && exit 0 || exit 1 ;;
is-active)
    [ "\$unit" = "$active" ] && { echo active; exit 0; } || { echo inactive; exit 3; } ;;
list-unit-files)
    case "\$unit" in
        *'*'*) exit 0 ;;  # the 'every other spira-*.timer' discovery glob — none here
        *) for u in $existing; do [ "\$unit" = "\$u" ] && { printf '%s enabled\n' "\$u"; exit 0; }; done; exit 0 ;;
    esac ;;
cat)
    for u in $existing; do [ "\$unit" = "\$u" ] && exit 0; done
    exit 1 ;;
list-units)
    case "\$unit" in
        *'@'*) [ -n "$legacy_watch" ] && printf '%s active running\n' "$legacy_watch"; exit 0 ;;
        spira-watch-*) [ -n "$inst_watch" ] && printf '%s active running\n' "$inst_watch"; exit 0 ;;
        *) [ -n "$active" ] && printf '%s active running\n' "$active"; exit 0 ;;
    esac ;;
stop) exit 0 ;;
*)    exit 0 ;;
esac
SC
    chmod +x "$TMP/systemctl"
}

world_stop() {
    local inst="$1"; shift
    : > "$CALLS"
    PATH="$SH:$PATH" SPIRA_HOME="$SH" SPIRA_PROD="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_SYSTEMCTL="$TMP/systemctl" SPIRA_INSTANCE="$inst" \
        "$SH/world.sh" stop "$@" 2>&1
}

world_start() {
    local inst="$1"; shift
    : > "$CALLS"
    PATH="$SH:$PATH" SPIRA_HOME="$SH" SPIRA_PROD="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_SYSTEMCTL="$TMP/systemctl" SPIRA_INSTANCE="$inst" \
        "$SH/world.sh" start "$@" 2>&1
}

world_status() {
    local inst="$1"
    : > "$CALLS"
    PATH="$SH:$PATH" SPIRA_HOME="$SH" SPIRA_PROD="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_SYSTEMCTL="$TMP/systemctl" SPIRA_INSTANCE="$inst" \
        "$SH/world.sh" status 2>&1
}

# --------------------------------------------------------------------------------------
# SCENARIO A: POST-MIGRATION — instance-qualified unit is enabled/active
#
# spira-sentinel-prod.timer is the live unit. world.sh stop must stop it, not the
# plain name. Asserting on the call log: stop exits 0 on a disabled unit, so an
# assert on exit code would pass while the sentinel kept firing.
# --------------------------------------------------------------------------------------
echo
echo "A: instance-qualified (spira-sentinel-prod.timer active, post-migration):"

write_sc "spira-sentinel-prod.timer" "spira-sentinel-prod.timer" "" ""
world_stop "prod" >/dev/null
calls="$(cat "$CALLS")"
want   "A-stop: calls stop on instance-qualified sentinel" \
       "stop spira-sentinel-prod.timer"  "$calls"
nowant "A-stop: does not stop plain sentinel (it is disabled)" \
       "stop spira-sentinel.timer"        "$calls"

# status must query the resolved name, not the plain one.
write_sc "spira-sentinel-prod.timer" "spira-sentinel-prod.timer" "" ""
out="$(world_status "prod")"
want   "A-status: shows instance-qualified sentinel"  "spira-sentinel-prod.timer"  "$out"
want   "A-status: reports it as active"               "active"                     "$out"
# Guard against the old behaviour: plain name would have shown "inactive".
nowant "A-status: plain sentinel name not in timer list" \
       "spira-sentinel.timer " "$out"

# --------------------------------------------------------------------------------------
# SCENARIO B: PRE-MIGRATION FALLBACK — instance-qualified not enabled/active
#
# The instance-qualified timer does not exist on this box; world.sh must fall back to
# the plain timer name. If it unconditionally uses the instance-qualified name, the
# plain sentinel goes undetected and keeps firing after a "stop".
# --------------------------------------------------------------------------------------
echo
echo "B: plain-name fallback (spira-sentinel.timer active, pre-migration):"

# Neither is-enabled nor is-active returns success for the instance-qualified name.
write_sc "" "spira-sentinel.timer" "" ""
world_stop "prod" >/dev/null
calls="$(cat "$CALLS")"
want   "B-stop: falls back and stops plain sentinel"           \
       "stop spira-sentinel.timer"        "$calls"
nowant "B-stop: does not stop instance-qualified (it is disabled)" \
       "stop spira-sentinel-prod.timer"   "$calls"

write_sc "" "spira-sentinel.timer" "" ""
out="$(world_status "prod")"
want   "B-status: shows plain sentinel"        "spira-sentinel.timer"  "$out"
want   "B-status: reports it as active"        "active"                "$out"

# --------------------------------------------------------------------------------------
# SCENARIO C: --hard STOPS INSTANCE-QUALIFIED WATCHER UNITS
#
# After the per-instance migration, watchers are spira-watch-<name>-<instance>.service.
# list-units 'spira-watch@*' finds nothing; the instance-qualified form must also be
# queried.
# --------------------------------------------------------------------------------------
echo
echo "C: --hard stops instance-qualified watcher (spira-watch-answers-prod.service):"

write_sc "spira-sentinel-prod.timer" "spira-sentinel-prod.timer" "" "spira-watch-answers-prod.service"
world_stop "prod" --hard >/dev/null
calls="$(cat "$CALLS")"
want   "C: calls stop on instance-qualified watcher" \
       "stop spira-watch-answers-prod.service"  "$calls"

# --------------------------------------------------------------------------------------
# SCENARIO D: --hard STOPS LEGACY WATCHER UNITS (TEMPLATE-INSTANCE FORM)
#
# Before the migration, watchers ran as spira-watch@<name>.service (template instances).
# The --hard loop must still find and stop them during a mixed-state migration.
# --------------------------------------------------------------------------------------
echo
echo "D: --hard stops legacy template-instance watcher (spira-watch@answers.service):"

write_sc "spira-sentinel-prod.timer" "spira-sentinel-prod.timer" "spira-watch@answers.service" ""
world_stop "prod" --hard >/dev/null
calls="$(cat "$CALLS")"
want   "D: calls stop on legacy @ watcher" \
       "stop spira-watch@answers.service"  "$calls"

# --------------------------------------------------------------------------------------
# SCENARIO E (sp-ivfu3-2, seen red first against the old is-enabled/is-active
# resolution): THE WORLD IS STOPPED — spira-sentinel-prod.timer is the only unit that
# EXISTS (list-unit-files/cat agree), but it is disabled AND inactive, same as every
# essential timer right after a halt. The old resolution asked is-enabled/is-active,
# which both say no for a disabled+inactive unit — indistinguishable from "does not
# exist" — and picked the plain spira-sentinel.timer, which systemd has never heard of.
# status/start/stop must all choose the EXISTING qualified name regardless.
# --------------------------------------------------------------------------------------
echo
echo "E: world stopped (spira-sentinel-prod.timer exists, disabled+inactive) — chooses it, not the nonexistent plain name:"

write_sc "" "" "" "" "spira-sentinel-prod.timer"
world_stop "prod" >/dev/null
calls="$(cat "$CALLS")"
want   "E-stop: calls stop on the existing instance-qualified sentinel" \
       "stop spira-sentinel-prod.timer"  "$calls"
nowant "E-stop: never stops the nonexistent plain name" \
       "stop spira-sentinel.timer"  "$calls"

write_sc "" "" "" "" "spira-sentinel-prod.timer"
out="$(world_status "prod")"
want   "E-status: shows the existing instance-qualified sentinel" "spira-sentinel-prod.timer" "$out"
nowant "E-status: never lists the nonexistent plain name as a timer row" \
       "spira-sentinel.timer " "$out"

write_sc "" "" "" "" "spira-sentinel-prod.timer"
world_start "prod" >/dev/null
calls="$(cat "$CALLS")"
want   "E-start: queries/targets the existing instance-qualified sentinel" \
       "spira-sentinel-prod.timer"  "$calls"
nowant "E-start: never touches the nonexistent plain name" \
       "is-enabled spira-sentinel.timer"  "$calls"

# --------------------------------------------------------------------------------------
# SCENARIO F (sp-ivfu3-2): NEITHER FORM EXISTS AT ALL — a named refusal, never a guess
# at either name, and never a systemctl stop/start call on anything.
# --------------------------------------------------------------------------------------
echo
echo "F: neither spira-sentinel-prod.timer nor spira-sentinel.timer exists — refuses, named:"

write_sc "" "" "" "" ""
out="$(world_stop "prod")"; rc=$?
wantrc "F-stop: refuses (nonzero)" 1 "$rc"
want   "F-stop: names the missing qualified unit" "spira-sentinel-prod.timer" "$out"
want   "F-stop: names the missing plain unit"     "spira-sentinel.timer"      "$out"
calls="$(cat "$CALLS")"
nowant "F-stop: never calls stop on anything at all" "stop " "$calls"

write_sc "" "" "" "" ""
out="$(world_start "prod")"; rc=$?
wantrc "F-start: refuses (nonzero)" 1 "$rc"
want   "F-start: names the missing qualified unit" "spira-sentinel-prod.timer" "$out"
want   "F-start: names the missing plain unit"     "spira-sentinel.timer"      "$out"
calls="$(cat "$CALLS")"
nowant "F-start: never calls start on anything at all" "start " "$calls"

echo
tl_summary
