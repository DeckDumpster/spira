#!/usr/bin/env bash
# test-gate-touched.sh — the landing gate runs only the suites a branch adds or changes.
# covers: spira/gate-touched.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "want [$2] got [$3]"; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
R="$TMP/repo"; git init -q -b main "$R"; mkdir -p "$R/spira"
printf 'x\n' > "$R/spira/test-old.sh"; printf 'x\n' > "$R/spira/test-kept.sh"; printf 'x\n' > "$R/spira/lib.sh"
git -C "$R" add -A; git -C "$R" commit -q -m base
git -C "$R" checkout -q -b br
printf 'y\n' >> "$R/spira/test-old.sh"; printf 'n\n' > "$R/spira/test-new.sh"; printf 'y\n' >> "$R/spira/lib.sh"
git -C "$R" add -A; git -C "$R" commit -q -m change
touched() { (cd "$R" && bash "$HERE/gate-touched.sh" main br | sort | tr '\n' ' ' | sed 's/ $//'); }

echo "test-gate-touched.sh"
is "on the branch: added and changed suites, nothing else" "test-new.sh test-old.sh" "$(touched)"
git -C "$R" checkout -q main
is "on the base tree: a suite the branch adds is absent"   "test-old.sh" "$(touched)"
git -C "$R" checkout -q br
git -C "$R" checkout -q -b quiet main
is "a branch touching no suite selects nothing"            "" "$(cd "$R" && bash "$HERE/gate-touched.sh" main quiet)"
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
