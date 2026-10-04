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
#   Builds aerc at exactly AERC_VERSION into <vendor-bin-dir>/aerc (GOBIN). Reuses an
#   already-usable go (>= GO_MIN_MAJOR.GO_MIN_MINOR) when one is already on this build
#   machine — env GO, $HOME/.local/go/bin/go, or PATH, the same candidate order install's
#   own (removed, sp-41so3) find_usable_go used — and fetches a pinned, checksummed
#   go.dev/dl toolchain into $HOME/.cache/spira-build-tarball only when none is usable.
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
# binary release either), so this fetches the tag's own source archive and builds it from
# a local checkout instead, where no VCS tag lookup is needed at all.
AERC_VERSION="0.22.0"

# The pinned Go toolchain (go.dev/dl), fetched ONLY when no usable go (>=
# GO_MIN_MAJOR.GO_MIN_MINOR — aerc's own go.mod says `go 1.25.0`) is already reachable on
# this build machine.
GO_VERSION="1.27.1"
GO_MIN_MAJOR=1
GO_MIN_MINOR=25
# go.dev/dl's published sha256 for go${GO_VERSION}.linux-{amd64,arm64}.tar.gz — checked
# after download, because this one tarball becomes the compiler the rest of this script
# trusts.
GO_SHA256_AMD64="63d339f0da5ab53635a56f2490a7984dfe12dfcff22ad749f63edaf590168445"
GO_SHA256_ARM64="3450b45a3f9ee8568792736a5c5e70a1f2e9b36c35a8f74958c03e51d7d92bec"

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

goarch=""
case "$(uname -m)" in
    x86_64) goarch=amd64 ;;
    aarch64) goarch=arm64 ;;
    *)
        printf 'build-aerc.sh: no go toolchain build for architecture %s\n' "$(uname -m)" >&2
        exit 1
        ;;
esac

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
    cache="${HOME:-/tmp}/.cache/spira-build-tarball"
    mkdir -p "$cache"
    go_dest="$cache/go-$GO_VERSION"
    if [ ! -x "$go_dest/bin/go" ]; then
        url="https://go.dev/dl/go${GO_VERSION}.linux-${goarch}.tar.gz"
        want=""
        case "$goarch" in
            amd64) want="$GO_SHA256_AMD64" ;;
            arm64) want="$GO_SHA256_ARM64" ;;
        esac
        tgz="$cache/.go-toolchain.download.$$.tar.gz"
        printf 'build-aerc.sh: fetching go %s to build aerc (release-build time only)\n' "$GO_VERSION" >&2
        # batch-job: the release build fetches a pinned Go toolchain once per release
        # machine, only when aerc needs building and no usable go is already present;
        # bounded by curl --max-time 300.
        if ! curl -fsSL --retry 3 --retry-all-errors --connect-timeout 10 --max-time 300 -o "$tgz" "$url"; then
            printf 'build-aerc.sh: cannot fetch go toolchain from %s\n' "$url" >&2
            rm -f "$tgz"
            exit 1
        fi
        got="$(sha256sum "$tgz" | awk '{print $1}')"
        if [ "$got" != "$want" ]; then
            rm -f "$tgz"
            printf 'build-aerc.sh: go toolchain checksum mismatch for %s: got %s, want %s\n' "$url" "$got" "$want" >&2
            exit 1
        fi
        tmp="$cache/.go.new.$$"
        rm -rf "$tmp"
        mkdir -p "$tmp"
        # batch-job: unpacking the fetched go toolchain tree (~210 MB uncompressed) once per
        # release build; bounded at 180 s.
        if ! timeout 180 tar -xzf "$tgz" -C "$tmp"; then
            rm -f "$tgz"
            rm -rf "$tmp"
            printf 'build-aerc.sh: cannot unpack go toolchain from %s\n' "$url" >&2
            exit 1
        fi
        rm -f "$tgz"
        newgo="$tmp/go/bin/go"
        newgv="$(timeout 5 "$newgo" version 2>/dev/null)" || newgv=""
        if ! _go_version_at_least "$newgv" "$GO_MIN_MAJOR" "$GO_MIN_MINOR"; then
            rm -rf "$tmp"
            printf 'build-aerc.sh: fetched go toolchain at %s does not run or is below go%s.%s\n' \
                "$newgo" "$GO_MIN_MAJOR" "$GO_MIN_MINOR" >&2
            exit 1
        fi
        rm -rf "$go_dest"
        mv "$tmp/go" "$go_dest"
        rm -rf "$tmp"
        printf 'build-aerc.sh: installed go %s at %s\n' "$GO_VERSION" "$go_dest" >&2
    fi
    go="$go_dest/bin/go"
fi

src="$(mktemp -d)"
trap 'rm -rf "$src"' EXIT INT TERM
url="https://git.sr.ht/~rjarry/aerc/archive/${AERC_VERSION}.tar.gz"
tgz="$(mktemp)"
printf 'build-aerc.sh: building aerc %s (git.sr.ht/~rjarry/aerc) for the release tarball\n' "$AERC_VERSION" >&2
# batch-job: the release build downloads the pinned aerc source archive once per release;
# bounded by curl --max-time 300.
if ! curl -fsSL --retry 3 --retry-all-errors --connect-timeout 10 --max-time 300 -o "$tgz" "$url"; then
    printf 'build-aerc.sh: cannot fetch aerc source from %s\n' "$url" >&2
    rm -f "$tgz"
    exit 1
fi
# batch-job: unpacking the aerc source archive (a few hundred KB) once per release; bounded
# at 60 s.
if ! timeout 60 tar -xzf "$tgz" -C "$src" --strip-components=1; then
    rm -f "$tgz"
    printf 'build-aerc.sh: cannot unpack aerc source from %s\n' "$url" >&2
    exit 1
fi
rm -f "$tgz"

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
