#!/usr/bin/env bash
#
# sp-7xnpv: spira-landing-pass-prod.service never set its repository map variable, so
# every fire logged "not set" and exited 0 — 1704 consecutive
# no-op passes that presented as healthy from every angle a watcher could check (timer
# active, service succeeding, log growing). law-timers-active-is-not-running and
# law-absence-needs-a-positive-control: a pass with nothing to land and a pass that cannot
# see anything to land produce byte-identical output.
#
# The unit wiring itself (install.sh/unit-ensure.sh render() and the rendered
# spira-landing-pass.service) is covered by test-install-exec.sh, which already carries an
# equivalent fixture and is allowed to name the config file directly (config-fence-allow).
#
# This suite covers the other guard: the landing-pass binary (`--pass`, the real context
# seam through lib.sh/conf.sh — DESIGN.md §8 D1/D2(f)) refuses (non-zero exit) instead of
# succeeding silently when the map conf.sh resolves is unset or unreadable
# (law-fail-closed-at-the-source, law-a-control-that-cannot-check-must-refuse) — the actual
# defect: exiting 0 made 1704 no-op fires indistinguishable from a healthy pass.
#
# tier: T1
# covers: landing-pass/src/main.rs landing-pass/src/seam.rs landing-pass/src/real.rs landing-pass/src/model.rs spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-landing-pass-map.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo
echo "landing-pass --pass refuses (non-zero exit) rather than succeeding silently when the"
echo "map is unset or unreadable — the actual 1704-fire defect. This runs the REAL context"
echo "seam (bash + lib.sh + conf.sh, DESIGN.md §8 D1), not a stub — the private map parser"
echo "these fields once had is gone (D1)."


LP_BIN="$(command -v landing-pass 2>/dev/null || true)"
[ -n "$LP_BIN" ] || bail "landing-pass is not on PATH (the tree's build provides it)"

# A fixture SPIRA_HOME carrying REAL conf.sh/lib.sh (symlinked, never a hand copy): the
# context seam sources them exactly as production does. Deliberately no fallback config
# file lives here — that fallback existing is what makes a clean clone runnable, and its
# presence would make every candidate search succeed, masking the very defect under test.
LPHOME="$TMP/lphome"; mkdir -p "$LPHOME"
for f in conf.sh lib.sh suite-covers.sh; do
    ln -s "$HERE/$f" "$LPHOME/$f"
done
# locate_home no longer searches: SPIRA_HOME IS the home, and every binary reads
# <home>/conf.d for the registry.
ln -s "$HERE/conf.d" "$LPHOME/conf.d"
printf '# fixture watchers\n' > "$LPHOME/watchers"

LPRUN="$TMP/lprun"; mkdir -p "$LPRUN"
# SPIRA_REPO_MAP defaults to empty here — the exact "nothing anywhere the candidate search
# would find" shape (law-absence-needs-a-positive-control's POSITIVE CONTROL below): per
# landing-pass/src/seam.rs, repo_map_ok is `[ -r "${SPIRA_REPO_MAP:-}" ]`, which an empty
# string fails identically to a genuinely absent key. Later calls override it via tl_config.
tl_config SPIRA_RUN="$LPRUN" SPIRA_DB="$TMP/db" SPIRA_ID_PREFIX=sp \
    SPIRA_WATCHERS="$LPHOME/watchers" SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= SPIRA_REPO_MAP=
lp() {  # lp [KEY=val ...] — runs the real binary in an otherwise-empty environment
    # Extra SPIRA_* overrides (SPIRA_REPO_MAP below) are registered keys — declared via
    # tl_config rather than forwarded through env -i, which strips them.
    [ $# -gt 0 ] && tl_config "$@"
    env -i PATH="$PATH" HOME="$TMP/xhome" SPIRA_HOME="$LPHOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$SPIRA_TOML" \
        "$LP_BIN" --pass
}

# POSITIVE CONTROL: the map variable unset, nothing anywhere the candidate search would
# find. This is the exact shape from the bead — run by hand in a minimal environment that
# omits the variable.
LOG="$LPRUN/landing-pass.log"
: > "$LOG"
lp >>"$LOG" 2>&1; rc=$?
wantrc "unset map: exits non-zero, not the silent rc=0 that ran 1704 times" "1" "$rc"
want "unset map: log names the fault" "is not configured or unreadable" \
    "$(cat "$LOG" 2>/dev/null)"

# Unreadable: set but pointing nowhere.
: > "$LOG"
lp SPIRA_REPO_MAP="$TMP/does-not-exist/map" >>"$LOG" 2>&1; rc=$?
wantrc "unreadable map: exits non-zero" "1" "$rc"
want "unreadable map: log names the fault" "is not configured or unreadable" \
    "$(cat "$LOG" 2>/dev/null)"

# NEGATIVE CONTROL side of the same check: a configured, readable, empty map is a real
# empty work queue, not a config fault — this must stay rc=0, or the fix would have turned
# every box with nothing to land into a false alarm.
: > "$LOG"
printf '# no rows\n' > "$TMP/empty-map"
lp SPIRA_REPO_MAP="$TMP/empty-map" >>"$LOG" 2>&1; rc=$?
wantrc "configured but empty map: still exits 0 (empty queue is not a fault)" "0" "$rc"
nowant "configured but empty map: log never claims the map is unconfigured" \
    "is not configured or unreadable" "$(cat "$LOG" 2>/dev/null)"

# A map that resolves and names a real pr-mode repository: the pass must actually
# enumerate it (acceptance criterion: "the log contains at least one line reporting a
# repository actually enumerated" — the disappearance of the fault message alone would
# also happen if the unit were simply stopped).
PROBE_REPO="$TMP/probe-repo"; PROBE_REMOTE="$TMP/probe-remote.git"
git init -q -b main "$PROBE_REPO"
git -C "$PROBE_REPO" commit -q --allow-empty -m base
# A local bare clone stands in for the forge remote (never `git push` from an aeon
# session — law-tests-run-only-through-testenv's spirit applies to any real push path).
timeout 5 git clone -q --bare "$PROBE_REPO" "$PROBE_REMOTE"
git -C "$PROBE_REPO" remote add origin "$PROBE_REMOTE"
timeout 5 git -C "$PROBE_REPO" fetch -q origin
git -C "$PROBE_REPO" branch -q spira/sp-probe1 main
printf 'probe-repo | %s | pr | origin/main\n' "$PROBE_REPO" > "$TMP/real-map"

: > "$LOG"
# SPIRA_DB points nowhere real: `bd show` fails closed to an empty bead map, which drives
# the branch down the "not in bead db" path below — that path still names the repo and the
# branch, which is exactly the positive evidence this test needs, without requiring a live
# beads database.
lp SPIRA_REPO_MAP="$TMP/real-map" >>"$LOG" 2>&1; rc=$?
wantrc "configured map with a real pr-mode repo: exits 0" "0" "$rc"
want "configured map: log reports the repository by name — a repo was enumerated" \
    "probe-repo" "$(cat "$LOG" 2>/dev/null)"
nowant "configured map: log never falls back to the unconfigured fault" \
    "is not configured or unreadable" "$(cat "$LOG" 2>/dev/null)"

tl_summary
