#!/usr/bin/env bash
#
# test-aeon-prompt-layers.sh — the claude CLI argv built from the fayth's own knobs:
#   FAYTH_SYSTEM_PROMPT picks --system-prompt-file (replace) or --append-system-prompt-file
#   (append/default); FAYTH_PROJECT_INSTRUCTIONS=none adds --setting-sources user; --settings
#   is added only when there is a hook to wire in. system.md carries the statutes; task.md
#   carries the bead body and no statutes.
#
# RETIRED (sp-j89pd, wave 4.2): aeon_claude_argv, system_prompt_split and aeon_settings
# (lib.sh) had zero live callers — the whole table is now built in aeon/src/run.rs. Its
# direct-call T1 table is deleted with them; T3 below proves the same table end to end
# through the real `aeon` binary and real chamber fayths.
#
# defect: sp-d0rnp
# tier: T2
# covers: aeon/src/* spira/chamber/*.fayth spira/lib.sh UC-aeon-execution-06
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-prompt-layers.sh"

# ===========================================================================================
echo
echo "T3: real fayth files wire the same mechanism end to end"
# ===========================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-prompt-layers
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonlayers || { echo "test-aeon-prompt-layers: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
       GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed
git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
tl_config SPIRA_RUN="$SPIRA_RUN"
SPIRA_REPO_MAP="$TMP/repo-map"
tl_config SPIRA_REPO_MAP="$SPIRA_REPO_MAP"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

command -v aeon >/dev/null 2>&1 \
    || { printf 'test-aeon-prompt-layers: aeon is not on PATH\n' >&2; exit 1; }

BIN="$TMP/bin"; mkdir -p "$BIN"
# The lifecycle machine (testlib lc_aeon_mirror): since sp-v62vn the aeon's ready set is
# `spira-lc list` and its claim a Claim event; the stand-in tells the fixture's bd story in
# lifecycle terms, ahead of the tree's spira-lc on PATH.
lc_aeon_mirror "$TMP/lc"; export PATH="$TMP/lc:$PATH"
tl_config SPIRA_AGENT="$BIN/claude"
export TMP
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib aeon_fixture_agent.
aeon_fixture_agent "$BIN/claude"

# The stub records its argv and stdin, then emits a minimal success event.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$TMP/claude-argv"
cat /dev/stdin        > "$TMP/claude-stdin"
printf '{"type":"result","subtype":"success","duration_ms":1,"turns":1,"num_turns":1,"total_cost_usd":0}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

aeon() { command aeon --home "$SPIRA_HOME" "$@" 2>/dev/null; }

# Shared label for all test fayths.
T_LABEL="test-layers-bead"

# ---- helpers to build fayths and beads ------------------------------------------------
make_fayth() {          # make_fayth <name> [FAYTH_SYSTEM_PROMPT=<val>]
    local name="$1" sp_line="${2:-}"
    cat > "$SPIRA_HOME/chamber/$name.fayth" <<FAYTH
FAYTH_NAME=$name
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}$T_LABEL"
FAYTH_EXCLUDE_LABELS="spira-poison,\$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
${sp_line}
FAYTH
    # A minimal chamber .md with the <!-- task --> marker so the split exercises the path.
    printf 'You are test persona %s.\n\nStanding rule: never guess.\n\n<!-- task -->\n\n## The bead\n{{BEAD}}\n\n## Finishing\nClose {{BEAD_ID}}.\n' \
        "$name" > "$SPIRA_HOME/chamber/$name.md"
}

make_bead() {           # make_bead -> prints bead id
    local _labels="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}$T_LABEL,repo:fixture"
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" create "layers test bead" --type task \
        -l "$_labels" 2>/dev/null \
        | grep -oE 'sp-[a-z0-9]+' | head -1
}

# ==========================================================================================
echo
echo "replace fayth: --system-prompt-file in argv, bead body on stdin, no statute in stdin"
# ==========================================================================================
make_fayth testlayers-replace "FAYTH_SYSTEM_PROMPT=replace"
BID_R="$(make_bead)"
[ -n "$BID_R" ] || { printf 'test-aeon-prompt-layers: could not create replace bead\n' >&2; exit 1; }

aeon testlayers-replace

argv_r="$(cat "$TMP/claude-argv" 2>/dev/null)"
stdin_r="$(cat "$TMP/claude-stdin" 2>/dev/null)"
sys_r="$(cat "$SPIRA_RUN/$BID_R.system.md" 2>/dev/null)"

want "replace: argv has --system-prompt-file"     "--system-prompt-file"    "$argv_r"
nowant "replace: argv has no --append-system-prompt-file" "--append-system-prompt-file" "$argv_r"
want "replace: stdin contains the bead id"        "$BID_R"                  "$stdin_r"
nowant "replace: stdin has no 'Memories in force'" "Memories in force"      "$stdin_r"
want "replace: system.md has 'Memories in force'" "Memories in force"       "$sys_r"
want "replace: system.md has standing rule"       "never guess"             "$sys_r"
nowant "replace: task.md has no statute text"     "Memories in force"       "$(cat "$SPIRA_RUN/$BID_R.task.md" 2>/dev/null)"

# ==========================================================================================
echo
echo "system.md and task.md are written beside the session log:"
# ==========================================================================================
want "replace: system.md exists beside log" "1" "$([ -f "$SPIRA_RUN/$BID_R.system.md" ] && printf 1 || printf 0)"
want "replace: task.md exists beside log"   "1" "$([ -f "$SPIRA_RUN/$BID_R.task.md"   ] && printf 1 || printf 0)"

# ==========================================================================================
echo
echo "groomer system.md has the five operations; task.md has the finishing contract:"
# ==========================================================================================
# Use a real groomer fayth to verify the content split meets the acceptance criteria.
# Run through the real chamber file (not the test stub), using a groomer bead.
# HOME/SPIRA_CONF/SPIRA_TOML pinned to the fixture: sourcing conf.sh unguarded picks up
# whatever spira.conf/spira.toml the ambient HOME happens to have, and the auto-convert
# path WRITES there (sp-zs04v.2 — a suite that can reach ~/.config/spira is a production
# write, not a test).
GROOMER_LABEL="$(env -i PATH="$PATH" HOME="$TMP" SPIRA_HOME="$SPIRA_HOME" \
    SPIRA_CONF="$TMP/no.conf" SPIRA_TOML="$TMP/no.toml" \
    bash -c '. "$SPIRA_HOME/conf.sh" 2>/dev/null; printf "%s" "${SPIRA_GROOMER_LABEL:-groomer}"')"
BID_G="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" create "groomer layers test" --type task \
    -l "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}$GROOMER_LABEL,repo:fixture" 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+' | head -1)"
if [ -n "$BID_G" ]; then
    cp "$HERE/chamber/groomer.fayth" "$SPIRA_HOME/chamber/groomer.fayth"
    cp "$HERE/chamber/groomer.md"    "$SPIRA_HOME/chamber/groomer.md"
    aeon groomer
    sys_g="$(cat "$SPIRA_RUN/$BID_G.system.md" 2>/dev/null)"
    task_g="$(cat "$SPIRA_RUN/$BID_G.task.md" 2>/dev/null)"
    want "groomer system.md: has operations"    "Four operations, one refusal"  "$sys_g"
    want "groomer system.md: has refusal text"  "MUST NOT do"                   "$sys_g"
    nowant "groomer system.md: no Memories in task" "Memories in force"         "$task_g"
    want "groomer task.md: has finishing"       "finish the trigger bead"       "$task_g"
else
    printf '  skip  groomer bead creation failed (groomer partition not ready)\n'
fi

# ==========================================================================================
echo
echo "sp-4rzlw: a bead carrying an unresolved thrash streak leads its brief with the sticking point:"
# ==========================================================================================
# A DEDICATED LABEL, not $T_LABEL. Two beads created against the shared pool would leave
# selection order between them unspecified, and only ONE of the two assertions below needs a
# SPECIFIC bead claimed — the positive case. Isolating both onto their own label removes the
# ambiguity instead of relying on an undocumented "ready" ordering.
ST_LABEL="test-layers-sticking-bead"
make_fayth testlayers-sticking "FAYTH_SYSTEM_PROMPT=append"
# make_fayth writes $T_LABEL already expanded (the heredoc is unquoted), so the substitution
# below targets its literal value, not the variable name.
sed -i "s/$T_LABEL/$ST_LABEL/" "$SPIRA_HOME/chamber/testlayers-sticking.fayth"
make_sticking_bead() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" create "sticking-point test bead" --type task \
        -l "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}$ST_LABEL,repo:fixture" 2>/dev/null \
        | grep -oE 'sp-[a-z0-9]+' | head -1
}

BID_S="$(make_sticking_bead)"
[ -n "$BID_S" ] || { printf 'test-aeon-prompt-layers: could not create sticking-point bead\n' >&2; exit 1; }

# The tip aeon.sh's fresh worktree checks out is origin/main's own tip: this bead's branch has
# never been worked, so it is created from there — the same tip a real thrash requeue would
# have recorded metadata against on this bead's last (simulated) summon.
tip0="$(git -C "$REPO" rev-parse --short origin/main)"
bd -C "$SPIRA_DB" update "$BID_S" \
    --set-metadata "thrash_tip=$tip0" \
    --set-metadata "thrash_streak=2" \
    --set-metadata "thrash_last=stuck rerunning the full landing gate locally instead of testenv-batch.sh" \
    >/dev/null 2>&1

aeon testlayers-sticking

task_s="$(cat "$SPIRA_RUN/$BID_S.task.md" 2>/dev/null)"
want "task.md carries the STICKING POINT banner"   "STICKING POINT"                                              "$task_s"
want "task.md carries the recorded sticking point" "stuck rerunning the full landing gate locally"               "$task_s"

# "At the top" (the bead) — before "## The bead", not merely present somewhere below it.
sp_pos="$(grep -abo 'STICKING POINT' <<<"$task_s" | head -1 | cut -d: -f1)"
bead_pos="$(grep -abo '## The bead' <<<"$task_s" | head -1 | cut -d: -f1)"
if [ -n "$sp_pos" ] && [ -n "$bead_pos" ] && [ "$sp_pos" -lt "$bead_pos" ]; then
    ok "STICKING POINT appears before '## The bead', not buried in the notes below it"
else
    bad "STICKING POINT appears before '## The bead', not buried in the notes below it" \
        "sp_pos=$sp_pos bead_pos=$bead_pos"
fi

# NEGATIVE CONTROL, SAME LABEL, SAME FAYTH: a bead with no thrash metadata gets no banner at
# all — the positive assertions above prove the banner can appear; this proves it is
# conditional on the metadata, not unconditional boilerplate the template always renders.
BID_CLEAN="$(make_sticking_bead)"
[ -n "$BID_CLEAN" ] || { printf 'test-aeon-prompt-layers: could not create clean bead\n' >&2; exit 1; }
aeon testlayers-sticking
task_clean="$(cat "$SPIRA_RUN/$BID_CLEAN.task.md" 2>/dev/null)"
nowant "a bead with no thrash history gets no STICKING POINT banner" "STICKING POINT" "$task_clean"

tl_summary
