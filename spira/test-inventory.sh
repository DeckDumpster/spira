#!/usr/bin/env bash
# tier: T2
# covers: spira/inventory.sh spira/gate-spira.sh UC-safety-fences-25
#
# host-reason: creates scratch git repos to test inventory.sh; no container-hosted state
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-inventory.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

inv_at() {
    local root="$1"; shift
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/inventory.sh" "$@" 2>&1
}

ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/inventory.sh" "$HERE/inventory-deny" "$ROOT/spira/"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

# ---------------------------------------------------------------------------------------
# EMPTY INDEX — must exit 3, refusing to report clean (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------------------
out="$(inv_at "$ROOT")"; rc=$?
is   "empty index exits 3 (refuses to report clean)" "3" "$rc"
want "and says why"                                   "refusing to report clean" "$out"

printf '#!/usr/bin/env bash\necho ok\n' > "$ROOT/spira/helper.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m init

out="$(inv_at "$ROOT")"; rc=$?
is   "clean tree exits 0" "0" "$rc"
want "and says so"        "clean" "$out"

# ---------------------------------------------------------------------------------------
# SEEN RED: /home/<user>/path planted.
# ---------------------------------------------------------------------------------------
printf '# config at /home/testuser/config.conf\n' > "$ROOT/spira/planted.sh"
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant home path"

out="$(inv_at "$ROOT")"; rc=$?
is   "SEEN RED: /home/user/ path is refused" "1" "$rc"
want "names the file"                        "spira/planted.sh" "$out"
want "names the token"                       "/home/testuser/" "$out"

# ---------------------------------------------------------------------------------------
# SEEN GREEN: withdraw it.
# ---------------------------------------------------------------------------------------
git -C "$ROOT" rm -qf spira/planted.sh
git -C "$ROOT" commit -q -m "withdraw home path"

out="$(inv_at "$ROOT")"; rc=$?
is   "SEEN GREEN: clean after withdrawal" "0" "$rc"

# ---------------------------------------------------------------------------------------
# SEEN RED: violation in an untracked (not yet committed) file — sp-hm2vw.
# The positive control is the planted-and-caught test above; this proves the fix that
# added --others --exclude-standard works, because git ls-files alone misses it.
# ---------------------------------------------------------------------------------------
printf '# config at /home/testuser/config.conf\n' > "$ROOT/spira/untracked-violation.sh"
# File is intentionally NOT staged or committed — left untracked.

out="$(inv_at "$ROOT")"; rc=$?
is   "SEEN RED: untracked violation is caught" "1" "$rc"
want "names the untracked file"               "spira/untracked-violation.sh" "$out"

rm -f "$ROOT/spira/untracked-violation.sh"
out="$(inv_at "$ROOT")"; rc=$?
is   "SEEN GREEN: clean once untracked file removed" "0" "$rc"

# ---------------------------------------------------------------------------------------
# SEEN RED: /workspaces/ path.
# ---------------------------------------------------------------------------------------
printf '# repo lives at /workspaces/myproject\n' > "$ROOT/spira/ws.sh"
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant workspaces path"

out="$(inv_at "$ROOT")"; rc=$?
is   "SEEN RED: /workspaces/ path is refused" "1" "$rc"
want "names /workspaces/ token"               "/workspaces/" "$out"

git -C "$ROOT" rm -qf spira/ws.sh
git -C "$ROOT" commit -q -m "withdraw workspaces path"

# ---------------------------------------------------------------------------------------
# SEEN RED: provenance mark.
# ---------------------------------------------------------------------------------------
printf '# (per Alice, 2025-09-01) changed this\n' > "$ROOT/spira/prov.sh"
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant provenance mark"

out="$(inv_at "$ROOT")"; rc=$?
is   "SEEN RED: provenance mark is refused" "1" "$rc"
want "names the token"                      "per Alice" "$out"

git -C "$ROOT" rm -qf spira/prov.sh
git -C "$ROOT" commit -q -m "withdraw provenance mark"

# ---------------------------------------------------------------------------------------
# SEEN RED: real email address (non-exempt domain).
# ---------------------------------------------------------------------------------------
printf '# contact: user@realcompany.io\n' > "$ROOT/spira/mail.sh"
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant real email"

out="$(inv_at "$ROOT")"; rc=$?
is   "SEEN RED: real email is refused" "1" "$rc"
want "names the token"                 "user@realcompany.io" "$out"

git -C "$ROOT" rm -qf spira/mail.sh
git -C "$ROOT" commit -q -m "withdraw email"

# ---------------------------------------------------------------------------------------
# EXEMPTIONS. inventory.sh, inventory-deny, and test-inventory.sh are never flagged even
# when they contain tokens the fence would refuse in any other file. A real offender is
# planted alongside them so that a fence that silently exempts everything would still exit 1
# — the only way nowant assertions mean anything here.
# ---------------------------------------------------------------------------------------
printf '# /home/baduser/secrets\n' > "$ROOT/spira/offender.sh"
printf '# /home/exemptuser/path — in deny list, not scanned\n' >> "$ROOT/spira/inventory-deny"
printf '# /home/testuser/ — in test file, not scanned\n' > "$ROOT/spira/test-inventory.sh"
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant offender with exempted files"

out="$(inv_at "$ROOT")"; rc=$?
is   "offender is still caught alongside exempted files" "1" "$rc"
nowant "inventory.sh is not reported"      "spira/inventory.sh:"      "$out"
nowant "inventory-deny is not reported"    "spira/inventory-deny:"    "$out"
nowant "test-inventory.sh is not reported" "spira/test-inventory.sh:" "$out"
want   "offender is named"                 "spira/offender.sh"        "$out"

git -C "$ROOT" rm -qf spira/offender.sh
git -C "$ROOT" commit -q -m "withdraw offender"
out="$(inv_at "$ROOT")"; rc=$?
is "clean once offender is removed" "0" "$rc"

# ---------------------------------------------------------------------------------------
# EXEMPT EMAIL FORMS. git@ remote form and example domains must not be flagged.
# ---------------------------------------------------------------------------------------
printf 'remote: git@github.com:org/repo.git\n' > "$ROOT/spira/remote.sh"
printf '# author: user@example.com\n'          >> "$ROOT/spira/remote.sh"
printf '# admin: user@example.org\n'           >> "$ROOT/spira/remote.sh"
printf '# unit: watcher@watcher.service\n'     >> "$ROOT/spira/remote.sh"
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "add exempt email forms"

out="$(inv_at "$ROOT")"; rc=$?
is   "git@ remote form is not flagged" "0" "$rc"

# ---------------------------------------------------------------------------------------
# --scan <file>: matches without walking the tree; exit 0 either way.
# ---------------------------------------------------------------------------------------
PROBE="$TMP/probe.sh"
printf '# config at /home/someuser/conf.sh\n' > "$PROBE"
out="$(env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$HERE/inventory.sh" --scan "$PROBE" 2>&1)"
want "--scan finds the token" "/home/someuser/" "$out"

printf '#!/usr/bin/env bash\necho ok\n' > "$PROBE"
out="$(env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$HERE/inventory.sh" --scan "$PROBE" 2>&1)"
is "--scan returns empty on clean file" "" "$out"

# ---------------------------------------------------------------------------------------
# --patterns: prints the pattern list; each pattern is a non-empty line.
# ---------------------------------------------------------------------------------------
out="$(env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$HERE/inventory.sh" --patterns 2>&1)"
want "--patterns includes /home/ pattern"       "/home/" "$out"
want "--patterns includes /workspaces/ pattern" "/workspaces/" "$out"
want "--patterns includes per-name pattern"     "per " "$out"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION. gate-spira.sh:126 already runs `bash spira/inventory.sh` against the
# shipped tree on every landing (UC-safety-fences-25, D6) — re-running that same check here
# against a mirrored copy asserted nothing this suite's own rows and the gate's own run
# don't already cover, twice, on every branch. Whether the gate is still WIRED to
# inventory.sh at all is test-gate-fences.sh's row (D7), reading gate_fence_list rather
# than grepping this file's source for the name.
# ---------------------------------------------------------------------------------------
is   "inventory.sh is executable"       "0" "$([ -x "$HERE/inventory.sh" ]; echo $?)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
