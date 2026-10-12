#!/usr/bin/env bash
#
# test-dolt-git-push-pack.sh — the git that Dolt shells out to for a git+ssh remote push
# does not spend its deadline compressing and delta-searching already-compressed table files.
#
#   ./test-dolt-git-push-pack.sh
#
# Takes the git settings from the dolt-beads unit itself, so a unit that loses them fails here.
#
# tier: T1
# covers: systemd/dolt-beads.service beads-store/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
REPO="$(cd "$HERE/.." && pwd -P)"
UNIT="$REPO/systemd/dolt-beads.service"

echo "test-dolt-git-push-pack.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

unit_env=()
while IFS= read -r l; do unit_env+=("${l#Environment=}"); done < <(grep '^Environment=GIT_CONFIG_' "$UNIT")

git init -q --bare "$TMP/r.git"
head -c 3000000 /dev/urandom > "$TMP/a"
cp "$TMP/a" "$TMP/b"; printf 'X' | dd of="$TMP/b" bs=1 seek=1500000 conv=notrunc 2>/dev/null
a="$(git -C "$TMP/r.git" hash-object -w "$TMP/a")"; b="$(git -C "$TMP/r.git" hash-object -w "$TMP/b")"
pack_bytes() { printf '%s\n%s\n' "$a" "$b" | env "$@" git -C "$TMP/r.git" pack-objects --stdout | wc -c; }

echo
echo "A. the harness can see the delta search it is turning off:"
default="$(pack_bytes GIT_CONFIG_NOSYSTEM=1)"
[ "$default" -lt 4000000 ] && ok "git at defaults packs a near-duplicate pair as a delta ($default bytes)" \
    || bad "git at defaults packs a near-duplicate pair as a delta" "got $default bytes, expected under 4000000"

echo
echo "B. the unit's settings stop it:"
[ "${#unit_env[@]}" -ge 7 ] && ok "dolt-beads.service sets the git config" || bad "dolt-beads.service sets the git config" "found ${#unit_env[@]} GIT_CONFIG_ entries"
tuned="$(pack_bytes GIT_CONFIG_NOSYSTEM=1 "${unit_env[@]}")"
[ "$tuned" -ge 5800000 ] && ok "the same pair is stored whole under the unit's settings ($tuned bytes)" \
    || bad "the same pair is stored whole under the unit's settings" "got $tuned bytes, expected at least 5800000"

echo
tl_summary
