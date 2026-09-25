#!/usr/bin/env bash
#
# test-fayth-project-instructions.sh — FAYTH_PROJECT_INSTRUCTIONS=none gets
#   --setting-sources user (aeon_claude_argv, lib.sh — the one function both the sweep and
#   the bead call site build their argv through); archivist gets --add-dir for brain's
#   CLAUDE.md.
#
#   ./test-fayth-project-instructions.sh
#
# WHAT IS TESTED
# --------------
# 1. aeon_claude_argv with FAYTH_PROJECT_INSTRUCTIONS=none produces --setting-sources user;
#    repo (or unset) does not. See test-aeon-prompt-layers.sh for the rest of
#    aeon_claude_argv's table (this file keeps the mechanism's own name in its history).
# 2. Each production fayth declares the right value: ops/maechen/groomer/czar → none,
#    builder/spike → repo.
# 3. archivist.sh passes --add-dir <wiki> when SPIRA_WIKI is set.
#    Positive control: when SPIRA_WIKI is unset, --add-dir is absent.
#
# The stub agent writes its argv to a file and exits 0. conf.sh replaces $PATH, so a
# PATH shim would reach the real model — SPIRA_AGENT is the only safe injection point.
#
# defect: sp-1f56o
# covers: spira/aeon.sh spira/lib.sh spira/archivist.sh spira/chamber/ops.fayth
#   spira/chamber/maechen.fayth spira/chamber/groomer.fayth spira/chamber/czar.fayth
#   spira/chamber/builder.fayth spira/chamber/spike.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"

BIN="$TMP/bin"; mkdir -p "$BIN"
export SPIRA_AGENT="$BIN/claude" TMP
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$TMP/claude-argv"
cat /dev/stdin > /dev/null 2>&1
printf '{"type":"result","subtype":"success","duration_ms":1,"turns":1,"num_turns":1,"total_cost_usd":0}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

echo "test-fayth-project-instructions.sh"

# ==========================================================================================
echo
echo "T1: aeon_claude_argv — FAYTH_PROJECT_INSTRUCTIONS=none gets --setting-sources user"
# ==========================================================================================
# The full argv table (model, tools, --settings gating on aeon_settings) lives in
# test-aeon-prompt-layers.sh; this row is kept here under its original defect id.
ARGV_HOME="$TMP/argv-home"; mkdir -p "$ARGV_HOME/hooks"
argv_for_pi() {   # argv_for_pi <FAYTH_PROJECT_INSTRUCTIONS>
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_HOME="$ARGV_HOME" SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        ${1:+FAYTH_PROJECT_INSTRUCTIONS="$1"} \
        bash -c '. "$1"/lib.sh; aeon_claude_argv --append-system-prompt-file /dev/null' \
        _ "$HERE" 2>/dev/null
}

argv_none="$(argv_for_pi none)"
want "none fayth: --setting-sources in argv"  "--setting-sources" "$argv_none"
want "none fayth: sources value is user"      "$(printf -- '--setting-sources\nuser')" "$argv_none"

argv_repo="$(argv_for_pi repo)"
nowant "repo fayth: --setting-sources absent" "--setting-sources" "$argv_repo"

# ==========================================================================================
echo
echo "production fayths declare the correct FAYTH_PROJECT_INSTRUCTIONS:"
# ==========================================================================================
for f in ops maechen groomer czar; do
    val="$(grep '^FAYTH_PROJECT_INSTRUCTIONS=' "$HERE/chamber/$f.fayth" 2>/dev/null \
          | cut -d= -f2 | tr -d '"' | head -1)"
    is "$f.fayth declares none" "none" "$val"
done
for f in builder spike; do
    val="$(grep '^FAYTH_PROJECT_INSTRUCTIONS=' "$HERE/chamber/$f.fayth" 2>/dev/null \
          | cut -d= -f2 | tr -d '"' | head -1)"
    is "$f.fayth declares repo" "repo" "$val"
done

# ==========================================================================================
echo
echo "POSITIVE CONTROL — archivist adds --add-dir when SPIRA_WIKI is set:"
# ==========================================================================================
# Build a minimal transcript with enough data for ctx-meter.sh to parse.
PROJ_DIR="$TMP/projects/testproj"
mkdir -p "$PROJ_DIR"
for i in $(seq 1 60); do
    printf '{"type":"assistant","message":{"id":"m-%d","usage":{"input_tokens":1000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":10}}}\n' \
        "$i" >> "$PROJ_DIR/test-session.jsonl"
done

WIKI_DIR="$TMP/wiki"
mkdir -p "$WIKI_DIR"

rm -f "$TMP/claude-argv"
env -i HOME="$TMP/home" PATH="$PATH" \
    SPIRA_CONF="$TMP/no-such.conf" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_TOKEN_PROJECTS="$TMP/projects" \
    SPIRA_CTX_WARN=200000 SPIRA_CTX_HIGH=400000 SPIRA_CTX_LIMIT=1000000 \
    SPIRA_ARCHIVIST_TIMEOUT=10 \
    SPIRA_CHAMBER="$HERE/chamber" \
    SPIRA_AGENT="$BIN/claude" \
    SPIRA_WIKI="$WIKI_DIR" \
    TMP="$TMP" \
    bash "$HERE/archivist.sh" now "$PROJ_DIR/test-session.jsonl" 2>/dev/null || true
argv_arc_wiki="$(cat "$TMP/claude-argv" 2>/dev/null || true)"

want "archivist: --add-dir in argv when SPIRA_WIKI set" "--add-dir"  "$argv_arc_wiki"
want "archivist: wiki path in argv"                     "$WIKI_DIR"  "$argv_arc_wiki"

# ==========================================================================================
echo
echo "archivist WITHOUT SPIRA_WIKI does NOT add --add-dir:"
# ==========================================================================================
rm -f "$TMP/claude-argv"
env -i HOME="$TMP/home" PATH="$PATH" \
    SPIRA_CONF="$TMP/no-such.conf" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_TOKEN_PROJECTS="$TMP/projects" \
    SPIRA_CTX_WARN=200000 SPIRA_CTX_HIGH=400000 SPIRA_CTX_LIMIT=1000000 \
    SPIRA_ARCHIVIST_TIMEOUT=10 \
    SPIRA_CHAMBER="$HERE/chamber" \
    SPIRA_AGENT="$BIN/claude" \
    TMP="$TMP" \
    bash "$HERE/archivist.sh" now "$PROJ_DIR/test-session.jsonl" 2>/dev/null || true
argv_arc_nowiki="$(cat "$TMP/claude-argv" 2>/dev/null || true)"

nowant "archivist: no --add-dir when SPIRA_WIKI unset" "--add-dir" "$argv_arc_nowiki"

tl_summary
