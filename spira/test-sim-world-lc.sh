#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/src/world.rs
#
# test-sim-world-lc.sh — a sim world runs its own `spira-lc serve` (sp-hq1v76).
#
# Before sp-hq1v76, world up set the lifecycle socket empty and started no lifecycle
# service, so `work submit` (the stub agent's call) and claims could not apply inside a
# world. Now up starts `spira-lc serve` against the world's own Dolt fixture on
# <world>/lc.sock, records it in config/sim.toml (spira.lc_socket) and config/sim.env, and
# waits until it answers; down stops it by the PID up recorded, after checking its
# /proc/<pid>/cmdline.
#
#   1. Inside a world: `spira-lc create-bead`, a Claim event, then `work submit` (which has
#      no same-user fallback: the socket or nothing) — the row reads SUBMITTED. After down,
#      no process with that world's serve command line survives.
#   2. CONTROL: a second world whose serve is stopped (by its recorded PID) before the
#      submit. The same create-bead + Claim, then `work submit` fails "cannot tell" naming the
#      socket, and the row, read over a direct connection to the world's fixture, is still
#      WORKING. Without this, the SUBMITTED in 1 could be some path that never needed serve.
#
# Each call runs with no production locator (testenv sets SPIRA_RUN for every suite), the
# way test-sim-world-prebuilt.sh does; in-world calls get only the world's config and
# sim.env, as the simulator's own drive does (spira/sim/src/drive.rs).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

[ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set: run via testenv --with-bins"
SIM="$SPIRA_RELEASE/bin/sim"
[ -x "$SIM" ] || bail "no sim in the staged release: $SIM"
command -v testenv >/dev/null || bail "testenv is not on PATH"

T="$(mktemp -d)"
WORLDS=()
cleanup() {
    local w
    for w in "${WORLDS[@]}"; do [ -e "$w" ] && sim_world down "$w" >/dev/null 2>&1; done
    rm -rf "$T"
}
trap cleanup EXIT

# The tree a world seeds from: only the lifecycle schema the world applies.
REPO="$T/repo"
mkdir -p "$REPO/lifecycle"
cp "$HERE/../lifecycle/schema.sql" "$REPO/lifecycle/"
cp -r "$HERE/../lifecycle/migrations" "$REPO/lifecycle/"
git -C "$REPO" init -q
git -C "$REPO" add -A
git -C "$REPO" -c user.name=t -c user.email=t@t.invalid commit -q -m seed || bail "fixture repo commit failed"

NOLOC=(-u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)

sim_world() {  # sim_world <args...> — sim with no production locator, a prebuilt release
    (cd "$REPO" && env "${NOLOC[@]}" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$SIM" world "$@")
}

in_world() {  # in_world <world> <cmd...> — as drive.rs runs a command: the world's config only
    local w="$1"; shift
    local -a kv=()
    local line
    while IFS= read -r line; do [ -n "$line" ] && kv+=("$line"); done < "$w/config/sim.env"
    (cd "$w/work" && env "${NOLOC[@]}" SPIRA_TOML="$w/config/sim.toml" "${kv[@]}" \
        PATH="$w/bin:$w/release/bin:$PATH" "$@")
}

serve_up() {  # serve_up <world> — the PID up recorded is this world's serve
    local w="$1" pid
    pid="$(cat "$w/lc-serve.pid" 2>/dev/null)"
    [ -n "$pid" ] && [ "$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)" = "$w/release/bin/spira-lc serve $w/lc.sock " ]
}

serves_of() {  # serves_of <world> — PIDs whose command line names this world's socket (assertion only)
    local f
    for f in /proc/[0-9]*/cmdline; do
        tr '\0' ' ' < "$f" 2>/dev/null | grep -qF -- "spira-lc serve $1/lc.sock" && basename "$(dirname "$f")"
    done
}

state_of() {  # state_of <world> <bead> — the row's state
    in_world "$1" spira-lc show "$2" 2>&1 | sed -n 's/.*"state": *"\([A-Z_]*\)".*/\1/p' | head -1
}

world_up() {  # world_up <dir> — up, reported
    local out rc
    out="$(sim_world up "$1" 2>&1)"; rc=$?
    WORLDS+=("$1")
    wantrc "up $(basename "$1") succeeds" 0 "$rc"
    [ "$rc" = 0 ] || printf '%s\n' "$out" | sed 's/^/# up: /'
    return "$rc"
}

claim() {  # claim <world> <bead> — create the row and claim it
    local out rc
    out="$(in_world "$1" spira-lc create-bead "$2" 2>&1)"; rc=$?
    wantrc "create-bead $2 in the world" 0 "$rc"; [ "$rc" = 0 ] || printf '# %s\n' "$out"
    out="$(in_world "$1" spira-lc event bead "$2" --expect READY --version 0 --actor sim \
        --kind '{"Claim":{"holder":"aeon-1","lease_until":4102444800}}' 2>&1)"; rc=$?
    wantrc "Claim $2 in the world" 0 "$rc"; [ "$rc" = 0 ] || printf '# %s\n' "$out"
    is "$2 is WORKING after the claim" "WORKING" "$(state_of "$1" "$2")"
}

# --- 1. a world's own serve: create, claim, work submit -> SUBMITTED; down stops it ------
W="$T/w1"
if world_up "$W"; then
    W="$(cd "$W" && pwd -P)"
    serve_up "$W" && ok "up recorded the PID of this world's spira-lc serve" \
        || bad "up recorded the PID of this world's spira-lc serve" "pid [$(cat "$W/lc-serve.pid" 2>/dev/null)]"
    [ -S "$W/lc.sock" ] && ok "the socket is under the world dir" || bad "the socket is under the world dir" "no $W/lc.sock"
    is   "sim.toml declares the world's socket" "$W/lc.sock" "$(env "${NOLOC[@]}" "$SPIRA_RELEASE/bin/spira-config" get spira.lc_socket "$W/config/sim.toml" 2>&1)"
    want "sim.env names the world's socket" "SPIRA_LC_SOCKET=$W/lc.sock" "$(cat "$W/config/sim.env")"
    PID="$(cat "$W/lc-serve.pid")"

    claim "$W" sp-simlc1
    out="$(in_world "$W" env SPIRA_WORK_BEAD_ID=sp-simlc1 work submit 2>&1)"; rc=$?
    wantrc "work submit applies through the world's serve" 0 "$rc"
    [ "$rc" = 0 ] || printf '# submit: %s\n' "$out"
    is   "the row reads SUBMITTED" "SUBMITTED" "$(state_of "$W" sp-simlc1)"

    out="$(sim_world down "$W" 2>&1)"; rc=$?
    wantrc "down succeeds" 0 "$rc"
    [ "$rc" = 0 ] || printf '%s\n' "$out" | sed 's/^/# down: /'
    [ ! -e "$W" ] && ok "down removed the world" || bad "down removed the world" "$W exists"
    left="$( [ -d "/proc/$PID" ] && tr '\0' ' ' < "/proc/$PID/cmdline" 2>/dev/null)"
    case "$left" in *"serve $W/lc.sock"*) bad "the recorded serve PID is gone after down" "$PID: $left" ;;
                    *) ok "the recorded serve PID is gone after down" ;; esac
    is   "no serve process for that world survives down" "" "$(serves_of "$W")"
fi

# --- 2. CONTROL: the same submit with the world's serve stopped fails -------------------
W="$T/w2"
if world_up "$W"; then
    W="$(cd "$W" && pwd -P)"
    claim "$W" sp-simlc2
    PID="$(cat "$W/lc-serve.pid")"
    if serve_up "$W"; then
        kill -TERM "$PID"
        for _ in $(seq 50); do [ -z "$(serves_of "$W")" ] && break; sleep 0.1; done
    fi
    is   "control: the world's serve is stopped" "" "$(serves_of "$W")"
    out="$(in_world "$W" env SPIRA_WORK_BEAD_ID=sp-simlc2 work submit 2>&1)"; rc=$?
    wantrc "control: work submit without the serve fails cannot-tell" 2 "$rc"
    want   "control: the failure names the world's socket" "connecting to $W/lc.sock" "$out"
    # sim.env pins a same-user fallback to the world's own fixture: the row reads directly.
    is     "control: the row is still WORKING" "WORKING" "$(state_of "$W" sp-simlc2)"
    out="$(sim_world down "$W" 2>&1)"; rc=$?
    wantrc "control: down succeeds with its serve already gone" 0 "$rc"
    [ "$rc" = 0 ] || printf '%s\n' "$out" | sed 's/^/# down: /'
    [ ! -e "$W" ] && ok "control: down removed the world" || bad "control: down removed the world" "$W exists"
fi

tl_summary
