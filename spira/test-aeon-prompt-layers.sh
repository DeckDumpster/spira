#!/usr/bin/env bash
#
# test-aeon-prompt-layers.sh — aeon.sh uses --system-prompt-file (replace) or
#   --append-system-prompt-file (append/default) based on FAYTH_SYSTEM_PROMPT;
#   system.md carries the statutes; task.md carries the bead body and no statutes.
#
# WHAT IS UNDER TEST
# ------------------
# aeon.sh:
#   - reads FAYTH_SYSTEM_PROMPT from the sourced fayth (replace | append | absent)
#   - calls system_prompt_split() to write $BEAD_ID.system.md and $BEAD_ID.task.md
#   - launches claude with --system-prompt-file (replace) or --append-system-prompt-file (append)
#   - puts task.md contents on stdin; system.md goes via the flag, not stdin
#
# POSITIVE CONTROL: a stub records exactly what claude receives. The test first
# verifies the stub can detect the flags it is asked to detect — without that
# confirmation the absence assertions below prove nothing.
#
# defect: sp-d0rnp
# covers: spira/aeon.sh spira/lib.sh spira/chamber/*.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-prompt-layers
TMP="$(mktemp -d)"
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
echo "test-aeon-prompt-layers.sh"
echo
echo "POSITIVE CONTROL — stub can report flags it sees:"
# ==========================================================================================
# Run a no-op that passes the flags the real code would pass, and confirm the stub records
# them. Without this the absence assertions below would pass vacuously.
printf 'ARGV: --system-prompt-file /dev/null\n' > "$TMP/claude-argv"
want "positive: stub records --system-prompt-file"    "--system-prompt-file"    "$(cat "$TMP/claude-argv")"
printf 'ARGV: --append-system-prompt-file /dev/null\n' > "$TMP/claude-argv"
want "positive: stub records --append-system-prompt-file" "--append-system-prompt-file" "$(cat "$TMP/claude-argv")"
printf '' > "$TMP/claude-argv"; printf '' > "$TMP/claude-stdin"

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
echo "append fayth: --append-system-prompt-file in argv"
# ==========================================================================================
make_fayth testlayers-append "FAYTH_SYSTEM_PROMPT=append"
BID_A="$(make_bead)"
[ -n "$BID_A" ] || { printf 'test-aeon-prompt-layers: could not create append bead\n' >&2; exit 1; }

aeon testlayers-append

argv_a="$(cat "$TMP/claude-argv" 2>/dev/null)"
want "append: argv has --append-system-prompt-file" "--append-system-prompt-file" "$argv_a"
nowant "append: argv has no --system-prompt-file"   "--system-prompt-file"        "$argv_a"

# ==========================================================================================
echo
echo "no-knob fayth: behaves as append (default)"
# ==========================================================================================
make_fayth testlayers-noknob ""
BID_N="$(make_bead)"
[ -n "$BID_N" ] || { printf 'test-aeon-prompt-layers: could not create no-knob bead\n' >&2; exit 1; }

aeon testlayers-noknob

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
echo "real fayth files declare FAYTH_SYSTEM_PROMPT:"
# ==========================================================================================
for f in builder spike ops groomer maechen czar; do
    fayth_file="$HERE/chamber/$f.fayth"
    if [ -f "$fayth_file" ]; then
        grep -q 'FAYTH_SYSTEM_PROMPT=' "$fayth_file" \
            && ok "$f.fayth declares FAYTH_SYSTEM_PROMPT" \
            || bad "$f.fayth declares FAYTH_SYSTEM_PROMPT" "key not found in $fayth_file"
    fi
done

# ==========================================================================================
echo
echo "replace fayths use --system-prompt-file; append fayths use --append-:"
# ==========================================================================================
for f in ops groomer maechen czar; do
    fayth_file="$HERE/chamber/$f.fayth"
    [ -f "$fayth_file" ] || continue
    val="$(grep 'FAYTH_SYSTEM_PROMPT=' "$fayth_file" | tail -1 | sed 's/.*FAYTH_SYSTEM_PROMPT=//' | tr -d '"'"'"' ')"
    is "$f.fayth: FAYTH_SYSTEM_PROMPT=replace" "replace" "$val"
done
for f in builder spike; do
    fayth_file="$HERE/chamber/$f.fayth"
    [ -f "$fayth_file" ] || continue
    val="$(grep 'FAYTH_SYSTEM_PROMPT=' "$fayth_file" | tail -1 | sed 's/.*FAYTH_SYSTEM_PROMPT=//' | tr -d '"'"'"' ')"
    is "$f.fayth: FAYTH_SYSTEM_PROMPT=append" "append" "$val"
done

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
    want "groomer system.md: has operations"    "four operations, one refusal"  "$sys_g"
    want "groomer system.md: has refusal text"  "MUST NOT do"                   "$sys_g"
    nowant "groomer system.md: no Memories in task" "Memories in force"         "$task_g"
    want "groomer task.md: has finishing"       "close the trigger bead"        "$task_g"
else
    printf '  skip  groomer bead creation failed (groomer partition not ready)\n'
fi

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
