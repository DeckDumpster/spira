#!/usr/bin/env bash
#
# test-reconciler.sh — the reconciler: structural invariants and their deterministic
# remedies (sp-ocmes), and reading them from spira-desired-state's typed Composite in
# preference to the ad-hoc bash/env defaults each invariant fell back to before (sp-8c3ib).
#
# WHAT THIS SUITE CHECKS.
#   1. reconciler.sh exists, is executable, and a pass completes inside its budget.
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
#   9. Fleet: SPIRA_MAX_LIVE_AEONS unset → unobservable, never satisfied.
#  10. Cockpit, POSITIVE CONTROL: sessions + both dashboards present → satisfied.
#  11. Cockpit: no dashboards, no `cockpit.down` marker → gap → cockpit.sh --no-attach run.
#  12. Cockpit: `cockpit.down` present → satisfied, no remedy run (a deliberate state is
#      not a fault — law-a-deliberate-state-is-not-a-fault).
#  13. Release: checkout on a non-main branch with no activated release → gap, escalate.
#  14. Release: checkout on main → satisfied.
#  15. Queue: a certified branch that no longer merges onto its base → gap → queue.sh
#      eject run (reopens it for rebase).
#  16. Queue: a certified branch that still merges cleanly → satisfied.
#  17. Queue: a lock held past the pre-flight wall → gap, escalate.
#  18. Grace period: a fresh gap inside its grace period raises nothing — no remedy run,
#      no incident filed — but the time series still records the gap (seen, not smoothed).
#  19. Unobservable: systemctl unreadable → status=unobservable, never satisfied.
#  20. A remedy that does not close its gap escalates on the next pass instead of being
#      retried blind, and is not retried a second time.
#  21. Fleet: a materialised Composite's ceiling is read even when fleet-status.sh's own
#      SPIRA_MAX_LIVE_AEONS column is empty (so this is a gap, not unobservable).
#  22. Units: a materialised Composite's UnitsSpec names the unit set — units-manifest.sh's
#      own list is not consulted once one exists.
#  23. Cockpit: a materialised Composite's CockpitSpec.dashboards replaces the hardcoded
#      health+mail pair, both when it demands more and when it demands less.
#  24. Release: a materialised Composite's ReleaseSpec names the target explicitly, and
#      allow_override lets a local divergence from it satisfy instead of escalating.
#
# 1-20 run with no Composite ever materialised — every invariant's fallback to its old
# bash/env default, still the state of an install that has not adopted desired-state yet.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): 3 and 10 assert nothing fires
# on a healthy fixture before 4-9 and 11-17 add the trigger and assert it does.
#
# covers: reconciler/src/main.rs reconciler-engine/src/**.rs desired-state/src/store.rs
#         desired-state/src/resource.rs spira/reconciler.sh
#         spira/units-manifest.sh spira/fleet-status.sh spira/queue-certified-list.sh
#         spira/conf.sh cockpit/layout.sh
#         systemd/spira-reconciler.service systemd/spira-reconciler.timer
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s\n' "$1"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1: wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1: wanted [$2] got [$3]"; }

RECONCILER_SH="$HERE/reconciler.sh"
[ -x "$RECONCILER_SH" ] || { printf 'reconciler.sh not found or not executable: %s\n' "$RECONCILER_SH" >&2; exit 2; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-reconciler: cargo not found — reconciler binary cannot be built"
    exit 77
fi
RECONCILER_ROOT="$HERE/../reconciler"
RECONCILER_BIN="$RECONCILER_ROOT/target/release/reconciler"
if [ ! -x "$RECONCILER_BIN" ]; then
    cp -r "$RECONCILER_ROOT/." "$T/reconciler-src"
    cp -r "$HERE/../reconciler-engine" "$T/reconciler-engine"
    cp -r "$HERE/../desired-state" "$T/desired-state"
    printf '  (building reconciler into %s)\n' "$T/reconciler-target"
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/reconciler-target" \
        "$CARGO_BIN" build --release \
        --manifest-path "$T/reconciler-src/Cargo.toml" 2>&1 | tail -5
    RECONCILER_BIN="$T/reconciler-target/release/reconciler"
fi
if [ ! -x "$RECONCILER_BIN" ]; then
    printf 'reconciler binary not found at %s\n' "$RECONCILER_BIN" >&2
    printf '0 passed, 1 failed\n'
    exit 1
fi
export SPIRA_RECONCILER_BIN="$RECONCILER_BIN"

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_DB="$T/db"
mkdir -p "$SPIRA_RUN/queue" "$SPIRA_DB"

# Pinned so this suite's verdict never depends on whatever composite the real host it runs
# on has actually applied. desired_state_write below populates it on demand.
DESIRED_DIR="$T/desired"
export SPIRA_DESIRED_DIR="$DESIRED_DIR"

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
# Stubs. Every IO seam the reconciler shells out to is overridden with a small script this
# suite controls — no real systemctl/tmux/git/queue is ever touched (law-run-the-suite-in-a-container
# notwithstanding: this is the "explicit, minimal environment" half of that rule).
# ------------------------------------------------------------------------------------------

SYSTEMCTL_STATE="$T/systemctl-state"   # "<unit> <enabled|disabled> <active|inactive>" per line
SYSTEMCTL_CALLS="$T/systemctl-calls.log"
: > "$SYSTEMCTL_STATE"; : > "$SYSTEMCTL_CALLS"
STUB_SYSTEMCTL="$T/systemctl"
cat > "$STUB_SYSTEMCTL" <<'SEOF'
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
chmod +x "$STUB_SYSTEMCTL"
export SPIRA_SYSTEMCTL="$STUB_SYSTEMCTL" SYSTEMCTL_STATE SYSTEMCTL_CALLS

TMUX_STATE="$T/tmux-state"   # controls has-session/list-panes responses
: > "$TMUX_STATE"
STUB_TMUX="$T/tmux"
cat > "$STUB_TMUX" <<'TEOF'
#!/usr/bin/env bash
case "$1" in
    list-sessions) grep -q '^server=down$' "$TMUX_STATE" 2>/dev/null && exit 1; exit 0 ;;
    has-session)   grep -q '^server=down$' "$TMUX_STATE" 2>/dev/null && exit 1; exit 0 ;;
    list-panes)    grep '^tag=' "$TMUX_STATE" 2>/dev/null | sed 's/^tag=//'; exit 0 ;;
    *) exit 0 ;;
esac
TEOF
chmod +x "$STUB_TMUX"
export SPIRA_TMUX="$STUB_TMUX" TMUX_STATE

GIT_BRANCH_STATE="$T/git-branch"     # branch name for `rev-parse --abbrev-ref HEAD`
MERGE_STATE="$T/git-merge-state"     # "clean" or "conflict"
echo main > "$GIT_BRANCH_STATE"
echo clean > "$MERGE_STATE"
STUB_GIT="$T/git"
cat > "$STUB_GIT" <<'GEOF'
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
chmod +x "$STUB_GIT"
export SPIRA_GIT="$STUB_GIT" GIT_BRANCH_STATE MERGE_STATE

UNITS_LIST="$T/units-list"
: > "$UNITS_LIST"
STUB_UNITS_MANIFEST="$T/units-manifest.sh"
printf '#!/usr/bin/env bash\ncat "%s"\n' "$UNITS_LIST" > "$STUB_UNITS_MANIFEST"
chmod +x "$STUB_UNITS_MANIFEST"
export SPIRA_UNITS_MANIFEST_SH="$STUB_UNITS_MANIFEST"

FLEET_LINES="$T/fleet-lines"
printf 'TOTAL\t\t0\n' > "$FLEET_LINES"
STUB_FLEET_STATUS="$T/fleet-status.sh"
printf '#!/usr/bin/env bash\ncat "%s"\n' "$FLEET_LINES" > "$STUB_FLEET_STATUS"
chmod +x "$STUB_FLEET_STATUS"
export SPIRA_FLEET_STATUS_SH="$STUB_FLEET_STATUS"

CERTIFIED_LINES="$T/certified-lines"
: > "$CERTIFIED_LINES"
STUB_CERTIFIED_LIST="$T/queue-certified-list.sh"
printf '#!/usr/bin/env bash\ncat "%s"\n' "$CERTIFIED_LINES" > "$STUB_CERTIFIED_LIST"
chmod +x "$STUB_CERTIFIED_LIST"
export SPIRA_QUEUE_CERTIFIED_LIST_SH="$STUB_CERTIFIED_LIST"

COCKPIT_CALLS="$T/cockpit-calls.log"
: > "$COCKPIT_CALLS"
STUB_COCKPIT_SH="$T/cockpit.sh"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$COCKPIT_CALLS" > "$STUB_COCKPIT_SH"
chmod +x "$STUB_COCKPIT_SH"
export SPIRA_COCKPIT_SH="$STUB_COCKPIT_SH"

QUEUE_CALLS="$T/queue-calls.log"
: > "$QUEUE_CALLS"
STUB_QUEUE_SH="$T/queue.sh"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$QUEUE_CALLS" > "$STUB_QUEUE_SH"
chmod +x "$STUB_QUEUE_SH"
export SPIRA_QUEUE_SH="$STUB_QUEUE_SH"

INC_LOG="$T/incident-calls.log"
: > "$INC_LOG"
STUB_INC="$T/incident.sh"
cat > "$STUB_INC" <<'IEOF'
#!/usr/bin/env bash
cat >/dev/null
printf 'cause=%s ref=%s\n' "${SPIRA_INCIDENT_CAUSE:-}" "${SPIRA_INCIDENT_REF:-}" >> "$INC_LOG"
exit 0
IEOF
chmod +x "$STUB_INC"
export SPIRA_INCIDENT_SH="$STUB_INC" INC_LOG

export SPIRA_REPO_MAP="$T/repo-map"
printf 'testrepo | %s | queue | origin/main | | \n' "$T/repo" > "$SPIRA_REPO_MAP"
mkdir -p "$T/repo"
# queue_repo_names walks $SPIRA_RUN/queue/*/ — the Queue checks only ever look at a repo
# that has a directory here, so "testrepo" must exist for the whole run even on passes
# that leave its certified-list/lock fixtures empty.
mkdir -p "$SPIRA_RUN/queue/testrepo"

reset_state() {
    rm -f "$SPIRA_RUN/reconciler-state.json" "$SPIRA_RUN/reconciler-status.jsonl" \
          "$SPIRA_RUN/reconciler.log" "$SPIRA_RUN/cockpit.down"
    : > "$SYSTEMCTL_CALLS"; : > "$COCKPIT_CALLS"; : > "$QUEUE_CALLS"; : > "$INC_LOG"
    desired_state_clear
}

status_jsonl() { cat "$SPIRA_RUN/reconciler-status.jsonl" 2>/dev/null || true; }

printf 'test-reconciler.sh\n'

# ==========================================================================================
printf '\n%s\n' "1. reconciler.sh exists, executable, budget"
# ==========================================================================================
[ -f "$RECONCILER_SH" ] && ok "reconciler.sh exists" || bad "reconciler.sh missing"
[ -x "$RECONCILER_SH" ] && ok "reconciler.sh is executable" || bad "reconciler.sh not executable"
reset_state
_t0="$(date +%s)"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
_t1="$(date +%s)"
[ $((_t1-_t0)) -lt 10 ] && ok "pass completed in $((_t1-_t0))s (budget: 10s)" || bad "pass exceeded budget"

# ==========================================================================================
printf '\n%s\n' "2. flock: concurrent pass is skipped"
# ==========================================================================================
_lock="$SPIRA_RUN/reconciler.lock"
exec 9>"$_lock"; flock 9
_out="$(bash "$RECONCILER_SH" --pass 2>&1 || true)"
exec 9>&-
want "concurrent pass logs 'already running'" "already running" "$_out"

export SPIRA_RECONCILER_GRACE_SECS=0

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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
_calls="$(cat "$SYSTEMCTL_CALLS")"
lack "no enable/restart calls when everything is healthy" "enable" "$(grep -v is- <<<"$_calls" || true)"
[ ! -s "$INC_LOG" ] && ok "no incident filed when everything is healthy" || bad "unexpected incident: $(cat "$INC_LOG")"

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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
_n="$(grep -c -- '--user enable --now' "$SYSTEMCTL_CALLS" || true)"
is "3 disabled timers each get enable --now" "3" "$_n"
want "status jsonl records a gap for a.timer" '"key":"units-timer:a.timer"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "5. Units: active-but-not-enabled daemon gets enable, never --now"
# ==========================================================================================
reset_state
printf 'spira-loom-prod.service\n' > "$UNITS_LIST"
printf 'spira-loom-prod.service disabled active\n' > "$SYSTEMCTL_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "enable is called" "--user enable spira-loom-prod.service" "$(cat "$SYSTEMCTL_CALLS")"
lack "restart is never called for an active daemon" "restart" "$(cat "$SYSTEMCTL_CALLS")"
lack "--now is never used on a running daemon" "enable --now spira-loom-prod" "$(cat "$SYSTEMCTL_CALLS")"

# ==========================================================================================
printf '\n%s\n' "6. Units: an inactive daemon gets restarted"
# ==========================================================================================
reset_state
printf 'spira-loom-prod.service\n' > "$UNITS_LIST"
printf 'spira-loom-prod.service enabled inactive\n' > "$SYSTEMCTL_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "restart is called on an inactive daemon" "--user restart spira-loom-prod.service" "$(cat "$SYSTEMCTL_CALLS")"

# ==========================================================================================
printf '\n%s\n' "7. Store: dolt-beads is never restarted, only escalated"
# ==========================================================================================
reset_state
printf 'dolt-beads.service\n' > "$UNITS_LIST"
printf 'dolt-beads.service disabled inactive\n' > "$SYSTEMCTL_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
_calls="$(cat "$SYSTEMCTL_CALLS")"
lack "no restart of dolt-beads" "restart dolt-beads" "$_calls"
lack "no enable of dolt-beads" "enable --now dolt-beads" "$_calls"
lack "no plain enable of dolt-beads" "enable dolt-beads" "$_calls"
want "dolt-beads down escalates" "cause=store:dolt-beads.service" "$(cat "$INC_LOG")"

# ==========================================================================================
printf '\n%s\n' "8. Fleet: 3 live ceiling, 150 ready, 0 live -> gap, escalate, no remedy"
# ==========================================================================================
reset_state
: > "$UNITS_LIST"
printf 'builder\t150\t0\nTOTAL\t3\t0\n' > "$FLEET_LINES"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "fleet gap escalates" "cause=fleet:builder" "$(cat "$INC_LOG")"
want "status jsonl records desired=3 observed=0" '"desired":"3","observed":"0"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "9. Fleet: SPIRA_MAX_LIVE_AEONS unset -> unobservable, never satisfied"
# ==========================================================================================
reset_state
printf 'builder\t150\t0\nTOTAL\t\t0\n' > "$FLEET_LINES"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "fleet with no ceiling is unobservable" '"status":"unobservable"' "$(status_jsonl)"
lack "unobservable fleet is never reported satisfied" '"key":"fleet:builder","status":"satisfied"' "$(status_jsonl)"
printf 'TOTAL\t\t0\n' > "$FLEET_LINES"

# ==========================================================================================
printf '\n%s\n' "10. Cockpit, POSITIVE CONTROL: sessions + both dashboards present"
# ==========================================================================================
reset_state
cat > "$TMUX_STATE" <<'EOF'
tag=health
tag=mail
EOF
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
[ ! -s "$COCKPIT_CALLS" ] && ok "no cockpit repair when dashboards are present" || bad "unexpected cockpit call: $(cat "$COCKPIT_CALLS")"

# ==========================================================================================
printf '\n%s\n' "11. Cockpit: no dashboards, no down marker -> gap -> repair run"
# ==========================================================================================
reset_state
: > "$TMUX_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "cockpit.sh --no-attach is run to repair" "--no-attach" "$(cat "$COCKPIT_CALLS")"

# ==========================================================================================
printf '\n%s\n' "12. Cockpit: layout.sh down marker present -> satisfied, no repair"
# ==========================================================================================
reset_state
: > "$TMUX_STATE"
: > "$SPIRA_RUN/cockpit.down"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
[ ! -s "$COCKPIT_CALLS" ] && ok "a deliberate down is never repaired" || bad "unexpected cockpit call: $(cat "$COCKPIT_CALLS")"
want "status jsonl reports satisfied while down" '"key":"cockpit","status":"satisfied"' "$(status_jsonl)"
rm -f "$SPIRA_RUN/cockpit.down"

# ==========================================================================================
printf '\n%s\n' "13. Release: non-main branch, no activated release -> gap, escalate"
# ==========================================================================================
reset_state
echo "feature-x" > "$GIT_BRANCH_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "release gap escalates" "cause=release" "$(cat "$INC_LOG")"

# ==========================================================================================
printf '\n%s\n' "14. Release: checkout on main -> satisfied"
# ==========================================================================================
reset_state
echo "main" > "$GIT_BRANCH_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "status jsonl reports release satisfied on main" '"key":"release","status":"satisfied"' "$(status_jsonl)"

# ==========================================================================================
printf '\n%s\n' "15. Queue: a certified branch that no longer merges -> gap -> eject"
# ==========================================================================================
reset_state
echo "sp-abc123 spira/sp-abc123 origin/main" > "$CERTIFIED_LINES"
echo "conflict" > "$MERGE_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "queue.sh eject is run for the unmergeable branch" "eject sp-abc123" "$(cat "$QUEUE_CALLS")"

# ==========================================================================================
printf '\n%s\n' "16. Queue: a certified branch that still merges cleanly -> satisfied"
# ==========================================================================================
reset_state
echo "sp-abc123 spira/sp-abc123 origin/main" > "$CERTIFIED_LINES"
echo "clean" > "$MERGE_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
[ ! -s "$QUEUE_CALLS" ] && ok "no eject for a branch that still merges" || bad "unexpected eject: $(cat "$QUEUE_CALLS")"
: > "$CERTIFIED_LINES"

# ==========================================================================================
printf '\n%s\n' "17. Queue: a lock held past the pre-flight wall -> gap, escalate"
# ==========================================================================================
reset_state
export SPIRA_PREFLIGHT_WALL_SECS=0
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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "a held, stale lock escalates" "cause=queue-lock-age:testrepo" "$(cat "$INC_LOG")"
wait "$_holder" 2>/dev/null || true
rm -f "$_lockfile"
unset SPIRA_PREFLIGHT_WALL_SECS

# ==========================================================================================
printf '\n%s\n' "18. Grace period: a fresh gap inside grace raises nothing"
# ==========================================================================================
export SPIRA_RECONCILER_GRACE_SECS=300
reset_state
printf 'a.timer\n' > "$UNITS_LIST"
printf 'a.timer disabled inactive\n' > "$SYSTEMCTL_STATE"
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
lack "no remedy run inside the grace period" "enable --now" "$(cat "$SYSTEMCTL_CALLS")"
[ ! -s "$INC_LOG" ] && ok "no incident filed inside the grace period" || bad "unexpected incident: $(cat "$INC_LOG")"
want "the gap is still recorded in the time series" '"key":"units-timer:a.timer","status":"gap"' "$(status_jsonl)"
export SPIRA_RECONCILER_GRACE_SECS=0

# ==========================================================================================
printf '\n%s\n' "19. Unobservable: systemctl unreadable -> unobservable, never satisfied"
# ==========================================================================================
reset_state
printf 'z.timer\n' > "$UNITS_LIST"
: > "$SYSTEMCTL_STATE"   # no row for z.timer: the stub reports "unreadable" and exits 1
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1   # pass 1: remedy attempted
_n1="$(grep -c -- '--user enable --now r.timer' "$SYSTEMCTL_CALLS" || true)"
is "pass 1 attempts the remedy once" "1" "$_n1"
[ ! -s "$INC_LOG" ] && ok "pass 1 does not escalate yet" || bad "pass 1 escalated early: $(cat "$INC_LOG")"

bash "$RECONCILER_SH" --pass >/dev/null 2>&1   # pass 2: still a gap
_n2="$(grep -c -- '--user enable --now r.timer' "$SYSTEMCTL_CALLS" || true)"
is "pass 2 does not retry the remedy blind" "1" "$_n2"
want "pass 2 escalates instead" "cause=units-timer:r.timer" "$(cat "$INC_LOG")"

# ==========================================================================================
printf '\n%s\n' "21. Fleet: a materialised Composite's ceiling is read instead of fleet-status.sh's own SPIRA_MAX_LIVE_AEONS column"
# ==========================================================================================
reset_state
printf 'TOTAL\t\t0\nbuilder\t5\t0\n' > "$FLEET_LINES"   # empty TOTAL column: fleet-status.sh itself has no ceiling
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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
lack "the Composite's ceiling makes this observable, not unobservable" '"key":"fleet:builder","status":"unobservable"' "$(status_jsonl)"
want "fleet gap escalates against the Composite's ceiling" "cause=fleet:builder" "$(cat "$INC_LOG")"

# ==========================================================================================
printf '\n%s\n' "22. Units: a materialised Composite's UnitsSpec names the unit set instead of units-manifest.sh"
# ==========================================================================================
reset_state
printf 'wrong.service\n' > "$UNITS_LIST"    # units-manifest.sh's own list — must be ignored
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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
lack "units-manifest.sh's own unit is never checked" "wrong.service" "$(cat "$SYSTEMCTL_CALLS")"
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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "health alone no longer satisfies once the Composite asks for queue" "--no-attach" "$(cat "$COCKPIT_CALLS")"

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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "a non-main checkout not at the declared release still gaps and escalates" "cause=release" "$(cat "$INC_LOG")"

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
bash "$RECONCILER_SH" --pass >/dev/null 2>&1
want "allow_override lets a local override satisfy — a deliberate state is not a fault" '"key":"release","status":"satisfied"' "$(status_jsonl)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
