#!/usr/bin/env bash
#
# test-dwarf-resume-base.sh — a dwarf cuts a fresh branch from the freshly fetched origin/main, not
#   from a stale local main.
#
# SPLIT FROM test-dwarf-resume.sh for the 30 s cap (each real dwarf launch costs about 9 s).
# defect: sp-2e4v
# tier: T2
# covers: dwarf/src/* landing-pass/src/* spira/lib.sh UC-dwarf-execution-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=120

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-dwarf-resume-base.sh"

# ===========================================================================================
echo
echo "T3: the real wiring — a slain wip commit produces both briefs in a real prompt"
# ===========================================================================================
# testdb-mode: default (embedded).
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-dwarf-resume-base
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up dwarfresumebase || { echo "test-dwarf-resume-base: could not build a fixture database"; exit 1; }

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; timeout 5 git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; timeout 5 git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/runes"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"; tl_config SPIRA_RUN="$SPIRA_RUN"
DWARF_MAP="$TMP/repomap"; tl_config SPIRA_REPO_MAP="$DWARF_MAP"
# SPIRA_RUNES_DIR no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at this suite's own fixture runes explicitly.
tl_config SPIRA_RUNES_DIR="$SPIRA_HOME/runes"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$DWARF_MAP"
cat > "$SPIRA_HOME/runes/builder.rune" <<RUNE
RUNE_NAME=builder
RUNE_WORKING_EXITS="SUBMITTED ask park hold"
RUNE_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
RUNE_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=600
RUNE
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/runes/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export TMP
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib dwarf_fixture_agent.
# It only `export`s SPIRA_AGENT (testlib.sh, not edited here) — a registered key, so the dwarf
# binary also needs it declared through tl_config, read back from what dwarf_fixture_agent set.
dwarf_fixture_agent "$BIN/claude"
tl_config SPIRA_AGENT="$SPIRA_AGENT"
# The lifecycle machine (testlib lc_dwarf_mirror): since sp-v62vn the dwarf's ready set is
# `spira-lc list` and its claim a Claim event; the stand-in tells the fixture's bd story in
# lifecycle terms, ahead of the tree's spira-lc on PATH.
lc_dwarf_mirror "$TMP/lc"; export PATH="$TMP/lc:$PATH"
command -v dwarf >/dev/null 2>&1 \
    || { echo "test-dwarf-resume-base: dwarf is not on PATH — refusing to run the real model" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'prior work\n' >> f
git add -A && git -c user.email=a@a -c user.name=dwarf commit -qm "$id — the work"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"%s","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "${2:-open}" "$_lbl" | testdb_seed
}
run_dwarf() { rm -rf "$SPIRA_RUN/worktree"; dwarf --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1; }

echo "T3: stale local base — the branch is cut from fresh origin/main, not stale local main"
# ===========================================================================================
# The sentinel lands from a dedicated worktree and never fast-forwards the shared checkout,
# so origin/main advances without updating local main. dwarf.sh must fetch from the remote
# before cutting the branch, so the new branch starts at the FRESH origin/main.
#
# The discriminator: git merge-base(branch, origin/main).
#   If cut from fresh origin/main (B):  merge-base = B  (origin/main is an ancestor)
#   If cut from stale local main (A):   merge-base = A  (diverged before origin/main)
# defect: sp-stale-base
testdb_reset; seed sp-ar-stalebase
SECOND="$TMP/second"; timeout 5 git clone -q "$ORIGIN" "$SECOND" 2>/dev/null
git -C "$SECOND" config user.email t@t; git -C "$SECOND" config user.name t
printf 'sentinel-landed\n' >> "$SECOND/g"; git -C "$SECOND" add g
git -C "$SECOND" commit -qm "sentinel: landed something — origin/main advances"
timeout 5 git -C "$SECOND" push -q origin main 2>/dev/null
local_main_sha="$(git -C "$REPO" rev-parse main)"  # stale — does not include the push above
run_dwarf
fresh_origin="$(git -C "$REPO" rev-parse origin/main)"   # updated by the fetch inside dwarf.sh
branch_base="$(git -C "$REPO" merge-base "spira/sp-ar-stalebase" origin/main 2>/dev/null)"
nowant "setup: local main must differ from origin/main for this test to mean anything" \
    "$fresh_origin" "$local_main_sha"
is "stale-local-base: branch is cut from fresh origin/main, not stale local main" \
    "$fresh_origin" "$branch_base"

tl_summary
