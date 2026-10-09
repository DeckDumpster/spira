#!/usr/bin/env bash
# test-script-lint-rules.sh — spira-lint's covers-entries and script-exec rules, each shown
# able to fire on a planted offender before its silence is believed.
# tier: T1
# covers: spira-lint/* UC-test-infrastructure-02 UC-test-infrastructure-41
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

command -v spira-lint >/dev/null 2>&1 || bail "spira-lint is not on PATH"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
R="$TMP/repo"
mkdir -p "$R/spira"
git init -q -b main "$R"
commit() { git -C "$R" add -A && git -C "$R" -c user.email=t@t -c user.name=t commit -q -m "$1"; }
lint() { env -i PATH="$PATH" HOME="$TMP" spira-lint --root "$R" --only "$1" 2>&1; }

printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/real.sh spira/*.sh UC-area-01\n#   spira/ghost.sh\nset -uo pipefail\n' > "$R/spira/test-a.sh"
printf '#!/usr/bin/env bash\ntrue\n' > "$R/spira/real.sh"
chmod +x "$R/spira/test-a.sh" "$R/spira/real.sh"
commit planted

out="$(lint covers-entries)"; rc=$?
wantrc "covers-entries: a dangling token fails"                1 "$rc"
want   "covers-entries: it names the suite"                    "spira/test-a.sh" "$out"
want   "covers-entries: it names the token (continuation line)" "spira/ghost.sh" "$out"
nowant "covers-entries: a catalogue id is not a path"          "UC-area-01" "$out"
sed -i '/ghost/d' "$R/spira/test-a.sh"
commit cleared
out="$(lint covers-entries)"; rc=$?
wantrc "covers-entries: resolvable tokens and a glob pass"     0 "$rc"
want   "covers-entries: it reports clean"                      "clean" "$out"

chmod -x "$R/spira/real.sh"
out="$(lint script-exec)"; rc=$?
wantrc "script-exec: a non-executable operator script fails"   1 "$rc"
want   "script-exec: it names the script"                      "spira/real.sh" "$out"
nowant "script-exec: a suite is out of scope"                  "test-a.sh" "$(chmod -x "$R/spira/test-a.sh"; lint script-exec)"
chmod +x "$R/spira/test-a.sh" "$R/spira/real.sh"
out="$(lint script-exec)"; rc=$?
wantrc "script-exec: executable scripts pass"                  0 "$rc"

printf '#!/usr/bin/env bash\n# lib.sh — Sourced, never executed.\n' > "$R/spira/libx.sh"
chmod -x "$R/spira/libx.sh"
commit lib
out="$(lint script-exec)"; rc=$?
wantrc "script-exec: a declared sourced-only library passes"   0 "$rc"

tl_summary
