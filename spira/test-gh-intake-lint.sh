#!/usr/bin/env bash
#
# test-gh-intake-lint.sh — the fence that refuses a constructed GitHub write or a
# credential reference in gh-intake.sh (UC-dispatch-06).
#
#   ./test-gh-intake-lint.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# "no write constructed" and "no credential read" used to be two grep assertions inside
# test-gh-intake.sh, a T2 suite that stands up a stub curl/bd fixture to check two lines the
# fixture never runs. They are now spira/gh-intake-lint.sh, a T0 fence, and this suite is
# its positive control (law-absence-needs-a-positive-control): a bad shape is planted, the
# fence is required to name its line, the plant is withdrawn, and only then is the shipped
# file's silence evidence of anything.
#
# tier: T0
# covers: spira/gh-intake-lint.sh spira/gh-intake.sh spira/gate-spira.sh UC-dispatch-06
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
PROBE="$TMP/probe.sh"

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL — every offending shape, planted in one file.
# ---------------------------------------------------------------------------------------
cat > "$PROBE" <<'EOF'
#!/usr/bin/env bash
curl -X POST https://api.github.com/repos/x/y/issues
curl --data '{}' https://api.github.com
if [ -n "$GITHUB_TOKEN" ]; then :; fi
echo "$github.token"
curl -H "Authorization: token $t"
EOF

out="$(bash "$HERE/gh-intake-lint.sh" --scan "$PROBE")"
want "SEEN RED: a mutating POST is caught"    "curl -X POST"          "$out"
want "and a mutating --data is caught"        "curl --data"           "$out"
want "and GITHUB_TOKEN is caught"             "GITHUB_TOKEN"          "$out"
want "and an Authorization header is caught"  "Authorization:"        "$out"

# ---------------------------------------------------------------------------------------
# THE CORRECT FORMS ARE SILENT, even sitting right next to a comment mentioning the words.
# ---------------------------------------------------------------------------------------
cat > "$PROBE" <<'EOF'
#!/usr/bin/env bash
# gh-intake.sh must never construct a write or read GITHUB_TOKEN / Authorization:
curl -sS --max-time 30 "https://api.github.com/repos/x/y/issues?state=open"
curl -sS -o /dev/null -w '%{http_code}' "https://api.github.com/orgs/x/members/y"
EOF
out="$(bash "$HERE/gh-intake-lint.sh" --scan "$PROBE")"
is "GREEN: read-only curl and a comment mentioning the words are both silent" "" "$out"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION. A fence nothing invokes is a file.
# ---------------------------------------------------------------------------------------
want "the gate names this fence" "spira/gh-intake-lint.sh" "$(cat "$HERE/gate-spira.sh")"
is   "and it is executable"      "0" "$([ -x "$HERE/gh-intake-lint.sh" ]; echo $?)"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, this now means something.
# ---------------------------------------------------------------------------------------
out="$(bash "$HERE/gh-intake-lint.sh")"; rc=$?
is "the shipped gh-intake.sh passes" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

tl_summary
