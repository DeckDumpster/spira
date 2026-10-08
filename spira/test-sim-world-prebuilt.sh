#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/src/world.rs spira/sim/src/main.rs
#
# test-sim-world-prebuilt.sh — `sim world up` inside testenv runs a prebuilt release
# (SPIRA_SIM_RELEASE) and never builds one (sp-o4s4t4).
#
# Before sp-o4s4t4, world up did `git archive` plus a cold `cargo build --release --workspace
# --bins` of the tree: alone it outlasted the suite cap, and it was an uncapped build on the
# shared host. This suite runs the real `sim` binary of the staged release, nested inside a
# testenv run, with a `cargo` on PATH that records any call and fails.
#
#   1. CONTROL: SPIRA_SIM_RELEASE unset inside testenv — up refuses, names the variable,
#      leaves no world and calls no cargo. Without this the pass in 2 could be a build that
#      happened to be fast.
#   2. SPIRA_SIM_RELEASE=$SPIRA_RELEASE — up succeeds with no cargo: the
#      world's release is that directory, its Dolt fixture is a `testenv testdb up` nested in
#      this testenv run (under testenv's tmpfs TESTDB_ROOT), and the lifecycle migrations
#      applied through the release's spira-lc. Then down removes the world and its fixture.
#
# The world refuses production locators (SPIRA_RUN, SPIRA_DB, SPIRA_LC_*), and testenv sets
# SPIRA_RUN for every suite, so each call unsets them: the world is its own run directory.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

[ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set: run via testenv --with-bins"
SIM="$SPIRA_RELEASE/bin/sim"
[ -x "$SIM" ] || bail "no sim in the staged release: $SIM"
command -v testenv >/dev/null || bail "testenv is not on PATH"

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

# A cargo that must never run.
mkdir -p "$T/stub"
printf '#!/bin/sh\necho "cargo $*" >> %q\nexit 97\n' "$T/cargo.log" > "$T/stub/cargo"
chmod +x "$T/stub/cargo"
: > "$T/cargo.log"

# The tree a world seeds from: only the lifecycle schema the world applies.
REPO="$T/repo"
mkdir -p "$REPO/lifecycle"
cp "$HERE/../lifecycle/schema.sql" "$REPO/lifecycle/"
cp -r "$HERE/../lifecycle/migrations" "$REPO/lifecycle/"
git -C "$REPO" init -q
git -C "$REPO" add -A
git -C "$REPO" -c user.name=t -c user.email=t@t.invalid commit -q -m seed || bail "fixture repo commit failed"

sim_world() {  # sim_world <args...> — sim with no production locator, cargo stubbed
    (cd "$REPO" && env -u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET \
        PATH="$T/stub:$PATH" "$SIM" world "$@")
}

# --- 1. CONTROL: unset SPIRA_SIM_RELEASE inside testenv is refused --------------------------
W0="$T/world0"
out="$(unset SPIRA_SIM_RELEASE; SPIRA_IN_TESTENV=1 sim_world up "$W0" 2>&1)"; rc=$?
wantrc "control: up without SPIRA_SIM_RELEASE inside testenv fails" 1 "$rc"
want   "control: the refusal names SPIRA_SIM_RELEASE" "SPIRA_SIM_RELEASE is not set" "$out"
[ ! -e "$W0" ] && ok "control: the refusal left no world" || bad "control: the refusal left no world" "$W0 exists"
is     "control: no cargo was called" "" "$(cat "$T/cargo.log")"

# --- 2. a prebuilt release: up then down, no cargo ------------------------------
W="$T/world"
out="$(SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 sim_world up "$W" 2>&1)"; rc=$?
wantrc "up with SPIRA_SIM_RELEASE succeeds" 0 "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out" | sed 's/^/# up: /'
is     "up called no cargo" "" "$(cat "$T/cargo.log")"
is     "the world's release is the prebuilt one" "$(readlink -f "$SPIRA_RELEASE")" "$(readlink -f "$W/release")"
[ ! -e "$W/cache" ] && ok "no build cache was made" || bad "no build cache was made" "$W/cache exists"
fx="$(cat "$W/db.fixture" 2>/dev/null)"
[ -n "$fx" ] && [ -s "$fx/server.port" ] && ok "the world's Dolt fixture is up (nested testenv testdb up)" \
    || bad "the world's Dolt fixture is up (nested testenv testdb up)" "fixture [$fx]"
if [ -n "${TESTDB_ROOT:-}" ]; then
    want "the fixture lives under testenv's TESTDB_ROOT" "$TESTDB_ROOT/" "$fx"
fi
[ -s "$W/config/sim.toml" ] && ok "the world's config was written by the release's spira-config" \
    || bad "the world's config was written by the release's spira-config" "no $W/config/sim.toml"

out="$(sim_world down "$W" 2>&1)"; rc=$?
wantrc "down succeeds" 0 "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out" | sed 's/^/# down: /'
[ ! -e "$W" ] && ok "down removed the world" || bad "down removed the world" "$W exists"
[ -n "$fx" ] && [ ! -e "$fx" ] && ok "down removed the Dolt fixture" || bad "down removed the Dolt fixture" "$fx exists"
[ -x "$SIM" ] && ok "down left the prebuilt release alone" || bad "down left the prebuilt release alone" "$SIM gone"

tl_summary
