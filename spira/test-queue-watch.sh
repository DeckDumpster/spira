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
#      stays up, and health passes — a daemon row must not crash-loop an install without a
#      queue. A spira.toml that does not parse is fatal.
#   5. The watchd manifest row expands, and conf.sh resolves SPIRA_QUEUE_WATCH_BIN.
#
# QUEUE_WATCH_BIN may point at another binary; pointing it at a stub that prints nothing is
# how every assertion below was seen to fail first.
#
# covers: queue-watch/* spira/watchers spira/forge.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s\n' "$1"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1: wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }

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
if out="$(CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/target" "$CARGO_BIN" test --manifest-path "$ROOT/queue-watch/Cargo.toml" 2>&1)"; then
    ok "queue-watch unit replays pass"
else
    bad "queue-watch unit replays: $(printf '%s\n' "$out" | command grep -E 'FAILED|panicked|error' | head -5)"
fi

BIN="${QUEUE_WATCH_BIN:-}"
if [ -z "$BIN" ]; then
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/target" "$CARGO_BIN" build --release \
        --manifest-path "$ROOT/queue-watch/Cargo.toml" >/dev/null 2>&1
    BIN="$T/target/release/queue-watch"
fi
[ -x "$BIN" ] || { bad "queue-watch binary built at $BIN"; printf '%d passed, %d failed\n' "$pass" "$fail"; exit 1; }

# --- fixtures --------------------------------------------------------------------------------
RUN="$T/run"; FX="$T/fx"; Q="$RUN/queue/q"
mkdir -p "$Q" "$RUN/landstate" "$FX"

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
chmod +x "$FX/forge.sh" "$FX/bd"

# Poll 1 sees: batch 50 (sp-a, sp-b), sp-o certified in another repo.
printf 'pr=50\nhead=h1\nmembers=sp-a:%s sp-b:bbbb\n' "$TIP_A" > "$Q/open"
echo "CERTIFIED x 1" > "$RUN/landstate/sp-o"
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
printf 'sp-c:cccc\nsp-b:bbbb\n' > "$Q/bisect"
touch -d '3 hours ago' "$Q/bisect"
printf 'pr=51\nhead=h3\nmembers=sp-c:cccc\n' > "$Q/open"
echo "CERTIFIED dddd 1" > "$RUN/landstate/sp-d"
EOF
# After poll 4: PR 51 closed without merging; its member back in CERTIFIED.
cat > "$FX/step-4.sh" <<EOF
echo closed > "$FX/state-51"
rm -f "$Q/open"
echo "CERTIFIED cccc 1" > "$RUN/landstate/sp-c"
EOF

export FX SPIRA_BD="$FX/bd" SPIRA_FORGE="$FX/forge.sh"
out="$("$BIN" watch --ticks 5 --interval 1 --run "$RUN" --home "$FX" --config "$FX/spira.toml" 2>&1)"
printf '%s\n' "$out" | sed 's/^/    | /'

# --- 2. the event stream ---------------------------------------------------------------------
want "baseline names the open batch"             "q watching: PR 50 open with 2 member(s)" "$out"
want "baseline strips a foreign bead-id prefix"   "sp-b (P3) beta work" "$out"
lack "another repo's certified bead is not counted" "sp-o" "$out"
want "CI red reported"                            "q ci: PR 50 CI red" "$out"
want "ejection reported from state"               "q ejected: sp-b (P3) beta work ejected from PR 50 after a red run" "$out"
want "re-push reported"                           "q repushed: PR 50 re-pushed with 1 member(s)" "$out"
want "landing verified on the real base"          "q landed: PR 50 landed (verified on base)" "$out"
want "new batch reported"                         "q opened: PR 51 opened with 1 member(s): sp-c (P3) gamma work" "$out"
want "stale bisect group named as a forced cut"   "q forced-cut: PR 51's membership was forced by a bisect group recorded 180m ago" "$out"
want "priority inversion names the waiting P0"    "q priority-inversion: PR 51 took P3 work while 1 more urgent certified bead(s) wait: sp-d (P0 express) delta urgent" "$out"
want "close without landing reported from state"  "q closed-unlanded: PR 51 closed without landing: all members back in CERTIFIED" "$out"
lack "the push-mode repo is not watched"          " p watching" "$out"

# --- 3. health -------------------------------------------------------------------------------
"$BIN" health --run "$RUN" >/dev/null 2>&1 && ok "health passes after a good poll" || bad "health passes after a good poll"

rm -rf "$RUN/landstate"
blind="$("$BIN" watch --ticks 1 --interval 1 --run "$RUN" --home "$FX" --config "$FX/spira.toml" 2>&1)"
want "an unreadable queue is reported blind"      "q blind: cannot see the queue" "$blind"
herr="$("$BIN" health --run "$RUN" 2>&1)"; hrc=$?
[ "$hrc" -ne 0 ] && ok "health fails after a blind poll" || bad "health fails after a blind poll (rc=$hrc)"
want "health says why"                            "blind" "$herr"

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
[ "$hrc" -eq 0 ] && ok "health passes when idle" || bad "health passes when idle (rc=$hrc)"
want "health names the idle reason"               "idle:" "$hout"
printf 'not = [valid\n' > "$FX/broken.toml"
bout="$("$BIN" watch --ticks 1 --run "$IRUN" --home "$FX" --config "$FX/broken.toml" 2>&1)"; brc=$?
[ "$brc" -ne 0 ] && ok "an unparseable spira.toml is fatal" || bad "an unparseable spira.toml is fatal (rc=$brc)"

# --- 5. wiring -------------------------------------------------------------------------------
row="$(command grep -E '^queue-watch\|daemon\|' "$HERE/watchers" || true)"
want "watchd row runs the binary"                 "@SPIRA_QUEUE_WATCH_BIN@ watch --run @SPIRA_RUN@" "$row"
want "watchd row carries a health probe"          "@SPIRA_QUEUE_WATCH_BIN@ health --run @SPIRA_RUN@" "$row"
want "watchd knows the placeholder"               "SPIRA_QUEUE_WATCH_BIN" "$(command grep -E '^WATCHD_KEYS=|SPIRA_QUEUE_WATCH_BIN"' "$HERE/watchd.sh")"
want "conf.sh resolves the binary"                'SPIRA_QUEUE_WATCH_BIN="$SPIRA_REPO/target/release/queue-watch"' "$(cat "$HERE/conf.sh")"

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
