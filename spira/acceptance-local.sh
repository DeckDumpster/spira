#!/usr/bin/env bash
# acceptance-local.sh — the pre-release gate: run acceptance-run.sh phase A
# against a tarball built from a working tree, inside a real testenv
# container, before any release is cut (or to reproduce one that failed).
#
# Every acceptance failure found on a real release run — install.sh phase 5
# assuming a git checkout, the scope label read from the release directory
# name, a broken pre-acceptance step, the probe bead labelled with the wrong
# scope, broker units surviving uninstall — would have failed a run of this
# script in minutes rather than the 25-40 it costs to learn on a real VM.
#
# Usage: acceptance-local.sh <tree>
#
#   <tree>  a working tree of this repository (a worktree, a plain checkout,
#           or the harness itself) to build the tarball from and mount at
#           /workspace, so acceptance-run.sh and acceptance-agent.sh run from
#           the tree under test.
#
# 1. Build the release tarball from <tree> via build-tarball.sh, under the
#    Rust toolchain release.yml pins (SPIRA_RELEASE_RUST_TOOLCHAIN) — the
#    same binaries the release job would produce.
# 2. Bring up a testenv container: a real systemd user session, the same
#    image CI's suites run against.
# 3. Set up a scratch repo the same way acceptance-ci.sh does for the forge
#    run, then run acceptance-run.sh phase A inside the container against the
#    built tarball (--tarball, so nothing is downloaded) with --agent
#    acceptance-agent.sh, so no model credential is needed. No --prev-tag: a
#    local rehearsal has no predecessor to upgrade from, so only phase A runs.
# 4. Report the same PASS/FAIL lines and exit code acceptance-run.sh always
#    prints; on a phase A FAIL, copy its forensics snapshot out to the host.
#
# EXIT
#   0  phase A passed
#   1  phase A failed — see the FAIL lines above and the forensics dir printed
#   2  usage error, tarball build failed, or the container could not come up
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"

TREE="${1:-}"
if [ -z "$TREE" ] || [ ! -d "$TREE" ]; then
    printf 'usage: acceptance-local.sh <tree>\n' >&2
    exit 2
fi
TREE="$(cd "$TREE" && pwd -P)"
if [ ! -f "$TREE/spira/build-tarball.sh" ]; then
    printf 'acceptance-local: %s does not look like a spira checkout (no spira/build-tarball.sh)\n' \
        "$TREE" >&2
    exit 2
fi

CNAME="${SPIRA_ACCEPTANCE_LOCAL_NAME:-spira-acceptance-local}"
FORENSICS_OUT="${SPIRA_ACCEPTANCE_LOCAL_FORENSICS:-$SPIRA_RUN/acceptance-local-forensics}"

_wd="$(mktemp -d)"
_al_cleanup() {
    bash "$HERE/testenv.sh" down --name "$CNAME" >/dev/null 2>&1 || true
    rm -rf "$_wd"
}
trap _al_cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# 1. BUILD — on the host, under the pinned release toolchain. Compiling is not
# what the container buys here; the container below is for the install/run
# environment, which needs the real systemd user session install.sh drives.
# ---------------------------------------------------------------------------
log "acceptance-local: building $TREE under Rust $SPIRA_RELEASE_RUST_TOOLCHAIN"
if command -v rustup >/dev/null 2>&1; then
    rustup toolchain install "$SPIRA_RELEASE_RUST_TOOLCHAIN" --profile minimal >&2
    _al_got="$(RUSTUP_TOOLCHAIN="$SPIRA_RELEASE_RUST_TOOLCHAIN" rustc --version 2>/dev/null \
        | awk '{print $2}')"
    if [ "$_al_got" != "$SPIRA_RELEASE_RUST_TOOLCHAIN" ]; then
        printf 'acceptance-local: could not select Rust %s (rustup reports %s)\n' \
            "$SPIRA_RELEASE_RUST_TOOLCHAIN" "${_al_got:-none}" >&2
        exit 2
    fi
    RUSTUP_TOOLCHAIN="$SPIRA_RELEASE_RUST_TOOLCHAIN" make -C "$TREE" build >&2 || {
        printf 'acceptance-local: workspace build failed\n' >&2
        exit 2
    }
else
    printf 'acceptance-local: rustup not on PATH — building with whatever cargo resolves to\n' >&2
    printf 'acceptance-local:   (%s), not the pinned %s release toolchain\n' \
        "$(rustc --version 2>/dev/null || printf 'no rustc')" "$SPIRA_RELEASE_RUST_TOOLCHAIN" >&2
    make -C "$TREE" build >&2 || {
        printf 'acceptance-local: workspace build failed\n' >&2
        exit 2
    }
fi

_al_tarball="$(bash "$TREE/spira/build-tarball.sh" build --workspace "$TREE" \
    --output "$_wd" --repo-name "${SPIRA_HOME_REPO}")" || {
    printf 'acceptance-local: build-tarball.sh failed\n' >&2
    exit 2
}
log "acceptance-local: tarball built: $(basename "$_al_tarball")"

# ---------------------------------------------------------------------------
# 2. CONTAINER — a real systemd user session, the same image CI's suites run
# against. <tree> is mounted at /workspace so acceptance-run.sh and
# acceptance-agent.sh run from the source tree under test, exactly as
# acceptance.yml's own checkout drives a tarball downloaded separately.
# ---------------------------------------------------------------------------
bash "$HERE/testenv.sh" down --name "$CNAME" >/dev/null 2>&1 || true
log "acceptance-local: starting container $CNAME"
bash "$HERE/testenv.sh" up --name "$CNAME" --checkout "$TREE" >&2 || {
    printf 'acceptance-local: container did not come up\n' >&2
    exit 2
}

_al_ctar="/tmp/$(basename "$_al_tarball")"
podman cp "$_al_tarball" "$CNAME:$_al_ctar" || {
    printf 'acceptance-local: could not copy the tarball into the container\n' >&2
    exit 2
}

_al_tag="local-$(git -C "$TREE" rev-parse --short=12 HEAD 2>/dev/null || printf unknown)-$(date -u +%Y%m%dT%H%M%SZ)"

# ---------------------------------------------------------------------------
# 3. RUN — the same scratch-repo shape acceptance-ci.sh builds for the forge
# run, then acceptance-run.sh phase A against the tarball.
# ---------------------------------------------------------------------------
bash "$HERE/testenv.sh" exec --name "$CNAME" --user spirauser bash -c '
set -uo pipefail
git init --bare --initial-branch=main "$HOME/scratch-repo.git" >/dev/null
git clone "$HOME/scratch-repo.git" "$HOME/scratch-repo" >/dev/null
git -C "$HOME/scratch-repo" config user.email "acceptance@spira.local"
git -C "$HOME/scratch-repo" config user.name "Spira Acceptance"
git -C "$HOME/scratch-repo" commit --allow-empty -m "init" >/dev/null
git -C "$HOME/scratch-repo" push origin main >/dev/null
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/spira"
printf "scratch-repo | %s | push | origin/main | |\n" "$HOME/scratch-repo" \
    > "${XDG_CONFIG_HOME:-$HOME/.config}/spira/repo-map"
' || {
    printf 'acceptance-local: could not set up the scratch repo in the container\n' >&2
    exit 2
}

log "acceptance-local: running acceptance-run.sh phase A (tag=$_al_tag)"
bash "$HERE/testenv.sh" exec --name "$CNAME" --user spirauser bash -c "
set -uo pipefail
export SPIRA_ACCEPTANCE_FORENSICS=\"\$HOME/acceptance-forensics\"
mkdir -p \"\$SPIRA_ACCEPTANCE_FORENSICS\"
exec bash /workspace/spira/acceptance-run.sh '$_al_tag' \
    --scratch-repo \"\$HOME/scratch-repo\" \
    --tarball '$_al_ctar' \
    --agent /workspace/spira/acceptance-agent.sh \
    --bd-db \"\$HOME/spira-acceptance-test-db\"
"
_al_rc=$?

# ---------------------------------------------------------------------------
# 4. FORENSICS on FAIL. A build or container-startup failure (exit 2) never
# reaches here — there is no run to have left forensics behind.
# ---------------------------------------------------------------------------
if [ "$_al_rc" -eq 1 ]; then
    rm -rf "$FORENSICS_OUT"
    mkdir -p "$(dirname "$FORENSICS_OUT")"
    _al_home="$(bash "$HERE/testenv.sh" exec --name "$CNAME" --user spirauser \
        bash -c 'printf %s "$HOME"' 2>/dev/null)"
    if [ -n "$_al_home" ] \
        && podman cp "$CNAME:$_al_home/acceptance-forensics" "$FORENSICS_OUT" 2>/dev/null; then
        printf 'forensics copied to: %s\n' "$FORENSICS_OUT"
    else
        printf 'acceptance-local: could not copy forensics out of the container\n' >&2
    fi
fi

exit "$_al_rc"
