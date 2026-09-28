#!/usr/bin/env bash
#
# test-queue-watch.sh — queue-watch's cargo tests, then the built binary end to end.
#
# WHAT IS EXERCISED
#   1. The crate's own #[test]s: fixture replays of every transition the queue made on
#      2026-09-24, including the ones its log never recorded (an eviction), a bisect-forced
#      cut, a priority inversion, stalls, and a blind poll that must not read as quiet.
#   2. The binary against a fixture SPIRA_RUN, a fake bead store and a fake forge, over five
#      polls in one process: baseline, then red + ejection + re-push, then a landing verified
#      on a real git base, then a forced cut with a priority inversion, then a close without
#      landing. The fake `bd` is called once per poll and advances the scenario, so the
#      sequence is deterministic with no sleeps to race.
#   3. health: passes after a good poll, fails when the last poll was blind, and fails when
#      it has never polled — silence must never read as a quiet queue.
#   4. An install with no mode=queue repository (or no spira.toml yet) is IDLE: it says so,
#      stays up, and health FAILS (DEGRADED) rather than passing — idle must not look like a
#      healthy quiet queue. A spira.toml that does not parse is fatal.
#   4b. An idle watcher re-reads its config every tick rather than latching idle forever: a
#       config gutted down to no queue-mode repo, then restored, is noticed and watching
#       resumes with no restart.
#   5. A job stuck in the forge's own "queued" state with no runner ever assigned is named
#      distinctly from a job that is actually running, judged on its own queue time
#      (SPIRA_CI_QUEUED_MAX_SECS) rather than the much longer head-stall window — and the
#      finding is handed to a fake incident.sh, not left in the log for a reader who has to be
#      tailing it.
#   6. The watchd manifest row expands, and names queue-watch bare.
#
# QUEUE_WATCH_BIN may point at another binary; pointing it at a stub that prints nothing is
# how every assertion below was seen to fail first.
#
# tier: T3
# covers: queue-watch/* spira/watchers forge/src/*
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

. "$HERE/testlib.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-queue-watch: cargo not found on PATH or at ~/.cargo/bin" >&2
    exit 77
fi
export PATH="$(dirname "$CARGO_BIN"):$PATH"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# --- 1. unit replays -------------------------------------------------------------------------
UNIT_OUT="$T/unit-test.out"
CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/target" "$CARGO_BIN" test --no-fail-fast \
    --manifest-path "$ROOT/queue-watch/Cargo.toml" > "$UNIT_OUT" 2>&1
_rc=$?
cat "$UNIT_OUT"
report_cargo "$UNIT_OUT" "$_rc"

# The tree's queue-watch, by name on the suite's PATH (sp-gypjk).
BIN=queue-watch
command -v "$BIN" >/dev/null 2>&1 || bail "queue-watch is not on PATH"

# --- fixtures --------------------------------------------------------------------------------
RUN="$T/run"; FX="$T/fx"; Q="$RUN/queue/q"
mkdir -p "$Q" "$FX"

# A real base: an origin with main, and a checkout whose origin/main a tip can be an
# ancestor of — "landed" is verified against the commit graph, never taken from the forge.
git init -q --bare "$T/origin.git"
git clone -q "$T/origin.git" "$T/repo" 2>/dev/null
git -C "$T/repo" -c user.name=t -c user.email=t@t commit -q --allow-empty -m base
git -C "$T/repo" push -q origin HEAD:main 2>/dev/null
git -C "$T/repo" -c user.name=t -c user.email=t@t commit -q --allow-empty -m "land sp-a"
TIP_A="$(git -C "$T/repo" rev-parse HEAD)"
git -C "$T/repo" push -q origin HEAD:main 2>/dev/null

cat > "$FX/spira.toml" <<EOF
[repo.q]
path = "$T/repo"
mode = "queue"
base = "origin/main"

[repo.p]
path = "$T/repo"
mode = "push"
EOF

cat > "$FX/beads.json" <<'EOF'
[
 {"id":"sp-a","priority":0,"title":"alpha work","labels":["repo:q"]},
 {"id":"sp-b","priority":3,"title":"sp-zzz99: beta work","labels":["repo:q"]},
 {"id":"sp-c","priority":3,"title":"gamma work","labels":["repo:q"]},
 {"id":"sp-d","priority":0,"title":"delta urgent","labels":["repo:q","express"]},
 {"id":"sp-o","priority":0,"title":"other repo","labels":["repo:elsewhere"]}
]
EOF

# Fake forge: answers from files the scenario writes.
cat > "$FX/forge.sh" <<'EOF'
#!/usr/bin/env bash
sub="$1"; pr="${3:-}"
case "$sub" in
    check-status) cat "$FX/ci-$pr" 2>/dev/null || echo pending ;;
    pr-state)     cat "$FX/state-$pr" 2>/dev/null || echo unknown ;;
    *) exit 2 ;;
esac
EOF

# Fake bd: answers `show --json <ids>` from beads.json, then advances the scenario one step.
cat > "$FX/bd" <<'EOF'
#!/usr/bin/env bash
[ "$1" = show ] || exit 2
shift; [ "$1" = --json ] && shift
python3 - "$FX/beads.json" "$@" <<'PY'
import json, sys
want = set(sys.argv[2:])
print(json.dumps([b for b in json.load(open(sys.argv[1])) if b["id"] in want]))
PY
n=$(( $(cat "$FX/calls" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$FX/calls"
[ -f "$FX/step-$n.sh" ] && bash "$FX/step-$n.sh"
exit 0
EOF

# Fake spira-lc: answers `list --state CERTIFIED` from certified.json — the CERTIFIED pool
# now lives in spira_lifecycle, read through spira-lc, never a landstate directory scan.
# lc-broken is a POSITIVE CONTROL toggle: with it present, spira-lc refuses to tell, and
# read_certified must surface that as blind rather than reading zero certified beads.
cat > "$FX/spira-lc" <<'EOF'
#!/usr/bin/env bash
[ -f "$FX/lc-broken" ] && { echo "cannot tell: db unreachable" >&2; exit 2; }
[ "$1" = list ] && [ "$2" = --state ] && [ "$3" = CERTIFIED ] || { echo "unexpected args: $*" >&2; exit 2; }
cat "$FX/certified.json" 2>/dev/null || echo '[]'
EOF
chmod +x "$FX/forge.sh" "$FX/bd" "$FX/spira-lc"

# Poll 1 sees: batch 50 (sp-a, sp-b), sp-o certified in another repo.
printf 'pr=50\nhead=h1\nmembers=sp-a:%s sp-b:bbbb\n' "$TIP_A" > "$Q/open"
printf '[{"bead_id":"sp-o"}]' > "$FX/certified.json"
# After poll 1: CI red, sp-b ejected, survivors re-pushed.
cat > "$FX/step-1.sh" <<EOF
echo red > "$FX/ci-50"
printf 'pr=50\nhead=h2\nmembers=sp-a:$TIP_A\n' > "$Q/open.t" && mv "$Q/open.t" "$Q/open"
EOF
# After poll 2: PR 50 merged; the batch file is gone.
cat > "$FX/step-2.sh" <<EOF
echo merged > "$FX/state-50"
rm -f "$Q/open"
EOF
# After poll 3: PR 51 forced to a stale bisect group of P3 work while a P0 waits.
cat > "$FX/step-3.sh" <<EOF
printf 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeef sp-c:cccc\nsp-b:bbbb\n' > "$Q/bisect"
touch -d '3 hours ago' "$Q/bisect"
printf 'pr=51\nhead=h3\nmembers=sp-c:cccc\n' > "$Q/open"
printf '[{"bead_id":"sp-o"},{"bead_id":"sp-d"}]' > "$FX/certified.json"
EOF
# After poll 4: PR 51 closed without merging; its member back in CERTIFIED.
cat > "$FX/step-4.sh" <<EOF
echo closed > "$FX/state-51"
rm -f "$Q/open"
printf '[{"bead_id":"sp-o"},{"bead_id":"sp-d"},{"bead_id":"sp-c"}]' > "$FX/certified.json"
EOF

export FX SPIRA_BD="$FX/bd" SPIRA_FORGE="$FX/forge.sh" SPIRA_LC_BIN="$FX/spira-lc"
out="$("$BIN" watch --ticks 5 --interval 1 --run "$RUN" --home "$FX" --config "$FX/spira.toml" 2>&1)"
printf '%s\n' "$out" | sed 's/^/    | /'

# --- 2. the event stream ---------------------------------------------------------------------
want "baseline names the open batch"             "q watching: PR 50 open with 2 member(s)" "$out"
want "baseline strips a foreign bead-id prefix"   "sp-b (P3) beta work" "$out"
nowant "another repo's certified bead is not counted" "sp-o" "$out"
want "CI red reported"                            "q ci: PR 50 CI red" "$out"
want "ejection reported from state"               "q ejected: sp-b (P3) beta work ejected from PR 50 after a red run" "$out"
want "re-push reported"                           "q repushed: PR 50 re-pushed with 1 member(s)" "$out"
want "landing verified on the real base"          "q landed: PR 50 landed (verified on base)" "$out"
want "new batch reported"                         "q opened: PR 51 opened with 1 member(s): sp-c (P3) gamma work" "$out"
want "stale bisect group named as a forced cut"   "q forced-cut: PR 51's membership was forced by a bisect group recorded 180m ago" "$out"
want "priority inversion names the waiting P0"    "q priority-inversion: PR 51 took P3 work while 1 more urgent certified bead(s) wait: sp-d (P0 express) delta urgent" "$out"
want "close without landing reported from state"  "q closed-unlanded: PR 51 closed without landing: all members back in CERTIFIED" "$out"
nowant "the push-mode repo is not watched"          " p watching" "$out"

# --- 3. health -------------------------------------------------------------------------------
"$BIN" health --run "$RUN" >/dev/null 2>&1 && ok "health passes after a good poll" || bad "health passes after a good poll"

touch "$FX/lc-broken"
blind="$("$BIN" watch --ticks 2 --interval 1 --run "$RUN" --home "$FX" --config "$FX/spira.toml" 2>&1)"
want "an unreadable queue is reported blind after two failed polls"      "q blind: cannot see the queue" "$blind"
herr="$("$BIN" health --run "$RUN" 2>&1)"; hrc=$?
[ "$hrc" -ne 0 ] && ok "health fails after a blind poll" || bad "health fails after a blind poll (rc=$hrc)"
want "health says why"                            "blind" "$herr"
rm -f "$FX/lc-broken"

herr="$("$BIN" health --run "$T/never" 2>&1)"; hrc=$?
[ "$hrc" -ne 0 ] && ok "health fails when it has never polled" || bad "health fails when it has never polled (rc=$hrc)"
want "never-polled is named"                      "never polled" "$herr"

# --- 4. nothing to watch is idle, not a crash loop -------------------------------------------
printf '[repo.p]\npath = "%s"\nmode = "push"\n' "$T/repo" > "$FX/push-only.toml"
IRUN="$T/idle-run"
rout="$("$BIN" watch --ticks 1 --interval 1 --run "$IRUN" --home "$FX" --config "$FX/push-only.toml" 2>&1)"; rrc=$?
[ "$rrc" -eq 0 ] && ok "no queue-mode repository is idle, not an exit" || bad "no queue-mode repository is idle, not an exit (rc=$rrc)"
want "idle says why"                              'idle: '"$FX"'/push-only.toml: no repository has mode = "queue"' "$rout"
hout="$("$BIN" health --run "$IRUN" 2>&1)"; hrc=$?
[ "$hrc" -ne 0 ] && ok "idle reads DEGRADED, not healthy" || bad "idle reads DEGRADED, not healthy (rc=$hrc)"
want "health names the idle reason"               "idle:" "$hout"
printf 'not = [valid\n' > "$FX/broken.toml"
bout="$("$BIN" watch --ticks 1 --run "$IRUN" --home "$FX" --config "$FX/broken.toml" 2>&1)"; brc=$?
[ "$brc" -ne 0 ] && ok "an unparseable spira.toml is fatal" || bad "an unparseable spira.toml is fatal (rc=$brc)"

# --- 4b. a gutted config is re-read, not latched idle forever ---------------------------------
# Mirrors the incident: spira.toml loses its queue repo, queue-watch goes idle, the file is
# restored a while later, and nothing brings watching back until a human notices.
APPEAR="$FX/appear.toml"
printf '[repo.p]\npath = "%s"\nmode = "push"\n' "$T/repo" > "$APPEAR"
ARUN="$T/appear-run"
mkdir -p "$ARUN/queue/q"
(
    sleep 0.5
    printf '[repo.q]\npath = "%s"\nmode = "queue"\nbase = "origin/main"\n' "$T/repo" > "$APPEAR.tmp"
    mv "$APPEAR.tmp" "$APPEAR"
) &
appear_pid=$!
aout="$("$BIN" watch --ticks 3 --interval 2 --run "$ARUN" --home "$FX" --config "$APPEAR" 2>&1)"; arc=$?
wait "$appear_pid" 2>/dev/null || true
[ "$arc" -eq 0 ] && ok "watch keeps running across the config being restored" || bad "watch keeps running across the config being restored (rc=$arc)"
want "starts idle on the gutted config"            'idle: '"$APPEAR"': no repository has mode = "queue"' "$aout"
want "notices the restored repo and resumes"       "watching resumed: 1 queue-mode repo(s) found" "$aout"
hout2="$("$BIN" health --run "$ARUN" 2>&1)"; hrc2=$?
[ "$hrc2" -eq 0 ] && ok "health is healthy again once watching resumed" || bad "health is healthy again once watching resumed (rc=$hrc2)"

# --- 5. a queued job with no runner is named distinctly and gets a durable delivery path -----
# The incident this fixture reproduces: a batch's CI job sits in "queued" with no runner ever
# assigned, and the queue's log is the only place that says so — read by nobody once the
# session watching it is gone.
SRUN="$T/stall-run"; SQ="$SRUN/queue/qs"
mkdir -p "$SQ" "$SRUN/landstate"
STALL_BRANCH="spira/queue/20260927T150304Z"
printf 'pr=419\nhead=deadbeef\nmembers=sp-a:%s\nbranch=%s\n' "$TIP_A" "$STALL_BRANCH" > "$SQ/open"

cat > "$FX/stall.toml" <<EOF
[repo.qs]
path = "$T/repo"
mode = "queue"
base = "origin/main"
EOF

# Never resolves past "pending" (no run has completed) and reports the sole job queued since
# the epoch — arbitrarily far in the past, so the queued threshold fires on the first poll
# regardless of wall-clock skew between the fixture's setup and the binary's first tick.
cat > "$FX/stall-forge.sh" <<'EOF'
#!/usr/bin/env bash
case "$1" in
    check-status) echo pending ;;
    queued-since) echo 0 ;;
    pr-state)     echo open ;;
    *) exit 2 ;;
esac
EOF
chmod +x "$FX/stall-forge.sh"

# Fake incident.sh: records what it was filed with rather than touching a real bead store.
cat > "$FX/incident.sh" <<EOF
#!/usr/bin/env bash
[ "\$1" = file ] || exit 2
title="\$2"
body="\$(cat)"
{
    printf 'title=%s\n' "\$title"
    printf 'ref=%s\n' "\${SPIRA_INCIDENT_REF:-}"
    printf 'repo=%s\n' "\${SPIRA_INCIDENT_REPO:-}"
    printf 'cause=%s\n' "\${SPIRA_INCIDENT_CAUSE:-}"
    printf 'body=%s\n' "\$body"
} >> "$FX/incidents.log"
EOF
chmod +x "$FX/incident.sh"

# QUEUE_WATCH_HEAD_STALL_SECS is set absurdly high so a "stall" firing here can only be the
# queued-threshold path — proof the two are judged separately, not that the smaller number
# always wins.
sout="$(SPIRA_BD="$FX/bd" SPIRA_FORGE="$FX/stall-forge.sh" SPIRA_CI_QUEUED_MAX_SECS=1 QUEUE_WATCH_HEAD_STALL_SECS=100000000 \
    "$BIN" watch --ticks 2 --interval 1 --run "$SRUN" --home "$FX" --config "$FX/stall.toml" 2>&1)"
printf '%s\n' "$sout" | sed 's/^/    | /'

want "a queued job with no runner is named distinctly"        "qs stall: PR 419 CI queued" "$sout"
want "the queued wording names the absent runner"              "with no runner ever assigned" "$sout"
nowant "it is not judged against the much larger head-stall window" "unchanged for" "$sout"

[ -f "$FX/incidents.log" ] && ok "the head-stall filed an incident" || bad "the head-stall filed an incident (no $FX/incidents.log written)"
ilog="$(cat "$FX/incidents.log" 2>/dev/null)"
want "the incident is keyed to this repo and PR"          "ref=incident:queue-watch-stall-qs-419" "$ilog"
want "the incident names the repo"                        "repo=qs" "$ilog"
want "the incident carries the queued wording, not just the log line" "with no runner ever assigned" "$ilog"

# --- 6. wiring -------------------------------------------------------------------------------
row="$(command grep -E '^queue-watch\|daemon\|' "$HERE/watchers" || true)"
want "watchd row runs the binary by name"         "|queue-watch watch --run @SPIRA_RUN@" "$row"
want "watchd row carries a health probe"          "|queue-watch health --run @SPIRA_RUN@" "$row"

tl_summary
