# testdb.sh — a REAL `bd` on a throwaway Dolt database. Sourced by a suite,
# never executed.
#
#   . "$HERE/testdb.sh"
#   testdb_require fayth          # skips the suite, loudly, if no bd engine is usable
#   testdb_up fayth               # creates the database, exports SPIRA_DB
#   trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
#   testdb_seed <<'JSONL' ... JSONL
#   testdb_reset                  # back to empty, in ~6ms (embedded) or ~6s (server)
#
# WHY NOT A STUB. `.claude/spira/testbin/bd` modelled 16 of bd's 118 subcommands, and its
# fidelity was wrong twice in one day in ways that made CALLERS look broken: `list` ignored
# --label entirely, so the CI sweep ran against every fixture bead and the suite failed on
# code that was correct; and a dead duplicate `ready` handler meant two implementations of
# one query disagreed. A partial model of a dependency drifts silently, and every hour spent
# restoring its fidelity reimplements something that already exists and is correct by
# definition (law-prefer-the-real-dependency).
#
# TWO MODES. The embedded mode is preferred: each fixture is a private tmpdir directory,
# cleanup is rm -rf, and six concurrent builds complete in ~25s with 0 failures. Server
# mode is the fallback for boxes where bd-embedded is not available; it uses
# dolt-beads-test.service on SPIRA_TESTDB_PORT, which testdb.sh starts on demand.
#
# WHY EMBEDDED IS PREFERRED OVER SERVER. testdb_up against a shared Dolt server cost 84s
# solo and 610s for six concurrent builds (5 of 6 failing), because schema migrations hold
# a global lock and leaked fixtures from SIGKILL'd suites compounded through an age-based
# sweep that ran concurrently against the same server. With the embedded engine each fixture
# is a directory, cleanup is rm -rf, and the problems disappear. The route through
# --proxied-server was also tested and rejected: 'bd import' fails in that mode, and import
# is how fixture-using suites seed their data.
#
# Server mode is the fallback, not the primary path. On any box where bd-embedded is
# available, this file never touches dolt-beads-test.service.
#
# REQUIRES bd-embedded FOR EMBEDDED MODE. The standard binary is built CGO_ENABLED=0 and
# refuses embedded mode. Install: npm install -g @beads/bd (or the project's install.sh).
# The old binary is at ~/.local/bin/bd.cgo0-backup for rollback.

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

TESTDB_BD="${TESTDB_BD:-${SPIRA_TESTDB_BD:-bd-embedded}}"
# bd binary for server mode: the standard binary (not bd-embedded) talks to dolt sql-server.
TESTDB_SERVER_BD="${TESTDB_SERVER_BD:-bd}"
# Preserved if already set, so a caller that built a shared fixture and exported it is not
# erased by the act of sourcing this file.
TESTDB_NAME="${TESTDB_NAME:-}"
TESTDB_DIR="${TESTDB_DIR:-}"
TESTDB_BASELINE="${TESTDB_BASELINE:-}"
TESTDB_BIN="${TESTDB_BIN:-}"
# TESTDB_MODE: "embedded" when using the embedded engine, "server" when using
# dolt-beads-test.service. Empty before testdb_up is first called.
TESTDB_MODE="${TESTDB_MODE:-}"
# TESTDB_STARTED_SERVICE: 1 if THIS process started dolt-beads-test.service; 0 otherwise.
# Used in testdb_drop to know whether to stop the service. Exported so fixture_drop in
# aeon.sh (which runs testdb_drop in a subshell) sees the correct value.
export TESTDB_STARTED_SERVICE="${TESTDB_STARTED_SERVICE:-0}"

# _testdb_sc <subcommand> [args...] — run "systemctl --user" with XDG_RUNTIME_DIR fallback.
# gate.sh runs suites under env -i without XDG_RUNTIME_DIR; without it, systemctl --user
# cannot connect to the session bus. Setting XDG_RUNTIME_DIR=/run/user/$(id -u) gives the
# single-user fallback path that works from the gate environment (sp-f342).
_testdb_sc() {
    local sc="${SPIRA_SYSTEMCTL:-systemctl}"
    "$sc" --user "$@" 2>/dev/null && return 0
    XDG_RUNTIME_DIR="/run/user/$(id -u)" "$sc" --user "$@"
}

# _testdb_server_clean_sp <restart:0|1>
# Stop dolt-beads-test.service, remove the server-side sp database directory, and (when
# restart=1) start the service again and wait for its port. Must be called before any bd
# init that uses --prefix sp --server --external: a surviving sp directory from a prior run
# holds the old project_id, and the new bd init creates a fresh local project_id, giving a
# PROJECT IDENTITY MISMATCH that blocks all subsequent bd operations (sp-f342).
_testdb_server_clean_sp() {
    local do_restart="${1:-0}"
    [ -n "${SPIRA_TESTDB_DATA:-}" ] || return 0
    _testdb_sc stop dolt-beads-test.service 2>/dev/null || true
    rm -rf "${SPIRA_TESTDB_DATA:?}/sp"
    [ "$do_restart" = 1 ] || return 0
    _testdb_sc start dolt-beads-test.service 2>/dev/null || {
        printf 'testdb: dolt-beads-test.service failed to start after sp cleanup\n' >&2
        return 1
    }
    local port="${SPIRA_TESTDB_PORT:-3308}" i=0
    while [ $i -lt 30 ]; do
        echo -n "" >/dev/tcp/127.0.0.1/"$port" 2>/dev/null && return 0
        sleep 0.5; i=$((i+1))
    done
    printf 'testdb: dolt-beads-test.service did not become ready on port %s\n' "$port" >&2
    return 1
}

# Cached result of the embedded-engine check: "yes", "no", or "" (not yet checked).
# The check itself is slow (it runs bd init), so the result is memoised for the session.
_TESTDB_EMBEDDED_RESULT=""

_testdb_embedded_check() {   # 0 if bd-embedded works on this box
    if [ "$_TESTDB_EMBEDDED_RESULT" = yes ]; then return 0; fi
    if [ "$_TESTDB_EMBEDDED_RESULT" = no  ]; then return 1; fi
    # Fast path: shared fixture already confirmed embedded is working.
    if [ "${TESTDB_SHARED:-0}" = 1 ] && [ "${TESTDB_MODE:-}" = embedded ] && \
       [ -d "${TESTDB_DIR:-}" ]; then
        _TESTDB_EMBEDDED_RESULT=yes; return 0
    fi
    if command -v "$TESTDB_BD" >/dev/null 2>&1; then
        local tmp; tmp="$(mktemp -d)"
        if ( cd "$tmp" && env -i PATH="$PATH" HOME="$HOME" TERM=dumb BD_NON_INTERACTIVE=1 \
            "$TESTDB_BD" init --non-interactive --prefix sp --skip-agents --skip-hooks -q \
            2>/dev/null ); then
            rm -rf "$tmp"
            _TESTDB_EMBEDDED_RESULT=yes; return 0
        fi
        rm -rf "$tmp"
    fi
    _TESTDB_EMBEDDED_RESULT=no; return 1
}

testdb_available() {     # 0 if embedded or server mode is usable on this box
    # Fast path: a shared fixture was already built this session.
    [ "${TESTDB_SHARED:-0}" = 1 ] && [ -d "${TESTDB_DIR:-}" ] && return 0
    # Embedded path: preferred, works without any running service.
    _testdb_embedded_check && return 0
    # Server path: usable if the test server data directory is configured.
    [ -n "${SPIRA_TESTDB_DATA:-}" ] && return 0
    return 1
}

# A SUITE THAT CANNOT RUN MUST NOT READ AS A SUITE THAT PASSED. 77 is the automake skip
# convention; gate-brain.sh knows it and names the skipped suite in the gate's own output,
# because the gate discards suite stdout and a silent skip there is indistinguishable from
# a pass (law-alerts-must-be-actionable).
testdb_require() {       # testdb_require <suite-name>
    testdb_available && return 0
    printf 'SKIP %s: no bd engine is available.\n' "$1" >&2
    printf '  embedded: install bd-embedded (npm install -g @beads/bd)\n' >&2
    printf '  server: set SPIRA_TESTDB_DATA in spira.conf\n' >&2
    exit 77
}

# testdb_server_ensure — start dolt-beads-test.service if not already running.
# Sets TESTDB_STARTED_SERVICE=1 if this call started the service (so testdb_drop
# can stop it). Waits for the port to be ready before returning.
testdb_server_ensure() {
    [ -n "${SPIRA_TESTDB_DATA:-}" ] || return 1
    # Already running: do not start, do not take ownership of the lifecycle.
    _testdb_sc is-active dolt-beads-test.service >/dev/null 2>&1 && return 0
    _testdb_sc start dolt-beads-test.service 2>/dev/null || {
        printf 'testdb: systemctl start dolt-beads-test.service failed\n' >&2
        return 1
    }
    TESTDB_STARTED_SERVICE=1
    export TESTDB_STARTED_SERVICE
    # Wait for the port to accept connections (dolt starts in ~1s; allow up to 15s).
    local port="${SPIRA_TESTDB_PORT:-3308}"
    local i=0
    while [ $i -lt 30 ]; do
        echo -n "" >/dev/tcp/127.0.0.1/"$port" 2>/dev/null && return 0
        sleep 0.5
        i=$((i+1))
    done
    printf 'testdb: dolt-beads-test.service did not become ready on port %s\n' "$port" >&2
    return 1
}

# testdb_up <tag> — a fixture workspace with a throwaway database. Exports SPIRA_DB
# and SPIRA_BD: lib.sh's `bdq` reads both, and SPIRA_BD must point at the right
# binary so every bdq call inside a test reaches the engine the fixture was created with.
#
# `--prefix sp` with no --server: the issue prefix is production's, so ids read `sp-a3f`
# exactly as they do live, while the database is private with no server traffic.
#
# A FIXTURE MAY BE INHERITED. `bd init` costs ~6s (schema DDL) and the gate runs suites
# that each build the same thing. When a caller has already built one and exported
# TESTDB_SHARED=1, reset to its baseline instead.
#
# SERVER-MODE SHARING. With TESTDB_MODE=server and TESTDB_SHARED=1, the shared fixture
# is on the dolt-beads-test server. A reset drops and reinitialises the database (~6s).
# There is no copy-swap shortcut for server databases, but the cost is still paid once
# per suite run, not once per suite.
testdb_up() {            # testdb_up <tag>
    local tag="$1"

    # FAIL SAFE. Unset SPIRA_DB before any work so that a failure on any path below
    # leaves it unusable rather than pointing at whatever the caller had — which is
    # production. A suite that ignores our return then dies on its first bd call with
    # a named error (unbound variable under set -u, or "no such database") instead of
    # writing to the real store. SPIRA_BD is paired because it names the binary for that
    # database; leaving one set and the other unset would let a call slip through to the
    # wrong engine.
    unset SPIRA_DB SPIRA_BD

    # ---- SHARED FIXTURE: EMBEDDED (has TESTDB_BASELINE) ----
    if [ "${TESTDB_SHARED:-0}" = 1 ] && [ -n "${TESTDB_NAME:-}" ] && \
       [ -n "${TESTDB_BASELINE:-}" ]; then
        testdb_reset || {
            printf 'testdb: could not reset shared fixture %s\n' "$TESTDB_NAME" >&2
            # A borrower that cannot start the shared fixture is not a failing suite.
            # Exit with TESTDB_FAULT_EXIT so the runner (suites.sh) classifies this
            # suite as a pass-level fixture fault and files one bead for the collapse
            # rather than one per borrower. Running this suite individually against a
            # healthy fixture will pass.
            exit "${TESTDB_FAULT_EXIT:-75}"
        }
        export SPIRA_DB="$TESTDB_DIR" SPIRA_BD="$TESTDB_BD"
        # conf.sh resets PATH from SPIRA_PATH; add TESTDB_BIN to both so child processes
        # that re-source conf.sh still find the embedded binary.
        if [ -n "${TESTDB_BIN:-}" ]; then
            export PATH="$TESTDB_BIN:$PATH"
            export SPIRA_PATH="$TESTDB_BIN${SPIRA_PATH:+:$SPIRA_PATH}"
        fi
        return 0
    fi

    # ---- SHARED FIXTURE: SERVER (TESTDB_MODE=server, no TESTDB_BASELINE) ----
    if [ "${TESTDB_SHARED:-0}" = 1 ] && [ -n "${TESTDB_NAME:-}" ] && \
       [ "${TESTDB_MODE:-}" = server ] && [ -d "${TESTDB_DIR:-}" ]; then
        testdb_reset || {
            printf 'testdb: could not reset shared server fixture %s\n' "$TESTDB_NAME" >&2
            exit "${TESTDB_FAULT_EXIT:-75}"
        }
        export SPIRA_DB="$TESTDB_DIR" SPIRA_BD="$TESTDB_SERVER_BD"
        return 0
    fi

    # ---- FRESH FIXTURE: CHOOSE MODE ----
    if _testdb_embedded_check; then
        # EMBEDDED MODE: private tmpdir, cleanup is rm -rf.
        TESTDB_MODE=embedded
        TESTDB_NAME="sptest_${tag}_$(date +%s)_$$"
        TESTDB_DIR="$(mktemp -d)"
        # env -i is deliberate. A gate, a check or a test invoked by automation runs in an
        # explicit minimal environment, never the caller's: BEADS_ACTOR and friends leak into
        # `created_by` and `owner`, and ambient configuration silently deciding a verdict is
        # exactly law-gates-run-in-a-clean-environment.
        # THE FAILURE MUST CARRY ITS REASON. Errors go to stderr, where a gate capturing
        # output can still see them.
        local init_out init_rc
        init_out="$( cd "$TESTDB_DIR" && env -i PATH="$PATH" HOME="$HOME" TERM=dumb \
            BD_NON_INTERACTIVE=1 \
            "$TESTDB_BD" init --non-interactive --prefix sp --skip-agents --skip-hooks \
            -q 2>&1 )"
        init_rc=$?
        [ $init_rc -eq 0 ] || {
            printf 'testdb: bd init failed (rc=%s) for %s in %s\n' \
                "$init_rc" "$TESTDB_NAME" "$TESTDB_DIR" >&2
            printf '%s\n' "$init_out" | sed 's/^/testdb:   /' >&2
            rm -rf "$TESTDB_DIR"; TESTDB_DIR=""; TESTDB_NAME=""; return 1
        }

        # THE BASELINE IS A SNAPSHOT OF .beads AT INIT TIME. Reset replaces .beads with
        # this copy, so every reset is identical to a fresh init without the 6s cost.
        TESTDB_BASELINE="$(mktemp -d)"
        cp -rp "$TESTDB_DIR/.beads" "$TESTDB_BASELINE/.beads"
        [ -d "$TESTDB_BASELINE/.beads" ] || {
            printf 'testdb: baseline snapshot failed for %s\n' "$TESTDB_NAME" >&2
            rm -rf "$TESTDB_DIR" "$TESTDB_BASELINE"
            TESTDB_DIR=""; TESTDB_BASELINE=""; TESTDB_NAME=""
            return 1
        }

        # MAKE `bd` RESOLVE TO THE EMBEDDED BINARY IN THIS PROCESS TREE. Test suites call
        # `bd` directly (not through bdq) for helper functions; without this, those calls
        # hit the production binary (CGO_ENABLED=0) which cannot open the embedded store.
        # A symlink in a private tempdir prepended to PATH intercepts all bare `bd`
        # invocations for the life of the test while leaving every other command untouched.
        #
        # SPIRA_PATH AS WELL AS PATH. conf.sh line 700 rebuilds PATH from scratch:
        #   export PATH="${SPIRA_PATH:+$SPIRA_PATH:}$HOME/.local/bin:..."
        # Any child process that sources conf.sh (including aeon.sh when it runs as a
        # subprocess of a test suite) loses a bare PATH modification. conf.sh honors
        # SPIRA_PATH from the environment (env-first `:=` pattern), so prepending
        # TESTDB_BIN there makes the shim survive conf.sh resets in every child.
        local _bd_real; _bd_real="$(command -v "$TESTDB_BD" 2>/dev/null)"
        if [ -n "$_bd_real" ]; then
            TESTDB_BIN="$(mktemp -d)"
            ln -sf "$_bd_real" "$TESTDB_BIN/bd"
            export PATH="$TESTDB_BIN:$PATH"
            export SPIRA_PATH="$TESTDB_BIN${SPIRA_PATH:+:$SPIRA_PATH}"
        fi

        export SPIRA_DB="$TESTDB_DIR" SPIRA_BD="$TESTDB_BD"
        return 0
    fi

    # SERVER MODE: fallback for boxes without bd-embedded.
    [ -n "${SPIRA_TESTDB_DATA:-}" ] || {
        printf 'testdb: no embedded bd and SPIRA_TESTDB_DATA is not set\n' >&2
        return 1
    }
    testdb_server_ensure || {
        printf 'testdb: could not start dolt-beads-test.service\n' >&2
        return 1
    }
    TESTDB_MODE=server
    TESTDB_NAME="sptest_${tag}_$(date +%s)_$$"
    TESTDB_DIR="$SPIRA_TESTDB_DATA/$TESTDB_NAME"
    mkdir -p "$TESTDB_DIR" || { printf 'testdb: mkdir %s failed\n' "$TESTDB_DIR" >&2; return 1; }
    # Remove any surviving server-side sp from a prior run before init. A leftover sp carries
    # the old project_id; the new bd init creates a fresh one; the mismatch then blocks every
    # subsequent bd operation with PROJECT IDENTITY MISMATCH (sp-f342).
    _testdb_server_clean_sp 1 || {
        printf 'testdb: failed to clean server-side sp before init\n' >&2
        rm -rf "$TESTDB_DIR"; TESTDB_DIR=""; TESTDB_NAME=""; return 1
    }
    local init_out init_rc
    init_out="$( cd "$TESTDB_DIR" && env -i PATH="$PATH" HOME="$HOME" TERM=dumb \
        BD_NON_INTERACTIVE=1 \
        "$TESTDB_SERVER_BD" init --non-interactive --prefix sp --skip-agents --skip-hooks \
        --server --server-host 127.0.0.1 --server-port "${SPIRA_TESTDB_PORT:-3308}" \
        --external -q 2>&1 )"
    init_rc=$?
    [ $init_rc -eq 0 ] || {
        printf 'testdb: bd init (server) failed (rc=%s) for %s\n' \
            "$init_rc" "$TESTDB_NAME" >&2
        printf '%s\n' "$init_out" | sed 's/^/testdb:   /' >&2
        rm -rf "$TESTDB_DIR"; TESTDB_DIR=""; TESTDB_NAME=""; return 1
    }
    # Server mode: no baseline (copy-swap does not apply to server databases).
    TESTDB_BASELINE=""
    TESTDB_BIN=""
    export SPIRA_DB="$TESTDB_DIR" SPIRA_BD="$TESTDB_SERVER_BD"
    return 0
}

# Back to an empty database. For embedded mode: a directory swap rather than a table wipe.
# bd writes across multiple tables and a wipe that misses one leaves state no test asked for.
# The swap is 6ms and is guaranteed complete.
# For server mode: drops the server-side database and reinitialises it (~6s).
testdb_reset() {
    [ -n "$TESTDB_NAME" ] || return 1
    if [ "${TESTDB_MODE:-embedded}" = server ]; then
        [ -d "${TESTDB_DIR:-}" ] || return 1
        # Stop the service, remove server-side sp, restart, and reinitialise from scratch.
        # --reinit-local alone creates a new project_id without touching the server-side sp,
        # so the local .beads and server always diverge. A stop+rm+start+fresh-init pair
        # creates matching project_ids on both sides (sp-f342).
        _testdb_server_clean_sp 1 || return 1
        rm -rf "$TESTDB_DIR/.beads"
        local init_out init_rc
        init_out="$( cd "$TESTDB_DIR" && env -i PATH="$PATH" HOME="$HOME" TERM=dumb \
            BD_NON_INTERACTIVE=1 \
            "$TESTDB_SERVER_BD" init --non-interactive --prefix sp --skip-agents \
            --skip-hooks --server --server-host 127.0.0.1 \
            --server-port "${SPIRA_TESTDB_PORT:-3308}" --external -q 2>&1 )"
        init_rc=$?
        if [ $init_rc -ne 0 ]; then
            printf 'testdb: server reset failed (rc=%s) for %s\n' "$init_rc" "$TESTDB_NAME" >&2
            printf '%s\n' "$init_out" | sed 's/^/testdb:   /' >&2
            return 1
        fi
        return 0
    fi
    # Embedded mode: directory swap using rename rather than rm-then-cp.
    #
    # WHY NOT rm -rf THEN cp -rp. Two hazards in sequence:
    #   1. rm -rf can fail with ENOTEMPTY on overlay2 filesystems (a known Docker
    #      kernel bug where the rename-based unlink of a whiteout entry conflicts
    #      with a concurrent readdir). The failure is non-fatal to rm itself but
    #      leaves the directory partially populated.
    #   2. cp -rp into an EXISTING directory copies the source AS A SUBDIRECTORY,
    #      not over it. If step 1 left .beads alive, step 2 creates .beads/.beads,
    #      and the next reset then fails to remove the nested copy — compounding
    #      the damage across every subsequent call (sp-i0vz5).
    #
    # mv (rename(2)) is atomic, does not recurse, and has no overlay2 edge case.
    # The strategy: copy baseline to a fresh sibling name, rename old out of the
    # way, rename new into place. Only the final cleanup rm can still fail, and
    # by that point the live database is already in the correct state.
    [ -d "$TESTDB_BASELINE/.beads" ] || return 1
    local _new; _new="$TESTDB_DIR/.beads.new"
    local _old; _old="$TESTDB_DIR/.beads.old"
    rm -rf "$_new" "$_old"   # clean up any leftovers from a previous interrupted reset
    cp -rp "$TESTDB_BASELINE/.beads" "$_new" || { rm -rf "$_new"; return 1; }
    mv "$TESTDB_DIR/.beads" "$_old" 2>/dev/null || true   # no-op when .beads absent
    mv "$_new" "$TESTDB_DIR/.beads"                       || return 1
    rm -rf "$_old" 2>/dev/null || true
}

testdb_seed() {          # testdb_seed  < JSONL on stdin
    local f; f="$(mktemp)"
    cat > "$f"
    # Use SPIRA_BD (set by testdb_up to the right binary for the current mode).
    "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" import "$f" >/dev/null 2>&1
    local rc=$?
    rm -f "$f"
    return $rc
}

# THE BORROWER DOES NOT DROP. Suites trap testdb_drop on EXIT; with a shared fixture the
# first suite to finish would delete the directories the rest are still using. Only the
# process that created it owns it, and it drops by clearing TESTDB_SHARED first (as
# aeon.sh's fixture_drop does: TESTDB_SHARED=0 before sourcing and calling testdb_drop).
testdb_drop() {
    [ "${TESTDB_SHARED:-0}" = 1 ] && return 0
    [ -n "${TESTDB_NAME:-}" ] || return 0
    rm -rf "${TESTDB_DIR:-}" "${TESTDB_BASELINE:-}" "${TESTDB_BIN:-}"
    # Server mode: clean up the server-side sp so the next testdb_up finds a fresh server.
    # If this process did NOT start the service (TESTDB_STARTED_SERVICE=0) the service was
    # already running when we arrived, so restart it after removing sp. If we started it
    # (TESTDB_STARTED_SERVICE=1), leave it stopped — that restores the pre-test state.
    if [ "${TESTDB_MODE:-}" = server ]; then
        local _drop_restart=1
        [ "${TESTDB_STARTED_SERVICE:-0}" = 1 ] && _drop_restart=0
        _testdb_server_clean_sp "$_drop_restart" || true
        TESTDB_STARTED_SERVICE=0
        export TESTDB_STARTED_SERVICE
    fi
    TESTDB_NAME=""; TESTDB_DIR=""; TESTDB_BASELINE=""; TESTDB_BIN=""
    TESTDB_MODE=""
    return 0
}
