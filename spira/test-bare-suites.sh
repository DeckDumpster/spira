#!/usr/bin/env bash
#
# test-bare-suites.sh — bare-suites.sh selects and runs the bare-host suites, records their
# results, and refuses what it cannot select; G12 and the round lane call it.
#
# tier: T2
# covers: spira/bare-suites.sh spira/bare-local.sh spira/host-check.sh .github/workflows/gate.yml round-vm/src/run.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
R="$TMP/repo"; mkdir -p "$R/spira"
cp "$HERE/bare-suites.sh" "$HERE/host-check.sh" "$R/spira/"
mk() { printf '#!/usr/bin/env bash\n# bare-host: fixture\n%s\n' "$2" > "$R/spira/$1"; }
mk test-aa.sh 'echo ran-aa'
mk test-bb.sh 'echo ran-bb; exit 1'
mk test-cc.sh 'echo ran-cc; exit 77'
printf '#!/usr/bin/env bash\necho ran-plain\n' > "$R/spira/test-plain.sh"
git init -q -b main "$R" && git -C "$R" add -A && git -C "$R" commit -qm fixture
run() { bash "$R/spira/bare-suites.sh" "$@" 2>&1; }

out="$(run --suites test-aa.sh)"; rc=$?
wantrc "a passing selection exits 0" 0 "$rc"
want "the selected suite ran" "ran-aa" "$out"
nowant "an unselected bare suite did not run" "ran-bb" "$out"
want "the unselected bare suite is named as skipped" "test-bb.sh not selected" "$out"
want "it reports how many it checked" "checked 1 suites" "$out"

out="$(run --suites test-bb.sh)"; rc=$?
wantrc "a failing suite exits 1" 1 "$rc"
want "the failure is named" "test-bb.sh FAILED (exit 1)" "$out"

out="$(run --suites test-cc.sh)"; rc=$?
wantrc "a skip (77) is not a failure" 0 "$rc"

out="$(run --suites all)"; rc=$?
wantrc "all runs every bare suite and fails on the red one" 1 "$rc"
want "all reaches the first" "ran-aa" "$out"; want "all reaches the last" "ran-cc" "$out"
nowant "a suite without the marker is never run" "ran-plain" "$out"

out="$(run --suites test-plain.sh)"; rc=$?
wantrc "a name that is not bare-host is refused" 2 "$rc"
want "the refusal names it" "test-plain.sh' is not a bare-host suite" "$out"

out="$(run --selected test-plain.sh,test-aa.sh,test-nope.sh)"; rc=$?
wantrc "--selected ignores what is not bare-host" 0 "$rc"
want "--selected runs the bare suite among them" "ran-aa" "$out"
nowant "--selected does not run the plain one" "ran-plain" "$out"

out="$(run --selected "")"; rc=$?
wantrc "an empty selection exits 0" 0 "$rc"
nowant "an empty selection runs nothing" "ran-" "$out"
want "an empty selection says so" "checked 0 suites" "$out"

out="$(run)"; rc=$?; wantrc "no selection flag is a usage error" 2 "$rc"

res="$TMP/res"
run --suites all --results "$res" >/dev/null
read -r st _ _ _ _ _ rrc < "$res/test-bb.sh.result"
is "a red suite is recorded red" red "$st"; is "its exit code is recorded" 1 "$rrc"
read -r st _ < "$res/test-aa.sh.result"; is "a green suite is recorded ok" ok "$st"
read -r st _ < "$res/test-cc.sh.result"; is "a skip is recorded skip" skip "$st"
want "the output is kept beside the result" "ran-bb" "$(cat "$res/test-bb.sh.out")"

rm -f "$R"/spira/test-aa.sh "$R"/spira/test-bb.sh "$R"/spira/test-cc.sh
out="$(run --suites all)"; rc=$?
wantrc "with no bare suite there is nothing to select from" 2 "$rc"

want "G12 calls the shared script" "spira/bare-suites.sh --selected" "$(cat "$HERE/../.github/workflows/gate.yml")"
nowant "G12 keeps no loop of its own" "_g12_fail" "$(cat "$HERE/../.github/workflows/gate.yml")"
want "bare-local calls the shared script" "spira/bare-suites.sh" "$(cat "$HERE/bare-local.sh")"
want "the real tree has bare suites to select" "test-install-rehearsal.sh" "$(bash "$HERE/host-check.sh" --bare-suites)"

tl_summary
