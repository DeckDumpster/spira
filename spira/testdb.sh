# testdb.sh — a REAL `bd` on a throwaway Dolt database.
# Sourced, never executed.
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
# mode (a suite that needs `bd sql`, or a box without bd-embedded) gives each fixture a
# PRIVATE Dolt sql-server started from a pre-initialised template by `testenv testdb`
# (testenv/DESIGN-testdb.md, sp-v2lqd): ~0.1s up, ~0.3s reset, nothing shared.
#
# WHY EMBEDDED IS PREFERRED OVER SERVER. testdb_up against a shared Dolt server cost 84s
# solo and 610s for six concurrent builds (5 of 6 failing), because schema migrations hold
# a global lock and leaked fixtures from SIGKILL'd suites compounded through an age-based
# sweep that ran concurrently against the same server. With the embedded engine each fixture
# is a directory, cleanup is rm -rf, and the problems disappear. The route through
# --proxied-server was also tested and rejected: 'bd import' fails in that mode, and import
# is how fixture-using suites seed their data.
#
# ONE SERVER PER FIXTURE, NEVER A SHARED ONE. Server mode used to put every fixture on
# dolt-beads-test.service (CPUQuota=50%, Nice=10) behind one init flock: in every full run
# on 2026-09-29, 5-9 of the 19 server-mode suites timed out at 600s while the server's
# cgroup was throttled in 99.8% of CFS periods and the host sat at load 11 of 32. This
# file no longer starts that unit; the fixture lifecycle lives in testenv (Rust).
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
# TESTDB_FIXTURE: server mode only — the fixture directory `testenv testdb` owns (its
# private server, data and workspace). Reset and drop hand it back to testenv.
TESTDB_FIXTURE="${TESTDB_FIXTURE:-}"

# _testdb_testenv — the testenv binary that owns server-mode fixtures: the artifact under
# test inside testenv, $SPIRA_REPO/bin otherwise (spira_bin), else PATH. TESTDB_TESTENV
# overrides.
_testdb_testenv() {
    if [ -n "${TESTDB_TESTENV:-}" ]; then printf '%s\n' "$TESTDB_TESTENV"; return 0; fi
    spira_bin testenv 2>/dev/null && return 0
    command -v testenv
}

# _testdb_kv <KEY> <text> — the value of KEY=value in testenv's report.
_testdb_kv() { printf '%s\n' "$2" | sed -n "s/^$1=//p" | head -1; }


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
    # Server path: a private dolt sql-server per fixture needs only dolt on PATH.
    command -v "${TESTDB_DOLT:-dolt}" >/dev/null 2>&1 && return 0
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
    printf '  server: install dolt\n' >&2
    exit 77
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
# SERVER MODE IS NEVER SHARED. A borrower that needs a server builds its own fixture from
# the template (~0.1s); resetting one database other borrowers were reading is the misuse
# sp-v2lqd removed.
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
        :
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

    # SERVER MODE: a private dolt sql-server for this fixture alone (testenv/DESIGN-testdb.md).
    # --owner is this shell: if the suite is killed and never runs its trap, testenv's
    # watchdog takes the server down. TESTDB_OWNER_PID=0 keeps it (testenv.sh scratch).
    local _te _out
    _te="$(_testdb_testenv)" || {
        printf 'testdb: no testenv binary for a server-mode fixture\n' >&2; return 1; }
    _out="$("$_te" testdb up --tag "$tag" --bd "$TESTDB_SERVER_BD" \
        --dolt "${TESTDB_DOLT:-dolt}" --owner "${TESTDB_OWNER_PID:-$$}")" || {
        printf 'testdb: server fixture failed for %s\n' "$tag" >&2; return 1; }
    TESTDB_MODE=server
    TESTDB_NAME="$(_testdb_kv TESTDB_NAME "$_out")"
    TESTDB_DIR="$(_testdb_kv TESTDB_DIR "$_out")"
    TESTDB_FIXTURE="$(_testdb_kv TESTDB_FIXTURE "$_out")"
    [ -n "$TESTDB_NAME" ] && [ -d "$TESTDB_DIR" ] || {
        printf 'testdb: testenv testdb up reported no fixture:\n%s\n' "$_out" >&2; return 1; }
    printf 'testdb: server fixture %s port=%s up_ms=%s\n' "$TESTDB_NAME" \
        "$(_testdb_kv TESTDB_SERVER_PORT "$_out")" "$(_testdb_kv TESTDB_UP_MS "$_out")" >&2
    TESTDB_BASELINE=""
    TESTDB_BIN=""
    TESTDB_OWNS_SERVER_FIXTURE=1; export TESTDB_OWNS_SERVER_FIXTURE
    export SPIRA_DB="$TESTDB_DIR" SPIRA_BD="$TESTDB_SERVER_BD"
    return 0
}

# Back to an empty database. For embedded mode: a directory swap rather than a table wipe.
# bd writes across multiple tables and a wipe that misses one leaves state no test asked for.
# The swap is 6ms and is guaranteed complete.
# For server mode: testenv restarts the fixture's private server on a fresh copy of the
# template (~0.3s) — exactly a new fixture, including the dolt-ignored tables.
testdb_reset() {
    [ -n "$TESTDB_NAME" ] || return 1
    if [ "${TESTDB_MODE:-embedded}" = server ]; then
        [ -n "${TESTDB_FIXTURE:-}" ] && [ -d "${TESTDB_DIR:-}" ] || return 1
        local _te _out
        _te="$(_testdb_testenv)" || return 1
        _out="$("$_te" testdb reset --fixture "$TESTDB_FIXTURE")" || {
            printf 'testdb: server reset failed for %s\n' "$TESTDB_NAME" >&2; return 1; }
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
    # Server mode: testenv stops this fixture's private server and removes the fixture.
    if [ "${TESTDB_MODE:-}" = server ] && [ -n "${TESTDB_FIXTURE:-}" ]; then
        local _te _out
        if _te="$(_testdb_testenv 2>/dev/null)"; then
            _out="$("$_te" testdb down --fixture "$TESTDB_FIXTURE" 2>&1)" && \
                printf 'testdb: server fixture %s down cpu_ms=%s life_ms=%s resets=%s\n' \
                    "$TESTDB_NAME" "$(_testdb_kv TESTDB_SERVER_CPU_MS "$_out")" \
                    "$(_testdb_kv TESTDB_LIFE_MS "$_out")" "$(_testdb_kv TESTDB_RESETS "$_out")" >&2
        fi
        TESTDB_DIR=""
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
    TESTDB_MODE=""; TESTDB_PRIVATE_DIR=""; TESTDB_OWNS_SERVER_FIXTURE=0; TESTDB_FIXTURE=""
    return 0
}
