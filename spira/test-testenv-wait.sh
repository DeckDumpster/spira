#!/usr/bin/env bash
#
# test-testenv-wait.sh — sp-tcarr: a waiter keyed on testenv's `VERDICT` marker can wait
# forever, because a run killed by SIGKILL mid-flight writes nothing (observed 2026-10-01:
# three such loops alive 1h12m, 2h21m and 5h04m against runs already dead). Two things to
# prove:
#
#   1. `testenv wait <out>` blocks on the run's own pid (kill -0), not on the marker:
#      killed with -9, it still returns FAULT within seconds. The pid under test here is
#      a plain `sleep`, standing in for any backgrounded run — `wait`'s contract is pid
#      liveness plus whatever the output file holds, not knowledge of what produced it.
#   2. testenv itself: a caught SIGTERM arriving mid-*build* (not just mid-wait) ends the
#      run promptly with a terminal `VERDICT FAULT` line, not a silent multi-minute hang
#      until cargo finishes on its own (the actual gap this bead closes in testenv/src/
#      build.rs — before the fix, the build's poll loop checked only the --deadline, never
#      the signal).
#
# tier: T2
# covers: testenv/src/wait.rs testenv/src/build.rs sp-tcarr
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"

echo "test-testenv-wait.sh — sp-tcarr"

command -v testenv >/dev/null 2>&1 || { echo "  SKIP  testenv is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ==========================================================================
echo
echo "case 1 — SIGKILL mid-run: testenv wait returns FAULT within seconds:"
# ==========================================================================
# A plain sleep stands in for a backgrounded run: wait's contract is the pid in
# <out>.pid plus whatever <out> holds, nothing more.
OUT1="$TMP/out1"
: > "$OUT1"
sleep 30 & SLEEPPID=$!
echo "$SLEEPPID" > "$OUT1.pid"
kill -9 "$SLEEPPID" 2>/dev/null || true

t0=$(date +%s)
wait1_out="$(testenv wait "$OUT1" 2>&1)"; wait1_rc=$?
elapsed1=$(( $(date +%s) - t0 ))
[ "$elapsed1" -le 10 ] \
    && ok "testenv wait returned within seconds of the SIGKILL (${elapsed1}s)" \
    || bad "testenv wait latency" "took ${elapsed1}s"
is   "testenv wait's exit code is a fault (2)"   2 "$wait1_rc"
want "testenv wait's own line says VERDICT FAULT" "VERDICT FAULT" "$wait1_out"
want "and names why: gone"                        "reason=gone"   "$wait1_out"
wait "$SLEEPPID" 2>/dev/null || true

# ==========================================================================
echo
echo "case 2 — SIGTERM mid-build: the output ends with VERDICT FAULT:"
# ==========================================================================
if ! command -v cargo >/dev/null 2>&1; then
    echo "  note  cargo is not on PATH — case 2 cannot run on this host, leaving it to case 1's coverage"
else

REPO="$TMP/repo"
mkdir -p "$REPO/src" "$REPO/spira"
cat > "$REPO/Cargo.toml" <<'EOF'
[package]
name = "slowbuild"
version = "0.1.0"
edition = "2021"
build = "build.rs"
EOF
# A build script that blocks for a fixed, deterministic 20s — long enough to kill -TERM
# the runner mid-build regardless of how fast this host compiles things, and short enough
# the suite does not stall if the signal somehow never lands.
cat > "$REPO/build.rs" <<'EOF'
fn main() {
    std::thread::sleep(std::time::Duration::from_secs(20));
}
EOF
printf 'fn main() { println!("ok"); }\n' > "$REPO/src/main.rs"
printf 'target/\n' > "$REPO/.gitignore"
printf '#!/usr/bin/env bash\nset -uo pipefail\necho ok\n' > "$REPO/spira/test-a.sh"
chmod +x "$REPO/spira/test-a.sh"
(
    cd "$REPO" && git init -q -b topic \
        && git -c user.name=t -c user.email=t@t add -A \
        && git -c user.name=t -c user.email=t@t commit -qm base
)

OUT2="$TMP/out2"
mkdir -p "$TMP/home" "$TMP/run" "$TMP/verdicts"
# `cd` is its own statement, not chained with `&&` onto the backgrounded command: an
# AND-OR list backgrounded as one job (`a && b &`) can leave an extra wrapper shell
# between $! and the real process, so a signal to $! would hit the wrapper and orphan
# testenv underneath it, still running, unsignalled. A single simple command in `&` has
# no such layer — `env` execs directly into testenv, keeping the same pid.
(
    cd "$REPO"
    env \
        SPIRA_TESTENV_HARNESS="$ROOT" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_VERDICTS="$TMP/verdicts" \
        SPIRA_VERDICT_TTL=0 \
        SPIRA_TESTENV_WARM_SLOTS=0 \
        SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB=0 \
        SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB=0 \
        SPIRA_BATCH_LIVENESS_SLEEP=0 \
        SPIRA_BATCH_PSI_THRESHOLD=0 \
        SPIRA_BATCH_MAXPAR=2 \
        HOME="$TMP/home" \
        testenv --profile dev --suites test-a.sh topic "$REPO" >"$OUT2" 2>&1 &
    echo $! > "$OUT2.pid"
)
RUNPID="$(cat "$OUT2.pid" 2>/dev/null || true)"

# Wait for the run to actually reach `cargo build` before signalling it — a TERM sent
# before then would just hit the usage/resolve/select steps, proving nothing about the
# build-phase gap this bead closes. Generous budget: this host's compile-admission pool
# (spira-admit) is shared with whatever else is building right now, so "reaches the log
# line" can legitimately take a while under contention — that wait is not what either
# assertion below times.
reached_build=0
for _ in $(seq 1 600); do
    grep -q 'cargo build --profile dev --workspace' "$OUT2" 2>/dev/null && { reached_build=1; break; }
    sleep 0.1
done
if [ "$reached_build" != 1 ] || [ -z "$RUNPID" ]; then
    bad "testenv reached cargo build before the signal" "pid=[${RUNPID:-}] log: $(tail -5 "$OUT2" 2>/dev/null)"
else
    # Give cargo a moment to actually be inside the 20s build-script sleep, not still
    # compiling build.rs itself (slower under host contention, hence the margin).
    sleep 3
    kill -TERM "$RUNPID" 2>/dev/null || true
    t0=$(date +%s)
    for _ in $(seq 1 300); do
        kill -0 "$RUNPID" 2>/dev/null || break
        sleep 0.1
    done
    elapsed2=$(( $(date +%s) - t0 ))
    [ "$elapsed2" -le 15 ] \
        && ok "a SIGTERMed build ends promptly, not after cargo's own 20s sleep (${elapsed2}s)" \
        || bad "SIGTERM-to-exit latency" "took ${elapsed2}s"
    last_line="$(tail -1 "$OUT2" 2>/dev/null)"
    case "$last_line" in
        "VERDICT FAULT"*) ok "the output ends with VERDICT FAULT" ;;
        *) bad "SIGTERM output" "last line: $last_line — full tail: $(tail -5 "$OUT2" 2>/dev/null)" ;;
    esac
fi

fi

tl_summary
