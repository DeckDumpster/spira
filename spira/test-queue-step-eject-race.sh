#!/usr/bin/env bash
#
# test-queue-step-eject-race.sh — gap G13 (docs/test-plan/landing-merge-queue.md
# section 6): a landing pass's `queue step` (its in-process verdict, then the batcher) racing a
# concurrent operator `queue.sh eject`, on the SAME repo. Every existing lock case
# (test-queue-ops.sh) holds the flock itself, in the SAME process, before calling
# the command under test — it proves the refusal message, never a real race between
# two separately-launched processes. This suite launches `step` as a real background
# process, waits for EXTERNAL evidence (a probe flock from a third process) that it
# actually holds the per-repo lock, then launches a real `eject` while it is held.
#
# tier: T3
# covers: queue/src/* UC-landing-merge-queue-30
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/lc-fixture.sh"

echo "test-queue-step-eject-race.sh"

TMP="$(mktemp -d)"; trap 'lcfix_down; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || { echo "test-queue-step-eject-race: could not build a lifecycle fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixq-race
QUEUEDIR="$RUN/queue"
FORGE_LOG="$TMP/forge.log"
BD_LOG="$TMP/bd.log"
SLEEP_FILE="$TMP/sleep-seconds"; printf '3\n' > "$SLEEP_FILE"

git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m init
mkdir -p "$RUN/worktree" "$SH" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"

# The forge fixture sleeps on check-status — the FIRST call the verdict makes after
# taking the per-repo lock (queue/src/ops/verdict.rs forge_pass locks, then process reads
# check-status) — so `step`'s hold on the lock has a wide, deterministic window
# for the probe below to observe, rather than a race decided by scheduler luck.
cat > "$SH/forge-fake.sh" <<ENDFAKE
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$FORGE_LOG"
case "\${1:-}" in
    check-status)     sleep "\$(cat "$SLEEP_FILE" 2>/dev/null || echo 0)"; printf 'pending\n' ;;
    main-gate-status) printf 'green deadbeef\n' ;;
    runs-active)      printf '0\n' ;;
    *) : ;;
esac
exit 0
ENDFAKE
chmod +x "$SH/forge-fake.sh"

cat > "$SH/bd-stub.sh" <<'BDSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${BD_LOG:?}"
case "${1:-}" in
    show) exit 0 ;;
    *)    exit 0 ;;
esac
BDSTUB
chmod +x "$SH/bd-stub.sh"

printf '#!/usr/bin/env bash\ntrue\n' > "$SH/mail"; chmod +x "$SH/mail"
# the verdict's observe-flake goes to `testenv suites` now (testenv/DESIGN-suites.md §9).
printf '#!/usr/bin/env bash\ntrue\n' > "$SH/testenv-stub.sh"; chmod +x "$SH/testenv-stub.sh"

# _batch_cut (the queue binary) unconditionally invokes the batcher binary — this race
# is about the per-repo lock, not the batcher, so the stub does nothing and exits 0.
printf '#!/usr/bin/env bash\ntrue\n' > "$SH/batcher-stub.sh"; chmod +x "$SH/batcher-stub.sh"
# Stubs are injected by name, first on PATH (sp-gypjk).
STUBBIN="$TMP/stubbin"; mkdir -p "$STUBBIN"
ln -sf "$SH/batcher-stub.sh" "$STUBBIN/batcher"
ln -sf "$SH/testenv-stub.sh" "$STUBBIN/testenv"

RMAP="$TMP/repo-map"
printf '%s | %s | queue | main | | |\n' "$REPONAME" "$REPO" > "$RMAP"

run() {
    tl_config SPIRA_RUN="$RUN" SPIRA_BD="$SH/bd-stub.sh" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_DIR="$QUEUEDIR" SPIRA_QUEUE_CI_MAXSEC=3600 \
        SPIRA_QUEUE_CI_IDLE_SEC=600 SPIRA_QUEUE_INFRA_RETRIES=2 SPIRA_FORGE="$SH/forge-fake.sh"
    env -i SPIRA_TOML="$SPIRA_TOML" $(lcfix_env) PATH="$STUBBIN:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$SH" \
        SPIRA_DB="${SPIRA_DB:-/nonexistent}" \
        BD_LOG="$BD_LOG" \
        FORGE_LOG="$FORGE_LOG" \
        queue "$@" 2>&1
}

TIP="aabbcc1100000000000000000000000000000001"
{
    printf 'pr=9\n'
    printf 'head=%s\n' "$TIP"
    printf 'base=0000000000000000000000000000000000000000\n'
    printf 'members=sp-race:%s\n' "$TIP"
    printf 'opened=%s\n' "$(date +%s)"
    printf 'branch=spira/queue/racetest\n'
} > "$QUEUEDIR/$REPONAME/open"
lcfix_seed sp-race IN_DELIVERY "$TIP"

LOCKFILE="$QUEUEDIR/$REPONAME/lock"
# _lock_free — a THIRD process's own flock attempt: 0 (free, and now released again
# when the subshell's fd closes) or non-zero (held by someone else right now).
_lock_free() { ( exec 8>"$LOCKFILE"; flock -n 8 ) 2>/dev/null; }

echo
echo "a landing pass's queue.sh step and a concurrent operator eject:"

STEP_OUT="$TMP/step.out"
run step "$REPONAME" > "$STEP_OUT" 2>&1 &
STEP_PID=$!

# Wait for EXTERNAL evidence that step holds the lock (up to 5s), not a fixed sleep.
held=0
for _ in $(seq 1 50); do
    if ! _lock_free; then held=1; break; fi
    sleep 0.1
done

if [ "$held" -eq 1 ]; then
    ok "step actually holds the per-repo lock (observed by a third process)"
else
    bad "step actually holds the per-repo lock (observed by a third process)" \
        "never observed contention — the race below would prove nothing"
fi

EJECT_OUT="$(run eject sp-race "$REPONAME")"; EJECT_RC=$?

wait "$STEP_PID"
STEP_RC=$?

[ "$EJECT_RC" -ne 0 ] && ok "eject refused while step holds the lock (non-zero exit)" \
    || bad "eject refused while step holds the lock" "exit 0: $EJECT_OUT"
want "eject names the lock as the reason" "holds the lock" "$EJECT_OUT"
is   "step itself completes (its own lock take succeeded)" "0" "$STEP_RC"

# Exactly one actor could have changed sp-race's lifecycle row: step's verdict saw
# "pending" and made no change, and eject never got the lock.
is "lifecycle row is still IN_DELIVERY at its tip — untouched by the loser, unchanged by the winner" \
   "IN_DELIVERY $TIP" "$(lcfix_state sp-race) $(lcfix_tip sp-race)"

echo
echo "once the lock is free, the same eject genuinely succeeds:"
if _lock_free; then ok "lock is free again after step finished"
else bad "lock is free again after step finished" "still held"; fi

OUT2="$(run eject sp-race "$REPONAME")"; RC2=$?
is "eject now exits 0" "0" "$RC2"
is "lifecycle row is now REWORK — the winner (this time, eject) actually acted" "REWORK" "$(lcfix_state sp-race)"

tl_summary
