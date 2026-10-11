#!/usr/bin/env bash
#
# bare-local.sh — run the bare-host suites on a tree, here, in minutes instead of a publish cycle.
#
#   bare-local.sh <tree|ref> [--suites a.sh,b.sh]     (default: every bare-host suite)
#
# Does what the publish gate's G12 step does and nothing different: builds the tree, stages the
# build as a release, sets SPIRA_RELEASE / SPIRA_REPO / PATH outright, then runs
# bare-suites.sh — the script G12 itself calls. Run it before submitting anything that touches
# install, configure, uninstall, ready or the owned manifest.
#
# <tree>  a working tree (committed — the staged release names its HEAD), or a ref of
#         $SPIRA_REPO, which is cloned to a scratch tree first.
# This machine must have podman and a user systemd session; the suites build their own
# containers, so the fresh environment is the container. A round's bare lane runs the same
# script on a fresh VM.
#
# EXIT  as bare-suites.sh; 2 for usage, a dirty tree, or a missing prerequisite.
#
# covers: spira/bare-local.sh spira/bare-suites.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

usage() { echo "usage: bare-local.sh <tree|ref> [--suites a.sh,b.sh]" >&2; exit 2; }
[ $# -ge 1 ] || usage
target="$1"; shift
suites=all
while [ $# -gt 0 ]; do
    case "$1" in
        --suites) suites="${2:?--suites needs a value}"; shift 2 ;;
        *) usage ;;
    esac
done

command -v podman >/dev/null 2>&1 || { echo "bare-local: podman is not on PATH" >&2; exit 2; }
systemctl --user show-environment >/dev/null 2>&1 || { echo "bare-local: no user systemd session for $(id -un)" >&2; exit 2; }
command -v cargo >/dev/null 2>&1 || { echo "bare-local: cargo is not on PATH" >&2; exit 2; }

. "$HERE/bare-local-scratch.sh"
bare_local_reap "${TMPDIR:-/tmp}" "${SPIRA_BARE_LOCAL_CAP:-7200}"
bare_local_scratch_new "${TMPDIR:-/tmp}" || exit 2
if [ -d "$target" ]; then
    tree="$(cd "$target" && pwd -P)"
else
    repo="$(git -C "$HERE" rev-parse --show-toplevel)"
    # batch-job: clones and checks out a whole tree
    git clone -q --shared "$repo" "$scratch/tree" && git -C "$scratch/tree" checkout -q "$target" \
        || { echo "bare-local: '$target' is neither a directory nor a ref of $repo" >&2; exit 2; }
    tree="$scratch/tree"
fi
[ -z "$(git -C "$tree" status --porcelain)" ] \
    || { echo "bare-local: $tree has uncommitted changes — the staged release is built from HEAD" >&2; exit 2; }
sha="$(git -C "$tree" rev-parse HEAD)"

echo "bare-local: building $sha" >&2
( cd "$tree" && cargo build -q --profile release --workspace ) || { echo "bare-local: build failed" >&2; exit 2; }
rel_root="$scratch/releases"
rel_sha="$("$tree/target/release/release" build "$sha" --repo "$tree" --bin-dir "$tree/target/release" --releases "$rel_root")" \
    || { echo "bare-local: release build failed" >&2; exit 2; }
rel="$rel_root/$rel_sha"
path="$rel/bin:$rel/spira:/usr/local/bin:/usr/bin:/bin:$PATH"
got="$(PATH="$path" command -v spira-config)"
[ "$got" = "$rel/bin/spira-config" ] || { echo "bare-local: spira-config resolves to ${got:-nothing}, not the staged release $rel" >&2; exit 2; }

env -i HOME="$HOME" USER="$(id -un)" TERM="${TERM:-dumb}" \
    XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" \
    ${DBUS_SESSION_BUS_ADDRESS:+DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"} \
    ${SPIRA_TESTENV_REGISTRY:+SPIRA_TESTENV_REGISTRY="$SPIRA_TESTENV_REGISTRY"} \
    SPIRA_RELEASE="$rel" SPIRA_REPO="$tree" PATH="$path" \
    bash "$tree/spira/bare-suites.sh" --suites "$suites"
