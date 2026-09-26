#!/usr/bin/env bash
#
# test-persona-model.sh — persona_model: one resolver for a persona's launch model.
#
# sp-z134z: changing which model a persona runs on required a change to main — aeon.sh read
# FAYTH_MODEL straight out of the sourced fayth, and concierge.sh read it through fayth_get,
# so an operator's only way to change a persona's model was a PR. sp-zs04v.4 deletes that
# file-scraping and replaces it with ONE resolver, persona_model (lib.sh), which reads
# persona.<fayth>.model out of spira.toml through spira-config — the operator's own override
# surface — falling back to a built-in default when spira.toml has no entry.
#
# 1. T0: the file-scraping this resolver replaces is gone — aeon_claude_argv (both of
#    aeon.sh's launch paths run through it) and concierge.sh's MODEL resolution no longer
#    read FAYTH_MODEL, and the two models that used to be bare literals (the liveness judge,
#    reflect.sh's inference tier) are config keys now.
# 2. persona_model's own fallback ladder, unit-tested directly: spira.toml override, missing
#    entry -> built-in default, caller-supplied default, no spira.toml -> built-in, no
#    spira-config binary -> built-in.
# 3. capacity_probe defaults to the builder's own resolved model, not a bare literal.
# 4. A stubbed claude captures argv, proving persona.builder.model reaches both of aeon.sh's
#    launch paths — and that spira.toml wins even when the fayth still declares a (now
#    unused) FAYTH_MODEL of its own.
#
# defect: sp-zs04v.4
# covers: spira/lib.sh spira/aeon.sh concierge.sh spira/reflect.sh spira/conf.sh spira/chamber/*.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
HARNESS="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-persona-model.sh"

# ==========================================================================
echo
echo "T0 — the file-scraping this resolver replaces is gone:"
# ==========================================================================
# POSITIVE CONTROL FIRST: prove the grep patterns below actually match the shape they are
# meant to catch, against a deliberately reintroduced offender, before trusting their
# silence on the real files (law-absence-needs-a-positive-control).
PC='--model "${FAYTH_MODEL:-claude-opus-5}"'
case "$PC" in *'FAYTH_MODEL:-'*) ok "PC: the FAYTH_MODEL-fallback pattern matches its own offender" ;;
              *) bad "PC: the FAYTH_MODEL-fallback pattern matches its own offender" "did not match" ;; esac

if grep -q 'FAYTH_MODEL' "$HARNESS/spira/lib.sh"; then
    bad "lib.sh no longer reads FAYTH_MODEL" "still present"
else
    ok "lib.sh no longer reads FAYTH_MODEL"
fi
want "aeon_claude_argv calls persona_model" \
     'persona_model "$FAYTH"' "$(cat "$HARNESS/spira/lib.sh")"

if grep -q 'FAYTH_MODEL' "$HARNESS/concierge.sh"; then
    bad "concierge.sh no longer reads FAYTH_MODEL" "still present"
else
    ok "concierge.sh no longer reads FAYTH_MODEL"
fi
want "concierge.sh's MODEL resolution calls persona_model" \
     'MODEL="$(persona_model "$FAYTH")"' "$(cat "$HARNESS/concierge.sh")"

# THE TWO HARD-CODED MODELS. Both must now be config keys, not bare literals in the
# claude invocation itself.
if grep -q -- '--model claude-haiku-4-5-20251001' "$HARNESS/spira/lib.sh"; then
    bad "the liveness judge no longer hardcodes its model" "still a bare literal"
else
    ok "the liveness judge no longer hardcodes its model"
fi
want "the liveness judge reads SPIRA_LIVENESS_MODEL" \
     'SPIRA_LIVENESS_MODEL:-claude-haiku-4-5-20251001' "$(cat "$HARNESS/spira/lib.sh")"

if grep -q -- '--model claude-opus-5' "$HARNESS/spira/reflect.sh"; then
    bad "reflect.sh no longer hardcodes its model" "still a bare literal"
else
    ok "reflect.sh no longer hardcodes its model"
fi
want "reflect.sh reads SPIRA_REFLECT_MODEL" \
     'SPIRA_REFLECT_MODEL:-claude-opus-5' "$(cat "$HARNESS/spira/reflect.sh")"

# THE CAPACITY PROBE DEFAULTS TO THE BUILDER'S MODEL, not its own bare literal.
if grep -q -- '--model "\${SPIRA_CAPACITY_PROBE_MODEL:-claude-sonnet-4-6}"' "$HARNESS/spira/lib.sh"; then
    bad "capacity_probe no longer hardcodes a fallback model" "still the old literal fallback"
else
    ok "capacity_probe no longer hardcodes a fallback model"
fi
want "capacity_probe falls back to persona_model builder" \
     'SPIRA_CAPACITY_PROBE_MODEL:-$(persona_model builder)' "$(cat "$HARNESS/spira/lib.sh")"

# ==========================================================================
echo
echo "resolving/building spira-config:"
# ==========================================================================
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-persona-model: cargo not found — spira-config binary cannot be built"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 77
fi
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
CRATE="$HARNESS/spira-config"
SPIRA_CONFIG_BIN="$HARNESS/target/release/spira-config"
if [ ! -x "$SPIRA_CONFIG_BIN" ]; then
    printf '  (building spira-config into %s)\n' "$T/target"
    CARGO_TARGET_DIR="$T/target" "$CARGO_BIN" build --release \
        --manifest-path "$CRATE/Cargo.toml" >/dev/null 2>&1
    SPIRA_CONFIG_BIN="$T/target/release/spira-config"
fi
if [ -x "$SPIRA_CONFIG_BIN" ]; then
    ok "spira-config binary is present and executable"
else
    bad "spira-config binary" "not found/built at $SPIRA_CONFIG_BIN"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

# ==========================================================================
echo
echo "persona_model — the resolver's own fallback ladder:"
# ==========================================================================
TOML="$T/spira.toml"
cat > "$TOML" <<'EOF'
[persona.builder]
model = "toml-override-model"
EOF

# EXPLICIT, MINIMAL ENVIRONMENT: env -i plus a fixture $HOME, so a real spira.conf/
# spira.toml/repo-map/chamber on the host this suite happens to run on can never leak in and
# answer for the fixture (law-gates-run-only-in-a-clean-environment). SPIRA_TOML steers
# spira_toml_file(); SPIRA_CHAMBER points at an empty directory so spira_toml_resolve's
# fayth-auto-convert never fires and overwrites the fixture's own [persona.builder] entry.
FX_HOME="$T/fixture-home"; mkdir -p "$FX_HOME"
mkdir -p "$T/empty-chamber"
resolve() {  # resolve <fayth> [default] [toml] [config-bin]
    env -i PATH="$PATH" HOME="$FX_HOME" SPIRA_HOME="$HARNESS/spira" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$T/run-resolve" \
        SPIRA_REPO_MAP=/nonexistent SPIRA_CHAMBER="$T/empty-chamber" \
        SPIRA_TOML="${3-$TOML}" SPIRA_CONFIG_BIN="${4-$SPIRA_CONFIG_BIN}" \
        bash -c '. "$1"/lib.sh >/dev/null 2>&1; persona_model "$2" "${3-}"' \
        _ "$HARNESS/spira" "$1" "${2:-}"
}

is "spira.toml entry wins"                 "toml-override-model" "$(resolve builder)"
is "no entry for this persona -> built-in" "claude-opus-5"        "$(resolve groomer)"
is "no entry, caller default -> caller's"  "caller-default"       "$(resolve groomer caller-default)"
is "no spira.toml at all -> built-in"      "claude-opus-5"        "$(resolve builder '' /nonexistent/spira.toml)"
is "no spira-config binary -> built-in"    "claude-opus-5"        "$(resolve builder '' "$TOML" /nonexistent/spira-config)"

# ==========================================================================
echo
echo "capacity_probe — defaults to persona.builder.model when unset:"
# ==========================================================================
BIN="$T/bin"; mkdir -p "$BIN"
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$SHIM_ARGV_OUT"
printf 'ok'
exit 0
SHIM
chmod +x "$BIN/claude"

_probe_argv="$T/probe-argv"
rm -f "$_probe_argv"
env -i PATH="$PATH" HOME="$FX_HOME" SPIRA_HOME="$HARNESS/spira" \
    SPIRA_CONF=/nonexistent SPIRA_RUN="$T/run-probe" \
    SPIRA_REPO_MAP=/nonexistent SPIRA_CHAMBER="$T/empty-chamber" \
    SPIRA_TOML="$TOML" SPIRA_CONFIG_BIN="$SPIRA_CONFIG_BIN" \
    SPIRA_AGENT="$BIN/claude" SHIM_ARGV_OUT="$_probe_argv" \
    bash -c '. "$1/lib.sh" >/dev/null 2>&1; unset SPIRA_CAPACITY_PROBE_MODEL; capacity_probe' \
    _ "$HARNESS/spira" >/dev/null 2>&1
want "capacity_probe's --model came from persona.builder.model" \
     "toml-override-model" "$(cat "$_probe_argv" 2>/dev/null || true)"

# ==========================================================================
echo
echo "aeon.sh launch paths — a stubbed claude captures argv:"
# ==========================================================================
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
       GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$T/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$T/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"; git -C "$REPO" add f; git -C "$REPO" commit -qm seed
git -C "$REPO" push -q origin main 2>/dev/null

# A FRESH $SPIRA_HOME so the real chamber's builder.fayth is never touched, and a fixture
# builder.fayth still declares its OWN (now unused) FAYTH_MODEL — proving spira.toml wins
# over a fayth declaration rather than merely over an absent one.
SH="$T/home"; mkdir -p "$SH/chamber"
cp "$HARNESS/spira/lib.sh" "$HARNESS/spira/conf.sh" "$HARNESS/spira/aeon.sh" \
   "$HARNESS/spira/suite-covers.sh" "$SH/" 2>/dev/null
cp -r "$HARNESS/spira/actors" "$SH/" 2>/dev/null || true
cat > "$SH/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS="test-persona-model-bead"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH_MODEL=fayth-declared-model-should-not-be-used
FAYTH
printf 'work {{BEAD_ID}} on {{BRANCH}}\n{{PARK}}\n' > "$SH/chamber/builder.md"

# [repo.fixture] straight into $TOML, since this fixture's aeon.sh needs a repo to claim
# a bead against and repo_field/repo_root are not this bead's surface to change.
cat >> "$TOML" <<EOF

[repo.fixture]
path = "$REPO"
mode = "push"
EOF
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$T/repo-map"

# SPIRA_CHAMBER POINTS AWAY FROM ANYTHING REAL, DELIBERATELY. spira_toml_resolve's
# auto-convert watches it for a fayth newer than the cached $TOML (freshly written, seconds
# ago) and would otherwise silently regenerate $TOML from it, overwriting the very
# persona.builder.model entry this section asserts on. aeon.sh sources
# $SPIRA_HOME/chamber/$FAYTH.fayth directly for its other fields regardless of SPIRA_CHAMBER,
# so the fixture fayth above still applies.
export SPIRA_HOME="$SH" SPIRA_RUN="$T/run" SPIRA_REPO_MAP="$T/repo-map" SPIRA_CONF=/nonexistent \
       SPIRA_CHAMBER="$T/empty-chamber" SPIRA_TOML="$TOML" SPIRA_CONFIG_BIN="$SPIRA_CONFIG_BIN" T
mkdir -p "$SPIRA_RUN"

cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$T/claude-argv"
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}\n'
printf '{"type":"result","subtype":"success","duration_ms":1,"turns":1,"num_turns":1,"total_cost_usd":0}\n'
exit 0
SHIM
chmod +x "$BIN/claude"
export SPIRA_AGENT="$BIN/claude"

aeon() { bash "$SH/aeon.sh" "$@" 2>/dev/null; }

# --sweep: no bead needed.
rm -f "$T/claude-argv"
aeon builder --sweep --prompt "persona-model sweep probe"
want "sweep launch path's --model came from persona.builder.model" \
     "toml-override-model" "$(cat "$T/claude-argv" 2>/dev/null || true)"
nowant "sweep launch path did not use the fayth's own FAYTH_MODEL" \
     "fayth-declared-model-should-not-be-used" "$(cat "$T/claude-argv" 2>/dev/null || true)"

# non-sweep: claims a real bead through the bd/testdb fixture.
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-persona-model
trap 'testdb_drop; rm -rf "$T"' EXIT INT TERM
testdb_up personamodel || { echo "test-persona-model: could not build fixture database"; exit 1; }

BID="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" create "persona-model bead launch" --type task \
    -l "test-persona-model-bead,repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
[ -n "$BID" ] || { printf 'test-persona-model: could not create bead\n' >&2; exit 1; }

rm -f "$T/claude-argv"
aeon builder
want "bead-claim launch path's --model came from persona.builder.model" \
     "toml-override-model" "$(cat "$T/claude-argv" 2>/dev/null || true)"
nowant "bead-claim launch path did not use the fayth's own FAYTH_MODEL" \
     "fayth-declared-model-should-not-be-used" "$(cat "$T/claude-argv" 2>/dev/null || true)"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
