#!/usr/bin/env bash
# testlib/lc-fixture.sh — a throwaway control store (SQLite), the lifecycle's one record, for
# suites whose subject reads or writes bead and batch state.
#
# Sourced, never executed, by a suite that already set TMP:
#   . "$HERE/testlib/lc-fixture.sh"
#   lcfix_up || exit 1            # makes the store, declares SPIRA_LC_STORE_PATH
#   lcfix_seed sp-a CERTIFIED <tip>
#   env -i SPIRA_RELEASE="$(tl_release)" ... $(lcfix_env) queue ...   # carry the config through an `env -i`
#   lcfix_state sp-a              # -> CERTIFIED
#
# Every record is made by the machine's own events, so a seeded row is one the machine could have
# reached: a bead is READY, Claim makes it WORKING, Submit SUBMITTED, GatePass CERTIFIED. A
# state no short walk reaches (REWORK, LANDED, ...) is the classifier's correction of a READY row.
#
# lcfix_up_on_testdb is lcfix_up for a suite that also resets its bd fixture, and whose subject
# reads the beads through spira-lc: the planning connection names the testdb's server, and the
# store is made empty again with every testdb_reset.
#
# lcfix_down is idempotent; call it from the suite's EXIT trap.
LCFIX_DIR=""
LCFIX_TIP="1111111111111111111111111111111111111111"

# The staged release's spira-lc, so a stub a suite puts ahead of it on PATH never intercepts
# fixture setup.
_lcfix_lc() {
    local rel="${_TL_STAGED_RELEASE:-${SPIRA_RELEASE:-}}"
    if [ -n "$rel" ] && [ -d "$rel/bin" ]; then
        SPIRA_RELEASE="$rel" PATH="$rel/bin:$rel/spira:$PATH" spira-lc "$@"
    else
        spira-lc "$@"
    fi
}

_lcfix_create_store() {
    rm -f "$LCFIX_DIR/control.db"
    _lcfix_lc admin-create-store >"$LCFIX_DIR/create.log" 2>&1 \
        || { echo "lc-fixture: cannot create the control store: $(cat "$LCFIX_DIR/create.log")" >&2; return 1; }
}

lcfix_up() {
    command -v spira-lc >/dev/null 2>&1 || { echo "lc-fixture: spira-lc not on PATH" >&2; return 1; }
    LCFIX_DIR="$(mktemp -d "${_TL_CONF_DIR:-${TMPDIR:-/tmp}}/lcfix.XXXXXX")" || return 1
    unset SPIRA_LC_SOCKET
    # The dolt half of spira-lc's connection is lazy and used only for planning joins: a
    # placeholder credential satisfies its construction, and nothing here connects to it. A
    # suite that also stands up the dolt fixture keeps that fixture's own credential.
    if [ -z "${LCDOLT_DIR:-}" ]; then
        export SPIRA_LC_PASSWORD=unused
        tl_config SPIRA_LC_SOCKET="" SPIRA_LC_PASSWORD_FILE="" || return 1
    fi
    tl_config SPIRA_LC_STORE_PATH="$LCFIX_DIR/control.db" SPIRA_LC_LIFECYCLE_ON_STORE=1 \
        || { echo "lc-fixture: cannot declare the control store" >&2; return 1; }
    _lcfix_create_store
}

lcfix_up_on_testdb() {
    [ "${TESTDB_MODE:-}" = server ] || { echo "lc-fixture: lcfix_up_on_testdb needs a server-mode testdb (SPIRA_TESTDB_MODE=server)" >&2; return 1; }
    lcfix_up || return 1
    # spira-lc reads the planning store over its dolt connection, so it sees the beads only
    # when that connection names the testdb's own server.
    : > "$LCFIX_DIR/credential"
    tl_config SPIRA_LC_PASSWORD_FILE="$LCFIX_DIR/credential" SPIRA_LC_SOCKET="" \
        || { echo "lc-fixture: cannot declare the planning connection" >&2; return 1; }
    export SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_DATA_DIR="$LCFIX_DIR" SPIRA_LC_USER=root SPIRA_LC_PASSWORD=""
    LCFIX_ON_TESTDB=1
    _lcfix_point_at_testdb && _lcfix_stamp || return 1
    [ -n "${_LCFIX_ON_TESTDB:-}" ] && return 0
    _LCFIX_ON_TESTDB=1
    # A testdb_reset restarts that server, on another port: the connection follows it, and the
    # store is made empty again, so each section starts on empty stores.
    eval "$(declare -f testdb_reset | sed '1s/testdb_reset/_lcfix_testdb_reset_store/')"
    testdb_reset() { _lcfix_testdb_reset_store "$@" && _lcfix_point_at_testdb && _lcfix_create_store && _lcfix_stamp; }
}

# Both planes carry a planning watermark, and a repair sweep acts only while they agree: stamped
# once, they do.
_lcfix_stamp() {
    _lcfix_lc plan stamp >"$LCFIX_DIR/stamp.log" 2>&1 \
        || { echo "lc-fixture: cannot stamp the planning watermark: $(cat "$LCFIX_DIR/stamp.log")" >&2; return 1; }
}

_lcfix_point_at_testdb() {
    local port
    port="$(cat "$SPIRA_DB/.beads/dolt-server.port" 2>/dev/null)" \
        || { echo "lc-fixture: the testdb names no server port" >&2; return 1; }
    export SPIRA_LC_PORT="$port"
}

lcfix_down() {
    [ -n "$LCFIX_DIR" ] && rm -rf "$LCFIX_DIR"
    LCFIX_DIR=""
}

# lcfix_round_files — the queue keeps its open-batch record in a round file, not on the store:
# for a suite whose subject is that file. The bead rows stay the store's.
lcfix_round_files() { tl_config SPIRA_LC_LIFECYCLE_ON_STORE=0; }

# lcfix_reset — an empty store: every bead and batch forgotten.
lcfix_reset() { _lcfix_create_store; }

# lcfix_env — the config a suite carries through an `env -i`.
lcfix_env() {
    if [ -n "${LCFIX_ON_TESTDB:-}" ]; then
        printf 'SPIRA_TOML=%s SPIRA_LC_HOST=%s SPIRA_LC_PORT=%s SPIRA_LC_DATA_DIR=%s SPIRA_LC_USER=%s SPIRA_LC_PASSWORD=' \
            "$SPIRA_TOML" "$SPIRA_LC_HOST" "$SPIRA_LC_PORT" "$SPIRA_LC_DATA_DIR" "$SPIRA_LC_USER"
    else
        printf 'SPIRA_TOML=%s SPIRA_LC_PASSWORD=unused' "$SPIRA_TOML"
    fi
}

# lcfix_decl — the same config as KEY=value words, for a fixture that declares its own layer
# (gate_fixture_run) instead of carrying SPIRA_TOML.
lcfix_decl() {
    printf 'SPIRA_LC_STORE_PATH=%s SPIRA_LC_LIFECYCLE_ON_STORE=1 SPIRA_LC_SOCKET= SPIRA_LC_PASSWORD_FILE= SPIRA_LC_PASSWORD=unused' "$LCFIX_DIR/control.db"
}

# _lcfix_show <id> — the bead's `show` JSON. The store has one opener at a time, so a process a
# moment from exiting can make the answer "cannot tell" (2): that is retried, not read as empty.
_lcfix_show() {
    local out rc n
    for n in 1 2 3 4 5 6 7 8 9 10; do
        out="$(_lcfix_lc show "$1" 2>/dev/null)"; rc=$?
        [ "$rc" -ne 2 ] && break
        sleep 0.3
    done
    printf '%s' "$out"
}

# lcfix_field <id> <field> — one field of the bead's row, empty when it is null or absent.
lcfix_field() {
    _lcfix_show "$1" | python3 -c '
import json, sys
try:
    b = (json.load(sys.stdin) or {}).get("bead") or {}
except ValueError:
    b = {}
v = b.get(sys.argv[1])
print("" if v is None else v)' "$2"
}

# lcfix_batch_state <batch-id> — the round's state.
lcfix_batch_state() {
    _lcfix_lc show-batch "$1" 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin) or {}
except ValueError:
    d = {}
b = d.get("batch") or d
v = b.get("state")
print("" if v is None else v)'
}

lcfix_state() { lcfix_field "$1" state; }
lcfix_tip() { lcfix_field "$1" tip; }
lcfix_reason() { lcfix_field "$1" reason; }
lcfix_holder() { lcfix_field "$1" holder; }

# lcfix_event <id> <expect-state> <kind-json> [actor] — one bead event at the row's own version.
lcfix_event() {
    local id="$1" expect="$2" kind="$3" actor="${4:-fixture}" version out
    version="$(lcfix_field "$id" version)"
    out="$(_lcfix_lc event bead "$id" --expect "$expect" --version "$version" --actor "$actor" --kind "$kind" 2>&1)" \
        || { echo "lc-fixture: $id $expect <- $kind failed: $out" >&2; return 1; }
}

# lcfix_seed <id> <STATE> [tip] [gate-key] — the bead, born READY and brought to STATE. Seeding a bead again
# starts it over. A seed that does not take says so on stderr and returns non-zero.
lcfix_seed() {
    local id="$1" state="$2" tip="${3:-$LCFIX_TIP}" gate_key="${4:-fixture-gate-key}" out
    _lcfix_lc delete-bead-row "$id" >/dev/null 2>&1
    out="$(_lcfix_lc create-bead "$id" --ready 2>&1)" \
        || { echo "lc-fixture: creating $id failed: $out" >&2; return 1; }
    case "$state" in
        READY) ;;
        WORKING) lcfix_claim "$id" ;;
        SUBMITTED) lcfix_claim "$id" && lcfix_event "$id" WORKING "{\"Submit\":{\"tip\":\"$tip\"}}" ;;
        CERTIFIED)
            lcfix_claim "$id" && lcfix_event "$id" WORKING "{\"Submit\":{\"tip\":\"$tip\"}}" \
                && lcfix_event "$id" SUBMITTED "{\"GatePass\":{\"tip\":\"$tip\",\"gate_key\":\"$gate_key\"}}" ;;
        *) lcfix_event "$id" READY "{\"Reclassify\":{\"state\":\"$(_lcfix_camel "$state")\",\"rule\":\"fixture\"}}" classifier ;;
    esac
}

# IN_DELIVERY -> InDelivery: the spelling the event's JSON gives a state.
_lcfix_camel() { printf '%s' "$1" | python3 -c 'import sys; print("".join(w.capitalize() for w in sys.stdin.read().split("_")))'; }

# lcfix_claim <id> [holder] [lease-until] — a READY bead claimed by <holder> (default: fixture).
# LCFIX_STACK='{"sp-prereq":"<tip>",...}' makes the claim a stacked one (its depth the entries').
lcfix_claim() {
    local stack="" depth persona=""
    [ -n "${LCFIX_PERSONA:-}" ] && persona=",\"persona\":\"$LCFIX_PERSONA\""
    if [ -n "${LCFIX_STACK:-}" ]; then
        depth="$(printf '%s' "$LCFIX_STACK" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
        stack=",\"stack\":$LCFIX_STACK,\"stack_depth\":$depth,\"stack_max_depth\":8"
    fi
    lcfix_event "$1" READY "{\"Claim\":{\"holder\":\"${2:-${LCFIX_HOLDER:-fixture}}\",\"lease_until\":${3:-$(( $(date +%s) + 3600 ))}$stack$persona}}"
}

# lcfix_sifted <id> <tip> — the sift pass vouches for <tip> on a CERTIFIED bead.
lcfix_sifted() { lcfix_event "$1" CERTIFIED "{\"Sifted\":{\"tip\":\"$2\"}}"; }

# lcfix_hold <id> <expect-state> <Poison|Ask|Wait|Manual> [cause: attempts-exhausted, operator-question, ...] — the bead takes that hold.
lcfix_hold() {
    local cause="${4:-}"
    case "$3" in
        Poison) cause="${cause:-attempts-exhausted}" ;;
        Ask) cause="${cause:-operator-question}" ;;
        Wait) cause="${cause:-unlanded-blocker}" ;;
    esac
    lcfix_event "$1" "$2" "{\"Hold\":{\"kind\":\"$3\",\"cause\":\"$cause\"}}"
}
