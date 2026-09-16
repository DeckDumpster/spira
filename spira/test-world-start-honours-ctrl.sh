#!/usr/bin/env bash
# covers: spira/world.sh spira/ctrl.sh
#
# world.sh start must not restart timers that are suspended in the control plane
# or that are disabled in systemd. A single stop → start cycle must not silently
# undo recorded operator decisions (sp-cqyfy).
#
# FOUR CASES, each with a positive control (law-absence-needs-a-positive-control):
#
#   1. ctrl-suspended timer: skipped; the reason is printed.
#      Positive: after the suspension is lifted, the same timer IS started.
#
#   2. disabled-in-systemd timer (not suspended): skipped.
#      Positive: an enabled timer IS started.
#
# Both properties are driven through a running world.sh to catch wiring bugs in
# the start loop, not just static code checks.
#
# systemctl IS STUBBED. ctrl.sh runs against a real (but temp) control file.
# A suite that queries the real service manager is green for as long as the box
# happens to be in the state its author had (law-gates-run-in-a-clean-environment).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-world-start-honours-ctrl.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
SH="$TMP/spira"
RUN="$TMP/run"
CALLS="$TMP/sc-calls"
export CALLS SH RUN

mkdir -p "$SH" "$RUN"
cp "$HERE/world.sh" "$HERE/ctrl.sh" "$HERE/conf.sh" "$SH/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/slay.sh"; chmod +x "$SH/slay.sh"

# write_sc <enabled_units>
# Write a systemctl stub. list-unit-files returns spira-testsubject.timer.
# is-enabled returns "enabled" for units in the enabled_units list (space-separated),
# "disabled" for everything else. Records every call to $CALLS.
write_sc() {
    local enabled_units="${1:-}"
    cat > "$TMP/sc" <<SC
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$CALLS"
enabled_units="${enabled_units}"
verb=""; subj=""
for a; do
    case "\$a" in
        --user|--no-legend|--all|--no-pager|--state=active) ;;
        *) if [ -z "\$verb" ]; then verb="\$a"; else subj="\$a"; fi ;;
    esac
done
case "\$verb" in
    list-unit-files)
        printf 'spira-testsubject.timer disabled\n'
        exit 0 ;;
    list-units)
        exit 0 ;;
    is-enabled)
        for eu in \$enabled_units; do
            [ "\$subj" = "\$eu" ] && { echo enabled; exit 0; }
        done
        echo disabled; exit 1 ;;
    is-active)
        echo inactive; exit 3 ;;
    show)
        echo oneshot; exit 0 ;;
    start|stop)
        exit 0 ;;
    *)
        exit 0 ;;
esac
SC
    chmod +x "$TMP/sc"
}

ctrl_run() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_RUN="$RUN" \
        SPIRA_SYSTEMCTL="$TMP/sc" \
        bash "$SH/ctrl.sh" "$@"
}

world_start() {
    : > "$CALLS"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_DB="$TMP/no-db" \
    SPIRA_INSTANCE="" \
    SPIRA_SYSTEMCTL="$TMP/sc" \
        bash "$SH/world.sh" start 2>&1
}

# ---- CASE 1: ctrl-suspended timer is not started ----
echo
echo "start honours ctrl.sh suspensions:"

# Stub: spira-testsubject.timer is enabled — proves ctrl suspension beats is-enabled.
write_sc "spira-testsubject.timer"

ctrl_run suspend spira-testsubject --reason "stopped for sp-cqyfy test" --owner sp-cqyfy

out="$(world_start)"
calls="$(cat "$CALLS")"

nowant "suspended timer is not started"           "start spira-testsubject.timer" "$calls"
want   "skip is printed"                          "skipped spira-testsubject.timer" "$out"
want   "skip includes the reason"                 "stopped for sp-cqyfy test"       "$out"

# Positive control: lift the suspension → same timer must be started.
echo
echo "positive control — after resume, same timer IS started:"
ctrl_run resume spira-testsubject

out="$(world_start)"
calls="$(cat "$CALLS")"

want   "unsuspended timer is started"             "start spira-testsubject.timer" "$calls"
nowant "and not reported as skipped"              "skipped spira-testsubject.timer" "$out"

# ---- CASE 2: disabled timer is not started ----
echo
echo "start skips disabled timers:"

# Stub: spira-testsubject.timer is disabled (not in enabled_units).
write_sc ""

out="$(world_start)"
calls="$(cat "$CALLS")"

nowant "disabled timer is not started"            "start spira-testsubject.timer" "$calls"
want   "disabled skip is printed"                 "skipped spira-testsubject.timer" "$out"
want   "and says disabled"                        "disabled" "$out"

# Positive control: mark the timer enabled → must be started.
echo
echo "positive control — enabled timer IS started:"

write_sc "spira-testsubject.timer"

out="$(world_start)"
calls="$(cat "$CALLS")"

want   "enabled timer is started"                 "start spira-testsubject.timer" "$calls"
nowant "and not reported as skipped"              "skipped spira-testsubject.timer" "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
