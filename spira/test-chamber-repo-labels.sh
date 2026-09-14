#!/usr/bin/env bash
#
# test-chamber-repo-labels.sh — every repo: literal in chamber briefs and open beads
# resolves to a checkout in the installer's repo-map.
#
# WHAT THIS SUITE IS FOR
# ----------------------
# A remedy bead cut by following a chamber brief inherits the repo: label verbatim. A
# hardcoded literal absent from the installer's repo-map parks every bead born from it
# under needs-operator — aeon.sh refuses to work a bead in the wrong repository rather
# than guess. The loop cannot break from inside: each retrospective pass files the same
# remedy bead, also parked. Two remedy beads accumulated this way on this install before
# db-mis landed (sp-maechen-remedy-born-parked).
#
# FILE INVARIANT: no bare repo: literal appears in spira/chamber/*.md or *.fayth. A bare
# literal is `repo:WORD` where WORD begins with a letter and contains only letters, digits,
# and hyphens — i.e., not a shell variable reference (${...}) and not a template
# placeholder (<...>). Any bare literal found is checked against repo_root; if it does not
# resolve the suite exits non-zero naming the file:line.
#
# BEAD INVARIANT: every repo: label on an open bead resolves via repo_root. An open bead
# with an unresolvable repo: label is permanently parked: no aeon will ever claim it.
#
# POSITIVE CONTROL IS FIRST. A check that silently finds nothing looks identical to one
# whose matcher never fires. A bare literal is planted in a scratch file; the matcher must
# name it before its silence over the real chamber tree is believed
# (law-absence-needs-a-positive-control).
#
# covers: spira/chamber/*.md spira/chamber/*.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-chamber-repo-labels.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# Load the harness functions so repo_root and SPIRA_DB are available.
# This check needs the REAL repo-map — it is a data-integrity assertion about what names
# a following-the-brief aeon would emit, and whether those names resolve on this install.
# Conf.sh is idempotent (SPIRA_CONF_LOADED guard): re-sourcing in a gate that already
# loaded it is a no-op with no side effects.
. "$HERE/conf.sh"
. "$HERE/lib.sh"

# find_bare_repo_labels <dir> — print file:line:match for every bare repo: literal.
# Bare means `repo:WORD` where WORD is [a-z][a-z0-9-]+, excluding shell variables
# (which start with ${) and template placeholders (which start with <).
find_bare_repo_labels() {
    grep -rnE 'repo:[a-z][a-z0-9-]+' "$1" \
        --include='*.md' --include='*.fayth' 2>/dev/null \
    | grep -v 'repo:<' \
    || true
}

# ==========================================================================
# POSITIVE CONTROL — the finder must fire before its silence means anything.
# ==========================================================================
PLANT_DIR="$TMP/chamber"
mkdir -p "$PLANT_DIR"
printf '    bd create -l "plan,repo:sentinel-test-literal" ...\n' \
    > "$PLANT_DIR/planted.md"

planted="$(find_bare_repo_labels "$PLANT_DIR")"
if [ -n "$planted" ]; then
    ok "positive control: bare repo: literal is detected"
else
    bad "positive control" "planted repo:sentinel-test-literal was not found — matcher broken"
fi
want "positive control names the literal" "repo:sentinel-test-literal" "$planted"
want "positive control names the file"    "planted.md"                 "$planted"

# ==========================================================================
# FILE CHECK — no bare repo: literals in the real chamber directory.
# For each literal found, also verify it resolves — an unresolvable name parks
# every bead the brief cuts.
# ==========================================================================
CHAMBER_DIR="$HERE/chamber"
file_out=""; file_fail=0
if [ -d "$CHAMBER_DIR" ]; then
    while IFS= read -r match; do
        [ -n "$match" ] || continue
        file_out="$file_out$match"$'\n'
        repo_name="$(printf '%s' "$match" | grep -oE 'repo:[a-z][a-z0-9-]+' | head -1 | cut -d: -f2)"
        file_loc="$(printf '%s' "$match" | cut -d: -f1-2)"
        if [ -z "$repo_name" ]; then
            bad "chamber file literal" "could not parse repo name from: $match"
            file_fail=$((file_fail+1))
        elif repo_root "$repo_name" >/dev/null 2>&1; then
            bad "chamber file $file_loc: repo:$repo_name" \
                "literal resolves but should be a shell variable — move to \${SPIRA_HOME_REPO}"
            file_fail=$((file_fail+1))
        else
            bad "chamber file $file_loc: repo:$repo_name" \
                "literal does not resolve in repo-map — every bead cut from this brief parks"
            file_fail=$((file_fail+1))
        fi
    done < <(find_bare_repo_labels "$CHAMBER_DIR")
    if [ "$file_fail" -eq 0 ]; then
        ok "no bare repo: literals in chamber files"
    fi
fi

# ==========================================================================
# BEAD CHECK — every open bead's repo: label must resolve.
# ==========================================================================

# Positive control: repo_root must reject an unknown name.
if repo_root "unresolvable-sentinel-xyz-test" >/dev/null 2>&1; then
    bad "positive control: repo_root must reject unknown repo" \
        "repo_root accepted a nonexistent name — check is untrustworthy"
else
    ok "positive control: repo_root rejects unknown repo names"
fi

if [ -z "${SPIRA_DB:-}" ]; then
    ok "bead check skipped: SPIRA_DB not set"
elif ! command -v "${SPIRA_BD:-bd}" >/dev/null 2>&1; then
    ok "bead check skipped: bd not in PATH"
else
    bead_fail=0
    while IFS=$'\t' read -r bead_id repo_name; do
        [ -n "$bead_id" ] && [ -n "$repo_name" ] || continue
        if repo_root "$repo_name" >/dev/null 2>&1; then
            ok "bead $bead_id: repo:$repo_name resolves"
        else
            bad "bead $bead_id: repo:$repo_name" \
                "does not resolve in repo-map — bead is permanently parked"
            bead_fail=$((bead_fail+1))
        fi
    done < <(
        "${SPIRA_BD:-bd}" -C "$SPIRA_DB" list \
            --status open --flat --limit 0 2>/dev/null \
        | while IFS= read -r line; do
            bead="$(printf '%s' "$line" | awk '{print $2}')"
            repo="$(printf '%s' "$line" \
                | grep -oE 'repo:[a-z][a-z0-9-]+' | head -1 | cut -d: -f2)"
            [ -n "$bead" ] && [ -n "$repo" ] \
                && printf '%s\t%s\n' "$bead" "$repo" || true
        done
    )
    if [ "$bead_fail" -eq 0 ]; then
        ok "all open beads have resolvable repo: labels"
    fi
fi

echo ""
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
