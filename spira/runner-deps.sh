#!/usr/bin/env bash
#
# runner-deps.sh — make a cold machine capable of running the suite corpus.
#
#   runner-deps.sh            install what is missing, then assert the properties
#   runner-deps.sh --check    assert only; install nothing (exit 1 on any gap)
#
# WHY THIS IS A SCRIPT IN THE REPOSITORY AND NOT A MACHINE IMAGE. CI runs on a
# virtual machine that has never run anything else and is destroyed afterwards, so
# nothing survives between runs except what is written down here. Baked into the
# image instead, the dependency goes back to being invisible: it is satisfied on
# one box by a fix nobody recorded, and the first machine that lacks it fails in a
# way that looks like the code broke. Declared here it is reviewed in a pull
# request like anything else.
#
# IT IS IDEMPOTENT. On a machine that already satisfies everything this is a
# handful of `command -v` calls, so there is no reason for a caller to guess
# whether it needs to run.
#
# WHAT IT ASSERTS IS THE PROPERTY, NOT THE PACKAGE. A box that satisfies a
# requirement another way — a different overlay driver, podman from a backport —
# is not forced onto this file's choices. The package list is how the gap gets
# closed when it is open, not the definition of being closed.
#
# THE FAILURE IT EXISTS TO PREVENT. testenv-batch.sh exits 2 when the container
# does not come up, and the gate maps that to 75, meaning "the harness broke, run
# it again". A missing host dependency is not transient: it would report as a
# harness fault on every run forever, and the log would say to retry. So every
# check here fails with 1 — a real failure, attributed to the step that owns it.
set -uo pipefail

CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

_u="$(id -un)"
_uid="$(id -u)"
rc=0
note() { printf 'runner-deps: %s\n' "$1"; }
gap()  { printf 'runner-deps: MISSING %s\n' "$1" >&2; rc=1; }

# ROOTLESS PODMAN IS THE SUBSTRATE. testenv.sh runs the image with --systemd=true,
# so PID 1 inside the container is system systemd and a user manager starts under
# it. uidmap supplies newuidmap/newgidmap, without which rootless refuses to map
# anything; the network and storage helpers are what podman falls back to when the
# kernel cannot give it a native path.
PKGS=(podman uidmap fuse-overlayfs slirp4netns catatonit
      git curl ca-certificates python3 jq)

if [ "$CHECK_ONLY" -eq 0 ]; then
    _missing=()
    for p in "${PKGS[@]}"; do
        dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q '^install ok installed$' \
            || _missing+=("$p")
    done
    if [ "${#_missing[@]}" -gt 0 ]; then
        note "installing: ${_missing[*]}"
        sudo DEBIAN_FRONTEND=noninteractive apt-get update -q \
            && sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq --no-install-recommends \
                 "${_missing[@]}" \
            || { printf 'runner-deps: apt-get failed\n' >&2; exit 1; }
    else
        note "packages already present"
    fi

    # SUBORDINATE ID RANGES. Rootless podman maps container uids into a range the
    # host has delegated to this user. A user created by cloud-init rather than by
    # `useradd` often has no range at all, and podman's error for that names an
    # internal storage path rather than the missing delegation.
    if ! grep -q "^${_u}:" /etc/subuid 2>/dev/null; then
        note "granting subordinate uid range to ${_u}"
        sudo usermod --add-subuids 100000-165535 "$_u" || true
    fi
    if ! grep -q "^${_u}:" /etc/subgid 2>/dev/null; then
        note "granting subordinate gid range to ${_u}"
        sudo usermod --add-subgids 100000-165535 "$_u" || true
    fi

    # LINGERING, AND THE RUNTIME DIRECTORY IT CREATES. The CI runner is a service,
    # not a login session, so /run/user/<uid> need not exist and XDG_RUNTIME_DIR
    # need not be set. Rootless podman keeps its own state there. enable-linger
    # makes systemd create and keep the directory for a user with no session.
    loginctl enable-linger "$_u" >/dev/null 2>&1 || true
fi

# ── assertions ───────────────────────────────────────────────────────────────
# Everything below runs in both modes: an install that reported success and left
# the machine incapable is the failure this section exists to catch.

command -v podman >/dev/null 2>&1 || gap "podman is not on PATH"

# XDG_RUNTIME_DIR is exported for the REST OF THE JOB, not just this shell. A
# later step gets a fresh shell, so setting it here and stopping would leave
# podman to pick a fallback and the container to come up in a different place
# than the one this script verified.
if [ -d "/run/user/${_uid}" ]; then
    export XDG_RUNTIME_DIR="/run/user/${_uid}"
    [ -n "${GITHUB_ENV:-}" ] && printf 'XDG_RUNTIME_DIR=%s\n' "$XDG_RUNTIME_DIR" >> "$GITHUB_ENV"
else
    gap "/run/user/${_uid} does not exist — rootless podman has nowhere to keep its state"
fi

# CGROUP V2 WITH DELEGATION. A --systemd=true container needs to create cgroups
# of its own. Under v1, or under v2 without the controllers delegated to this
# user's slice, systemd inside the container starts and then fails to bring up
# any unit — including the user manager the suites need, which surfaces as a
# probe timeout rather than as a cgroup problem.
if [ ! -e /sys/fs/cgroup/cgroup.controllers ]; then
    gap "cgroup v2 unified hierarchy (/sys/fs/cgroup/cgroup.controllers absent)"
else
    _deleg="/sys/fs/cgroup/user.slice/user-${_uid}.slice/cgroup.controllers"
    if [ -e "$_deleg" ]; then
        grep -qw memory "$_deleg" 2>/dev/null \
            || gap "the memory controller is not delegated to user-${_uid}.slice"
        grep -qw pids "$_deleg" 2>/dev/null \
            || gap "the pids controller is not delegated to user-${_uid}.slice"
    else
        gap "no delegated cgroup for user-${_uid}.slice — lingering did not take"
    fi
fi

# THE POSITIVE CONTROL. Everything above is an inference from a file or a path;
# this is the only step that demonstrates the capability rather than predicting
# it. A check that finds nothing wrong must first prove it could have: if podman
# cannot actually start a container here, the gaps above were the wrong gaps.
if command -v podman >/dev/null 2>&1; then
    if ! podman info >/dev/null 2>&1; then
        gap "podman info fails — the runtime is installed but cannot start"
    elif [ "$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null)" != "true" ]; then
        gap "podman is not running rootless"
    else
        note "podman rootless ok ($(podman --version 2>/dev/null))"
    fi
fi

if [ "$rc" -ne 0 ]; then
    printf 'runner-deps: this machine cannot run the suites. The gaps above are host\n' >&2
    printf 'runner-deps: configuration, not a fault in the branch under test.\n' >&2
    exit 1
fi
note "ok"
