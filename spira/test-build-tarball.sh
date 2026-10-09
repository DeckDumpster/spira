#!/usr/bin/env bash
#
# test-build-tarball.sh — build-tarball.sh names its tarball and directory the same,
# records the source commit in MANIFEST, lets --name pin stem and timestamp, and
# `verify` accepts a MANIFEST whose commit exists and refuses one whose commit does not.
#
# tier: T1
# covers: spira/build-tarball.sh UC-instance-lifecycle-02 UC-instance-lifecycle-04
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-build-tarball.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/bins"

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t
printf 'tracked\n' > "$REPO/file.txt"
git -C "$REPO" add .
git -C "$REPO" commit -qm "fixture: initial"
SHA="$(git -C "$REPO" rev-parse HEAD)"

printf '#!/usr/bin/env bash\necho fixture\n' > "$TMP/bins/fixture-bin"
chmod +x "$TMP/bins/fixture-bin"

run_tarball() {
    env -i PATH="$PATH" HOME="$TMP/home" GIT_CONFIG_GLOBAL=/dev/null \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        bash "$HERE/build-tarball.sh" "$@" 2>&1
}

top_dir() { tar -tzf "$1" 2>/dev/null | head -1 | cut -d/ -f1; }

out="$(run_tarball build --output "$TMP/out1" --bin-dir "$TMP/bins" "$SHA" "$REPO")"
rc=$?
is "default build exits 0" 0 "$rc"
tarball="$(find "$TMP/out1" -name 'spira-*.tar.gz' 2>/dev/null | head -1)"
[ -f "${tarball:-}" ] || bad "default build produced a tarball" "$out"
stem="$(basename "${tarball%.tar.gz}")"
if printf '%s' "$stem" | grep -qE '^spira-[0-9]{8}T[0-9]{6}Z$'; then
    ok "tarball stem is spira-YYYYMMDDTHHMMSSZ"
else
    bad "tarball stem is spira-YYYYMMDDTHHMMSSZ" "stem: $stem"
fi
is "tarball unpacks to a directory of the same name" "$stem" "$(top_dir "$tarball")"

mkdir -p "$TMP/unpack1"
tar -xzf "$tarball" -C "$TMP/unpack1"
TREE1="$TMP/unpack1/$stem"
want "MANIFEST records the source commit" "commit $SHA" "$(cat "$TREE1/MANIFEST" 2>/dev/null)"
want "MANIFEST timestamp is the stem's" "timestamp ${stem#spira-}" "$(cat "$TREE1/MANIFEST" 2>/dev/null)"

PIN=spira-20260102T030405Z
out="$(run_tarball build --output "$TMP/out2" --bin-dir "$TMP/bins" --name "$PIN" "$SHA" "$REPO")"
rc=$?
is "build with --name exits 0" 0 "$rc"
is "--name pins the tarball stem" 1 "$([ -f "$TMP/out2/$PIN.tar.gz" ] && echo 1 || echo 0)"
is "--name pins the unpacked directory" "$PIN" "$(top_dir "$TMP/out2/$PIN.tar.gz")"
mkdir -p "$TMP/unpack2"
tar -xzf "$TMP/out2/$PIN.tar.gz" -C "$TMP/unpack2" 2>/dev/null
want "--name pins the MANIFEST timestamp" "timestamp 20260102T030405Z" "$(cat "$TMP/unpack2/$PIN/MANIFEST" 2>/dev/null)"

out="$(run_tarball verify "$TMP/unpack1/$stem" --repo "$REPO")"
rc=$?
is "verify accepts a MANIFEST whose commit exists (positive control)" 0 "$rc"
want "verify names the commit it accepted" "$SHA" "$out"

BAD="$TMP/bad-release"
mkdir -p "$BAD"
printf 'commit %s\ntimestamp 20260101T000000Z\nrepo x\n' "$(printf 'a%.0s' $(seq 1 40))" > "$BAD/MANIFEST"
out="$(run_tarball verify "$BAD" --repo "$REPO")"
rc=$?
is "verify refuses a commit absent from the source repo" 1 "$rc"
want "verify says the commit does not exist" "does not exist" "$out"

mkdir -p "$TMP/empty-release"
run_tarball verify "$TMP/empty-release" --repo "$REPO" >/dev/null
is "verify refuses a release with no MANIFEST" 1 "$?"

tl_summary
