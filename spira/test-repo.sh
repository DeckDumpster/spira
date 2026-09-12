#!/usr/bin/env bash
#
# test-repo.sh — doctor.sh refuses a repo-map row with fewer than six columns.
#
#   ./test-repo.sh
#
# THE DEFECT THIS PREVENTS. repo-map.example's header stated that a check refused
# short rows; no such check existed. The hazard is real: repo_field maps NF<6 rows
# by position so that a five-field row's gate field is read as a formatter, and
# whatever the gate column holds — `origin/main`, a test expression — runs as a
# program in the branch's tree after a rebase and commits the result. The fix is
# refusing to load a short row, not changing how repo_field reads one.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control). The check fires on a
# five-field fixture row before the wider tree is declared clean. A validator tested
# only against well-formed input proves nothing: it passes just as well when its
# pattern never matches.
#
# defect: sp-2inx
# covers: spira/doctor.sh spira/lib.sh
# scar: when the while loop that checks NF<6 was removed, "fewer than six columns is
# stated" failed: wanted [fewer than six] in the doctor.sh output but it was absent.
# Seen red (sp-841s): 3 failed — narrow row: fewer than six columns is stated: wanted
# [fewer than six] in [... 4 fatal, 16 warnings ...] — the awk block never ran.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-repo.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
REPO="$TMP/repo"
mkdir -p "$REPO" "$TMP/home" "$TMP/run"

# A real git repo so the per-row path checks in the repositories section have
# something to resolve. doctor.sh uses the map's path column, not SPIRA_REPO,
# for those checks.
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
       GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REPO"
git -C "$REPO" fetch -q

# run_doctor <map-path> -> doctor.sh output; always exits 0 (we read the output).
# SPIRA_HOME points at $HERE (the source checkout) so doctor.sh locates lib.sh,
# conf.sh, and peer scripts without a separate copy. No stubs for skew.sh or
# install-session-hook.sh: those produce WARNs, not FAILs, and the assertions here
# check only for "fewer than six" (repo column count) and do not conflict with them.
run_doctor() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
        GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
        SPIRA_DB="$TMP/no-db" \
        SPIRA_REPO_MAP="$1" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_NOTIFY="$TMP/no-notify" \
        bash "$HERE/doctor.sh" 2>&1 || true
}

# ==========================================================================
echo
echo "POSITIVE CONTROL: a five-field row is refused by the column check:"
# ==========================================================================
# name|path|land|base|gate — five fields; FORMAT is absent so the gate column
# is misread as the formatter when NF==5 in repo_field.
NARROW="$TMP/narrow-map"
printf 'myrepo | %s | push | origin/main | true\n' "$REPO" > "$NARROW"

out="$(run_doctor "$NARROW")"
want "narrow row: repo name appears in FAIL output"  "myrepo"            "$out"
want "narrow row: FAIL is present"                   "FAIL"              "$out"
want "narrow row: fewer than six columns is stated"  "fewer than six"    "$out"

# ==========================================================================
echo
echo "a well-formed six-field row is not refused:"
# ==========================================================================
# name|path|land|base|format|gate — six fields; FORMAT is empty, gate is `true`.
WIDE="$TMP/wide-map"
printf 'myrepo | %s | push | origin/main |  | true\n' "$REPO" > "$WIDE"

out2="$(run_doctor "$WIDE")"
nowant "wide row: no narrow-column FAIL" "fewer than six" "$out2"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
