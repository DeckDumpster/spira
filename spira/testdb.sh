# testdb.sh — a REAL `bd` on a throwaway Dolt database. Sourced by a suite,
# never executed.
#
#   . "$HERE/testdb.sh"
#   testdb_require fayth          # skips the suite, loudly, if no bd engine is usable
#   testdb_up fayth               # creates the database, exports SPIRA_DB
#   trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
#   testdb_seed <<'JSONL' ... JSONL
#   testdb_reset                  # back to empty, in ~6ms (embedded) or ~2s (server)
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
TESTDB_PRIVATE_DIR="${TESTDB_PRIVATE_DIR:-}"
# TESTDB_MODE: "embedded" when using the embedded engine, "server" when using
# dolt-beads-test.service. Empty before testdb_up is first called.
TESTDB_MODE="${TESTDB_MODE:-}"
# TESTDB_STARTED_SERVICE: 1 if THIS process started dolt-beads-test.service; 0 otherwise.
# Used in testdb_drop to know whether to stop the service. Exported so fixture_drop in
# aeon.sh (which runs testdb_drop in a subshell) sees the correct value.
export TESTDB_STARTED_SERVICE="${TESTDB_STARTED_SERVICE:-0}"
# TESTDB_SERVER_INIT_HASH: the Dolt commit hash captured right after bd init in server mode.
# testdb_reset uses CALL DOLT_RESET('--hard', hash) to restore version-controlled tables to
# their post-init state without stopping or restarting the dolt service (sp-f342).
TESTDB_SERVER_INIT_HASH="${TESTDB_SERVER_INIT_HASH:-}"

# _testdb_sc <subcommand> [args...] — run "systemctl --user" with XDG_RUNTIME_DIR fallback.
# gate.sh runs suites under env -i without XDG_RUNTIME_DIR; without it, systemctl --user
# cannot connect to the session bus. Setting XDG_RUNTIME_DIR=/run/user/$(id -u) gives the
# single-user fallback path that works from the gate environment (sp-f342).
_testdb_sc() {
    local sc="${SPIRA_SYSTEMCTL:-systemctl}"
    "$sc" --user "$@" 2>/dev/null && return 0
    XDG_RUNTIME_DIR="/run/user/$(id -u)" "$sc" --user "$@"
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
    TESTDB_OWNS_SERVER_FIXTURE=0; export TESTDB_OWNS_SERVER_FIXTURE

    # ---- SHARED FIXTURE: EMBEDDED (has TESTDB_BASELINE) ----
    # Server mode sets TESTDB_BASELINE too (for .beads restoration), but must NOT take this
    # branch: the embedded path exports SPIRA_BD="$TESTDB_BD" (bd-embedded), which is the
    # wrong binary for a server-mode fixture. Server mode falls through to the SERVER branch
    # below, which exports SPIRA_BD="$TESTDB_SERVER_BD" (bd) (sp-f342).
    # A suite requesting server mode also bypasses this branch: bd-embedded refuses bd sql,
    # so the suite must build its own fresh server fixture or skip.
    if [ "${TESTDB_SHARED:-0}" = 1 ] && [ -n "${TESTDB_NAME:-}" ] && \
       [ -n "${TESTDB_BASELINE:-}" ] && [ "${TESTDB_MODE:-}" != server ] && \
       [ "${SPIRA_TESTDB_MODE:-}" != server ]; then
        TESTDB_PRIVATE_DIR="$(mktemp -d)"
        cp -rp "$TESTDB_BASELINE/.beads" "$TESTDB_PRIVATE_DIR/.beads" 2>/dev/null || {
            rm -rf "$TESTDB_PRIVATE_DIR"; TESTDB_PRIVATE_DIR=""
            printf 'testdb: could not copy baseline for shared fixture %s\n' "$TESTDB_NAME" >&2
            exit "${TESTDB_FAULT_EXIT:-75}"
        }
        [ -d "$TESTDB_PRIVATE_DIR/.beads" ] || {
            rm -rf "$TESTDB_PRIVATE_DIR"; TESTDB_PRIVATE_DIR=""
            printf 'testdb: shared fixture baseline is empty — fixture collapsed\n' >&2
            exit "${TESTDB_FAULT_EXIT:-75}"
        }
        printf 'testdb: baseline copy from %s\n' "$TESTDB_BASELINE" >&2
        export SPIRA_DB="$TESTDB_PRIVATE_DIR" SPIRA_BD="$TESTDB_BD"
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
    # SPIRA_TESTDB_MODE IS THE REQUEST; TESTDB_MODE IS THE REPORT. They are deliberately two
    # names. TESTDB_MODE is assigned by this function to say which engine it ended up on, so
    # a caller that set it as an input would be overwritten on the first call and would then
    # be reading its own stale answer on the second. A suite asks with SPIRA_TESTDB_MODE and
    # reads the result from TESTDB_MODE.
    #
    # WHY ANY SUITE WOULD ASK. Embedded is preferred because it needs no service, but bd in
    # embedded mode answers every `bd sql` with "not yet supported in embedded mode" — so a
    # suite covering the SQL half of the model (the _is_work generated column, the
    # spira_priority_range CHECK) cannot use it and, before this, had no way to say so. It
    # got an embedded fixture, every statement was refused, and the failure read as schema
    # drift. An unsatisfiable request fails rather than silently downgrading, because a
    # silent downgrade is how that read as drift in the first place.
    if [ "${SPIRA_TESTDB_MODE:-}" = server ]; then
        if [ -z "${SPIRA_TESTDB_DATA:-}" ]; then
            printf 'testdb: SPIRA_TESTDB_MODE=server but SPIRA_TESTDB_DATA is not set\n' >&2
            return 1
        fi
    elif _testdb_embedded_check; then
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

        # Full path: conf.sh rebuilds PATH from SPIRA_PATH + $HOME/.local/bin; a bare name
        # unreachable after that rebuild (e.g. private HOME in a parallel gate suite) fails.
        export SPIRA_DB="$TESTDB_DIR" SPIRA_BD="${_bd_real:-$TESTDB_BD}"
        return 0
    fi

    # SERVER MODE: fallback for boxes without bd-embedded.
    printf 'testdb: TRACE: entering server mode initialization for tag=%s\n' "$tag" >&2
    [ -n "${SPIRA_TESTDB_DATA:-}" ] || {
        printf 'testdb: no embedded bd and SPIRA_TESTDB_DATA is not set\n' >&2
        return 1
    }
    printf 'testdb: TRACE: calling testdb_server_ensure\n' >&2
    testdb_server_ensure || {
        printf 'testdb: could not start dolt-beads-test.service\n' >&2
        return 1
    }
    printf 'testdb: TRACE: testdb_server_ensure returned\n' >&2
    TESTDB_MODE=server
    TESTDB_NAME="sptest_${tag}_$(date +%s)_$$"
    # THE SERVER-SIDE DATABASE IS NAMED PER FIXTURE, AND THE WORKSPACE LIVES OUTSIDE THE
    # SERVER'S DATA ROOT. Both halves matter, and getting either wrong destroys other
    # people's fixtures:
    #
    #   1. `bd init --server` with no --database creates a database literally called `sp`.
    #      EVERY server-mode fixture therefore shared ONE database no matter how unique its
    #      directory name was, so a second testdb_up silently emptied the first: the earlier
    #      fixture's reads started returning [] mid-suite. Worse, the old cleanup below
    #      stopped dolt-beads-test.service and rm -rf'd that shared `sp` on every build, so
    #      a concurrent borrower lost the server under its feet as well as its data. This is
    #      the "shared fixture collapsed" fault: one timed run had 85 of ~150 suites unable
    #      to start because an aeon built its own fixture while the run was borrowing one.
    #      --database gives each fixture its own server-side database and they stop colliding.
    #
    #   2. The workspace goes in /var/tmp, not under $SPIRA_TESTDB_DATA. bd init --server
    #      walks UP from its working directory for a Dolt workspace and refuses when it finds
    #      one; $SPIRA_TESTDB_DATA is itself a Dolt repo (.dolt), so any subdirectory fails
    #      init with "already initialized". /var/tmp has no .dolt ancestor.
    TESTDB_DIR="/var/tmp/$TESTDB_NAME"
    mkdir -p "$TESTDB_DIR" || { printf 'testdb: mkdir %s failed\n' "$TESTDB_DIR" >&2; return 1; }
    printf 'testdb: TRACE: TESTDB_DIR created: %s\n' "$TESTDB_DIR" >&2
    # Serialize bd init: schema migrations hold a global Dolt lock (see testdb.sh header).
    # Concurrent inits queue behind it and each takes N×6s instead of 6s — enough to push
    # suites past the 600s timeout when more than ~6 server-mode suites run in parallel.
    local init_out init_rc _init_fd
    _init_fd=""
    printf 'testdb: TRACE: acquiring lock on %s/.server-init.lock\n' "$SPIRA_TESTDB_DATA" >&2
    if exec {_init_fd}>>"${SPIRA_TESTDB_DATA}/.server-init.lock" 2>/dev/null; then
        printf 'testdb: TRACE: lock file opened, calling flock\n' >&2
        flock -x "$_init_fd" 2>/dev/null || true
        printf 'testdb: TRACE: flock returned\n' >&2
    else
        printf 'testdb: TRACE: could not open lock file\n' >&2
    fi
    printf 'testdb: TRACE: calling bd init --server with database=%s\n' "$TESTDB_NAME" >&2
    init_out="$( cd "$TESTDB_DIR" && env -i PATH="$PATH" HOME="$HOME" TERM=dumb \
        BD_NON_INTERACTIVE=1 \
        "$TESTDB_SERVER_BD" init --non-interactive --prefix sp --skip-agents --skip-hooks \
        --server --server-host 127.0.0.1 --server-port "${SPIRA_TESTDB_PORT:-3308}" \
        --database "$TESTDB_NAME" --external -q 2>&1 )"
    init_rc=$?
    printf 'testdb: TRACE: bd init returned with rc=%s\n' "$init_rc" >&2
    [ -n "$_init_fd" ] && { exec {_init_fd}>&- 2>/dev/null; } || true
    [ $init_rc -eq 0 ] || {
        printf 'testdb: bd init (server) failed (rc=%s) for %s\n' \
            "$init_rc" "$TESTDB_NAME" >&2
        printf '%s\n' "$init_out" | sed 's/^/testdb:   /' >&2
        rm -rf "$TESTDB_DIR"; TESTDB_DIR=""; TESTDB_NAME=""; return 1
    }
    # PURGE WHAT EARLIER FIXTURES DROPPED. Dolt's DROP DATABASE is a MOVE: the data goes to
    # .dolt_dropped_databases under the server's data root so it can be restored, and nothing
    # removes it. One day of fixtures left 26MB on a data root the unit file itself calls
    # "disposable; never holds real data", and a fixture is never worth restoring.
    #
    # PURGING HERE AND NOT IN testdb_drop, WHICH IS WHERE IT BELONGS BY SYMMETRY. Measured:
    # a purge issued immediately after DROP DATABASE in the same session reclaims nothing —
    # the drop has not materialised yet — and three build/drop cycles still grew the
    # directory from 2MB to 8MB. By the time the NEXT fixture is built, every earlier drop
    # has settled, so one statement here reclaims all of them. Best-effort: an older Dolt
    # without the procedure fails and the fixture is unaffected.
    "$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
        "CALL DOLT_PURGE_DROPPED_DATABASES()" >/dev/null 2>&1 || true

    # Save baseline and the init commit hash for SQL-based reset (sp-f342). Unlike embedded
    # mode, copy-swap does not apply to the server-side database, but .beads is still local
    # and can be restored. The init hash lets testdb_reset use CALL DOLT_RESET('--hard',
    # hash) to restore version-controlled tables without stopping the dolt service.
    TESTDB_BASELINE="$(mktemp -d)"
    cp -rp "$TESTDB_DIR/.beads" "$TESTDB_BASELINE/.beads" 2>/dev/null || {
        rm -rf "$TESTDB_BASELINE"; TESTDB_BASELINE=""
    }
    # Strip all whitespace from each line before matching: handles trailing-space padding in
    # some bd sql output formats without changing the NF==1 invariant after strip. Dashes in
    # the separator line and pipes in MySQL-style table format do not match /^[a-z0-9]+$/.
    TESTDB_SERVER_INIT_HASH="$("$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
        "SELECT commit_hash FROM dolt_log LIMIT 1" 2>/dev/null \
        | awk '{ gsub(/[[:space:]]/, ""); } length($0)==32 && /^[a-z0-9]+$/' | head -1)"
    [ -n "$TESTDB_SERVER_INIT_HASH" ] || \
        printf 'testdb: warning: could not capture init hash for %s; reset will be slow\n' \
            "$TESTDB_NAME" >&2
    TESTDB_BIN=""
    TESTDB_OWNS_SERVER_FIXTURE=1; export TESTDB_OWNS_SERVER_FIXTURE
    export SPIRA_DB="$TESTDB_DIR" SPIRA_BD="$TESTDB_SERVER_BD"
    return 0
}

# Back to an empty database. For embedded mode: a directory swap rather than a table wipe.
# bd writes across multiple tables and a wipe that misses one leaves state no test asked for.
# The swap is 6ms and is guaranteed complete.
# For server mode: CALL DOLT_RESET to the init hash (clears version-controlled tables) plus
# DELETE from dolt_ignored tables; no service stop/restart (~2s instead of ~6s, sp-f342).
testdb_reset() {
    [ -n "$TESTDB_NAME" ] || return 1
    if [ "${TESTDB_MODE:-embedded}" = server ]; then
        [ -d "${TESTDB_DIR:-}" ] || return 1
        # SQL-based reset: no service stop/restart needed. CALL DOLT_RESET('--hard', hash)
        # restores all version-controlled tables (issues, dependencies, labels, …) to their
        # state at init time. Tables in dolt_ignore (events, bd_events_journal, leases, …)
        # are unaffected by DOLT_RESET and must be cleared with DELETE (sp-f342).
        #
        # Fallback: if TESTDB_SERVER_INIT_HASH is empty (shared fixture from a pre-sp-f342
        # caller), drop and recreate via stop+restart as before.
        if [ -z "${TESTDB_SERVER_INIT_HASH:-}" ]; then
            # DROP ONLY THIS FIXTURE'S DATABASE. This used to stop the whole dolt service
            # and delete the shared `sp` database — taking every other borrower's fixture
            # with it, and bouncing the server under their open connections. Scoping the
            # drop to TESTDB_NAME leaves concurrent fixtures untouched and needs no restart.
            "$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
                "DROP DATABASE IF EXISTS \`$TESTDB_NAME\`" >/dev/null 2>&1 || true

            rm -rf "$TESTDB_DIR/.beads"
            local init_out init_rc
            init_out="$( cd "$TESTDB_DIR" && env -i PATH="$PATH" HOME="$HOME" TERM=dumb \
                BD_NON_INTERACTIVE=1 \
                "$TESTDB_SERVER_BD" init --non-interactive --prefix sp --skip-agents \
                --skip-hooks --server --server-host 127.0.0.1 \
                --server-port "${SPIRA_TESTDB_PORT:-3308}" \
                --database "$TESTDB_NAME" --external -q 2>&1 )"
            init_rc=$?
            if [ $init_rc -ne 0 ]; then
                printf 'testdb: server reset (fallback) failed (rc=%s) for %s\n' \
                    "$init_rc" "$TESTDB_NAME" >&2
                printf '%s\n' "$init_out" | sed 's/^/testdb:   /' >&2
                return 1
            fi
            # Capture the new init hash and update TESTDB_BASELINE so subsequent resets
            # can use the fast SQL path instead of stop/restart again (sp-f342).
            if [ -d "${TESTDB_BASELINE:-}" ]; then
                rm -rf "$TESTDB_BASELINE/.beads"
                cp -rp "$TESTDB_DIR/.beads" "$TESTDB_BASELINE/.beads" 2>/dev/null || true
            fi
            TESTDB_SERVER_INIT_HASH="$("$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
                "SELECT commit_hash FROM dolt_log LIMIT 1" 2>/dev/null \
                | awk '{ gsub(/[[:space:]]/, ""); } length($0)==32 && /^[a-z0-9]+$/' \
                | head -1)"
            return 0
        fi
        local reset_out reset_rc
        reset_out="$("$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
            "CALL DOLT_RESET('--hard', '$TESTDB_SERVER_INIT_HASH')" 2>&1)"
        reset_rc=$?
        if [ $reset_rc -ne 0 ]; then
            printf 'testdb: server dolt_reset failed (rc=%s) for %s: %s\n' \
                "$reset_rc" "$TESTDB_NAME" "$reset_out" >&2
            return 1
        fi
        # Clear dolt_ignored tables that accumulate test data. ignored_schema_migrations
        # and local_metadata are excluded (migration state and project_id linkage).
        "$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
            "DELETE FROM events" 2>/dev/null || true
        "$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
            "DELETE FROM bd_events_journal" 2>/dev/null || true
        "$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
            "DELETE FROM leases" 2>/dev/null || true
        # Restore local .beads (project_id, sequence counters) from the init baseline.
        [ -d "${TESTDB_BASELINE:-}/.beads" ] || return 1
        local _new; _new="$TESTDB_DIR/.beads.new"
        local _old; _old="$TESTDB_DIR/.beads.old"
        rm -rf "$_new" "$_old"
        cp -rp "$TESTDB_BASELINE/.beads" "$_new" || { rm -rf "$_new"; return 1; }
        mv "$TESTDB_DIR/.beads" "$_old" 2>/dev/null || true
        mv "$_new" "$TESTDB_DIR/.beads"            || return 1
        rm -rf "$_old" 2>/dev/null || true
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
    local _td; _td="${TESTDB_PRIVATE_DIR:-$TESTDB_DIR}"
    local _new; _new="$_td/.beads.new"
    local _old; _old="$_td/.beads.old"
    rm -rf "$_new" "$_old"   # clean up any leftovers from a previous interrupted reset
    cp -rp "$TESTDB_BASELINE/.beads" "$_new" || { rm -rf "$_new"; return 1; }
    mv "$_td/.beads" "$_old" 2>/dev/null || true   # no-op when .beads absent
    mv "$_new" "$_td/.beads"                       || return 1
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

# Borrowers remove only their own private copy; shared dirs belong to the owner.
# Exception: server-mode suites under TESTDB_SHARED always build a fresh fixture
# (testdb_up skips shared paths when TESTDB_DIR is absent); TESTDB_OWNS_SERVER_FIXTURE=1
# marks that case so the database is dropped here rather than accumulated on the server.
testdb_drop() {
    if [ "${TESTDB_SHARED:-0}" = 1 ]; then
        [ -n "${TESTDB_PRIVATE_DIR:-}" ] && rm -rf "$TESTDB_PRIVATE_DIR"
        TESTDB_PRIVATE_DIR=""
        [ "${TESTDB_OWNS_SERVER_FIXTURE:-0}" != 1 ] && return 0
    fi
    [ -n "${TESTDB_NAME:-}" ] || return 0
    # Server mode: drop THIS fixture's own database and nothing else.
    #
    # This used to stop dolt-beads-test.service, rm -rf the shared `sp` directory and
    # restart the service. Because every server
    # fixture used to share that one `sp`, a suite finishing its run destroyed the fixture
    # any concurrently running suite or aeon was still borrowing — and bounced the server
    # under its open connections for good measure. Each fixture now owns a database named
    # after it, so a scoped DROP is both sufficient and safe to run while others are live.
    #
    # The drop runs BEFORE the workspace is removed: it needs .beads to find the server.
    if [ "${TESTDB_MODE:-}" = server ] && [ -n "${TESTDB_NAME:-}" ]; then
        "$TESTDB_SERVER_BD" -C "$TESTDB_DIR" sql \
            "DROP DATABASE IF EXISTS \`$TESTDB_NAME\`" >/dev/null 2>&1 || true
        # Only stop the service if THIS process started it; never restart one we found
        # running, because a restart is what used to break other borrowers.
        # In testenv the container is ephemeral; stopping here races any concurrent
        # suite still using the server, so skip the stop and let the container die.
        if [ "${TESTDB_STARTED_SERVICE:-0}" = 1 ] && [ "${SPIRA_IN_TESTENV:-}" != 1 ]; then
            _testdb_sc stop dolt-beads-test.service 2>/dev/null || true
        fi
        TESTDB_STARTED_SERVICE=0
        export TESTDB_STARTED_SERVICE
    fi
    # Rename first; rm -rf on overlay2 whiteouts blocks ~19s and pushes past Podman's exec timeout.
    local _d _gone
    for _d in "${TESTDB_DIR:-}" "${TESTDB_BASELINE:-}" "${TESTDB_BIN:-}"; do
        [ -n "$_d" ] || continue
        [ -e "$_d" ]  || continue
        _gone="${_d}.del-$$"
        if mv "$_d" "$_gone" 2>/dev/null; then
            rm -rf "$_gone" &
        else
            rm -rf "$_d" &
        fi
    done
    TESTDB_NAME=""; TESTDB_DIR=""; TESTDB_BASELINE=""; TESTDB_BIN=""
    TESTDB_MODE=""; TESTDB_PRIVATE_DIR=""; TESTDB_OWNS_SERVER_FIXTURE=0
    return 0
}
