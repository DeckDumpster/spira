#!/usr/bin/env bash
#
# test-bead-contract.sh — bead.sh contract: the live PERSONAS/KINDS/REPOS listing
# (G14, sp-9ce60.2.9). Exercised elsewhere only incidentally, through test-concierge.sh's
# check that the concierge brief names "bead.sh file" — nothing calls `bead.sh contract`
# itself and asserts its output.
#
# `_bead_contract` (spira/bead.sh) asks three live sources and prints each verbatim:
# fayth_names + FAYTH_LABELS for PERSONAS, schema.sh kinds for KINDS, SPIRA_REPO_MAP for
# REPOS. This suite pins a fixture chamber and repo-map (non-default names) so a pass here
# is not merely "matches the box's own default output" (per CLAUDE.md's config-fixture rule).
#
# covers: spira/bead.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/chamber"

# ---------------------------------------------------------------------------
# FIXTURE CHAMBER. Two personas: one with labels, one deliberately label-less
# (the "(no labels)" fallback in _bead_contract).
# ---------------------------------------------------------------------------
cat > "$T/chamber/alpha.fayth" <<'FAYTH'
FAYTH_LABELS="testscope,alpha-work"
FAYTH_TOOLS="Bash"
FAYTH_MODEL=claude-sonnet-5
FAYTH_MAX_CONCURRENT=1
FAYTH
cat > "$T/chamber/beta.fayth" <<'FAYTH'
FAYTH_LABELS=""
FAYTH_TOOLS="Bash"
FAYTH_MODEL=claude-sonnet-5
FAYTH_MAX_CONCURRENT=1
FAYTH

# ---------------------------------------------------------------------------
# FIXTURE REPO-MAP. Non-default names, a comment line and a blank line that
# must not surface as a repo.
# ---------------------------------------------------------------------------
cat > "$T/repo-map" <<'MAP'
# comment row, must not appear as a repo
custrepo-one | /tmp/one | push | origin/main | | true | plan

custrepo-two | /tmp/two | push | origin/main | | true | plan
MAP

run_contract() {
    env -i HOME="$T" PATH="/usr/bin:/bin" \
        SPIRA_CONF="$T/none.conf" \
        SPIRA_HOME="$T" \
        SPIRA_REPO_MAP="${1-$T/repo-map}" \
        bash "$HERE/bead.sh" contract 2>&1
}

echo "test-bead-contract.sh"

# ===========================================================================================
echo
echo "T1: PERSONAS/KINDS/REPOS, pinned to a non-default fixture"
# ===========================================================================================
out="$(run_contract)"; rc=$?
is "contract: exits 0" "0" "$rc"

# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the section headers
# themselves, before asserting anything about their contents.
want "contract: has PERSONAS header" "PERSONAS" "$out"
want "contract: has KINDS header"    "KINDS"    "$out"
want "contract: has REPOS header"    "REPOS"    "$out"

want "contract: alpha listed with its labels" \
     "$(printf '  %-14s %s' "alpha" "testscope,alpha-work")" "$out"
want "contract: beta listed as label-less" \
     "$(printf '  %-14s %s' "beta" "(no labels)")" "$out"

# KINDS is asked of the live schema, never hand-copied here — a suite that hardcoded the
# vocabulary would go stale the moment schema.sh's declaration changed under it.
kinds_expected="$(bash "$HERE/schema.sh" kinds)"
kinds_ok=1
while IFS= read -r k; do
    [ -n "$k" ] || continue
    case "$out" in *"$k"*) ;; *) kinds_ok=0 ;; esac
done <<< "$kinds_expected"
is "contract: every schema.sh kind appears" "1" "$kinds_ok"

want   "contract: custrepo-one listed"        "  custrepo-one" "$out"
want   "contract: custrepo-two listed"        "  custrepo-two" "$out"
nowant "contract: comment row not a repo"     "comment row"    "$out"

# ===========================================================================================
echo
echo "T1: REPOS falls back to '(no repo-map)' when SPIRA_REPO_MAP points nowhere"
# ===========================================================================================
out="$(run_contract "$T/no-such-repo-map")"; rc=$?
is   "no repo-map: exits 0"                 "0"              "$rc"
want "no repo-map: REPOS reports absence"   "(no repo-map)"  "$out"

tl_summary
