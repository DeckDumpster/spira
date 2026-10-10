#!/usr/bin/env bash
# build-fence.sh — a compile is a static check of the tree, not a suite.
#
#   build-fence.sh
#
# WHY THIS EXISTS (sp-9uro3). sp-upkae certified green under fences-only certification
# (SPIRA_GATE_SUITES=off) and then broke the whole batch's build job: its new dependency
# resolved a crate the pinned toolchain could not parse, and since there is one workspace
# Cargo.lock, every crate's build failed with it. Fences-only certification runs no suite at
# all, so nothing on that path ever invoked cargo, and the queue had to bisect the batch to
# find the offending branch.
#
# WHAT TRIGGERS A BUILD. Any changed path matching Cargo.toml, Cargo.lock, a crate's src/,
# rust-toolchain(.toml), or the Makefile itself. Anything else — a script, a doc, a bead
# label — is skipped, so a script-only branch pays nothing. The list comes from
# SPIRA_GATE_FILES (gate.sh's pre-computed STATUS<tab>FILE or bare FILE list, the same shape
# the selector and orphan-test.sh already read) or, failing that, a diff against SPIRA_GATE_BASE.
#
# WITH NEITHER SOURCE AT ALL, THIS REFUSES (exit 2, sp-ufbkh). It used to skip with exit 0,
# which the gate could not tell from a checked tree.
#
# AN EMPTY-LOOKING DIFF IS TWO DIFFERENT THINGS (sp-pg3c6). A no-code close (an empty
# acknowledgement commit — ops incidents, most of them) has a genuinely empty diff: its tree
# equals the merge-base's, by tree-id equality, and git proves that cheaply without ever
# computing a line-by-line diff. That branch has nothing to build and nothing to refuse —
# refusing it meant no no-code close could ever land, and worse, the base re-run of this same
# fence diffs the base against ITSELF, which is just as empty, so the base trial refused too
# and the gate blamed local/main (BASE_FAIL reason=base-red) for a branch that never touched
# a file. A diff this fence cannot even attempt to verify — no base to compare against, or
# the base does not resolve — is the other thing, and that one still refuses, fail closed:
# this fence does not get to guess that "no evidence" means "nothing changed."
#
# So: zero changed-files is verified empty (PASS, with a line saying so) only when the
# branch's tree is provably identical to the merge-base's tree; otherwise it is unresolved
# (REFUSE). On success, built or not, it prints `fence: build-fence checked <n> <unit>` to
# stderr, which the gate requires, with `n` the changed-file count for an ordinary diff or
# `1 tree` for a verified-empty one — either way `n > 0`, so a genuine skip is never mistaken
# for a silent fence (gate/src/fence.rs, DESIGN.md "Every fence proves it checked").
#
# It runs `cargo check --workspace --locked` — no codegen, no link, yet it resolves the same
# dependencies and lockfile a release build does; the round VM's release build is the real
# proof. Check artifacts go to one shared warm target dir (CARGO_TARGET_DIR, else
# SPIRA_FENCE_TARGET_DIR, else a cache dir), not each worktree's own.
#
# It fails CLOSED: cargo runs under the pinned toolchain, and a non-zero exit is a RED
# certification naming the command's own output, not a suite failure.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
cd "$HERE/.." 2>/dev/null || { printf 'build-fence: cannot reach the tree holding %s\n' "$0" >&2; exit 1; }

# touches_build_surface reads STATUS<tab>FILE lines (git diff --name-status, gate.sh's own
# shape) or bare FILE lines from stdin and returns 0 the moment one matches.
touches_build_surface() {
    local line f
    while IFS= read -r line || [ -n "$line" ]; do
        [ -n "$line" ] || continue
        case "$line" in *"	"*) f="${line#*	}" ;; *) f="$line" ;; esac
        case "$f" in
            Cargo.toml|Cargo.lock|Makefile|rust-toolchain|rust-toolchain.toml) return 0 ;;
            */Cargo.toml|*/Cargo.lock|*/rust-toolchain|*/rust-toolchain.toml) return 0 ;;
            */src/*) return 0 ;;
        esac
    done
    return 1
}

# verified_empty_diff: true only when SPIRA_GATE_BASE resolves, a merge-base with HEAD
# resolves, and that merge-base's tree id equals HEAD's tree id — proof the diff is empty,
# never an assumption from an empty line count alone (sp-pg3c6).
verified_empty_diff() {
    [ -n "${SPIRA_GATE_BASE:-}" ] || return 1
    local mb mb_tree head_tree
    mb="$(git merge-base "$SPIRA_GATE_BASE" HEAD 2>/dev/null)" || return 1
    [ -n "$mb" ] || return 1
    mb_tree="$(git rev-parse "${mb}^{tree}" 2>/dev/null)" || return 1
    head_tree="$(git rev-parse "HEAD^{tree}" 2>/dev/null)" || return 1
    [ -n "$mb_tree" ] && [ -n "$head_tree" ] && [ "$mb_tree" = "$head_tree" ]
}

changed=""
if [ -f "${SPIRA_GATE_FILES:-}" ]; then
    changed="$(cat "$SPIRA_GATE_FILES")"
elif [ -n "${SPIRA_GATE_BASE:-}" ]; then
    if ! changed="$(git diff --name-status "$SPIRA_GATE_BASE...HEAD" 2>&1)"; then
        printf 'build-fence: cannot diff %s...HEAD — refusing\n%s\n' "$SPIRA_GATE_BASE" "$changed" >&2
        exit 2
    fi
else
    printf 'build-fence: no SPIRA_GATE_FILES or SPIRA_GATE_BASE — no diff to check; refusing\n' >&2
    exit 2
fi
n="$(printf '%s\n' "$changed" | grep -c .)"
if [ "$n" -eq 0 ]; then
    if verified_empty_diff; then
        printf 'build-fence: branch tree == merge-base tree (verified by tree-id, not merely an empty line count) — nothing to build; passing\n' >&2
        printf 'fence: build-fence checked 1 tree\n' >&2
        exit 0
    fi
    if [ -n "${SPIRA_GATE_BASE:-}" ]; then
        printf 'build-fence: the diff names no file and the tree-id check against %s could not confirm it empty — refusing\n' \
            "$SPIRA_GATE_BASE" >&2
    else
        printf 'build-fence: the diff names no file and there is no SPIRA_GATE_BASE to verify it empty against — refusing\n' >&2
    fi
    exit 2
fi

if ! printf '%s\n' "$changed" | touches_build_surface; then
    printf 'build-fence: no build-surface file in the diff — skipped\n' >&2
    printf 'fence: build-fence checked %d changed-files\n' "$n" >&2
    exit 0
fi

CARGO_BIN="${CARGO:-cargo}"
command -v "$CARGO_BIN" >/dev/null 2>&1 || { printf 'build-fence: %s not found on PATH — refusing\n' "$CARGO_BIN" >&2; exit 2; }
# The default is this worktree's own: one directory shared by every worktree let cargo reuse another
# tree's lifecycle artifact (its freshness check is relative paths and mtimes), so the fence
# compiled spira-lc against the wrong crate and false-redded sp-mbefnb.1, sp-9sbdvw and sp-g3w50i.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${SPIRA_FENCE_TARGET_DIR:-$PWD/target/fence-check}}"
mkdir -p "$CARGO_TARGET_DIR" 2>/dev/null || { printf 'build-fence: cannot create target dir %s — refusing\n' "$CARGO_TARGET_DIR" >&2; exit 2; }

out="$("$CARGO_BIN" check --workspace --locked 2>&1)"; rc=$?
if [ "$rc" -ne 0 ]; then
    printf 'build-fence: cargo check FAILED (rc=%s) — RED certification, not a suite failure\n' "$rc" >&2
    printf '%s\n' "$out" >&2
    exit 1
fi
printf 'build-fence: cargo check ok\n' >&2
printf 'fence: build-fence checked %d changed-files\n' "$n" >&2
exit 0
