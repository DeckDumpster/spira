#!/usr/bin/env bash
# test-testenv-resource-diagnosis.sh — testenv.sh up: a boot failure names the resource it
# measured, and an oversubscribed host queues a new container rather than racing it into
# the same failure (sp-cvle7).
#
# WHAT THIS PROVES
#   A. POSITIVE CONTROL: against a podman stub that lets basic.target come up immediately,
#      `up` succeeds — the stub itself is a working double before anything below trusts it.
#   B. A failed basic.target wait measures and prints all four candidate resources
#      (inotify instances, pids, the user@ slice TasksMax, the keyring quota), not a
#      guess — and when one of them is at/over 90% of its cap, names it explicitly as
#      "exhausted resource".
#   C. SEEN RED FIRST (law-a-regression-test-must-be-seen-to-fail): the bare failure
#      message this suite is a regression test for ("did not reach basic.target", with no
#      resource attribution at all) does NOT match the naming pattern B requires — proving
#      the assertion is not vacuously true before trusting it to pass against the real code.
#   D. When the container is already gone by the time the diagnosis runs (a boot failure
#      deep enough that podman can no longer exec into it), the container-side checks read
#      "?" honestly rather than fabricating a number, while the host-wide inotify check
#      still runs.
#   E. CONCURRENT-START ADMISSION: `up` queues (does not fail, does not race ahead) while
#      the host already has SPIRA_TESTENV_MAX_CONCURRENT testenv containers running, and
#      proceeds as soon as the count drops below it. SPIRA_TESTENV_MAX_CONCURRENT=0
#      disables the gate for an operator who has already raised the underlying ceiling.
#
# FIXTURES. podman is stubbed on PATH and driven by environment variables, so no real
# container, image or resource exhaustion is involved — the same style as
# test-testenv-registry.sh. The stub records its own argv, which the assertions read back.
#
# host-reason: podman is stubbed on PATH; there is no container to run inside
# tier: T1
# covers: spira/testenv.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

# -E: assertions below use extended-regex quantifiers ([0-9]+); plain grep treats a bare
# `+` as a literal character, which would silently never match and pass no assertion at all.
saw()    { if grep -qE -- "$2" "$3" 2>/dev/null; then ok "$1"; else bad "$1" "no match [$2] in $3"; fi; }
notsaw() { if grep -qE -- "$2" "$3" 2>/dev/null; then bad "$1" "unexpected match [$2] in $3"; else ok "$1"; fi; }

echo "test-testenv-resource-diagnosis.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
FIXTURE="$TMP/spira"
mkdir -p "$FIXTURE" "$TMP/bin"
cp "$HERE/testenv.sh" "$FIXTURE/testenv.sh"
cp "$HERE/conf.sh"    "$FIXTURE/conf.sh"

# A pattern any of the four candidate resource names must match. This is what "named the
# resource" means for the rest of this suite.
RESOURCE_PATTERN='inotify|pids|user-.*\.slice tasks|keyring'

LOG="$TMP/podman.log"
cat > "$TMP/bin/podman" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PODMAN_LOG"
case "$1" in
    container) exit 1 ;;                 # never pre-exists
    image)     exit 0 ;;                 # pretend the image is already built — no build
    ps)
        [ -n "${STUB_RUNNING_IDS:-}" ] && printf '%s\n' $STUB_RUNNING_IDS
        exit 0 ;;
    run)  echo "fakecid"; exit 0 ;;
    stop|rm) exit 0 ;;
    exec)
        shift 2   # drop "exec" and the container name
        case "$*" in
            "systemctl is-active basic.target")
                [ "${STUB_BASIC_OK:-0}" = "1" ] && exit 0 || exit 1 ;;
            "systemctl is-active user@"*.service)
                exit 0 ;;
            *"pids.current"*)
                [ -n "${STUB_PIDS:-}" ] || exit 1
                printf '%s\n' $STUB_PIDS; exit 0 ;;
            *"systemctl show user-"*)
                [ -n "${STUB_TASKS:-}" ] || exit 1
                printf '%s\n' "$STUB_TASKS"; exit 0 ;;
            *"wc -l"*)
                [ -n "${STUB_KEYS_USED:-}" ] || exit 1
                printf '%s\n' "$STUB_KEYS_USED"; exit 0 ;;
            *"keys/maxkeys"*)
                [ -n "${STUB_KEYS_MAX:-}" ] || exit 1
                printf '%s\n' "$STUB_KEYS_MAX"; exit 0 ;;
            *) exit 0 ;;
        esac
        ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$TMP/bin/podman"

up() {   # up <name> — runs the fixture's `up` with the stub on PATH; result on stdout+stderr
    PATH="$TMP/bin:$PATH" PODMAN_LOG="$LOG" \
    STUB_BASIC_OK="${STUB_BASIC_OK:-0}" STUB_RUNNING_IDS="${STUB_RUNNING_IDS:-}" \
    STUB_PIDS="${STUB_PIDS:-}" STUB_TASKS="${STUB_TASKS:-}" \
    STUB_KEYS_USED="${STUB_KEYS_USED:-}" STUB_KEYS_MAX="${STUB_KEYS_MAX:-}" \
    SPIRA_TESTENV_BASIC_WAIT_TICKS=1 SPIRA_TESTENV_BASIC_RETRY_SLEEP=0 \
    SPIRA_TESTENV_MAX_CONCURRENT="${SPIRA_TESTENV_MAX_CONCURRENT:-0}" \
    SPIRA_TESTENV_QUEUE_TIMEOUT="${SPIRA_TESTENV_QUEUE_TIMEOUT:-2}" \
    SPIRA_TESTENV_QUEUE_POLL="${SPIRA_TESTENV_QUEUE_POLL:-1}" \
        bash "$FIXTURE/testenv.sh" up --name "$1" 2>&1
}

# ===========================================================================
echo
echo "A: positive control — the stub itself works"
# ===========================================================================
: > "$LOG"
out="$(STUB_BASIC_OK=1 up posctl)"
saw "up succeeds when basic.target comes up" "user@.*service active" <(printf '%s' "$out")

# ===========================================================================
echo
echo "C: SEEN RED — the bare unfixed message does not name a resource"
# ===========================================================================
if printf '%s\n' "testenv: system systemd did not reach basic.target after 2 attempt(s)" \
        | grep -qE "$RESOURCE_PATTERN"; then
    bad "the pre-fix message is a false positive for this check" "matched the resource pattern"
else
    ok "the pre-fix message does not name a resource (the check can fail)"
fi

# ===========================================================================
echo
echo "B: a boot failure measures and reports all four candidates"
# ===========================================================================
: > "$LOG"
out="$(STUB_BASIC_OK=0 STUB_PIDS="5 50" STUB_TASKS=$'TasksCurrent=3\nTasksMax=100' \
       STUB_KEYS_USED=2 STUB_KEYS_MAX=1000000 up boot-fail)"
outfile="$TMP/out-b"; printf '%s\n' "$out" > "$outfile"

saw "reports inotify"            "inotify instances"        "$outfile"
saw "reports pids"               "pids [0-9?]+/[0-9?]+"     "$outfile"
saw "reports the user slice"     "user-.*\.slice tasks"     "$outfile"
saw "reports the keyring"        "keyring [0-9?]+/[0-9?]+"  "$outfile"

if grep -qE "$RESOURCE_PATTERN" "$outfile"; then
    ok "the failure output names a resource (the check now passes)"
else
    bad "the failure output names a resource" "no candidate resource named"
fi

# ===========================================================================
echo
echo "B2: a resource at/over 90% of its cap is named as exhausted"
# ===========================================================================
: > "$LOG"
out="$(STUB_BASIC_OK=0 STUB_PIDS="48 50" STUB_TASKS=$'TasksCurrent=3\nTasksMax=100' \
       STUB_KEYS_USED=2 STUB_KEYS_MAX=1000000 up boot-fail-pids)"
saw "names pids as the exhausted resource" "exhausted resource for boot-fail-pids .* pids \(48/50" \
    <(printf '%s' "$out")

# ===========================================================================
echo
echo "D: an unreachable container reads \"?\" honestly, not a fabricated number"
# ===========================================================================
: > "$LOG"
out="$(STUB_BASIC_OK=0 up boot-fail-gone)"   # no STUB_PIDS/TASKS/KEYS_* set: every container exec fails
outfile="$TMP/out-d"; printf '%s\n' "$out" > "$outfile"
saw "pids reads as unmeasurable"    'pids \?/\?'              "$outfile"
saw "user slice reads unmeasurable" 'user-.*\.slice tasks \?/\?' "$outfile"
saw "keyring reads unmeasurable"    'keyring \?/\?'            "$outfile"
saw "inotify is still measured (host-wide, no exec needed)" \
    'inotify instances [0-9]+/[0-9]+' "$outfile"

# ===========================================================================
echo
echo "E: concurrent-start admission queues, then proceeds once a slot frees"
# ===========================================================================
: > "$LOG"
COUNTER="$TMP/running-count"
printf '3\n' > "$COUNTER"   # starts saturated at the limit
cat > "$TMP/bin/podman" <<STUB2
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$PODMAN_LOG"
case "\$1" in
    container) exit 1 ;;
    image)     exit 0 ;;
    ps)
        n="\$(cat "$COUNTER" 2>/dev/null || echo 0)"
        for i in \$(seq 1 "\$n" 2>/dev/null); do echo "cid\$i"; done
        exit 0 ;;
    run)  echo fakecid; exit 0 ;;
    stop|rm) exit 0 ;;
    exec)
        shift 2
        case "\$*" in
            "systemctl is-active basic.target") exit 0 ;;
            "systemctl is-active user@"*.service) exit 0 ;;
            *) exit 0 ;;
        esac ;;
    *) exit 0 ;;
esac
STUB2
chmod +x "$TMP/bin/podman"

# A slot frees 1s in — a background writer, independent of the `up` under test.
( sleep 1; printf '2\n' > "$COUNTER" ) &
BGPID=$!

start_ts=$(date +%s)
out="$(PATH="$TMP/bin:$PATH" PODMAN_LOG="$LOG" \
       SPIRA_TESTENV_BASIC_WAIT_TICKS=1 SPIRA_TESTENV_BASIC_RETRY_SLEEP=0 \
       SPIRA_TESTENV_MAX_CONCURRENT=3 SPIRA_TESTENV_QUEUE_TIMEOUT=10 SPIRA_TESTENV_QUEUE_POLL=1 \
           bash "$FIXTURE/testenv.sh" up --name admitted 2>&1)"
rc=$?
wait "$BGPID" 2>/dev/null
end_ts=$(date +%s)

[ "$rc" -eq 0 ] && ok "E1: up succeeds once the slot frees" \
                || bad "E1: up succeeds once the slot frees" "exited $rc: $out"
if printf '%s\n' "$out" | grep -q "queueing"; then
    ok "E2: up reported that it queued"
else
    bad "E2: up reported that it queued" "no queueing message: $out"
fi
elapsed=$((end_ts - start_ts))
if [ "$elapsed" -ge 1 ]; then
    ok "E3: up actually waited for the slot rather than racing ahead (${elapsed}s)"
else
    bad "E3: up actually waited for the slot rather than racing ahead" "returned in ${elapsed}s"
fi

# ===========================================================================
echo
echo "E4: SPIRA_TESTENV_MAX_CONCURRENT=0 disables the gate even when saturated"
# ===========================================================================
printf '99\n' > "$COUNTER"
: > "$LOG"
out="$(PATH="$TMP/bin:$PATH" PODMAN_LOG="$LOG" \
       SPIRA_TESTENV_BASIC_WAIT_TICKS=1 SPIRA_TESTENV_BASIC_RETRY_SLEEP=0 \
       SPIRA_TESTENV_MAX_CONCURRENT=0 \
           bash "$FIXTURE/testenv.sh" up --name unbounded 2>&1)"
rc=$?
[ "$rc" -eq 0 ] && ok "E4: 0 disables the gate" || bad "E4: 0 disables the gate" "exited $rc: $out"

echo
tl_summary
