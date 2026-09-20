#!/usr/bin/env bash
#
# build-bd.sh — build or download the one bd binary this box runs, from a RELEASE TAG,
# and install it.
#
#   build-bd.sh [--tag <tag>] [--probe] [--install] [--from-release]
#
#     --probe         build and verify; install nothing (default).
#     --install       install after every check passes.
#     --tag           override the pin below. You almost never want this.
#     --from-release  download a prebuilt release tarball instead of building from source.
#                     Requires curl. The tag is the same pin; BD_RELEASE_URL overrides the
#                     derived URL if the upstream naming convention differs.
#
# THE PIN IS A TAG, NOT A COMMIT AND NOT main.
BD_TAG_PIN="v1.2.1"
#
# WHY NOT npm, WHICH IS WHERE bd-embedded CAME FROM
# --------------------------------------------------
# `npm install -g @beads/bd` serves v1.2.2 as `latest`. v1.2.2 IS NOT A SUCCESSOR TO v1.2.1:
#
#     git merge-base v1.2.1 v1.2.2  ->  8e4e59d39 (2026-07-03, "refresh MCP lock for v1.1.0")
#     git merge-base --is-ancestor v1.2.1 v1.2.2  ->  false
#
# It was cut from a branch that forked at v1.1.0 in early July and never took the v1.2.0/v1.2.1
# work. Its highest migration is 0053 against v1.2.1's 0065, and it has NONE of unclaim,
# reclaim, heartbeat, sync, schema or conflicts. The version string gives no hint: the newer
# number is the older code.
#
# That one fact caused both bd outages here. On 2026-09-09 aeon.sh put that binary on live
# aeons' PATH; against the production store (v61) it printed "schema version mismatch … binary
# knows up to v53" WHILE EXITING 0, and four aeons escalated a destructive rollback of 3,296
# healthy beads. On 2026-09-10 test-attempts.sh ran its fixture on it, `bd unclaim` printed
# "unknown command", and gate-spira.sh went red against a clean origin/main — nothing landed
# for hours. Neither was embedded mode's fault. Both were "install the latest release".
#
# WHY BOTH BUILD FLAGS
# --------------------
#   CGO_ENABLED=1      embedded Dolt is opened through cgo. A CGO_ENABLED=0 binary REFUSES at
#                      runtime — "embedded Dolt requires a CGO build" — however it is tagged.
#                      Confirmed by building one and watching it refuse.
#   -tags gms_pure_go  go-mysql-server links ICU under cgo, so a bare CGO_ENABLED=1 build dies
#                      at the C linker on unicode/uregex.h. `make doctor-build` names the pair.
# Neither alone works. Both were got wrong once each on 2026-09-10.
set -uo pipefail

SRC="${BD_SRC:-$HOME/.cache/beads-src}"
REPO="https://github.com/steveyegge/beads.git"
GO="${GO:-$HOME/.local/go/bin/go}"
# THE TAG COMES FROM SPIRA_BD_TAG IF THE ENVIRONMENT CARRIES IT, and falls back to the
# hardcoded value above. conf.sh exposes the same key so doctor.sh reads the same pin
# without this script having to source conf.sh. Changing the default tag means changing
# both BD_TAG_PIN and the default in conf.sh's spira_conf_defaults.
TAG="${SPIRA_BD_TAG:-$BD_TAG_PIN}"; MODE=probe; RELEASE_MODE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --tag)          TAG="${2:?--tag needs a value}"; shift 2 ;;
        --probe)        MODE=probe;   shift ;;
        --install)      MODE=install; shift ;;
        --from-release) MODE=install; RELEASE_MODE=1; shift ;;
        *) echo "build-bd.sh: unknown argument '$1'" >&2; exit 2 ;;
    esac
done

if [ "$RELEASE_MODE" = 1 ]; then
    command -v curl      >/dev/null 2>&1 || { echo "build-bd.sh: --from-release needs curl" >&2; exit 1; }
    command -v sha256sum >/dev/null 2>&1 || { echo "build-bd.sh: --from-release needs sha256sum" >&2; exit 1; }
    _rel_os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    _rel_arch="$(uname -m)"
    case "$_rel_arch" in
        x86_64)  _rel_arch=amd64 ;;
        aarch64) _rel_arch=arm64 ;;
        *) echo "build-bd.sh: --from-release: unsupported architecture: $_rel_arch" >&2; exit 1 ;;
    esac
    # Upstream publishes beads_${version}_${os}_${arch}.tar.gz (project name, version without
    # the v prefix, underscores). BD_RELEASE_BASE_URL overrides the GitHub base for testing.
    _rel_asset="beads_${TAG#v}_${_rel_os}_${_rel_arch}.tar.gz"
    if [ -n "${BD_RELEASE_URL:-}" ]; then
        _rel_url="$BD_RELEASE_URL"
        _rel_checksum_url=""
    else
        _rel_base="${BD_RELEASE_BASE_URL:-https://github.com/steveyegge/beads/releases/download/$TAG}"
        _rel_url="$_rel_base/$_rel_asset"
        _rel_checksum_url="$_rel_base/checksums.txt"
    fi
    _rel_tmp="$(mktemp -d)"
    echo "build-bd.sh: downloading $TAG from $_rel_url" >&2
    if ! curl -fsSL --retry 3 -o "$_rel_tmp/bd.tar.gz" "$_rel_url"; then
        echo "build-bd.sh: download failed — $_rel_url" >&2
        rm -rf "$_rel_tmp"
        exit 1
    fi
    if [ -n "${_rel_checksum_url:-}" ]; then
        if ! curl -fsSL --retry 3 -o "$_rel_tmp/checksums.txt" "$_rel_checksum_url"; then
            echo "build-bd.sh: checksum download failed — $_rel_checksum_url" >&2
            rm -rf "$_rel_tmp"; exit 1
        fi
        _dl_sha256="$(sha256sum "$_rel_tmp/bd.tar.gz" | awk '{print $1}')"
        _exp_sha256="$(grep "$_rel_asset" "$_rel_tmp/checksums.txt" | awk '{print $1}')"
        if [ -z "${_exp_sha256:-}" ]; then
            echo "build-bd.sh: no checksum entry for $_rel_asset in checksums.txt" >&2
            rm -rf "$_rel_tmp"; exit 1
        fi
        if [ "$_dl_sha256" != "$_exp_sha256" ]; then
            echo "build-bd.sh: checksum mismatch for $_rel_asset: expected $_exp_sha256, got $_dl_sha256" >&2
            rm -rf "$_rel_tmp"; exit 1
        fi
    fi
    tar -xzf "$_rel_tmp/bd.tar.gz" -C "$_rel_tmp" || {
        echo "build-bd.sh: failed to unpack $_rel_tmp/bd.tar.gz" >&2; rm -rf "$_rel_tmp"; exit 1; }
    OUT="$(find "$_rel_tmp" -maxdepth 2 -name bd -perm /111 | head -1)"
    if [ -z "${OUT:-}" ]; then
        echo "build-bd.sh: no bd binary found in tarball" >&2; rm -rf "$_rel_tmp"; exit 1; fi
    echo "build-bd.sh: downloaded $TAG to $OUT" >&2
    # Fall through to the verification block below; skip the source build.
else
    command -v "$GO" >/dev/null 2>&1 || { echo "build-bd.sh: no go toolchain at $GO" >&2; exit 1; }
    command -v gcc  >/dev/null 2>&1 || { echo "build-bd.sh: CGO_ENABLED=1 needs gcc; none found" >&2; exit 1; }

    if [ -d "$SRC/.git" ]; then
        git -C "$SRC" fetch -q --tags origin || { echo "build-bd.sh: fetch failed" >&2; exit 1; }
    else
        mkdir -p "$(dirname "$SRC")"
        git clone -q "$REPO" "$SRC" || { echo "build-bd.sh: clone failed" >&2; exit 1; }
    fi
    # A TAG THAT IS NOT ON THE main LINEAGE IS REFUSED. This is the v1.2.2 trap, mechanised: a
    # release cut from a stale fork looks like an upgrade and is a downgrade.
    git -C "$SRC" rev-parse -q --verify "refs/tags/$TAG" >/dev/null || {
        echo "build-bd.sh: no such tag: $TAG" >&2; exit 1; }
    if ! git -C "$SRC" merge-base --is-ancestor "$TAG" origin/main 2>/dev/null; then
        echo "build-bd.sh: refusing — $TAG is not an ancestor of origin/main." >&2
        echo "  It was cut from a fork and may be older in capability than its number suggests." >&2
        echo "  merge-base with main: $(git -C "$SRC" merge-base "$TAG" origin/main 2>/dev/null)" >&2
        exit 1
    fi
    git -C "$SRC" checkout -q "$TAG"

    OUT="$(mktemp -d)/bd"
    echo "build-bd.sh: building $TAG (CGO_ENABLED=1 -tags gms_pure_go) — several minutes" >&2
    ( cd "$SRC" && CGO_ENABLED=1 "$GO" build -tags gms_pure_go -o "$OUT" ./cmd/bd ) || {
        echo "build-bd.sh: build failed" >&2; exit 1; }
fi

# ---- verify BEFORE installing -----------------------------------------------
fail=0
say() { printf '  %-30s %s\n' "$1" "$2"; }

for c in unclaim reclaim heartbeat sync schema conflicts; do
    "$OUT" "$c" --help >/dev/null 2>&1 && say "$c" ok || { say "$c" MISSING; fail=1; }
done

t="$(mktemp -d)"
( cd "$t" && env -i PATH="$PATH" HOME="$HOME" TERM=dumb BD_NON_INTERACTIVE=1 \
    "$OUT" init --non-interactive --prefix sp --skip-agents --skip-hooks -q >/dev/null 2>&1 )
[ $? -eq 0 ] && say "embedded init" ok || { say "embedded init" REFUSED; fail=1; }
rm -rf "$t"

# THE SCHEMA CHECK MUST CATCH *AHEAD* AS WELL AS *BEHIND*, and the obvious check does not.
# A read against a store the binary is ahead of succeeds silently; only a write refuses. So ask
# `migrate schema`, which states the relationship, and require the "already at" form. Checking
# with `list` is what let a v67 binary onto this box against a v61 store on 2026-09-10.
if [ -n "${SPIRA_DB:-}" ] && [ -d "${SPIRA_DB:-}" ]; then
    ms="$("$OUT" -C "$SPIRA_DB" migrate schema 2>&1 | head -1)"
    case "$ms" in
        *"already at"*) say "store schema" "ok — $ms" ;;
        *)              say "store schema" "MISMATCH — $ms"; fail=1 ;;
    esac
else
    say "store schema" "SKIPPED — no existing store to check compatibility against"
fi

[ "$fail" = 0 ] || { echo "build-bd.sh: verification FAILED — installing nothing" >&2; exit 1; }
[ "$MODE" = install ] || { echo "build-bd.sh: probe only; verified $TAG at $OUT" >&2; exit 0; }

# ---- install ----------------------------------------------------------------
# bd AND bd-embedded, because testdb.sh reads TESTDB_BD and lib.sh reads SPIRA_BD, and identical
# content under both names is what makes the skew unable to return.
#
# Install to the canonical paths only — SPIRA_PATH may place a policy shim ahead of the real
# binary, and `bd version` finds the shim first and reports a plausible version string. A shim
# was overwritten once by identifying the install target from `bd version` rather than the file.
stamp="$(date +%Y%m%d-%H%M%S)"
for dst in "$HOME/.local/bin/bd" "$HOME/.local/bin/bd-embedded"; do
    [ -e "$dst" ] && cp -p "$dst" "$dst.pre-$stamp"
    install -m 0755 "$OUT" "$dst.staging" && mv "$dst.staging" "$dst"
    printf '  installed %-34s (backup %s)\n' "$dst" "$(basename "$dst").pre-$stamp"
done
echo "build-bd.sh: $TAG installed. Run the suites before trusting it." >&2
