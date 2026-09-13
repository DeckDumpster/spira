# suite-assert.sh — assertion helpers with ASSERTIONS trailer. Sourced by suites.
#
# Source this file to get ok/bad/is/want/nowant helpers that count assertions
# and print "ASSERTIONS <n>" on EXIT. suites.sh reads this trailer to classify
# a non-zero exit with zero assertions as a setup fault rather than a suite red.
#
# A MISSING TRAILER means the suite has not yet adopted the counter; suites.sh
# treats it as red (as today), so un-migrated suites are unchanged. This mirrors
# the rule suites.sh already applies to an unreadable gate-suites list: absence
# never reads as "nothing to do".
#
# TRAP CHAINING. This file registers an EXIT trap. If the suite registers its own
# cleanup trap AFTER sourcing this file, the cleanup trap overwrites this one. To
# preserve the ASSERTIONS trailer, chain the cleanup explicitly:
#
#   TMP="$(mktemp -d)"
#   trap 'rm -rf "$TMP"; printf "ASSERTIONS %d\n" "$_sa_assertions"' EXIT
#
# Or, source this file AFTER setting your own trap. The EXIT trap registered here
# fires when the suite exits for any reason — setup failure, assertion pass or fail —
# so the count is always emitted, including from early-exit paths.

_sa_assertions=0
ok()  { _sa_assertions=$((_sa_assertions+1)); printf '  ok    %s\n' "$1"; }
bad() { _sa_assertions=$((_sa_assertions+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
trap 'printf "ASSERTIONS %d\n" "$_sa_assertions"' EXIT
