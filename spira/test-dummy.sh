#!/usr/bin/env bash
# covers: spira/suite-covers.sh
# host-reason: tests suite-covers.sh parsing; pure bash with temp files, no container dependency
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

. "$HERE/suite-covers.sh"

printf 'test-dummy.sh\n'
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# POSITIVE CONTROL: a file with a covers: line is found before trusting silence.
printf '# covers: spira/foo.sh spira/bar.sh\n' > "$TMP/suite-a.sh"
_cov="$(suite_covers_of "$TMP/suite-a.sh")"
is "covers: directive extracted" "spira/foo.sh spira/bar.sh" "$_cov"

# A file with no covers: line returns empty (run always).
printf '#!/usr/bin/env bash\n' > "$TMP/suite-b.sh"
_cov="$(suite_covers_of "$TMP/suite-b.sh")"
is "no covers: returns empty" "" "$_cov"

# A nonexistent file returns empty without error.
_cov="$(suite_covers_of "$TMP/no-such.sh")"
is "nonexistent file returns empty" "" "$_cov"

# Only the first covers: line is returned.
printf '# covers: spira/first.sh\n# covers: spira/second.sh\n' > "$TMP/suite-c.sh"
_cov="$(suite_covers_of "$TMP/suite-c.sh")"
is "only first covers: line returned" "spira/first.sh" "$_cov"

# POSITIVE CONTROL: a requires: directive is extracted.
printf '# requires: claude bd\n' > "$TMP/suite-d.sh"
_req="$(suite_requires_of "$TMP/suite-d.sh")"
is "requires: directive extracted" "claude bd" "$_req"

# No requires: line returns empty.
printf '#!/usr/bin/env bash\n' > "$TMP/suite-e.sh"
_req="$(suite_requires_of "$TMP/suite-e.sh")"
is "no requires: returns empty" "" "$_req"

printf '\nASSERTIONS %d\n' $((pass + fail))
[ "$fail" -eq 0 ]
