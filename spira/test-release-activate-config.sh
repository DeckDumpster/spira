#!/usr/bin/env bash
# test-release-activate-config.sh — activation applies the new tree's config delta (a key its
# registry newly requires, at its declared default) before `current` moves, and refuses,
# leaving `current` and the config as found, when the new tree cannot resolve every key.
#
# tier: T2
# covers: release/src/* spira/conf.d/* spira/conf.toml.d/* spira/conf-gen.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-release-activate-config.sh"

command -v release >/dev/null 2>&1 && command -v spira-config >/dev/null 2>&1 \
    || { echo "test-release-activate-config: release and spira-config must be on PATH"; exit 1; }

TMP="$(mktemp -d)"
mkdir -p "$TMP/home"
trap 'rm -rf "$TMP"' EXIT INT TERM

RELEASES="$TMP/releases"
OLD=$(printf "a%.0s" {1..40})
NEW=$(printf "b%.0s" {1..40})
NEWKEY=SPIRA_ZZ_ADDED_SECS

tree() {   # tree <name> [extra-key]: a release tree carrying the shipped registry
    local r="$RELEASES/$1"
    mkdir -p "$r/bin" "$r/spira" "$r/systemd"
    cp "$(command -v spira-config)" "$r/bin/"
    cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$r/spira/"
    if [ -n "${2:-}" ]; then
        printf "TYPE=u32\nDOC=%s\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"\${%s:=600}\"\nSPIRA_CONF_DEFAULT_EOF\n" "$2" "$2" > "$r/spira/conf.d/$2"
        bash "$r/spira/conf-gen.sh" >/dev/null 2>&1 || { echo "conf-gen failed in $r"; exit 1; }
    fi
    seal "$1"
}

seal() {   # seal <name>: the MANIFEST activation verifies the tree against
    local r="$RELEASES/$1"
    { printf 'commit %s\n' "$1"; (cd "$r" && find . -type f ! -name MANIFEST | sort | sed 's|^\./||' | while read -r p; do printf '%s %s\n' "$p" "$(sha256sum "$p" | cut -d' ' -f1)"; done); } > "$r/MANIFEST"
}

MOCK_SC="$TMP/systemctl"
printf '#!/bin/sh\nexit 0\n' > "$MOCK_SC"; chmod +x "$MOCK_SC"

go_live() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_TOML="$TMP/live.toml" SPIRA_RELEASES="$RELEASES" SPIRA_RUN="$TMP/run" \
        SPIRA_RELEASE="$RELEASES/$(readlink "$RELEASES/current")" SPIRA_SYSTEMCTL="$MOCK_SC" SPIRA_INSTANCE=prod \
        timeout 120 release activate "$1" --releases "$RELEASES" --run "$TMP/run" --settle 0 --drain-wait 0 2>&1  # batch-job: a real activation of a throwaway tree
}

resolve_under() {   # resolve_under <name>: stderr of that release's own resolve of the live config
    env -i PATH="$PATH" HOME="$TMP/home" SPIRA_TOML="$TMP/live.toml" \
        SPIRA_HOME="$RELEASES/$1/spira" SPIRA_REPO="$RELEASES/$1" \
        spira-config resolve --sh-all 2>&1 >/dev/null
}

fresh() {   # fresh [old-key]: the key the outgoing registry already declares, absent from the config
    rm -rf "$RELEASES" "$TMP/run"; mkdir -p "$RELEASES" "$TMP/run"
    cp "$HERE/testlib/complete.toml" "$TMP/live.toml"
    tree "$OLD" "${1:-}"; tree "$NEW" "$NEWKEY"; ln -s "$OLD" "$RELEASES/current"
}

echo
echo "1. a key both registries declare and the config lacks reaches the config before current moves"
fresh "$NEWKEY"
want "planted offender: the new tree refuses the config as it stands" "$NEWKEY is not declared" "$(resolve_under "$NEW")"
out="$(go_live "$NEW")"; rc=$?
wantrc "activation succeeds" 0 "$rc"
is "current moved to the new release" "$NEW" "$(readlink "$RELEASES/current")"
want "the added key is in the live config at its declared default" "zz_added_secs = 600" "$(cat "$TMP/live.toml")"
is "the new release resolves every key" "" "$(resolve_under "$NEW")"

echo
echo "1b. a key only the new registry declares reaches the config too"
fresh
go_live "$NEW" >/dev/null
is "current moved" "$NEW" "$(readlink "$RELEASES/current")"
want "the added key is in the live config at its declared default" "zz_added_secs = 600" "$(cat "$TMP/live.toml")"

echo
echo "2. a delta that cannot be applied refuses and current does not move"
fresh
printf 'TYPE=u32\nDOC=nodefault\n' > "$RELEASES/$NEW/spira/conf.d/SPIRA_ZZ_NODEFAULT_SECS"
bash "$RELEASES/$NEW/spira/conf-gen.sh" >/dev/null 2>&1
seal "$NEW"
before="$(cat "$TMP/live.toml")"
out="$(go_live "$NEW")"; rc=$?
[ "$rc" -ne 0 ] && ok "activation refuses" || bad "activation refuses" "rc=$rc: $out"
is "current did not move" "$OLD" "$(readlink "$RELEASES/current")"
is "the config is as found" "$before" "$(cat "$TMP/live.toml")"
want "the refusal names the unresolvable key" "NODEFAULT_SECS" "$out"

echo
echo "3. a key both registries declare, with no default to supply, refuses, naming it"
fresh
for r in "$OLD" "$NEW"; do
    printf 'TYPE=u32\nDOC=nodefault\n' > "$RELEASES/$r/spira/conf.d/SPIRA_ZZ_NODEFAULT_SECS"
    bash "$RELEASES/$r/spira/conf-gen.sh" >/dev/null 2>&1
    seal "$r"
done
before="$(cat "$TMP/live.toml")"
env -i PATH="$PATH" HOME="$TMP/home" SPIRA_TOML="$TMP/live.toml" SPIRA_HOME="$RELEASES/$NEW/spira" SPIRA_REPO="$RELEASES/$NEW" \
    spira-config validate >/dev/null 2>&1
wantrc "planted offender: validate accepts the config" 0 "$?"
want "while resolve rejects it" "zz_nodefault_secs" "$(resolve_under "$NEW")"
out="$(go_live "$NEW")"; rc=$?
[ "$rc" -ne 0 ] && ok "activation refuses" || bad "activation refuses" "rc=$rc: $out"
want "the refusal names the key" "zz_nodefault_secs" "$out"
is "current did not move" "$OLD" "$(readlink "$RELEASES/current")"
is "the config is as found" "$before" "$(cat "$TMP/live.toml")"

echo
echo "4. a release declaring no change activates"
fresh
cp -r "$RELEASES/$OLD/." "$RELEASES/$NEW/"
printf 'commit %s\n' "$NEW" > "$RELEASES/$NEW/MANIFEST"; seal "$NEW"
go_live "$NEW" >/dev/null
is "current moved" "$NEW" "$(readlink "$RELEASES/current")"

tl_summary
