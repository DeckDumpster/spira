#!/usr/bin/env bash
# acceptance-local.sh — the pre-release gate: run `release acceptance` phase A
# against a tarball built from a working tree, inside a real testenv
# container, before any release is cut (or to reproduce one that failed).
#
# Every acceptance failure found on a real release run — install.sh phase 5
# assuming a git checkout, the scope label read from the release directory
# name, a broken pre-acceptance step, the probe bead labelled with the wrong
# scope, broker units surviving uninstall — would have failed a run of this
# script in minutes rather than the 25-40 it costs to learn on a real VM.
#
# Usage: acceptance-local.sh <tree> [--predecessor <tag> [--predecessor-tarball <path>]]
#        acceptance-local.sh start <round> <tree> [...same args...]
#        acceptance-local.sh stop <round>
#
#   <tree>  a working tree of this repository (a worktree, a plain checkout,
#           or the harness itself) to build the tarball from and mount at
#           /workspace, so acceptance-agent.sh runs from the tree under test; the
#           run itself is the built tarball's own bin/release (sp-ak7qm).
#
#   start <round> <tree> [...]  runs this build+run exactly as the plain form does, but
#           detached inside a named systemd --user transient unit (spira-acc-<round>-<ts>),
#           logging to $SPIRA_RUN/acceptance-local-<round>.log. Use this for a run meant to
#           outlive the session that started it.
#   stop <round>  stops every spira-acc-<round>-* unit — its whole cgroup, nothing else.
#           This is the ONLY sanctioned way to end a run started with `start`. Never derive
#           a pid's PPid and kill it: an orphan's parent is the user manager itself, and
#           SIGTERM to it stops the whole session, not the leftover run (sp-kb0k5).
#
# 1. Build the release tarball from <tree> via build-tarball.sh, under the
#    Rust toolchain release.yml pins (SPIRA_RELEASE_RUST_TOOLCHAIN) — the
#    same binaries the release job would produce.
# 2. Bring up a testenv container: a real systemd user session, the same
#    image CI's suites run against.
# 3. Set up a scratch repo the same way acceptance-ci.sh does for the forge
#    run, then run the tarball's own `release acceptance` phase A inside the container against the
#    built tarball (--tarball, so nothing is downloaded) with --agent
#    acceptance-agent.sh, so no model credential is needed. Without
#    --predecessor only phase A runs.
#
# --predecessor <tag> (sp-oskp7) runs PHASES B, C AND D too, exactly as the forge
# run does, so an upgrade/rollback failure reproduces here in minutes instead of
# a 40-minute remote cycle:
#   - the published <tag> tarball is downloaded ON THE HOST with gh (the container
#     has no forge credential), or taken from --predecessor-tarball, and copied in;
#   - the release under test is named spira-release-<its tarball stem> and every
#     deploy of either release is handed its local tarball (deploy.sh --tarball);
#   - /workspace is a self-contained CLONE of <tree> at its HEAD, carrying the
#     repository's release tags plus a tag for the local release on HEAD — the
#     forge run's checkout carries both, and skew reads release currency and
#     the MANIFEST commit from them. A worktree's own .git does not resolve inside
#     the container, so mounting <tree> itself would leave skew with no tags.
#   Refused when <tree> has uncommitted changes: the tarball and the mounted
#   scripts must be the same commit.
# 4. Report the same PASS/FAIL lines and exit code `release acceptance` always
#    prints; on a phase A FAIL, copy its forensics snapshot out to the host.
#
# EXIT
#   0  every phase run passed (A, or A-D with --predecessor)
#   1  a phase failed — see the FAIL lines above and the forensics dir printed
#   2  usage error, tarball build failed, or the container could not come up
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"

if [ "${1:-}" = stop ]; then
    ROUND="${2:?usage: acceptance-local.sh stop <round>}"
    named_unit_stop "spira-acc-$ROUND-*"
    exit $?
fi
if [ "${1:-}" = start ]; then
    shift
    ROUND="${1:?usage: acceptance-local.sh start <round> <tree> [...]}"; shift
    UNIT="spira-acc-$ROUND-$(date +%s)"
    LOG="$SPIRA_RUN/acceptance-local-$ROUND.log"
    # THE ENVIRONMENT IS EXPLICIT, NOT INHERITED (like every other systemd-run seam here) —
    # pass what a detached run of this script itself reads and nothing else.
    "${SPIRA_SUMMON:-systemd-run}" --user --collect --quiet --unit="$UNIT" \
        --property=StandardOutput="append:$LOG" --property=StandardError="append:$LOG" \
        --setenv=PATH="$PATH" --setenv=HOME="$HOME" \
        --setenv=SPIRA_HOME="$SPIRA_HOME" --setenv=SPIRA_RUN="$SPIRA_RUN" \
        --setenv=SPIRA_CONF="${SPIRA_CONF:-}" --setenv=SPIRA_DB="${SPIRA_DB:-}" \
        --setenv=SPIRA_REPO="${SPIRA_REPO:-}" --setenv=SPIRA_HOME_REPO="$(spira_home_repo 2>/dev/null)" \
        --setenv=SPIRA_RELEASE_RUST_TOOLCHAIN="${SPIRA_RELEASE_RUST_TOOLCHAIN:-}" \
        --setenv=SPIRA_ACCEPTANCE_LOCAL_NAME="${SPIRA_ACCEPTANCE_LOCAL_NAME:-}" \
        --setenv=SPIRA_ACCEPTANCE_LOCAL_FORENSICS="${SPIRA_ACCEPTANCE_LOCAL_FORENSICS:-}" \
        --setenv=GH_TOKEN="${GH_TOKEN:-}" \
        -- "$(command -v acceptance-local.sh)" "$@" || exit 1   # a transient unit has no launcher PATH: hand it the resolved name
    printf 'acceptance-local: started %s\n' "$UNIT"
    printf 'acceptance-local: log at %s\n' "$LOG"
    printf 'acceptance-local: stop with: acceptance-local.sh stop %s\n' "$ROUND"
    exit 0
fi

TREE=""; PRED=""; PRED_TARBALL=""
while [ $# -gt 0 ]; do
    case "$1" in
        --predecessor)          PRED="${2:-}"; shift 2 || shift ;;
        --predecessor=*)        PRED="${1#--predecessor=}"; shift ;;
        --predecessor-tarball)  PRED_TARBALL="${2:-}"; shift 2 || shift ;;
        --predecessor-tarball=*) PRED_TARBALL="${1#--predecessor-tarball=}"; shift ;;
        -*) printf 'acceptance-local: unknown option: %s\n' "$1" >&2; exit 2 ;;
        *)  [ -z "$TREE" ] && TREE="$1" || { printf 'acceptance-local: too many arguments\n' >&2; exit 2; }
            shift ;;
    esac
done
if [ -z "$TREE" ] || [ ! -d "$TREE" ]; then
    printf 'usage: acceptance-local.sh <tree> [--predecessor <tag> [--predecessor-tarball <path>]]\n' >&2
    exit 2
fi
if [ -n "$PRED_TARBALL" ] && [ -z "$PRED" ]; then
    printf 'acceptance-local: --predecessor-tarball needs --predecessor <tag>\n' >&2; exit 2
fi
if [ -n "$PRED" ]; then
    case "$PRED" in spira-release-spira-*) ;; *)
        printf 'acceptance-local: --predecessor %s is not a spira-release-spira-* tag\n' "$PRED" >&2; exit 2 ;;
    esac
    if [ -n "$PRED_TARBALL" ] && [ ! -f "$PRED_TARBALL" ]; then
        printf 'acceptance-local: --predecessor-tarball: no such file: %s\n' "$PRED_TARBALL" >&2; exit 2
    fi
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
    testenv container down --name "$CNAME" >/dev/null 2>&1 || true
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
# against. <tree> is mounted at /workspace so acceptance-agent.sh runs from the
# source tree under test, exactly as acceptance.yml's own checkout does.
# ---------------------------------------------------------------------------
testenv container down --name "$CNAME" >/dev/null 2>&1 || true
log "acceptance-local: starting container $CNAME"
# WHAT /workspace IS, AND THE PREDECESSOR (phases B-D only).
_al_mount="$TREE"
_al_pred_file=""
if [ -n "$PRED" ]; then
    _al_stem="$(basename "$_al_tarball" .tar.gz)"
    if git -C "$TREE" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        if [ -n "$(git -C "$TREE" status --porcelain --untracked-files=no 2>/dev/null)" ]; then
            printf 'acceptance-local: %s has uncommitted changes — commit them: phases B-D mount a\n' "$TREE" >&2
            printf 'acceptance-local:   clone of HEAD, which must be what the tarball was built from\n' >&2
            exit 2
        fi
        _al_mount="$_wd/workspace"
        # batch-job: clone and tag fetch of the release tree into the acceptance mount
        git clone -q "$TREE" "$_al_mount" 2>/dev/null \
            && timeout 5 git -C "$_al_mount" fetch -q "$TREE" 'refs/tags/*:refs/tags/*' 2>/dev/null \
            && git -C "$_al_mount" checkout -q --detach "$(git -C "$TREE" rev-parse HEAD)" 2>/dev/null \
            && git -C "$_al_mount" tag -f "spira-release-$_al_stem" HEAD >/dev/null 2>&1 || {
            printf 'acceptance-local: could not build the self-contained clone of %s\n' "$TREE" >&2
            exit 2
        }
        log "acceptance-local: /workspace is a clone of $(git -C "$TREE" rev-parse --short HEAD) with its release tags + spira-release-$_al_stem"
    fi
    if [ -n "$PRED_TARBALL" ]; then
        _al_pred_file="$PRED_TARBALL"
    else
        mkdir -p "$_wd/pred"
        log "acceptance-local: downloading predecessor $PRED on the host"
        # batch-job: downloads the predecessor release tarball
        ( cd "$TREE" && gh release download "$PRED" --pattern 'spira-*.tar.gz' --dir "$_wd/pred" ) >&2 || {
            printf 'acceptance-local: could not download %s with gh on the host — is gh authenticated\n' "$PRED" >&2
            printf 'acceptance-local:   for this repository? (or pass --predecessor-tarball <path>)\n' >&2
            exit 2
        }
        _al_pred_file="$(ls "$_wd/pred"/spira-*.tar.gz 2>/dev/null | head -1)"
        [ -n "$_al_pred_file" ] || { printf 'acceptance-local: %s has no spira-*.tar.gz asset\n' "$PRED" >&2; exit 2; }
    fi
fi

testenv container up --name "$CNAME" --checkout "$_al_mount" >&2 || {
    printf 'acceptance-local: container did not come up\n' >&2
    exit 2
}

_al_hostdb_port="${SPIRA_ACCEPTANCE_LOCAL_HOSTDB_PORT:-3307}"
if ! testenv container exec --name "$CNAME" bash -c 'timeout 3 bash -c "exec 3<>/dev/tcp/127.0.0.1/$1" 2>/dev/null; [ "$?" -ne 0 ]' _ "$_al_hostdb_port" >/dev/null 2>&1; then
    printf 'acceptance-local: the container can connect to 127.0.0.1:%s — the host database is reachable; refusing to run\n' \
        "$_al_hostdb_port" >&2
    exit 2
fi

_al_ctar="/tmp/$(basename "$_al_tarball")"
# batch-job: copies the release tarball into the container
podman cp "$_al_tarball" "$CNAME:$_al_ctar" || {
    printf 'acceptance-local: could not copy the tarball into the container\n' >&2
    exit 2
}
# THE RUN IS THE CANDIDATE'S OWN `release acceptance` (sp-ak7qm), taken from the tarball just
# built — the same binary acceptance.yml takes from the release asset.
tar -xzOf "$_al_tarball" --wildcards '*/bin/release' > "$_wd/release" 2>/dev/null \
    && chmod +x "$_wd/release" && podman cp "$_wd/release" "$CNAME:/tmp/release" || { # batch-job: copies the release binary into the container
    printf 'acceptance-local: could not stage bin/release from %s in the container\n' "$(basename "$_al_tarball")" >&2
    exit 2
}

# acceptance-agent.sh SOURCES THE TREE'S conf.sh BY ITS OWN PATH (the script's $HERE), and
# conf.sh's conf-gen.sh then wants to write two generated fragments beside it. /workspace is
# bind-mounted straight from the host checkout (ryan's own uid), and rootless podman's user
# namespace maps that uid to root inside the container regardless of the host file modes — so
# spirauser can never write under /workspace, and conf-gen.sh failed with "Permission denied"
# on its .tmp file, leaving SPIRA_DB unbound in the agent (sp-dn2rl problem 2). Point the agent
# at a copy extracted BY spirauser instead: the tarball just staged above, which is this same
# candidate's own tree (every git-tracked file, per build-tarball.sh) — so it is writable with
# no loss of fidelity to what's under test.
_al_release_name="$(basename "$_al_tarball" .tar.gz)"
testenv container exec --name "$CNAME" --user spirauser bash -c "
set -uo pipefail
mkdir -p \"\$HOME/release-under-test\"
tar -xzf '$_al_ctar' -C \"\$HOME/release-under-test\"
" || {
    printf 'acceptance-local: could not extract %s for the agent in the container\n' "$_al_ctar" >&2
    exit 2
}
# The container user's home, asked of the container rather than assumed.
_al_chome="$(testenv container exec --name "$CNAME" --user spirauser bash -c 'printf %s "$HOME"')" && [ -n "$_al_chome" ] || {
    printf 'acceptance-local: could not read the container user'"'"'s HOME\n' >&2
    exit 2
}
_al_agent="$_al_chome/release-under-test/$_al_release_name/spira/acceptance-agent.sh"

_al_tag="local-$(git -C "$TREE" rev-parse --short=12 HEAD 2>/dev/null || printf unknown)-$(date -u +%Y%m%dT%H%M%SZ)"
_al_prev_args=""
if [ -n "$PRED" ]; then
    # deploy.sh needs a spira-release-<stem> tag, and skew reads the one tagged on HEAD above.
    _al_tag="spira-release-$(basename "$_al_tarball" .tar.gz)"
    _al_cpred="/tmp/$(basename "$_al_pred_file")"
    # batch-job: copies the predecessor tarball into the container
    podman cp "$_al_pred_file" "$CNAME:$_al_cpred" || {
        printf 'acceptance-local: could not copy the predecessor tarball into the container\n' >&2
        exit 2
    }
    _al_prev_args="--prev-tag '$PRED' --prev-tarball '$_al_cpred'"
fi

# ---------------------------------------------------------------------------
# 3. RUN — the same scratch-repo shape acceptance-ci.sh builds for the forge
# run, then `release acceptance` phase A against the tarball.
# ---------------------------------------------------------------------------
testenv container exec --name "$CNAME" --user spirauser bash -c '
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
for _m in queue.local:scratch-queue pr:scratch-pr; do
    git init --bare --initial-branch=main "$HOME/${_m#*:}.git" >/dev/null
    git clone "$HOME/${_m#*:}.git" "$HOME/${_m#*:}" >/dev/null 2>&1
    git -C "$HOME/${_m#*:}" config user.email "acceptance@spira.local"
    git -C "$HOME/${_m#*:}" config user.name "Spira Acceptance"
    git -C "$HOME/${_m#*:}" commit --allow-empty -m "init" >/dev/null
    git -C "$HOME/${_m#*:}" push origin main >/dev/null 2>&1
    _base="origin/main"
    if [ "${_m%%:*}" = "queue.local" ]; then
        # A queue.local base must be a LOCAL branch, never a remote-tracking ref: queue.sh
        # publish refuses one outright ("... — not a queue.local base", queue/src/ops/
        # publish.rs), and every real queue.local row carries a local/<branch> base (set by
        # queue.sh to-local). The forge target (origin/main) is resolved separately, by
        # name, never by splitting the land ref (spira_publish_forge).
        git -C "$HOME/${_m#*:}" branch local/main main >/dev/null
        _base="local/main"
    fi
    printf "%s | %s | %s | %s | |\n" "${_m#*:}" "$HOME/${_m#*:}" "${_m%%:*}" "$_base" \
        >> "${XDG_CONFIG_HOME:-$HOME/.config}/spira/repo-map"
done
' || {
    printf 'acceptance-local: could not set up the scratch repo in the container\n' >&2
    exit 2
}

# THE INSTALLED INSTANCE'S OWN DATABASE, exactly as acceptance-ci.sh passes it: the probe
# bead must be filed where the installed sentinel and aeons read. `release acceptance`'s own
# default (~/spira-acceptance-test-db) is a path no install creates, so the first local run
# failed at "bead filed" before reaching anything the release does.
log "acceptance-local: running release acceptance $([ -n "$PRED" ] && printf 'phases A-D (predecessor %s)' "$PRED" || printf 'phase A') (tag=$_al_tag)"
testenv container exec --name "$CNAME" --user spirauser bash -c "
set -uo pipefail
# cd OUT OF /workspace FIRST. /workspace is a bind mount of <tree> — a worktree whose
# .git FILE holds the gitdir path on the HOST (e.g. .../harness/.git/worktrees/<name>),
# which does not exist inside the container's mount namespace. install.sh's phase 5 runs
# \`git config --global beads.role maintainer\` (no -C) and exclude.sh's own ROOT fallback
# runs \`git rev-parse --show-toplevel\` — both cwd-relative — and with cwd=/workspace
# EVERY such call dies \"fatal: not a git repository\", which is what install.sh reported
# as \"could not set beads.role\" and \"exclude.sh install failed\" (sp-dn2rl problem 1).
# \$HOME is spirauser's own, real, non-worktree directory: cwd-relative git calls either
# succeed or cleanly find no repository there, exactly as install.sh intends when
# SPIRA_REPO (unset here, same as in acceptance-ci.sh) leaves it to discover one.
cd \"\$HOME\"
export SPIRA_ACCEPTANCE_FORENSICS=\"\$HOME/acceptance-forensics\"
mkdir -p \"\$SPIRA_ACCEPTANCE_FORENSICS\"
exec /tmp/release acceptance '$_al_tag' \
    --scratch-repo \"\$HOME/scratch-repo\" \
    --tarball '$_al_ctar' \
    --agent '$_al_agent' \
    --bd-db \"\$HOME/.local/share/spira/db\" $_al_prev_args
"
_al_rc=$?
if [ "$_al_rc" -eq 0 ] && [ -n "$PRED" ]; then
    spira-config local-pass record acceptance-ad "$(git -C "$TREE" rev-parse HEAD)" acceptance-local.sh \
        || printf 'acceptance-local: phases A-D passed but the pass could not be recorded\n' >&2
fi

# ---------------------------------------------------------------------------
# 4. FORENSICS on FAIL. A build or container-startup failure (exit 2) never
# reaches here — there is no run to have left forensics behind.
# ---------------------------------------------------------------------------
if [ "$_al_rc" -eq 1 ]; then
    rm -rf "$FORENSICS_OUT"
    mkdir -p "$(dirname "$FORENSICS_OUT")"
    _al_home="$(testenv container exec --name "$CNAME" --user spirauser \
        bash -c 'printf %s "$HOME"' 2>/dev/null)"
    if [ -n "$_al_home" ] \
        && timeout 5 podman cp "$CNAME:$_al_home/acceptance-forensics" "$FORENSICS_OUT" 2>/dev/null; then
        printf 'forensics copied to: %s\n' "$FORENSICS_OUT"
    else
        printf 'acceptance-local: could not copy forensics out of the container\n' >&2
    fi
fi

exit "$_al_rc"
