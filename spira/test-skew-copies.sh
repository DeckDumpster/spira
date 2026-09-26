#!/usr/bin/env bash
# tier: T2
# covers: spira/skew.sh
#
# test-skew-copies.sh — `skew.sh copies` names every mapped repository carrying a harness
# copy, self or second, without being fooled by drift between them.
#
#   ./test-skew-copies.sh
#
# WHAT THIS SUITE IS FOR (gap G2, docs/test-plan/instance-lifecycle.md)
# -----------------------------------------------------------------
# `copies` had no test at all before this suite: skew.sh:156 lists every mapped repository
# that carries the harness signature (a directory holding boundary, gate.sh and lib.sh
# together), tagging each self (== SPIRA_REPO) or second, and exits 3 — "the map or the
# matcher is wrong" — when nothing is found. Silence there is indistinguishable from a
# broken map (law-absence-needs-a-positive-control), so the first case proves a
# harness-bearing repo IS found before the last proves that none being found is reported
# rather than swallowed.
#
# THE DRIFTED-COPY CASE. The matcher (exclude.sh harness-in) looks at PATHS, not file
# content — a directory qualifies by holding the three signature files, whatever they
# contain. A second copy that has drifted from the harness's own tree (edited, not just
# copied) must still be found: if drift could hide a copy from this report, the report
# would read clean on exactly the boxes carrying a stale, diverged one.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

SH="$TMP/spira"; RUN="$TMP/run"; WS="$TMP/ws"
mkdir -p "$SH" "$RUN" "$WS"
cp "$HERE/conf.sh" "$HERE/lib.sh" "$HERE/exclude.sh" "$HERE/skew.sh" "$SH/"

sig() { local d="$1" body="${2:-x}"; mkdir -p "$d"; printf '%s\n' "$body" > "$d/boundary"; printf '%s\n' "$body" > "$d/gate.sh"; printf '%s\n' "$body" > "$d/lib.sh"; }
commit() { git -C "$1" add -A >/dev/null 2>&1; git -C "$1" commit -q -m "${2:-c}" >/dev/null 2>&1; } # hermetic-ok: $1 is always a path under $WS ($TMP); positional params can't be statically traced

# home — the harness's OWN repository, with the signature at the root. This is SPIRA_REPO.
git init -q -b main "$WS/home"
sig "$WS/home" "home body"
commit "$WS/home" base

# second — a DIFFERENT repository that also carries the signature, deliberately drifted:
# its files hold different content from home's, proving the matcher is path-based, not
# content-based.
git init -q -b main "$WS/second"
sig "$WS/second" "drifted body — does not match home"
commit "$WS/second" base

# plain — an ordinary repository carrying no harness at all.
git init -q -b main "$WS/plain"
printf 'hello\n' > "$WS/plain/README.md"
commit "$WS/plain" base

write_map() {   # write_map <name>... — a repo-map row per name, from a fixed table
    : > "$SH/repo-map"
    local n
    for n in "$@"; do
        printf '%s | %s | pr | main |  |\n' "$n" "$WS/$n" >> "$SH/repo-map"
    done
}

run_copies() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB=/nonexistent-spira-db \
        SPIRA_REPO="$WS/home" SPIRA_HOME_REPO=home \
        bash "$SH/skew.sh" copies 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

echo "test-skew-copies.sh"

# ===========================================================================
echo
echo "positive control — one harness-bearing repo (self) is listed:"
# ===========================================================================
write_map home
out="$(run_copies)"; rc=$?
is   "self only: exits 0"                    "0"      "$rc"
home_line="$(printf '%s\n' "$out" | grep '^home ')"
want "self only: names the repo"             "$WS/home" "$home_line"
want "self only: tags it self"               " self"    "$home_line"

# ===========================================================================
echo
echo "none found — no mapped repo carries a harness → exit 3:"
# ===========================================================================
write_map plain
out="$(run_copies)"; rc=$?
is   "no harness: exits 3"                                   "3"                                   "$rc"
want "no harness: says the map or matcher is wrong"          "the map or the matcher is wrong"      "$out"

# ===========================================================================
echo
echo "gap G2 — a deliberately drifted second copy is still reported, not silently dropped:"
# ===========================================================================
diff -q "$WS/home/gate.sh" "$WS/second/gate.sh" >/dev/null 2>&1 \
    && bad "fixture sanity: second's gate.sh differs from home's" "files are identical — drift not staged" \
    || ok "fixture sanity: second's gate.sh differs from home's"

write_map home second
out="$(run_copies)"; rc=$?
is "drifted second: exits 0" "0" "$rc"
home_line="$(printf '%s\n' "$out" | grep '^home ')"
second_line="$(printf '%s\n' "$out" | grep '^second ')"
want "drifted second: home still listed as self"       " self"   "$home_line"
want "drifted second: drifted copy path is reported"   "$WS/second" "$second_line"
want "drifted second: drifted copy tagged second, not self" " second" "$second_line"

tl_summary
