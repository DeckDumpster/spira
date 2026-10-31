# sl-fixture.sh — the fixture shared by test-submitted-lands*.sh: a throwaway bd store and
# lifecycle database behind the tree's own `spira-lc serve`, a push-mode origin and clone, a
# fixture home with stubs, the model stand-in that commits and finishes with `work submit`,
# and run_dwarf/landing/hel. Sourced after testlib.sh, with HERE and SL_SUITE set.
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require "$SL_SUITE"
TMP="$(mktemp -d)"
SERVE_PID=""
trap '[ -n "$SERVE_PID" ] && kill "$SERVE_PID" >/dev/null 2>&1; lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up submittedlands || { echo "$SL_SUITE: could not build a fixture database"; exit 1; }
lcfix_up || { echo "$SL_SUITE: could not build a lifecycle fixture"; exit 1; }
LC_SOCK="$TMP/lc.sock"
tl_config SPIRA_LC_SOCKET="$LC_SOCK"
spira-lc serve "$LC_SOCK" > "$TMP/serve.log" 2>&1 &
SERVE_PID=$!
lc_content_shim "$TMP/lcshim"; export PATH="$TMP/lcshim:$PATH"
for _ in $(seq 1 50); do [ -S "$LC_SOCK" ] && break; sleep 0.1; done
[ -S "$LC_SOCK" ] || bail "spira-lc serve never opened its socket: $(cat "$TMP/serve.log")"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; timeout 5 git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; timeout 5 git -C "$REPO" push -q origin main 2>/dev/null
timeout 5 git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main 2>/dev/null || true

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/runes"
tl_config SPIRA_RUNES_DIR="$SPIRA_HOME/runes"
# conf.d IS COPIED IN (matching test-dwarf-sweep.sh, test-dwarf-world-stop.sh, ...): dwarf's
# own in-process config registry (spira_config::resolve, dwarf::conf::merge_resolved_config)
# derives conf.d from THIS --home and now REFUSES to start if it is missing (sp-1cdgq) --
# a --home with no conf.d used to resolve silently to nothing instead of refusing.
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
cp -r "$HERE/conf.toml.d" "$SPIRA_HOME/"
find "$HERE" -maxdepth 1 \( -name '*.sh' -o -name '*.py' \) ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
SPIRA_RUN="$TMP/run"; export SPIRA_RUN; mkdir -p "$SPIRA_RUN/worktree"; tl_config SPIRA_RUN="$SPIRA_RUN"
FIXTURE_MAP="$TMP/repomap"; tl_config SPIRA_REPO_MAP="$FIXTURE_MAP"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$FIXTURE_MAP"

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SPIRA_HOME/$1"; chmod +x "$SPIRA_HOME/$1"; }
stub confine.sh 'exit 0'
stub mail    'exit 0'
stub gh         'exit 1'
# THE GATE STUB RECORDS ITS VERDICT AS THE REAL GATE DOES: gate/src/engine.rs ends every
# judged run with `spira-lc certify <SPIRA_GATE_BEAD> <branch tip> pass|red <detail> gate`
# — GatePass moves a SUBMITTED row to CERTIFIED, GateRed to REWORK. The landing pass then
# re-reads the row it is about to land. $TMP/gate-state keeps the state the row read right
# after the verdict, for the sections that assert what the gate itself did.
gate_stub() {   # gate_stub pass|red
    local verdict=PASS rc=0 detail=stub
    [ "$1" = red ] && { verdict=FAIL; rc=1; detail=stub-fail; }
    stub gate.sh "tip=\"\$(git -C '$REPO' rev-parse \"\$1\" 2>/dev/null)\"
[ -n \"\${SPIRA_GATE_BEAD:-}\" ] && [ -n \"\$tip\" ] && spira-lc certify \"\$SPIRA_GATE_BEAD\" \"\$tip\" $1 $detail gate >/dev/null 2>&1
spira-lc show \"\${SPIRA_GATE_BEAD:-}\" 2>/dev/null | python3 -c 'import sys,json; print(json.load(sys.stdin)[\"bead\"][\"state\"])' > '$TMP/gate-state' 2>/dev/null
echo \"gate: VERDICT=$verdict reason=$detail branch=\$1 repo=\${2:-?}\" >&2; exit $rc"
}
gate_pass() { gate_stub pass; }
gate_fail() { gate_stub red; }
gate_pass
# The fixture home (and its stubs) is the harness in force: bare names resolve here first.
export PATH="$SPIRA_HOME:$PATH"

cat > "$SPIRA_HOME/runes/builder.rune" <<RUNE
RUNE_NAME=builder
RUNE_WORKING_EXITS="SUBMITTED ask park hold"
RUNE_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
RUNE_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=600
RUNE
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/runes/builder.md"

# THE SHIM IS THE SESSION: commit, then finish with `work submit`, the builder's {{FINISH}}
# brief and the only exit a model has (sp-v62vn: the dwarf always runs it restricted — no
# bd, no SPIRA_DB, no TMP; PATH is model-bin/ and the system dirs). It used to `bd close`
# and rely on the dwarf converting that close into open + spira-submitted; there is no such
# conversion any more. conf.sh replaces PATH, so the model is injected through SPIRA_AGENT,
# and $TMP is baked in because the restricted environment does not carry it. The fixture
# ids are sp-sl1..sp-sl4, not sp-sl-1: `work` refuses a binding that is not sp-<alnum>.
BIN="$TMP/bin"; mkdir -p "$BIN"; tl_config SPIRA_AGENT="$BIN/claude"
command -v dwarf >/dev/null 2>&1 \
    || { echo "$SL_SUITE: dwarf is not on PATH" >&2; exit 1; }
cat > "$BIN/claude" <<SHIM
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="\$(sed -n 's/^work \\(sp-[a-z0-9-]*\\) .*/\\1/p' "$TMP/prompt" | head -1)"
printf '%s\\n' "\$id" >> "\$id.txt"
git add -A && git -c user.email=a@a -c user.name=dwarf commit -qm "\$id: the work"
work submit > "$TMP/submit.out" 2>&1
printf '%s' "\$?" > "$TMP/submit.rc"
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\\n'
exit 0
SHIM
chmod +x "$BIN/claude"

B() { bd -C "$SPIRA_DB" "$@"; } # batch-job: STORE_DOOR_CONSIDERED="a fixture call against the suite's own throwaway store"
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
v=d[0].get(sys.argv[1])
print(",".join(v) if isinstance(v,list) else (v or ""))' "$2" 2>/dev/null; }
seed() {   # seed <id> [lifecycle-state] [tip] — the bd content row plus its lifecycle row
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
    lcfix_seed "$1" "${2:-READY}" "${3:-}" || bail "could not seed $1's lifecycle row"
}
# A submit that did not apply says why, as TAP comments: the broker's answer, then the dwarf's.
run_dwarf() {
    rm -rf "$SPIRA_RUN/worktree" "$TMP/submit.rc" "$TMP/submit.out"
    dwarf --home "$SPIRA_HOME" builder > "$TMP/dwarf.out" 2>&1
    [ "$(cat "$TMP/submit.rc" 2>/dev/null)" = 0 ] && return 0
    { cat "$TMP/submit.out" 2>/dev/null; tail -n 15 "$TMP/dwarf.out"; } | sed 's/^/# /'
}
tl_config SPIRA_BATCHER_ENABLE=1 SPIRA_HOME_REPO=fixture SPIRA_ID_PREFIX=sp SPIRA_GH="$SPIRA_HOME/gh"
landing() {
    rm -f "$SPIRA_RUN/landing.progress" "$TMP/gate-state"
    SPIRA_REPO="$REPO" \
        landing-pass land 2>&1
}
hel() {
    SPIRA_REPO="$REPO" \
        command hel --all 2>&1
}
on_base() { timeout 5 git -C "$REPO" fetch -q origin 2>/dev/null; git -C "$REPO" log --format=%s origin/main 2>/dev/null; }
branch_tip() { git -C "$REPO" rev-parse "spira/$1" 2>/dev/null; }
