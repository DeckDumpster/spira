#!/usr/bin/env bash
#
# test-gate-host-cores.sh — gate exports SPIRA_GATE_HOST_CORES equal to the real host count.
#
# WHAT THIS CATCHES. An aeon's systemd unit carries a CPUQuota; nproc inside the gate
# returns ceil(quota/100%) rather than the host's physical core count. A repository whose
# CI preflight asserts a minimum core count fails on every branch — and on the base, so the
# verdict is BASE_FAIL and every branch of that repository is held for a condition no branch
# caused. gate.sh now exports SPIRA_GATE_HOST_CORES via host_cores() (which reads getconf
# and is immune to the cgroup fence) so a repository's CI has an escape from nproc.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
#   A stub nproc that outputs 1 is placed on PATH. The gate command reads
#   SPIRA_GATE_HOST_CORES and compares it to the stub's output. If run_gate() used nproc
#   directly instead of host_cores(), the test would see 1 and fail.
#
# covers: spira/gate.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want(){ [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
MAP="$TMP/repo-map"; VDIR="$TMP/verdicts"; GATELOG="$TMP/gate.log"; HOMEDIR="$TMP/home"
mkdir -p "$RUN/worktree" "$HOMEDIR" "$SH"

cp "$HERE/gate.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" "$HERE/skew.sh" \
   "$HERE/yield.sh" "$SH/"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

BR=spira/sp-hc1
W="$TMP/work"
git -C "$REPO" worktree add -q -b "$BR" "$W" origin/main
printf 'work\n' > "$W/f1.txt"
git -C "$W" add -A; git -C "$W" commit -q -m "feat: sp-hc1"
git -C "$REPO" worktree remove --force "$W"

# A stub nproc that always returns 1 — simulates CPUQuota=40% on any host.
mkdir -p "$TMP/bin"
printf '#!/bin/sh\necho 1\n' > "$TMP/bin/nproc"
chmod +x "$TMP/bin/nproc"

# Confirm the stub works (positive control).
_stub_out="$(PATH="$TMP/bin:$PATH" nproc)"
[ "$_stub_out" = "1" ] && ok "stub nproc returns 1 (positive control)" \
                        || { bad "stub nproc returns 1" "got '$_stub_out'"; exit 1; }

# Real core count via getconf.
_real_cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null)"
{ [ -n "$_real_cores" ] && [ "$_real_cores" -gt 0 ]; } 2>/dev/null \
    && ok "getconf _NPROCESSORS_ONLN readable ($_real_cores cores)" \
    || { bad "getconf _NPROCESSORS_ONLN readable" "got '$_real_cores'"; exit 1; }

# Gate command: write SPIRA_GATE_HOST_CORES to a file so we can read it after the gate
# passes (the gate does not print the command's stdout on success).
CORES_FILE="$TMP/cores-seen"
CMD_SCRIPT="$TMP/check-cores.sh"
printf '#!/usr/bin/env bash\nprintf "%%s" "$SPIRA_GATE_HOST_CORES" > "%s"\n' "$CORES_FILE" > "$CMD_SCRIPT"
chmod +x "$CMD_SCRIPT"
printf 'repo | %s | push | origin/main |  | bash %s\n' "$REPO" "$CMD_SCRIPT" > "$MAP"

rungate() {
    env -i HOME="$HOMEDIR" PATH="$TMP/bin:/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" SPIRA_REPO_MAP="$MAP" SPIRA_GATE_LOG="$GATELOG" \
        SPIRA_VERDICTS="$VDIR" SPIRA_VERDICT_TTL=0 \
        "$@" bash "$SH/gate.sh" "$BR" repo 2>&1
}

echo "test-gate-host-cores.sh"

# Run the gate with stub nproc first on PATH.
out="$(rungate)"; rc=$?

is   "gate passes"                     0  "$rc"
want "verdict is PASS"                 "VERDICT=PASS" "$out"

if [ ! -f "$CORES_FILE" ]; then
    bad "gate command wrote CORES_FILE" "file absent: $CORES_FILE"
else
    ok "gate command wrote CORES_FILE"
    _reported="$(cat "$CORES_FILE")"
    is "SPIRA_GATE_HOST_CORES equals real host count (not stub 1)" "$_real_cores" "$_reported"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
