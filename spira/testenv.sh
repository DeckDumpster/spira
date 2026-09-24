#!/usr/bin/env bash
# testenv.sh up|down|exec|probe — rootless podman test environment with user systemd.
#
# USAGE
#   testenv.sh up   [--name NAME] [--checkout PATH]
#   testenv.sh down [--name NAME] [--volumes]
#   testenv.sh exec [--name NAME] [--user USER] CMD ARGS...
#   testenv.sh probe [--name NAME]
#
# HOW USER SYSTEMD WORKS. The container runs with --systemd=true so PID 1 is system
# systemd. After startup, `loginctl enable-linger` inside the container causes systemd
# to start user@1001.service; once that unit is active, `systemctl --user` connects via
# XDG_RUNTIME_DIR=/run/user/1001. The `up` command waits for user@1001.service before
# returning. The `probe` command exits 0 when user systemd is confirmed working.
#
# CARGO CACHE. Two named volumes (${NAME}-cargo-reg and ${NAME}-cargo-git) hold the
# Cargo registry and git sources at CARGO_HOME=/var/spira/cargo. They survive `down`
# and are reused on the next `up` so each build after the first is incremental. Pass
# --volumes to `down` to also remove them.
#
# CHECKOUT. The caller's checkout (default: the repository this file lives in) is
# bind-mounted read-write at /workspace inside the container. Build artifacts written
# there persist on the host between container runs.
#
# FALLBACK. If user@1001.service does not start, `probe` exits non-zero. Callers
# that need systemctl --user should then set SPIRA_SYSTEMCTL to a recording stub and
# drive the code under test through that seam rather than the real unit manager
# (law-gates-run-in-a-clean-environment).

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
TESTENV_DIR="$HERE/testenv"

# These constants are baked into the container image. They must agree with the
# Containerfile: a mismatch between the UID here and the one useradd used means
# the cargo cache volume has the wrong owner and the first build fails with EACCES.
_SPIRA_USER="spirauser"
_SPIRA_UID=1001
_CONTAINER_CHECKOUT="/workspace"
_CONTAINER_CARGO="/var/spira/cargo"
# CARGO'S BUILD OUTPUT MUST NOT GO INTO THE BIND MOUNT. cargo defaults CARGO_TARGET_DIR to
# <crate>/target, which inside the container is under the bind-mounted checkout. That tree is
# owned by the HOST uid, not spirauser, so the build died with
#   error: failed to create directory `/workspace/cockpit/panel/target/debug`
#   Caused by: Permission denied (os error 13)
# and it would be wrong even if it succeeded: a container build would leave artifacts in the
# operator's own working tree. Pointing it under CARGO_HOME puts it on the named volume, which
# is writable, survives `down`, and makes the second build of a crate fast.
_CONTAINER_CARGO_TARGET="/var/spira/cargo/target"
_USER_RUNTIME="/run/user/${_SPIRA_UID}"
_DEFAULT_NAME="spira-testenv"

# Image tag derived from the build closure: Containerfile, the bd pin, and the dependency
# manifest in conf.sh. All three determine what the image must provide; a change to any of
# them must produce a new tag so a fresh build fires automatically rather than a stale image
# being reused.
_image_tag() {
    # conf.sh supplies SPIRA_BD_PIN when it is not already in the environment. The guard is
    # conf.sh's own (SPIRA_CONF_LOADED), so re-sourcing inside a session is a no-op.
    [ -z "${SPIRA_CONF_LOADED:-}" ] && . "$HERE/conf.sh"
    local _pin="${SPIRA_BD_PIN:-}"
    {
        # CONTENT, NOT PATHS. `sha256sum FILE` prints "<hash>  <path>", and hashing
        # that puts the checkout's location into the tag: two checkouts of the same
        # commit then compute different tags, so every worktree rebuilds its own
        # copy of a 1.8 GB image and an image published from one checkout can never
        # be pulled by another. Nothing reports a fault — the build simply always
        # runs. Redirecting from stdin makes sha256sum print "-" instead.
        #
        # Containerfile: the recipe for the image itself.
        sha256sum < "$TESTENV_DIR/Containerfile" 2>/dev/null || true
        # bd pin: migration count and build flags for the installed bd binary. A rebuild that
        # changes the migration count may invalidate schema expectations in the test suites, so
        # the image must be rebuilt when the pin changes.
        [ -f "$_pin" ] && cat "$_pin"
        # Dependency manifest: SPIRA_BINS and spira_bin_tier. When a program is added or its
        # tier changes, doctor-check.sh's FATAL/WARN sets move — the tag must move first so a
        # build fires.
        sed -n '/^SPIRA_BINS=/,/^}/p' "$HERE/conf.sh" 2>/dev/null || true
    } | sha256sum | cut -c1-12
}

cmd_tag() {
    printf '%s\n' "$(_image_tag)"
}

# Acquire the image and print the local ref. Useful on its own: it is how a cold
# machine warms itself before a batch, so the acquisition cost is attributed to a
# step that says that is what it is doing rather than to the first suite.
cmd_image() {
    local img; img="$(_ensure_image)" || return 1
    printf '%s\n' "$img"
}

# Publish the image under the closure hash.
#
# NOTHING FLOATING IS EVER PUSHED. A :latest would be a second name for an image
# whose whole safety property is that its name is derived from its inputs — one
# pull of it would silently substitute an image built from a different
# Containerfile, which is the failure the tag scheme exists to make impossible.
cmd_publish() {
    local remote
    remote="$(_remote_ref)" || {
        printf 'testenv publish: SPIRA_TESTENV_REGISTRY is unset — nowhere to publish to\n' >&2
        return 1
    }
    local img; img="$(_ensure_image)" || return 1
    podman tag "$img" "$remote" || {
        printf 'testenv publish: could not tag %s as %s\n' "$img" "$remote" >&2
        return 1
    }
    printf 'testenv: pushing %s\n' "$remote" >&2
    podman push "$remote" >&2 || {
        printf 'testenv publish: push failed\n' >&2
        return 1
    }
    printf '%s\n' "$remote"
}

_image_ref() {
    printf 'localhost/spira-testenv:%s' "$(_image_tag)"
}

# Where that same image lives in a registry, if one is configured. Returns
# non-zero when none is, which is the signal to build rather than an error.
#
# THE REF IS ASSEMBLED, NEVER WRITTEN DOWN. Only the repository prefix is
# configured; the tag is the build-closure hash, so a published image cannot be
# given a name that disagrees with what is inside it.
_remote_ref() {
    # conf.sh's own guard makes a second source a no-op. It has to happen in this
    # function's scope: _image_tag sources it inside a command substitution, where
    # the assignments do not survive.
    [ -z "${SPIRA_CONF_LOADED:-}" ] && . "$HERE/conf.sh"
    local reg="${SPIRA_TESTENV_REGISTRY:-}"
    [ -n "$reg" ] || return 1
    printf '%s/spira-testenv:%s' "${reg%/}" "$(_image_tag)"
}

# Build the image if the computed tag is not present. Prints the image ref on stdout
# so callers can capture it; all progress goes to stderr.
#
# BUILD CONTEXT IS HERE (the spira/ directory), not TESTENV_DIR. The Containerfile's
# COPY instructions reference both spira/conf.sh and spira/testenv/doctor-check.sh,
# and a file outside the build context cannot be COPY'd. Using HERE as context with -f
# pointing at the Containerfile satisfies both: podman resolves COPY paths relative to
# the context root (HERE), and the Containerfile itself is specified explicitly.
#
# ACQUIRE, THEN BUILD. A configured registry is consulted before building, because
# building is the expensive path: a Go toolchain downloaded, bd compiled from
# source, a Rust toolchain installed. A machine that keeps the image between runs
# pays that once; a machine created for one CI run and destroyed afterwards pays
# it every run, and it dominates the wall clock.
#
# A MISS IS SLOW, NEVER FATAL. An empty registry, an unreachable one, a machine
# with no credentials — all fall through to the build. They must: the first run
# after any change to the closure necessarily misses, because the tag it would
# pull has never been published. A registry that went down would otherwise stop
# every gate rather than slow it.
#
# THE LOCAL REF IS WHAT CALLERS GET, whichever way it arrived. A pulled image is
# retagged to the same localhost name a built one would have, so nothing
# downstream has to know or care which happened.
_ensure_image() {
    local img; img="$(_image_ref)"
    if podman image exists "$img" 2>/dev/null; then
        printf '%s' "$img"
        return 0
    fi

    local remote
    if remote="$(_remote_ref)"; then
        printf 'testenv: trying %s\n' "$remote" >&2
        if podman pull -q "$remote" >/dev/null 2>&1 \
           && podman tag "$remote" "$img" 2>/dev/null; then
            printf 'testenv: pulled %s\n' "$remote" >&2
            printf '%s' "$img"
            return 0
        fi
        printf 'testenv: %s is not in the registry — building it\n' "$remote" >&2
    fi

    printf 'testenv: building image %s\n' "$img" >&2
    podman build -q -t "$img" -f "$TESTENV_DIR/Containerfile" "$HERE" >&2 || {
        printf 'testenv: image build failed — check Containerfile in %s\n' "$TESTENV_DIR" >&2
        return 1
    }
    printf '%s' "$img"
}

# Wait up to N half-second ticks for CMD ARGS to exit 0.
_wait_for() {   # _wait_for N CMD ARGS... -> 0 if satisfied within N*0.5s, 1 if timed out
    local n="$1"; shift
    local i=0
    while [ "$i" -lt "$n" ]; do
        "$@" >/dev/null 2>&1 && return 0
        sleep 0.5
        i=$((i+1))
    done
    return 1
}

cmd_up() {
    local name="$_DEFAULT_NAME" checkout=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --name)     name="$2";     shift 2 ;;
            --checkout) checkout="$2"; shift 2 ;;
            *) printf 'testenv up: unknown argument: %s\n' "$1" >&2; return 1 ;;
        esac
    done

    # Default checkout: the repository root this script lives in.
    [ -z "$checkout" ] && checkout="$(cd "$HERE/.." && pwd -P)"

    if podman container exists "$name" 2>/dev/null; then
        printf 'testenv: %s already exists; nothing to do\n' "$name" >&2
        return 0
    fi

    local img; img="$(_ensure_image)" || return 1

    # Named volumes for the cargo registry and git sources. They are created by podman
    # on first use and reused on every subsequent `up`, so crate downloads only happen
    # once across the lifetime of the volume.
    local vol_reg="${name}-cargo-reg"
    local vol_git="${name}-cargo-git"

    # PIDS LIMIT: rootless podman defaults to 2048. Running many parallel test suites
    # simultaneously — each spawning bd, git, python3, and bash subshells, plus systemd
    # and the dolt test-fixture server in the baseline — exhausts the default: fork()
    # returns EAGAIN, suites die with "Resource temporarily unavailable", and dolt cannot
    # start because systemd cannot fork it. Measured: 11 heavy parallel suites consumed
    # ~1 300 PIDs (baseline ~100 + ~100/suite); 52 suites exhausted the 2 048 default.
    # 8 192 is 4x the default and fits well within a typical user nproc ceiling (~60 000).
    podman run -d \
        --name "$name" \
        --systemd=true \
        --pids-limit 8192 \
        --volume "${checkout}:${_CONTAINER_CHECKOUT}:z" \
        --volume "${vol_reg}:${_CONTAINER_CARGO}/registry" \
        --volume "${vol_git}:${_CONTAINER_CARGO}/git" \
        "$img" >/dev/null

    # Wait for system systemd to reach basic.target (~1s normally). Failing here
    # means something is wrong with the image or the cgroup setup, not the test code.
    if ! _wait_for 20 podman exec "$name" systemctl is-active basic.target; then
        printf 'testenv: system systemd did not reach basic.target\n' >&2
        podman stop "$name" >/dev/null 2>&1 || true
        podman rm   "$name" >/dev/null 2>&1 || true
        return 1
    fi

    # loginctl enable-linger writes /var/lib/systemd/linger/spirauser, which causes the
    # system systemd to start user@1001.service and keep it running without a login session.
    podman exec "$name" loginctl enable-linger "$_SPIRA_USER" 2>/dev/null || true

    # Trust the bind-mounted checkout. The host UID owning the files may differ from
    # spirauser (1001) inside the container; git refuses operations on directories it
    # considers dubiously owned. safe.directory is set at the system level so it applies
    # to all users, including spirauser running test suites.
    podman exec "$name" git config --system --add safe.directory "$_CONTAINER_CHECKOUT" \
        >/dev/null 2>&1 || true

    # CARGO CACHE OWNERSHIP. The registry and git sources are named volumes mounted BELOW
    # CARGO_HOME, and podman creates an empty volume's mountpoint as root:root when the
    # image carries no directory at that path to seed the ownership from. Suites run as
    # spirauser, so cargo's first write died with
    #   error: failed to create directory `/var/spira/cargo/registry/cache/...`
    #   Caused by: Permission denied (os error 13)
    # and every Rust suite went red. test-artifact-install.sh had already met this and
    # worked around it privately by pointing CARGO_HOME at a temp directory of its own,
    # which is why it stayed green and the defect stayed hidden.
    #
    # Chowning after the mount is what makes the volume usable whether it is fresh or being
    # reused, and whether or not the image happens to carry the directory. The owner is
    # checked first so the recursive repair runs once on a wrong-owned volume rather than
    # walking a warm registry of thousands of crate files on every `up`.
    podman exec "$name" bash -c '
        mkdir -p "$1/target" 2>/dev/null
            for d in "$1" "$1/registry" "$1/git" "$1/target"; do
            [ -d "$d" ] || continue
            [ "$(stat -c %u "$d" 2>/dev/null)" = "$2" ] || chown -R "$2:$2" "$d"
        done' _ "$_CONTAINER_CARGO" "$_SPIRA_UID" >/dev/null 2>&1 || true

    # Wait for the user session manager to become active (~1-2s after enable-linger).
    if _wait_for 20 podman exec "$name" systemctl is-active "user@${_SPIRA_UID}.service"; then
        printf 'testenv: user@%s.service active; systemctl --user ready\n' "$_SPIRA_UID" >&2
    else
        printf 'testenv: WARNING user@%s.service did not start\n' "$_SPIRA_UID" >&2
        printf 'testenv: probe exits non-zero; use SPIRA_SYSTEMCTL stub for systemctl --user\n' >&2
    fi
}

cmd_down() {
    local name="$_DEFAULT_NAME" rm_volumes=0
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --name)    name="$2"; shift 2 ;;
            --volumes) rm_volumes=1; shift ;;
            *) printf 'testenv down: unknown argument: %s\n' "$1" >&2; return 1 ;;
        esac
    done

    # stop and rm are no-ops when the container does not exist, satisfying the
    # "second down exits 0" requirement without extra checks.
    podman stop "$name" >/dev/null 2>&1 || true
    podman rm   "$name" >/dev/null 2>&1 || true

    if [ "$rm_volumes" = 1 ]; then
        podman volume rm "${name}-cargo-reg" >/dev/null 2>&1 || true
        podman volume rm "${name}-cargo-git" >/dev/null 2>&1 || true
    fi
}

cmd_exec() {
    local name="$_DEFAULT_NAME" user="root"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --name) name="$2"; shift 2 ;;
            --user) user="$2"; shift 2 ;;
            --) shift; break ;;
            -*) printf 'testenv exec: unknown option: %s\n' "$1" >&2; return 1 ;;
            *)  break ;;
        esac
    done

    [ $# -gt 0 ] || { printf 'testenv exec: command required\n' >&2; return 1; }

    # When running as spirauser, inject the environment variables that make systemctl
    # --user and cargo connect to the right places. Running as root needs none of these.
    if [ "$user" = "$_SPIRA_USER" ]; then
        podman exec --user "$user" \
            -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
            -e "DBUS_SESSION_BUS_ADDRESS=unix:path=${_USER_RUNTIME}/bus" \
            -e "CARGO_HOME=${_CONTAINER_CARGO}" \
            -e "CARGO_TARGET_DIR=${_CONTAINER_CARGO_TARGET}" \
            "$name" "$@"
    else
        podman exec --user "$user" "$name" "$@"
    fi
}

cmd_probe() {
    local name="$_DEFAULT_NAME"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --name) name="$2"; shift 2 ;;
            *) printf 'testenv probe: unknown argument: %s\n' "$1" >&2; return 1 ;;
        esac
    done

    # Confirm that user@UID.service is active AND that systemctl --user can connect to
    # the session bus. Both must hold: the service being active is necessary but not
    # sufficient if the bus socket is not yet accepting connections.
    podman exec "$name" systemctl is-active "user@${_SPIRA_UID}.service" >/dev/null 2>&1 && \
    podman exec --user "$_SPIRA_USER" \
        -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
        "$name" systemctl --user is-active default.target >/dev/null 2>&1
}

cmd_scratch() {
    # scratch — create a fresh throwaway bd database, print its path, and exit.
    # The database persists after exit; the caller is responsible for cleanup:
    #   path="$(testenv.sh scratch)"; bd -C "$path" list; rm -rf "$path"
    #
    # A session running suites sets TESTDB_SHARED=1 so suites reset the shared fixture
    # rather than rebuilding it. scratch always wants a NEW database, independent of any
    # suite's fixture, so override unconditionally before sourcing testdb.sh.
    TESTDB_SHARED=0
    TESTDB_NAME=
    TESTDB_DIR=
    TESTDB_BASELINE=
    TESTDB_BIN=
    TESTDB_MODE=

    . "$HERE/testdb.sh"

    testdb_available || {
        printf 'testenv scratch: no bd engine available\n' >&2
        printf 'testenv scratch:   embedded: install bd-embedded (npm install -g @beads/bd)\n' >&2
        printf 'testenv scratch:   server: set SPIRA_TESTDB_DATA in spira.conf\n' >&2
        return 1
    }

    testdb_up scratch || {
        printf 'testenv scratch: database build failed\n' >&2
        return 1
    }

    printf '%s\n' "$SPIRA_DB"
    # Do NOT call testdb_drop: the caller holds the path and cleans it up.
    # Cleanup: rm -rf the printed path when done.
}

cmd_shell() {
    # shell — drop into a subshell with SPIRA_DB, SPIRA_RUN and SPIRA_SPOOL pointing at
    # throwaway directories. Every harness command typed inside targets the fixture.
    # Leaving the shell (exit or Ctrl-D) tears the fixture down.
    #
    # Same override rationale as cmd_scratch: always build fresh, never reset a suite's fixture.
    TESTDB_SHARED=0
    TESTDB_NAME=
    TESTDB_DIR=
    TESTDB_BASELINE=
    TESTDB_BIN=
    TESTDB_MODE=

    . "$HERE/testdb.sh"

    testdb_available || {
        printf 'testenv shell: no bd engine available\n' >&2
        printf 'testenv shell:   embedded: install bd-embedded (npm install -g @beads/bd)\n' >&2
        printf 'testenv shell:   server: set SPIRA_TESTDB_DATA in spira.conf\n' >&2
        return 1
    }

    testdb_up shell || {
        printf 'testenv shell: database build failed\n' >&2
        return 1
    }

    local scratch_run scratch_spool
    scratch_run="$(mktemp -d)"
    scratch_spool="$(mktemp -d)"

    # testdb_drop + scratch dirs on exit, regardless of how the shell exits.
    # TESTDB_SHARED is already 0, so testdb_drop will actually drop.
    #
    # Double-quoted so the paths expand NOW (printf %q for safe quoting), while
    # scratch_run and scratch_spool are still in scope. Single-quoting defers
    # expansion to when the trap fires — by then cmd_shell has returned and both
    # names are out of scope; under set -u that is a fatal unbound-variable error
    # that kills the trap before testdb_drop runs, leaking the fixture.
    trap "testdb_drop; rm -rf $(printf '%q' "$scratch_run") $(printf '%q' "$scratch_spool")" EXIT INT TERM

    printf 'testenv: scratch shell — harness commands use throwaway database\n' >&2
    printf 'testenv:   SPIRA_DB=%s\n' "$SPIRA_DB" >&2
    printf 'testenv:   SPIRA_RUN=%s\n' "$scratch_run" >&2
    printf 'testenv:   SPIRA_SPOOL=%s\n' "$scratch_spool" >&2
    printf 'testenv:   exit or Ctrl-D to tear down\n' >&2

    # Use -i (interactive) when stdin is a terminal AND no arguments were given so
    # the prompt appears and job control works. When arguments are provided (e.g.
    # -c 'cmd'), pass them straight to bash without -i. Without -i, piped stdin
    # works fine for scripted use (test suites).
    if [ -t 0 ] && [ $# -eq 0 ]; then
        SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$scratch_run" SPIRA_SPOOL="$scratch_spool" \
        PS1="[scratch] \$ " bash --norc --noprofile -i
    else
        SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$scratch_run" SPIRA_SPOOL="$scratch_spool" \
        bash --norc --noprofile "$@"
    fi
    return $?
}

case "${1:-}" in
    up)      shift; cmd_up      "$@" ;;
    down)    shift; cmd_down    "$@" ;;
    exec)    shift; cmd_exec    "$@" ;;
    probe)   shift; cmd_probe   "$@" ;;
    scratch) shift; cmd_scratch "$@" ;;
    shell)   shift; cmd_shell   "$@" ;;
    tag)     shift; cmd_tag              ;;
    image)   shift; cmd_image            ;;
    publish) shift; cmd_publish          ;;
    *)
        printf 'usage: testenv.sh up|down|exec|probe|scratch|shell|tag|image|publish [OPTIONS]\n' >&2
        printf '  up      [--name NAME] [--checkout PATH]\n' >&2
        printf '  down    [--name NAME] [--volumes]\n' >&2
        printf '  exec    [--name NAME] [--user USER] CMD ARGS...\n' >&2
        printf '  probe   [--name NAME]\n' >&2
        printf '  scratch          # print a throwaway SPIRA_DB path; caller cleans up\n' >&2
        printf '  shell            # subshell with SPIRA_DB/RUN/SPOOL on throwaway paths\n' >&2
        printf '  tag              # print the computed image tag (the build closure hash)\n' >&2
        printf '  image            # acquire the image (pull if configured, else build); print its ref\n' >&2
        printf '  publish          # push the image to SPIRA_TESTENV_REGISTRY under that tag\n' >&2
        exit 1 ;;
esac
