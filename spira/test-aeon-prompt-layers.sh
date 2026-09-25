#!/usr/bin/env bash
#
# test-aeon-prompt-layers.sh — aeon_claude_argv (lib.sh) builds the claude CLI argv from the
#   fayth's own knobs: FAYTH_SYSTEM_PROMPT picks --system-prompt-file (replace) or
#   --append-system-prompt-file (append/default, via system_prompt_split's SPIRA_SYSTEM_FLAG);
#   FAYTH_PROJECT_INSTRUCTIONS=none adds --setting-sources user; --settings is added only
#   when aeon_settings() has a hook to wire in. system.md carries the statutes; task.md
#   carries the bead body and no statutes.
#
# defect: sp-d0rnp
# covers: spira/aeon.sh spira/lib.sh spira/chamber/*.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-prompt-layers.sh"

# ===========================================================================================
echo
echo "T1: system_prompt_split + aeon_claude_argv — no aeon run, no bd"
# ===========================================================================================
FAYTH_HOME="$TMP/argv-home"; mkdir -p "$FAYTH_HOME/hooks"

argv_for() {   # argv_for <FAYTH_SYSTEM_PROMPT> <FAYTH_PROJECT_INSTRUCTIONS> [model] [tools]
    local sp="$1" pi="$2" model="${3:-}" tools="${4:-}"
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_HOME="$FAYTH_HOME" SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        ${sp:+FAYTH_SYSTEM_PROMPT="$sp"} ${pi:+FAYTH_PROJECT_INSTRUCTIONS="$pi"} \
        ${model:+FAYTH_MODEL="$model"} ${tools:+FAYTH_TOOLS="$tools"} \
        bash -c '. "$1"/lib.sh
sysfile="$2/sys.md"; taskfile="$2/task.md"
system_prompt_split "$sysfile" "$taskfile" "statutes" "prompt <!-- task --> body"
aeon_claude_argv "$SPIRA_SYSTEM_FLAG" "$sysfile"' _ "$HERE" "$TMP" 2>/dev/null
}

out="$(argv_for replace '')"
want   "replace: --system-prompt-file in argv"          "--system-prompt-file" "$out"
nowant "replace: no --append-system-prompt-file"         "--append-system-prompt-file" "$out"
nowant "replace, no project-instructions knob: no --setting-sources" "--setting-sources" "$out"

out="$(argv_for append '')"
want   "append: --append-system-prompt-file in argv"     "--append-system-prompt-file" "$out"
nowant "append: no --system-prompt-file"                 "--system-prompt-file" "$out"

out="$(argv_for '' '')"
want   "no-knob: behaves as append (default)"             "--append-system-prompt-file" "$out"
nowant "no-knob: no --system-prompt-file"                  "--system-prompt-file" "$out"

out="$(argv_for replace none)"
want "FAYTH_PROJECT_INSTRUCTIONS=none: --setting-sources user, in EITHER system-prompt mode" \
     "$(printf -- '--setting-sources\nuser')" "$out"
out="$(argv_for append none)"
want "same in append mode too — the knob is independent of the system-prompt flag" \
     "$(printf -- '--setting-sources\nuser')" "$out"

out="$(argv_for replace repo)"
nowant "FAYTH_PROJECT_INSTRUCTIONS=repo: no --setting-sources" "--setting-sources" "$out"

out="$(argv_for replace '')"
want "the static flags are always present: --dangerously-skip-permissions" "--dangerously-skip-permissions" "$out"
want "and --system-prompt-snapshot on"                                      "$(printf -- '--system-prompt-snapshot\non')" "$out"
want "the default model"                                                    "claude-opus-5" "$out"
want "the default tool allowlist"                                           "Bash,Read,Edit,Write,Glob,Grep" "$out"

out="$(argv_for replace '' claude-sonnet-5 'Bash,Read')"
want   "FAYTH_MODEL overrides the default"       "claude-sonnet-5" "$out"
nowant "and the default model is not also present" "claude-opus-5" "$out"
want   "FAYTH_TOOLS overrides the default"       "$(printf -- '--allowedTools\nBash,Read')" "$out"

# --settings is present only when aeon_settings() (lib.sh) finds a hook to wire in — proved
# both ways, or the absence rows above would mean nothing (law-absence-needs-a-positive-control).
out="$(argv_for replace '')"
nowant "no hooks installed: no --settings" "--settings" "$out"
cat > "$FAYTH_HOME/hooks/aeon-fence.sh" <<'HOOK'
#!/usr/bin/env bash
exit 0
HOOK
chmod +x "$FAYTH_HOME/hooks/aeon-fence.sh"
out="$(argv_for replace '')"
want "aeon-fence.sh installed: --settings appears" "--settings" "$out"
want "and names the fence hook"                    "aeon-fence.sh" "$out"
rm -f "$FAYTH_HOME/hooks/aeon-fence.sh"

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
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/suite-covers.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { printf 'test-aeon-prompt-layers: aeon.sh has no SPIRA_AGENT injection point\n' >&2; exit 1; }

BIN="$TMP/bin"; mkdir -p "$BIN"
export SPIRA_AGENT="$BIN/claude" TMP

# The stub records its argv and stdin, then emits a minimal success event.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$TMP/claude-argv"
cat /dev/stdin        > "$TMP/claude-stdin"
printf '{"type":"result","subtype":"success","duration_ms":1,"turns":1,"num_turns":1,"total_cost_usd":0}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

aeon() { bash "$SPIRA_HOME/aeon.sh" "$@" 2>/dev/null; }

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

argv_n="$(cat "$TMP/claude-argv" 2>/dev/null)"
want "no-knob: argv has --append-system-prompt-file (default)" "--append-system-prompt-file" "$argv_n"
nowant "no-knob: argv has no --system-prompt-file"             "--system-prompt-file"        "$argv_n"

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
GROOMER_LABEL="$(. "$HERE/conf.sh" 2>/dev/null; printf '%s' "${SPIRA_GROOMER_LABEL:-groomer}")"
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
    want "groomer task.md: has finishing"       "close the trigger bead"        "$task_g"
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
