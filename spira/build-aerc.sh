#!/usr/bin/env bash
#
# build-aerc.sh — build aerc (git.sr.ht/~rjarry/aerc) ONCE, at release-build time, and
# place the resulting binary at <vendor-bin-dir>/aerc (sp-41so3).
#
# Called only from spira/build-tarball.sh's `do_build`, against the STAGED release tree,
# and only when that tree carries this very file (CONDITIONAL ON THE FILE EXISTING, same
# reasoning build-tarball.sh already applies to conf-gen.sh: a synthetic test fixture, or
# any repository this harness never ships from, has no spira/build-aerc.sh and packages
# with no vendor/ at all).
#
# WHY HERE, NOT AT INSTALL TIME (sp-41so3)
# Before this bead, install's own phase -1 built aerc itself: fetch a Go toolchain (when no
# usable one was already present), fetch aerc's pinned source archive, then `go install`
# it — which resolves and compiles ~95 of aerc's own module dependencies over the network,
# on every host install ever ran on. On 2026-10-04 local acceptance failed when every one
# of those module fetches hit "net/http: TLS handshake timeout" (this host's IPv6 is
# unreachable; load was high) — the identical build had passed earlier the same day. An
# install step that needs ~95 network round-trips to proxy.golang.org is fragile by
# construction. law-install-installs-every-dependency still requires install to provide
# aerc — there is no optional tier — so the build moves here instead: once per release,
# on the release-build machine, where a failure is the release-build machine's problem to
# retry, never a fresh host's. install's phase -1 (install_aerc, install/src/bin/install.rs)
# now only ever COPIES the binary this script produces.
#
# Usage: build-aerc.sh <vendor-bin-dir>
#   Builds aerc at exactly AERC_VERSION into <vendor-bin-dir>/aerc (GOBIN). Uses an
#   already-usable go (>= GO_MIN_MAJOR.GO_MIN_MINOR) when one is on this build machine — env
#   GO, $HOME/.local/go/bin/go, or PATH — and otherwise the go in the harness flake's
#   spira-env, pinned by flake.lock. The aerc source is the pinned git tag.
#
# EXIT
#   0  <vendor-bin-dir>/aerc built and reports exactly AERC_VERSION
#   1  any failure: unsupported architecture, a network fetch, a checksum mismatch, the
#      build itself, or a built binary that does not report the pinned version. Always
#      loud — see the comment above: this is the one place aerc's own network cost is
#      supposed to be paid, and a swallowed failure here ships a release with no aerc at
#      all, discovered only when install's own fail-closed refusal (install_aerc) fires on
#      some later, unrelated host.
set -uo pipefail

# The pinned aerc tag (git.sr.ht/~rjarry/aerc). NOT a "v"-prefixed go module version (its
# tags are bare "0.22.0", which `go install git.sr.ht/~rjarry/aerc@0.22.0` cannot resolve —
# go module version queries require a canonical `vX.Y.Z` — and aerc publishes no prebuilt
# binary release either), so this clones the tag and builds it from
# a local checkout instead, where no VCS tag lookup is needed at all.
AERC_VERSION="0.22.0"

GO_MIN_MAJOR=1
GO_MIN_MINOR=25

VENDOR_BIN="${1:?usage: build-aerc.sh <vendor-bin-dir>}"

# Parses `go version`'s stdout ("go version go1.27.1 linux/amd64") and reports (via exit
# status) whether it names a version >= $2.$3. Malformed/unexpected output fails, never
# aborts the script (set -u/-o pipefail do not cover arithmetic on an empty string here).
_go_version_at_least() {
    local tok major minor
    tok="$(printf '%s' "$1" | awk '{print $3}')"
    tok="${tok#go}"
    major="${tok%%.*}"
    case "$major" in '' | *[!0-9]*) return 1 ;; esac
    minor="${tok#*.}"
    minor="${minor%%.*}"
    case "$minor" in '' | *[!0-9]*) minor=0 ;; esac
    [ "$major" -gt "$2" ] && return 0
    [ "$major" -eq "$2" ] && [ "$minor" -ge "$3" ]
}

# The first already-usable go found among: $GO, the conventional $HOME/.local/go/bin/go, or
# PATH — the same candidate order doctor's own go check and install's own (removed,
# sp-41so3) find_usable_go used, so this reuses exactly what an operator or a prior run
# already placed rather than fetching a second toolchain.
go=""
for candidate in "${GO:-}" "${HOME:-}/.local/go/bin/go" "$(command -v go 2>/dev/null || true)"; do
    [ -n "$candidate" ] && [ -x "$candidate" ] || continue
    gv="$(timeout 5 "$candidate" version 2>/dev/null)" || continue
    _go_version_at_least "$gv" "$GO_MIN_MAJOR" "$GO_MIN_MINOR" || continue
    go="$candidate"
    break
done

if [ -z "$go" ]; then
    command -v nix >/dev/null 2>&1 || { printf 'build-aerc.sh: no usable go and no nix to provide one\n' >&2; exit 1; }
    flake_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
    env_dir="$(nix --extra-experimental-features 'nix-command flakes' build "path:$flake_dir#spira-env" --no-link --print-out-paths)" \
        || { printf 'build-aerc.sh: nix build of spira-env failed\n' >&2; exit 1; }
    go="$env_dir/bin/go"
    gv="$(timeout 5 "$go" version 2>/dev/null)" || gv=""
    _go_version_at_least "$gv" "$GO_MIN_MAJOR" "$GO_MIN_MINOR" || { printf 'build-aerc.sh: flake go %s is below go%s.%s\n' "$go" "$GO_MIN_MAJOR" "$GO_MIN_MINOR" >&2; exit 1; }
fi

src="$(mktemp -d)"
trap 'rm -rf "$src"' EXIT INT TERM
printf 'build-aerc.sh: building aerc %s (git.sr.ht/~rjarry/aerc) for the release tarball\n' "$AERC_VERSION" >&2
# batch-job: the release build clones the pinned aerc tag once per release.
if ! timeout 300 git clone -q --depth 1 --branch "$AERC_VERSION" https://git.sr.ht/~rjarry/aerc "$src"; then
    printf 'build-aerc.sh: cannot clone aerc %s\n' "$AERC_VERSION" >&2
    exit 1
fi

mkdir -p "$VENDOR_BIN"
# batch-job: `go install` resolves and compiles aerc's own module dependencies over the
# network (no vendor/ directory is bundled in the source archive) and compiles the
# program — the release build's own job, done once per release; bounded at 600 s.
# GOTOOLCHAIN=local pins the build to exactly the go binary just resolved, never a second,
# possibly-newer toolchain auto-fetched mid-build because go.mod names a newer `go` line
# than expected.
if ! (cd "$src" && GOBIN="$VENDOR_BIN" GOFLAGS="-mod=mod" GOTOOLCHAIN=local timeout 600 "$go" install -trimpath -ldflags "-X main.Version=${AERC_VERSION}" .); then
    printf 'build-aerc.sh: go install aerc failed — see output above\n' >&2
    exit 1
fi

ver="$(timeout 5 "$VENDOR_BIN/aerc" -v 2>/dev/null)" || ver=""
case "$ver" in
    "aerc $AERC_VERSION "*) ;;
    *)
        printf 'build-aerc.sh: built %s does not report version %s (got: %s)\n' "$VENDOR_BIN/aerc" "$AERC_VERSION" "$ver" >&2
        exit 1
        ;;
esac
printf 'build-aerc.sh: built aerc %s at %s\n' "$AERC_VERSION" "$VENDOR_BIN/aerc" >&2
