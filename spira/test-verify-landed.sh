#!/usr/bin/env bash
# test-verify-landed.sh — a bead's declared production check runs after deploy; a failure
# files a child bead carrying the output, a pass records evidence, nothing is reopened.
#
# tier: T1
# covers: spira/verify-landed.sh spira/deploy.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
export SPIRA_CONF=/nonexistent SPIRA_VERIFY_TIMEOUT=3 SPIRA_DB=/nonexistent
tl_config SPIRA_VERIFY_TIMEOUT=3 SPIRA_DB=/nonexistent

echo "test-verify-landed.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
FIX="$TMP/fix"; mkdir -p "$FIX"; export FIX

cat > "$TMP/bd" <<'STUB'
#!/usr/bin/env bash
shift 2
case "$1" in
    show) cat "$FIX/$2.json" ;;
    comments)
        if [ "${2:-}" = add ]; then printf '%s\n' "$4" >> "$FIX/$3.comments"; echo "$1 $2 $3 $4" >> "$FIX/calls"
        else cat "$FIX/$2.comments" 2>/dev/null; fi ;;
    *) echo "$*" >> "$FIX/calls" ;;
esac
STUB
cat > "$TMP/bead.sh" <<'STUB'
#!/usr/bin/env bash
echo "FILE $*" >> "$FIX/filed"
for a in "$@"; do [ -f "$a" ] && cat "$a" >> "$FIX/filed"; done
STUB
chmod +x "$TMP/bd" "$TMP/bead.sh"
export SPIRA_BD="$TMP/bd" SPIRA_VERIFY_BEAD_SH="$TMP/bead.sh"
tl_config SPIRA_BD="$SPIRA_BD"

mkbead() { # id description
    python3 -c 'import json,sys; print(json.dumps([{"id":sys.argv[1],"labels":["repo:zzrepo"],"description":sys.argv[2]}]))' "$1" "$2" > "$FIX/$1.json"
}
V="$HERE/verify-landed.sh"

mkbead sp-pass1 $'body\nverify: echo healthy => healthy'
mkbead sp-fail1 $'body\nverify: echo broken; exit 1'
mkbead sp-miss1 $'body\nverify: echo up => healthy'
mkbead sp-none1 'no check declared'

out="$("$V" sp-pass1 sp-none1 2>&1)"
want "passing check is reported"            "PASS sp-pass1" "$out"
want "pass is recorded on the bead"         "PASS: echo healthy" "$(cat "$FIX/sp-pass1.comments")"
is   "pass files nothing"                   "" "$(cat "$FIX/filed" 2>/dev/null)"
nowant "bead with no check is untouched"    "sp-none1" "$out"

out="$("$V" sp-fail1 2>&1)"; rc=$?
want "failing check is reported"            "FAIL sp-fail1" "$out"
filed="$(cat "$FIX/filed")"
want "one follow-up filed as a child"       "--parent sp-fail1" "$filed"
want "follow-up carries the output"         "broken" "$filed"
want "follow-up takes the bead's repo"      "--repo zzrepo" "$filed"
is   "exactly one follow-up"                "1" "$(grep -c '^FILE' "$FIX/filed")"
nowant "the bead is never reopened"         "reopen" "$(cat "$FIX/calls")"

: > "$FIX/filed"
"$V" sp-miss1 >/dev/null 2>&1
want "wrong output fails an expectation"    "expected output to contain: healthy" "$(cat "$FIX/filed")"

: > "$FIX/filed"
"$V" sp-fail1 >/dev/null 2>&1
is   "a re-run does not file twice"         "" "$(cat "$FIX/filed")"

mkbead sp-hang1 $'verify: sleep 30'
: > "$FIX/filed"
"$V" sp-hang1 >/dev/null 2>&1
want "a hanging check is bounded and fails" "sp-hang1" "$(cat "$FIX/filed")"

R="$TMP/repo"; git init -q "$R"; git -C "$R" -c user.name=t -c user.email=t@t commit -q --allow-empty -m "base"
git -C "$R" -c user.name=t -c user.email=t@t commit -q --allow-empty -m "sp-pass1: fix"
# SPIRA_ID_PREFIX is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG):
# declare via tl_config, not the env prefix below, which no process reads it from any more.
tl_config SPIRA_ID_PREFIX=sp
out="$("$V" --range HEAD~1..HEAD --repo "$R" 2>&1)"
want "ids come from the landed range"       "PASS sp-pass1" "$out"

tl_summary
