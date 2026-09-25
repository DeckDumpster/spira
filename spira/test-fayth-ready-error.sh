#!/usr/bin/env bash
#
# test-fayth-ready-error.sh — fayth_ready distinguishes TWO different failures that used to
# collapse into one: no such fayth file in the chamber, versus a fayth that exists but whose
# ready query could not complete.
#
# THE DEFECT (sp-3ntca). CHECK 7 read `r="$(fayth_ready "$f")" || { log "... no fayth in the
# chamber — skipped"; ...}`. Under `set -o pipefail` (both sentinel.sh and aeon.sh set it),
# a bd failure inside ready_count already propagated as fayth_ready's own exit status — so a
# TRANSIENT STORE FAILURE on an EXISTING fayth file produced the exact same branch, and the
# exact same log line, as a fayth file that was never there at all. At 15:38:18 the sentinel
# logged "CHECK7 builder: no fayth in the chamber — skipped" while builder.fayth had been on
# disk the whole time (GNU sed -i replaces atomically) — the real cause was a failed count
# query, misreported as a missing persona.
#
# POSITIVE CONTROL FOR THE PLUMBING: the no-fayth case is asserted first, on a name that
# genuinely has no chamber file, so the two branches are shown to differ before either one is
# trusted (law-absence-needs-a-positive-control).
#
# WHAT THIS SUITE DOES NOT USE. No bd, no database: ready_count is overridden after sourcing
# lib.sh (fayth_ready's subshell inherits the override, same technique as test-fayth.sh) so
# the suite exercises fayth_ready's own exit-code and stderr plumbing without a real store.
#
# defect: sp-3ntca
# covers: spira/lib.sh spira/sentinel.sh
# hermetic-ok: no database, no systemd; ready_count is a stub
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run" "$T/home/chamber"

export SPIRA_HOME="$T/home"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

cat > "$SPIRA_HOME/chamber/probe.fayth" <<'FAYTH'
FAYTH_NAME=probe
FAYTH_LABELS="plan"
FAYTH_EXCLUDE_LABELS=""
FAYTH

echo "test-fayth-ready-error.sh"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — no fayth file at all: rc 2, stdout '0', reason names it"
# ==========================================================================================
errf="$T/err1"
out="$(fayth_ready nosuchpersona 2>"$errf")"; rc=$?
errmsg="$(cat "$errf" 2>/dev/null)"
is   "no-fayth: rc is 2 (distinct from a failed query)" "2" "$rc"
is   "no-fayth: stdout is '0'"                          "0" "$out"
want "no-fayth: reason names the missing file"          "no fayth in the chamber" "$errmsg"

# ==========================================================================================
echo
echo "THE DEFECT — fayth file EXISTS, but ready_count fails: rc 1, NOT the no-fayth code"
# ==========================================================================================
ready_count() {
    printf 'ready_count: query failed: Error: the database is locked by another dolt process\n' >&2
    printf '0'
    return 1
}
errf="$T/err2"
out="$(fayth_ready probe 2>"$errf")"; rc=$?
errmsg="$(cat "$errf" 2>/dev/null)"
is   "query-failed: rc is 1, not 2 — the fayth file is right there" "1" "$rc"
want "query-failed: reason names the bd failure, not a missing file" \
     "locked by another dolt process" "$errmsg"
[[ "$errmsg" != *"no fayth"* ]] \
    && ok  "query-failed: reason does NOT say 'no fayth'" \
    || bad "query-failed: reason does NOT say 'no fayth'" "$errmsg"

# ==========================================================================================
echo
echo "a real count, including a real zero — rc 0, no error text at all"
# ==========================================================================================
ready_count() { printf '0'; return 0; }
errf="$T/err3"
out="$(fayth_ready probe 2>"$errf")"; rc=$?
errmsg="$(cat "$errf" 2>/dev/null)"
is "real-zero: rc is 0"           "0" "$rc"
is "real-zero: stdout is '0'"     "0" "$out"
is "real-zero: no stderr at all"  ""  "$errmsg"

ready_count() { printf '7'; return 0; }
out="$(fayth_ready probe 2>/dev/null)"; rc=$?
is "nonzero-count: rc is 0"    "0" "$rc"
is "nonzero-count: count is 7" "7" "$out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
