#!/usr/bin/env bash
#
# test-reconciler.sh — the reconciler: structural invariants and their deterministic
# remedies (sp-ocmes), and reading them from spira-desired-state's typed Composite in
# preference to the ad-hoc bash/env defaults each invariant fell back to before (sp-8c3ib).
#
# WHAT THIS SUITE CHECKS.
#   1. reconciler is on PATH, and a pass completes inside its budget.
#   2. flock: a concurrent pass is skipped.
#   3. Units, POSITIVE CONTROL: every unit enabled+active → no gap, no remedy run.
#   4. Units: N disabled timers → each gets `enable --now`, each escalates only if the
#      remedy does not hold.
#   5. Units: a daemon that is active but not enabled gets `enable` (never `--now`,
#      never restarted just to flip its enablement bit).
#   6. Units: an inactive daemon gets `restart`.
#   7. Store: the store unit down NEVER runs a remedy command — it only escalates,
#      whatever state it is in (sp-ocmes evidence, 2026-09-25: never restart dolt-beads).
#   8. Fleet: a fleet of 3 with 150 ready and 0 live → gap, escalate, no remedy command
#      (starting an aeon is not one of this bead's deterministic remedies).
#   9. Fleet, POSITIVE CONTROL: a staffed partition → satisfied, nothing escalated.
#  10. Cockpit, POSITIVE CONTROL: sessions + both dashboards present → satisfied.
#  11. Cockpit: no dashboards, no `cockpit.down` marker → gap → `layout ensure` run; no tmux
#      server → gap → `rebuild` run.
#  12. Cockpit: `cockpit.down` present → satisfied, no remedy run (a deliberate state is
#      not a fault — law-a-deliberate-state-is-not-a-fault).
#  13. Release: checkout on a non-main branch with no activated release → gap, escalate.
#  14. Release: checkout on main → satisfied.
#  15. Queue: a certified branch that no longer merges onto its base → gap → queue
#      eject run (reopens it for rebase).
#  16. Queue: a certified branch that still merges cleanly → satisfied.
#  17. Queue: a lock held past the pre-flight wall → gap, escalate.
#  18. Grace period: a fresh gap inside its grace period raises nothing — no remedy run,
#      no incident filed — but the time series still records the gap (seen, not smoothed).
#  19. Unobservable: systemctl unreadable → status=unobservable, never satisfied.
#  20. A remedy that does not close its gap escalates on the next pass instead of being
#      retried blind, and is not retried a second time.
#  21. Fleet: a materialised Composite's ceiling is read even when the configured
#      SPIRA_MAX_LIVE_AEONS column is empty (so this is a gap, not unobservable).
#  22. Units: a materialised Composite's UnitsSpec names the unit set — the installer's ENABLE
#      own list is not consulted once one exists.
#  23. Cockpit: a materialised Composite's CockpitSpec.dashboards replaces the hardcoded
#      health+mail pair, both when it demands more and when it demands less.
#  24. Release: a materialised Composite's ReleaseSpec names the target explicitly, and
#      allow_override lets a local divergence from it satisfy instead of escalating.
#  25. Fleet: the task pool deliberately at 0 (SPIRA_MAX_AEONS=0) satisfies a starved task
#      partition instead of gapping — a deliberate state is not a fault (sp-qsz01).
#  26. Fleet: that same pool-at-0 carve-out does not silence a lane partition, which draws
#      outside SPIRA_MAX_AEONS and so is starved for a different reason.
#  27. Disk, POSITIVE CONTROL: every path above its floor -> satisfied, no remedy run.
#  28. Disk: a path below the floor -> gap -> the remedy sequence (target-reap, podman prunes).
#  29. Disk: a Composite-named path df cannot read -> unobservable, never satisfied, no remedy run.
#  30. Disk: a materialised Composite's DiskSpec names the paths checked — the
#      no-Composite default set is never consulted once one exists.
#  31. Disk: a materialised Composite's DiskSpec.floor_pct replaces the bash/env default
#      (SPIRA_DISK_FLOOR_PCT), so the same reading can gap under one and satisfy the other.
#
# Unit tests in reconciler/src/main.rs cover what config cannot express here: no Fleet ceiling
# at all (SPIRA_MAX_LIVE_AEONS is a u32 that always resolves) is unobservable, never satisfied,
# and is ONE escalation however many partitions report.
#
#  33. Junk rows: a READY lifecycle row whose id is no bead in the store is dropped through
#      spira-lc with the evidence; a row whose bead exists is untouched; an unreadable
#      store drops nothing.
#
# 1-20 run with no Composite ever materialised — every invariant's fallback to its old
# bash/env default, still the state of an install that has not adopted desired-state yet.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): 3 and 10 assert nothing fires
# on a healthy fixture before 4-9 and 11-17 add the trigger and assert it does.
#
# tier: T2
# covers: reconciler/src/main.rs reconciler-engine/src/**.rs desired-state/src/store.rs
#         desired-state/src/resource.rs spira/conf.sh cockpit/ops/src/layout.rs
#         systemd/spira-reconciler.service systemd/spira-reconciler.timer
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# (this crate's own #[test]s run once per round as the workspace unit-test step, not here.)

command -v reconciler >/dev/null 2>&1 || bail "reconciler is not on PATH"

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_DB="$T/db"
mkdir -p "$SPIRA_RUN/queue" "$SPIRA_DB"

# Pinned so this suite's verdict never depends on whatever composite the real host it runs
# on has actually applied. desired_state_write below populates it on demand.
DESIRED_DIR="$T/desired"
export SPIRA_DESIRED_DIR="$DESIRED_DIR"
# SPIRA_QUEUE_DIR is a registered key the reconciler binary reads directly (cfg(...)?, no
# env fallback); its conf.d DEFAULT derives it from SPIRA_RUN, but that derivation runs
# against the base fixture's SPIRA_RUN, not this suite's override, so the queue invariants
# (15/17 below) were reading an unrelated queue dir with no "testrepo" in it. Declare it
# explicitly (one source of config, per Ryan 2026-10-05).
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_DB="$SPIRA_DB" SPIRA_DESIRED_DIR="$SPIRA_DESIRED_DIR" \
    SPIRA_QUEUE_DIR="$SPIRA_RUN/queue"

# desired_state_write <resource-toml-fragment>... — hand-writes a single-version composite
# document directly in the store's own on-disk shape (meta + [[resource]]), rather than
# shelling out to the `spira` binary this suite does not otherwise build: read_current()
# never checks content_hash, only that each resource still parses.
desired_state_write() {
    mkdir -p "$DESIRED_DIR/versions"
    {
        printf '[meta]\nversion = 1\ncontent_hash = "test"\nmaterialized_at = "2026-09-25T00:00:00Z"\n'
        printf '%s\n' "$@"
    } > "$DESIRED_DIR/versions/000001.toml"
    printf '1\n' > "$DESIRED_DIR/current"
}
desired_state_clear() {
    rm -rf "$DESIRED_DIR"
}

# ------------------------------------------------------------------------------------------
# Stubs. The reconciler runs every tool by bare name, so each IO seam is a small script of
# that name in STUB_BIN, which rpass() puts ahead of the real PATH for the pass alone — no
# real systemctl/tmux/git/queue is ever touched, and there is no environment seam to set.
# ------------------------------------------------------------------------------------------

STUB_BIN="$T/stub-bin"; mkdir -p "$STUB_BIN"
rpass() { PATH="$STUB_BIN:$PATH" reconciler --pass >/dev/null 2>&1; }

SYSTEMCTL_STATE="$T/systemctl-state"   # "<unit> <enabled|disabled> <active|inactive>" per line
SYSTEMCTL_CALLS="$T/systemctl-calls.log"
: > "$SYSTEMCTL_STATE"; : > "$SYSTEMCTL_CALLS"
cat > "$STUB_BIN/systemctl" <<'SEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SYSTEMCTL_CALLS"
args=("$@"); args=("${args[@]:1}")   # drop --user
verb="${args[0]}"
unit="${args[-1]}"                    # unit is always the last argument
row="$(grep -E "^$unit " "$SYSTEMCTL_STATE" 2>/dev/null | tail -1)"
if [ -z "$row" ]; then
    case "$verb" in
        is-enabled|is-active) echo "unreadable"; exit 1 ;;
        *) exit 0 ;;
    esac
fi
read -r _u enabled active <<<"$row"
case "$verb" in
    is-enabled) echo "$enabled"; [ "$enabled" = "enabled" ] ;;
    is-active)  echo "$active";  [ "$active" = "active" ] ;;
    *) exit 0 ;;
esac
SEOF
chmod +x "$STUB_BIN/systemctl"
export SYSTEMCTL_STATE SYSTEMCTL_CALLS

TMUX_STATE="$T/tmux-state"   # controls has-session/list-panes responses
: > "$TMUX_STATE"
cat > "$STUB_BIN/tmux" <<'TEOF'
#!/usr/bin/env bash
case "$1" in
    list-sessions) grep -q '^server=down$' "$TMUX_STATE" 2>/dev/null && exit 1; exit 0 ;;
    has-session)   grep -q '^server=down$' "$TMUX_STATE" 2>/dev/null && exit 1; exit 0 ;;
    list-panes)    grep '^tag=' "$TMUX_STATE" 2>/dev/null | sed 's/^tag=//'; exit 0 ;;
    *) exit 0 ;;
esac
TEOF
chmod +x "$STUB_BIN/tmux"
export TMUX_STATE

GIT_BRANCH_STATE="$T/git-branch"     # branch name for `rev-parse --abbrev-ref HEAD`
MERGE_STATE="$T/git-merge-state"     # "clean" or "conflict"
echo main > "$GIT_BRANCH_STATE"
echo clean > "$MERGE_STATE"
cat > "$STUB_BIN/git" <<'GEOF'
#!/usr/bin/env bash
case " $* " in
    *" --abbrev-ref "*) cat "$GIT_BRANCH_STATE"; exit 0 ;;
    *" merge-base "*) echo "deadbeef"; exit 0 ;;
    *" merge-tree "*)
        if [ "$(cat "$MERGE_STATE")" = "conflict" ]; then
            echo "<<<<<<< HEAD"
        fi
        exit 0
        ;;
esac
exit 0
GEOF
chmod +x "$STUB_BIN/git"
export GIT_BRANCH_STATE MERGE_STATE

UNITS_LIST="$T/units-list"
: > "$UNITS_LIST"
printf '#!/usr/bin/env bash\n[ "$1" = "--list-enable" ] && cat "%s"\n' "$UNITS_LIST" > "$STUB_BIN/units-install"
chmod +x "$STUB_BIN/units-install"

# FLEET_LINES: "<labels>\t<ready>\t<live>\t<is-task>" per partition (a missing 4th column
# means task). The chamber, the claimable-work count and the live-aeon count are three tools,
# so three stubs read this one fixture; a partition's fayth is its labels with commas as "_".
FLEET_LINES="$T/fleet-lines"
: > "$FLEET_LINES"
cat > "$STUB_BIN/spira-config" <<'FEOF'
#!/usr/bin/env bash
fayth_of() { printf 'p-%s' "${1//,/_}"; }
case "$1 $2" in
    "fayth partitions") awk -F'\t' 'NF>=3{print $1"\t"}' "$FLEET_LINES" ;;
    "fayth task") awk -F'\t' 'NF>=3 && ($4==""||$4==1){gsub(",","_",$1); printf "p-%s ", $1}' "$FLEET_LINES" ;;
    "fayth lane") awk -F'\t' 'NF>=3 && $4=="0"{gsub(",","_",$1); printf "p-%s ", $1}' "$FLEET_LINES" ;;
    "fayth for-labels") fayth_of "$3"; echo ;;
    "repo landref") echo "origin/main" ;;
    *) exit 1 ;;
esac
FEOF
cat > "$STUB_BIN/spira-claim" <<'FEOF'
#!/usr/bin/env bash
[ "$1" = "ready-count" ] || exit 1
awk -F'\t' -v l="$2" '$1==l{print $2; found=1} END{if(!found)print 0}' "$FLEET_LINES"
FEOF
cat > "$STUB_BIN/strand" <<'FEOF'
#!/usr/bin/env bash
[ "$1" = "aeon-count" ] || exit 1
awk -F'\t' -v f="$2" '{l=$1; gsub(",","_",l); if("p-" l==f){print $3; found=1}} END{if(!found)print 0}' "$FLEET_LINES"
FEOF
chmod +x "$STUB_BIN/spira-config" "$STUB_BIN/spira-claim" "$STUB_BIN/strand"
export FLEET_LINES

CERTIFIED_LINES="$T/certified-lines"   # "<bead-id> <tip> <epoch>" per certified bead
: > "$CERTIFIED_LINES"
printf '#!/usr/bin/env bash\n[ "$1" = "certified-list" ] && cat "%s"\n' "$CERTIFIED_LINES" > "$STUB_BIN/queue-helpers"
chmod +x "$STUB_BIN/queue-helpers"

COCKPIT_CALLS="$T/cockpit-calls.log"
: > "$COCKPIT_CALLS"
for tool in rebuild layout; do
    printf '#!/usr/bin/env bash\nprintf "%s %%s\\n" "$*" >> "%s"\n' "$tool" "$COCKPIT_CALLS" > "$STUB_BIN/$tool"
    chmod +x "$STUB_BIN/$tool"
done

DISK_LINES="$T/disk-lines"        # "<path>\t<free-pct>" per line; a path not listed is unreadable
: > "$DISK_LINES"
DISK_USAGE_CALLS="$T/disk-usage-calls.log"
: > "$DISK_USAGE_CALLS"
cat > "$STUB_BIN/df" <<'DUEOF'
#!/usr/bin/env bash
path="${!#}"
printf '%s\n' "$path" >> "$DISK_USAGE_CALLS"
pct="$(awk -F'\t' -v p="$path" '$1==p{print $2}' "$DISK_LINES")"
[ -n "$pct" ] || { echo "df: $path: No such file or directory" >&2; exit 1; }
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\nfake 100 %s %s 0%% %s\n' "$((100 - pct))" "$pct" "$path"
DUEOF
chmod +x "$STUB_BIN/df"
export DISK_LINES DISK_USAGE_CALLS

DISK_REMEDY_CALLS="$T/disk-remedy-calls.log"
: > "$DISK_REMEDY_CALLS"
printf '#!/usr/bin/env bash\nprintf "target-reap %%s\\n" "$*" >> "%s"\n' "$DISK_REMEDY_CALLS" > "$STUB_BIN/target-reap"
cat > "$STUB_BIN/podman" <<'POEOF'
#!/usr/bin/env bash
[ "$1" = "info" ] && exit 0
printf 'podman %s\n' "$*" >> "$DISK_REMEDY_CALLS"
POEOF
chmod +x "$STUB_BIN/target-reap" "$STUB_BIN/podman"
export DISK_REMEDY_CALLS

QUEUE_CALLS="$T/queue-calls.log"
: > "$QUEUE_CALLS"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$QUEUE_CALLS" > "$STUB_BIN/queue"
chmod +x "$STUB_BIN/queue"

# Stub mail: an escalation is one `send concierge` whose body carries "invariant: <key>".
MAIL_LOG="$T/mail-calls.log"
: > "$MAIL_LOG"
cat > "$STUB_BIN/mail" <<'IEOF'
#!/usr/bin/env bash
[ "$1" = "send" ] && [ "$2" = "concierge" ] || exit 1
{ printf 'CALL %s\n' "$*"; cat; printf '\n---\n'; } >> "$MAIL_LOG"
IEOF
chmod +x "$STUB_BIN/mail"
export MAIL_LOG

export SPIRA_REPO_MAP="$T/repo-map"
tl_config SPIRA_REPO_MAP="$SPIRA_REPO_MAP" SPIRA_DOLT_DATA=/var/lib/dolt
printf 'testrepo | %s | queue | origin/main | | \n' "$T/repo" > "$SPIRA_REPO_MAP"
mkdir -p "$T/repo"
# queue_repo_names walks $SPIRA_RUN/queue/*/ — the Queue checks only ever look at a repo
# that has a directory here, so "testrepo" must exist for the whole run even on passes
# that leave its certified-list/lock fixtures empty.
mkdir -p "$SPIRA_RUN/queue/testrepo"

LC_CALLS="$T/lc-calls.log"; BD_JSON="$T/bd-json"; LC_ROWS="$T/lc-rows"
STUB_LC="$STUB_BIN/spira-lc"; STUB_BD="$T/bd"
cat > "$STUB_LC" <<'LEOF'
#!/usr/bin/env bash
case "$1" in
    list) cat "$LC_ROWS" ;;
    *) printf '%s\n' "$*" >> "$LC_CALLS" ;;
esac
LEOF
cat > "$STUB_BD" <<'BEOF'
#!/usr/bin/env bash
cat "$BD_JSON"; [ -f "$BD_JSON.fail" ] && exit 1; exit 0
BEOF
chmod +x "$STUB_LC" "$STUB_BD"
export SPIRA_BD="$STUB_BD" LC_CALLS BD_JSON LC_ROWS
tl_config SPIRA_BD="$STUB_BD"
: > "$LC_CALLS"; printf '[{"id":"sp-real"}]\n' > "$BD_JSON"; printf '[]\n' > "$LC_ROWS"

reset_state() {
    rm -f "$SPIRA_RUN/reconciler-state.json" "$SPIRA_RUN/reconciler-alerted.json" \
          "$SPIRA_RUN/tsd/reconciler-status.jsonl" "$SPIRA_RUN/reconciler.log" "$SPIRA_RUN/cockpit.down"
    : > "$SYSTEMCTL_CALLS"; : > "$COCKPIT_CALLS"; : > "$QUEUE_CALLS"; : > "$MAIL_LOG"
    : > "$DISK_USAGE_CALLS"; : > "$DISK_REMEDY_CALLS"
    desired_state_clear
}

# under run/tsd/ (design reconciler-time-series-2026-09-27 §2, sp-69m85): moved from
# $SPIRA_RUN directly.
status_jsonl() { cat "$SPIRA_RUN/tsd/reconciler-status.jsonl" 2>/dev/null || true; }

printf 'test-reconciler.sh\n'

# ==========================================================================================
printf '\n%s\n' "1. reconciler is on PATH, and a pass completes inside its budget"
# ==========================================================================================
reset_state
_t0="$(date +%s)"
rpass
_t1="$(date +%s)"
[ $((_t1-_t0)) -lt 10 ] && ok "pass completed in $((_t1-_t0))s (budget: 10s)" || bad "pass exceeded budget"

# ==========================================================================================
printf '\n%s\n' "2. flock: concurrent pass is skipped"
# ==========================================================================================
_lock="$SPIRA_RUN/reconciler.lock"
exec 9>"$_lock"; flock 9
_out="$(PATH="$STUB_BIN:$PATH" reconciler --pass 2>&1 || true)"
exec 9>&-
want "concurrent pass logs 'already running'" "already running" "$_out"

export SPIRA_RECONCILER_GRACE_SECS=0
tl_config SPIRA_RECONCILER_GRACE_SECS=0

# ==========================================================================================
printf '\n%s\n' "3. Units, POSITIVE CONTROL: everything enabled+active"
# ==========================================================================================
reset_state
printf 'spira-sentinel.timer\nspira-loom-prod.service\n%s\n' "dolt-beads.service" > "$UNITS_LIST"
cat > "$SYSTEMCTL_STATE" <<EOF
spira-sentinel.timer enabled active
spira-loom-prod.service enabled active
dolt-beads.service enabled active
EOF
rpass
_calls="$(cat "$SYSTEMCTL_CALLS")"
lack "no enable/restart calls when everything is healthy" "enable" "$(grep -v is- <<<"$_calls" || true)"
[ ! -s "$MAIL_LOG" ] && ok "no incident filed when everything is healthy" || bad "unexpected incident: $(cat "$MAIL_LOG")"

# ==========================================================================================
printf '\n%s\n' "4. Units: disabled timers each get enable --now"
# ==========================================================================================
reset_state
printf 'a.timer\nb.timer\nc.timer\n' > "$UNITS_LIST"
cat > "$SYSTEMCTL_STATE" <<EOF
a.timer disabled inactive
b.timer disabled inactive
c.timer disabled inactive
EOF
rpass
_n="$(grep -c -- '--user enable --now' "$SYSTEMCTL_CALLS" || true)"
is "3 disabled timers each get enable --now" "3" "$_n"
want "status jsonl records a gap for a.timer" '"key":"units-timer:a.timer"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "5. Units: active-but-not-enabled daemon gets enable, never --now"
# ==========================================================================================
reset_state
printf 'spira-loom-prod.service\n' > "$UNITS_LIST"
printf 'spira-loom-prod.service disabled active\n' > "$SYSTEMCTL_STATE"
rpass
want "enable is called" "--user enable spira-loom-prod.service" "$(cat "$SYSTEMCTL_CALLS")"
lack "restart is never called for an active daemon" "restart" "$(cat "$SYSTEMCTL_CALLS")"
lack "--now is never used on a running daemon" "enable --now spira-loom-prod" "$(cat "$SYSTEMCTL_CALLS")"

# ==========================================================================================
printf '\n%s\n' "6. Units: an inactive daemon gets restarted"
# ==========================================================================================
reset_state
printf 'spira-loom-prod.service\n' > "$UNITS_LIST"
printf 'spira-loom-prod.service enabled inactive\n' > "$SYSTEMCTL_STATE"
rpass
want "restart is called on an inactive daemon" "--user restart spira-loom-prod.service" "$(cat "$SYSTEMCTL_CALLS")"

# ==========================================================================================
printf '\n%s\n' "7. Store: dolt-beads is never restarted, only escalated"
# ==========================================================================================
reset_state
printf 'dolt-beads.service\n' > "$UNITS_LIST"
printf 'dolt-beads.service disabled inactive\n' > "$SYSTEMCTL_STATE"
rpass
_calls="$(cat "$SYSTEMCTL_CALLS")"
lack "no restart of dolt-beads" "restart dolt-beads" "$_calls"
lack "no enable of dolt-beads" "enable --now dolt-beads" "$_calls"
lack "no plain enable of dolt-beads" "enable dolt-beads" "$_calls"
want "dolt-beads down escalates" "invariant: store:dolt-beads.service" "$(cat "$MAIL_LOG")"

# ==========================================================================================
printf '\n%s\n' "8. Fleet: 3 live ceiling, 150 ready, 0 live -> gap, escalate, no remedy"
# ==========================================================================================
reset_state
: > "$UNITS_LIST"
printf 'builder\t150\t0\n' > "$FLEET_LINES"
tl_config SPIRA_MAX_LIVE_AEONS=3 SPIRA_MAX_AEONS=6
rpass
want "fleet gap escalates" "invariant: fleet:builder" "$(cat "$MAIL_LOG")"
want "status jsonl records desired=3 observed=0" '"desired":"3","observed":"0"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "9. Fleet, POSITIVE CONTROL: live meets the desired -> satisfied, no escalation"
# ==========================================================================================
reset_state
printf 'builder\t2\t2\n' > "$FLEET_LINES"
rpass
want "a fully staffed partition is satisfied" '"key":"fleet:builder","status":"satisfied"' "$(status_jsonl)"
[ ! -s "$MAIL_LOG" ] && ok "no escalation for a staffed fleet" || bad "unexpected escalation: $(cat "$MAIL_LOG")"
: > "$FLEET_LINES"

# ==========================================================================================
printf '\n%s\n' "10. Cockpit, POSITIVE CONTROL: sessions + both dashboards present"
# ==========================================================================================
reset_state
cat > "$TMUX_STATE" <<'EOF'
tag=health
tag=mail
EOF
rpass
[ ! -s "$COCKPIT_CALLS" ] && ok "no cockpit repair when dashboards are present" || bad "unexpected cockpit call: $(cat "$COCKPIT_CALLS")"

# ==========================================================================================
printf '\n%s\n' "11. Cockpit: no dashboards, no down marker -> gap -> repair run"
# ==========================================================================================
reset_state
: > "$TMUX_STATE"
rpass
want "layout ensure heals a cockpit whose dashboards are missing" "layout ensure" "$(cat "$COCKPIT_CALLS")"
lack "rebuild is not run while the server and sessions are up" "rebuild" "$(cat "$COCKPIT_CALLS")"

reset_state
printf 'server=down\n' > "$TMUX_STATE"
rpass
want "rebuild makes a cockpit when the tmux server is gone" "rebuild" "$(cat "$COCKPIT_CALLS")"
lack "layout ensure cannot create a cockpit, so it is not the remedy for no server" "layout" "$(cat "$COCKPIT_CALLS")"

# ==========================================================================================
printf '\n%s\n' "12. Cockpit: layout down marker present -> satisfied, no repair"
# ==========================================================================================
reset_state
: > "$TMUX_STATE"
: > "$SPIRA_RUN/cockpit.down"
rpass
[ ! -s "$COCKPIT_CALLS" ] && ok "a deliberate down is never repaired" || bad "unexpected cockpit call: $(cat "$COCKPIT_CALLS")"
want "status jsonl reports satisfied while down" '"key":"cockpit","status":"satisfied"' "$(status_jsonl)"
rm -f "$SPIRA_RUN/cockpit.down"

# ==========================================================================================
printf '\n%s\n' "13. Release: non-main branch, no activated release -> gap, escalate"
# ==========================================================================================
reset_state
echo "feature-x" > "$GIT_BRANCH_STATE"
rpass
want "release gap escalates" "invariant: release" "$(cat "$MAIL_LOG")"

# ==========================================================================================
printf '\n%s\n' "14. Release: checkout on main -> satisfied"
# ==========================================================================================
reset_state
echo "main" > "$GIT_BRANCH_STATE"
rpass
want "status jsonl reports release satisfied on main" '"key":"release","status":"satisfied"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "15. Queue: a certified branch that no longer merges -> gap -> eject"
# ==========================================================================================
reset_state
echo "sp-abc123 deadbeef 1" > "$CERTIFIED_LINES"
echo "conflict" > "$MERGE_STATE"
rpass
want "queue eject is run for the unmergeable branch" "eject sp-abc123" "$(cat "$QUEUE_CALLS")"

# ==========================================================================================
printf '\n%s\n' "16. Queue: a certified branch that still merges cleanly -> satisfied"
# ==========================================================================================
reset_state
echo "sp-abc123 deadbeef 1" > "$CERTIFIED_LINES"
echo "clean" > "$MERGE_STATE"
rpass
[ ! -s "$QUEUE_CALLS" ] && ok "no eject for a branch that still merges" || bad "unexpected eject: $(cat "$QUEUE_CALLS")"
: > "$CERTIFIED_LINES"

# ==========================================================================================
printf '\n%s\n' "17. Queue: a lock held past the pre-flight wall -> gap, escalate"
# ==========================================================================================
reset_state
export SPIRA_PREFLIGHT_WALL_SECS=0
tl_config SPIRA_PREFLIGHT_WALL_SECS=0
mkdir -p "$SPIRA_RUN/queue/testrepo"
_lockfile="$SPIRA_RUN/queue/testrepo/lock"
: > "$_lockfile"
touch -d "@$(( $(date +%s) - 10 ))" "$_lockfile"
# Hold it from a real, separate process — testing the real flock(2) dependency, not a model
# of it (law-prefer-the-real-dependency) — and hold it long enough for the pass to observe.
# Opened read-write WITHOUT truncation (9<>, not 9>): truncating would reset the mtime just
# backdated above, and age-since-acquired is exactly what this check reads.
( exec 9<>"$_lockfile"; flock 9; sleep 5 ) &
_holder=$!
sleep 0.3
rpass
want "a held, stale lock escalates" "invariant: queue-lock-age:testrepo" "$(cat "$MAIL_LOG")"
wait "$_holder" 2>/dev/null || true
rm -f "$_lockfile"
unset SPIRA_PREFLIGHT_WALL_SECS
# Restore to the complete fixture's own declared default (240) — "nothing has a default"
# any more (per Ryan 2026-10-05), so unset alone no longer puts this back the way it was.
tl_config SPIRA_PREFLIGHT_WALL_SECS=240

# ==========================================================================================
printf '\n%s\n' "18. Grace period: a fresh gap inside grace raises nothing"
# ==========================================================================================
export SPIRA_RECONCILER_GRACE_SECS=300
tl_config SPIRA_RECONCILER_GRACE_SECS=300
reset_state
printf 'a.timer\n' > "$UNITS_LIST"
printf 'a.timer disabled inactive\n' > "$SYSTEMCTL_STATE"
rpass
lack "no remedy run inside the grace period" "enable --now" "$(cat "$SYSTEMCTL_CALLS")"
[ ! -s "$MAIL_LOG" ] && ok "no incident filed inside the grace period" || bad "unexpected incident: $(cat "$MAIL_LOG")"
want "the gap is still recorded in the time series" '"key":"units-timer:a.timer","status":"gap"' "$(status_jsonl)"
export SPIRA_RECONCILER_GRACE_SECS=0
tl_config SPIRA_RECONCILER_GRACE_SECS=0

# ==========================================================================================
printf '\n%s\n' "19. Unobservable: systemctl unreadable -> unobservable, never satisfied"
# ==========================================================================================
reset_state
printf 'z.timer\n' > "$UNITS_LIST"
: > "$SYSTEMCTL_STATE"   # no row for z.timer: the stub reports "unreadable" and exits 1
rpass
want "z.timer reports unobservable" '"key":"units:z.timer","status":"unobservable"' "$(status_jsonl)"
lack "unobservable is never reported satisfied" '"key":"units:z.timer","status":"satisfied"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "20. A remedy that does not close its gap escalates, and is not retried"
# ==========================================================================================
reset_state
printf 'r.timer\n' > "$UNITS_LIST"
# The stub's canned response never changes: enable --now is attempted but the unit stays
# disabled/inactive on every subsequent read, exactly like a unit whose enable is masked.
printf 'r.timer disabled inactive\n' > "$SYSTEMCTL_STATE"
rpass   # pass 1: remedy attempted
_n1="$(grep -c -- '--user enable --now r.timer' "$SYSTEMCTL_CALLS" || true)"
is "pass 1 attempts the remedy once" "1" "$_n1"
[ ! -s "$MAIL_LOG" ] && ok "pass 1 does not escalate yet" || bad "pass 1 escalated early: $(cat "$MAIL_LOG")"

rpass   # pass 2: still a gap
_n2="$(grep -c -- '--user enable --now r.timer' "$SYSTEMCTL_CALLS" || true)"
is "pass 2 does not retry the remedy blind" "1" "$_n2"
want "pass 2 escalates instead" "invariant: units-timer:r.timer" "$(cat "$MAIL_LOG")"

# ==========================================================================================
printf '\n%s\n' "21. Fleet: a materialised Composite's ceiling is read instead of the configured SPIRA_MAX_LIVE_AEONS"
# ==========================================================================================
reset_state
printf 'builder\t5\t0\n' > "$FLEET_LINES"
tl_config SPIRA_MAX_LIVE_AEONS=9   # the configured ceiling would desire 5 live; the Composite's 3 must win
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Fleet"
producer = "test"
[resource.metadata]
name = "fleet"
[resource.spec]
ceiling = 3
lane_cap = 1'
rpass
lack "the Composite's ceiling makes this observable, not unobservable" '"key":"fleet:builder","status":"unobservable"' "$(status_jsonl)"
want "the desired live count is bounded by the Composite's ceiling, not the configured one" '"desired":"3","observed":"0"' "$(status_jsonl)"
want "fleet gap escalates against the Composite's ceiling" "invariant: fleet:builder" "$(cat "$MAIL_LOG")"

# ==========================================================================================
printf '\n%s\n' "22. Units: a materialised Composite's UnitsSpec names the unit set instead of the installer's ENABLE set"
# ==========================================================================================
reset_state
printf 'wrong.service\n' > "$UNITS_LIST"    # the installer's own ENABLE list — must be ignored
printf 'right.service enabled active\n' > "$SYSTEMCTL_STATE"
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Units"
producer = "test"
[resource.metadata]
name = "units"
[[resource.spec.units]]
name = "right.service"
enabled = true
active = true'
rpass
lack "the installer's own unit is never checked" "wrong.service" "$(cat "$SYSTEMCTL_CALLS")"
want "the Composite's unit is checked and satisfied" '"key":"units-daemon:right.service","status":"satisfied"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "23. Cockpit: a materialised Composite's CockpitSpec.dashboards replaces the hardcoded health+mail pair"
# ==========================================================================================
reset_state
cat > "$TMUX_STATE" <<'TEOF'
tag=health
TEOF
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Cockpit"
producer = "test"
[resource.metadata]
name = "cockpit"
[resource.spec]
dashboards = ["queue"]'
rpass
want "health alone no longer satisfies once the Composite asks for queue" "layout ensure" "$(cat "$COCKPIT_CALLS")"

reset_state
cat > "$TMUX_STATE" <<'TEOF'
tag=queue
TEOF
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Cockpit"
producer = "test"
[resource.metadata]
name = "cockpit"
[resource.spec]
dashboards = ["queue"]'
rpass
[ ! -s "$COCKPIT_CALLS" ] && ok "queue alone satisfies the Composite's own dashboard list" || bad "unexpected cockpit call: $(cat "$COCKPIT_CALLS")"

# ==========================================================================================
printf '\n%s\n' "24. Release: a materialised Composite's ReleaseSpec names the target and can allow a local override"
# ==========================================================================================
reset_state
echo "feature-x" > "$GIT_BRANCH_STATE"
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Release"
producer = "test"
[resource.metadata]
name = "release"
[resource.spec]
release = "v9"
allow_override = false'
rpass
want "a non-main checkout not at the declared release still gaps and escalates" "invariant: release" "$(cat "$MAIL_LOG")"

reset_state
echo "feature-x" > "$GIT_BRANCH_STATE"
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Release"
producer = "test"
[resource.metadata]
name = "release"
[resource.spec]
release = "v9"
allow_override = true'
rpass
want "allow_override lets a local override satisfy — a deliberate state is not a fault" '"key":"release","status":"satisfied"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "25. Fleet: SPIRA_MAX_AEONS=0 satisfies a starved task partition, not a gap"
# ==========================================================================================
reset_state
printf 'builder\t150\t0\t1\n' > "$FLEET_LINES"
tl_config SPIRA_MAX_LIVE_AEONS=3 SPIRA_MAX_AEONS=0   # pool 0 == deliberately paused
rpass
want "a deliberately paused task pool satisfies the builder partition" '"key":"fleet:builder","status":"satisfied"' "$(status_jsonl)"
lack "no escalation while the pool is deliberately paused" "invariant: fleet:builder" "$(cat "$MAIL_LOG")"

# ==========================================================================================
printf '\n%s\n' "26. Fleet: the pool-at-0 carve-out does not cover a lane partition"
# ==========================================================================================
reset_state
printf 'ops\t5\t0\t0\n' > "$FLEET_LINES"   # is-task=0: ops draws outside SPIRA_MAX_AEONS
rpass
want "a starved lane partition still gaps while the task pool is paused" '"key":"fleet:ops","status":"gap"' "$(status_jsonl)"
want "a starved lane partition still escalates while the task pool is paused" "invariant: fleet:ops" "$(cat "$MAIL_LOG")"
: > "$FLEET_LINES"
tl_config SPIRA_MAX_AEONS=6

# ==========================================================================================
printf '\n%s\n' "27. Disk, POSITIVE CONTROL: every path above its floor -> satisfied, no remedy"
# ==========================================================================================
reset_state
: > "$UNITS_LIST"          # 22 left "wrong.service" here, unread by any systemctl fixture since
echo main > "$GIT_BRANCH_STATE"   # 24 left this on "feature-x" — a positive control must not inherit it
printf '/\t40\n/var/lib/dolt\t62\n' > "$DISK_LINES"
rpass
[ ! -s "$DISK_REMEDY_CALLS" ] && ok "no disk remedy when every path is above its floor" || bad "unexpected disk remedy: $(cat "$DISK_REMEDY_CALLS")"
want "status jsonl reports / satisfied" '"key":"disk:/","status":"satisfied"' "$(status_jsonl)"
[ ! -s "$MAIL_LOG" ] && ok "no incident filed when every path is healthy" || bad "unexpected incident: $(cat "$MAIL_LOG")"

# ==========================================================================================
printf '\n%s\n' "28. Disk: a path below the floor -> gap -> the remedy sequence runs"
# ==========================================================================================
reset_state
printf '/\t4\n' > "$DISK_LINES"
rpass
_calls="$(cat "$DISK_REMEDY_CALLS")"
want "the landed worktrees' build output is reaped" "target-reap" "$_calls"
want "dangling podman images are pruned" "podman image prune -f" "$_calls"
want "unattached podman volumes are pruned" "podman volume prune -f" "$_calls"
is "the remedy sequence runs once for the breached path" "3" "$(wc -l < "$DISK_REMEDY_CALLS" | tr -d ' ')"
want "status jsonl records the gap" '"key":"disk:/","status":"gap","desired":">= 15% free","observed":"4% free"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "29. Disk: a Composite-named path df cannot read -> unobservable, never satisfied, no remedy"
# ==========================================================================================
reset_state
: > "$DISK_LINES"   # df cannot read /no/such/path
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Disk"
producer = "test"
[resource.metadata]
name = "disk"
[resource.spec]
floor_pct = 15
paths = ["/no/such/path"]'
rpass
want "status jsonl reports unobservable" '"key":"disk:/no/such/path","status":"unobservable"' "$(status_jsonl)"
lack "unobservable disk path is never reported satisfied" '"key":"disk:/no/such/path","status":"satisfied"' "$(status_jsonl)"
[ ! -s "$DISK_REMEDY_CALLS" ] && ok "no remedy run for a path that cannot be read" || bad "unexpected disk remedy: $(cat "$DISK_REMEDY_CALLS")"

# ==========================================================================================
printf '\n%s\n' "30. Disk: a materialised Composite's DiskSpec names the paths checked"
# ==========================================================================================
reset_state
printf '/srv/custom\t40\n' > "$DISK_LINES"
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Disk"
producer = "test"
[resource.metadata]
name = "disk"
[resource.spec]
floor_pct = 15
paths = ["/srv/custom"]'
rpass
want "df is asked about the Composite's own path" "/srv/custom" "$(cat "$DISK_USAGE_CALLS")"
want "status jsonl reports the Composite's path satisfied" '"key":"disk:/srv/custom","status":"satisfied"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "31. Disk: a materialised Composite's floor_pct replaces the bash/env default"
# ==========================================================================================
reset_state
printf '/\t20\n' > "$DISK_LINES"   # satisfies the default 15% floor
desired_state_write \
'[[resource]]
apiVersion = "spira/v1"
kind = "Disk"
producer = "test"
[resource.metadata]
name = "disk"
[resource.spec]
floor_pct = 25
paths = ["/"]'
rpass
want "20% free gaps against a Composite floor of 25%, though it would satisfy the default 15%" '"key":"disk:/","status":"gap"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "33. Junk rows: a rowless READY id is dropped, a real bead's row is not"
# ==========================================================================================
reset_state; : > "$LC_CALLS"; rm -f "$BD_JSON.fail"
printf '[{"id":"sp-real"},{"id":"sp-other"}]\n' > "$BD_JSON"
printf '[{"bead_id":"sp-real","state":"READY"},{"bead_id":"fixture key","state":"READY"}]\n' > "$LC_ROWS"
rpass
want "the rowless READY id is dropped through spira-lc" "drop fixture key" "$(cat "$LC_CALLS")"
lack "a real bead's row is untouched" "sp-real" "$(cat "$LC_CALLS")"

reset_state; : > "$LC_CALLS"; touch "$BD_JSON.fail"
rpass
lack "an unreadable store drops nothing" "drop" "$(cat "$LC_CALLS")"
rm -f "$BD_JSON.fail"

tl_summary
