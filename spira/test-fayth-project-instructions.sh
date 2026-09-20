#!/usr/bin/env bash
#
# test-fayth-project-instructions.sh — FAYTH_PROJECT_INSTRUCTIONS=none gets
#   --setting-sources user; archivist gets --add-dir for brain's CLAUDE.md.
#
#   ./test-fayth-project-instructions.sh
#
# WHAT IS TESTED
# --------------
# 1. A fayth with FAYTH_PROJECT_INSTRUCTIONS=none produces a --setting-sources user
#    flag on the claude invocation (both sweep and bead modes share the mechanism;
#    sweep mode is tested as a proxy).
#    Positive control: a repo fayth's launch does NOT get --setting-sources.
# 2. Each production fayth declares the right value: ops/maechen/groomer/czar → none,
#    builder/spike → repo.
# 3. archivist.sh passes --add-dir <wiki> when SPIRA_WIKI is set.
#    Positive control: when SPIRA_WIKI is unset, --add-dir is absent.
#
# The stub agent writes its argv to a file and exits 0. conf.sh replaces $PATH, so a
# PATH shim would reach the real model — SPIRA_AGENT is the only safe injection point.
#
# defect: sp-1f56o
# covers: spira/aeon.sh spira/archivist.sh spira/chamber/ops.fayth
#   spira/chamber/maechen.fayth spira/chamber/groomer.fayth spira/chamber/czar.fayth
#   spira/chamber/builder.fayth spira/chamber/spike.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/suite-covers.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
# Empty scope label so any non-empty FAYTH_LABELS passes the fence (not a claim test).
export SPIRA_SCOPE_LABEL=""

grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { printf 'test-fayth-project-instructions: aeon.sh has no SPIRA_AGENT injection point\n' >&2; exit 1; }

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

aeon() { bash "$SPIRA_HOME/aeon.sh" "$@" 2>/dev/null; }

echo "test-fayth-project-instructions.sh"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — none fayth sweep gets --setting-sources user:"
# ==========================================================================================
# Run first: proves the stub can detect the flag before any absence assertion.
cat > "$SPIRA_HOME/chamber/testnone.fayth" <<'FAYTH'
FAYTH_NAME=testnone
FAYTH_LABELS=test-pi-none
FAYTH_EXCLUDE_LABELS=""
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH_PROJECT_INSTRUCTIONS=none
FAYTH
printf 'sweep prompt\n' > "$SPIRA_HOME/chamber/testnone.md"

rm -f "$TMP/claude-argv"
aeon testnone --sweep --prompt "check" || true
argv_none="$(cat "$TMP/claude-argv" 2>/dev/null || true)"

want "none fayth: --setting-sources in argv"  "--setting-sources" "$argv_none"
want "none fayth: sources value is user"      "user"              "$argv_none"

# ==========================================================================================
echo
echo "repo fayth sweep does NOT get --setting-sources:"
# ==========================================================================================
cat > "$SPIRA_HOME/chamber/testrepo.fayth" <<'FAYTH'
FAYTH_NAME=testrepo
FAYTH_LABELS=test-pi-repo
FAYTH_EXCLUDE_LABELS=""
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH_PROJECT_INSTRUCTIONS=repo
FAYTH
printf 'sweep prompt\n' > "$SPIRA_HOME/chamber/testrepo.md"

rm -f "$TMP/claude-argv"
aeon testrepo --sweep --prompt "check" || true
argv_repo="$(cat "$TMP/claude-argv" 2>/dev/null || true)"

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

# ==========================================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
