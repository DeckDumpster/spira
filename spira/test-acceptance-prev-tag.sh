#!/usr/bin/env bash
#
# test-acceptance-prev-tag.sh — acceptance-prev-tag.sh: derives the prev-tag for
# acceptance's upgrade (B) and aged-install (D) phases from `gh release list`,
# excluding drafts and the tag under test.
#
# WHAT IS UNDER TEST. release.yml creates every release as a draft; only
# acceptance PASS publishes it. acceptance.yml used to derive prev-tag from the
# sorted git tag list alone, which includes drafts that never passed
# acceptance — so upgrade/rollback/aged-install could pick an unaccepted
# predecessor. acceptance-prev-tag.sh fixes that by reading isDraft from gh and
# filtering on it.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
#   Case 3 plants a draft newer than the latest published release and requires
#   the derivation to skip it — proving the isDraft filter actually fires
#   before any other case's silence (no drafts present) is trusted.
#
# covers: spira/acceptance-prev-tag.sh .github/workflows/acceptance.yml
# host-reason: fake gh script in an isolated PATH dir; no container or database dependency
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SCRIPT="$HERE/acceptance-prev-tag.sh"
. "$HERE/testlib.sh"

echo "test-acceptance-prev-tag.sh"

# --- PROPERTY 1: self-check --------------------------------------------------
if [ ! -x "$SCRIPT" ]; then
    bad "self-check: acceptance-prev-tag.sh must exist and be executable" "missing"
    tl_summary; exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
BIN="$TMP/bin"
CALL_LOG="$TMP/calls.log"
mkdir -p "$BIN"

# Fake gh: "release list" prints $GH_RELEASE_LIST (JSON); records its args.
cat > "$BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
printf 'gh %s\n' "$*" >> "${CALL_LOG:-/dev/null}"
[ "${GH_EXIT:-0}" = "0" ] || exit "${GH_EXIT}"
if [ "${1:-}" = release ] && [ "${2:-}" = list ]; then
    printf '%s\n' "${GH_RELEASE_LIST:-[]}"
    exit 0
fi
exit 1
GHEOF
chmod +x "$BIN/gh"

run_derive() {
    # run_derive <env-assignments...> -- <tag-under-test> [extra-args...]
    local -a envs=()
    while [ "$1" != "--" ]; do envs+=("$1"); shift; done
    shift
    rm -f "$CALL_LOG"
    env "${envs[@]}" PATH="$BIN:$PATH" CALL_LOG="$CALL_LOG" \
        bash "$SCRIPT" "$@"
}

# --- PROPERTY 2: multiple published tags — picks the newest, excludes self --
_list='[
  {"tagName":"spira-release-spira-20260901T000000Z","isDraft":false},
  {"tagName":"spira-release-spira-20260910T000000Z","isDraft":false},
  {"tagName":"spira-release-spira-20260920T000000Z","isDraft":false}
]'
_out="$(run_derive "GH_RELEASE_LIST=$_list" -- "spira-release-spira-20260920T000000Z")"
is "newest published excluding tag-under-test" \
    "spira-release-spira-20260910T000000Z" "$_out"

# --- PROPERTY 3 (POSITIVE CONTROL): draft newer than latest published is
# never chosen as prev, even though it sorts after the published tag. -------
_list='[
  {"tagName":"spira-release-spira-20260901T000000Z","isDraft":false},
  {"tagName":"spira-release-spira-20260924T025221Z","isDraft":false},
  {"tagName":"spira-release-spira-20260925T010000Z","isDraft":true},
  {"tagName":"spira-release-spira-20260925T020000Z","isDraft":true}
]'
_out="$(run_derive "GH_RELEASE_LIST=$_list" -- "spira-release-spira-20260925T030000Z")"
is "draft newer than latest published is never chosen as prev" \
    "spira-release-spira-20260924T025221Z" "$_out"

# --- PROPERTY 4: only drafts exist — prints nothing, does not error --------
_list='[
  {"tagName":"spira-release-spira-20260925T010000Z","isDraft":true},
  {"tagName":"spira-release-spira-20260925T020000Z","isDraft":true}
]'
_rc=0
_out="$(run_derive "GH_RELEASE_LIST=$_list" -- "spira-release-spira-20260925T030000Z")" || _rc=$?
is "only-drafts: no prev-tag printed" "" "$_out"
is "only-drafts: exits 0" "0" "$_rc"

# --- PROPERTY 5: tag under test itself is excluded even when published -----
_list='[
  {"tagName":"spira-release-spira-20260901T000000Z","isDraft":false},
  {"tagName":"spira-release-spira-20260925T030000Z","isDraft":false}
]'
_out="$(run_derive "GH_RELEASE_LIST=$_list" -- "spira-release-spira-20260925T030000Z")"
is "tag under test excluded from its own candidate list" \
    "spira-release-spira-20260901T000000Z" "$_out"

# --- PROPERTY 6: gh unreachable — prints nothing, exits 0, never crashes ---
_rc=0
_out="$(run_derive "GH_EXIT=1" -- "spira-release-spira-20260925T030000Z")" || _rc=$?
is "gh unreachable: no prev-tag printed" "" "$_out"
is "gh unreachable: exits 0 (safe default, not a hard failure)" "0" "$_rc"

# --- PROPERTY 7: --repo is forwarded to gh ----------------------------------
_list='[{"tagName":"spira-release-spira-20260901T000000Z","isDraft":false}]'
run_derive "GH_RELEASE_LIST=$_list" -- "spira-release-spira-20260925T030000Z" \
    --repo "acme/spira" >/dev/null
_call="$(cat "$CALL_LOG" 2>/dev/null || true)"
case "$_call" in
    *"--repo acme/spira"*) ok "--repo forwarded to gh release list" ;;
    *) bad "--repo forwarded to gh release list" "call log: $_call" ;;
esac

tl_summary
