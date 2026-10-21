#!/usr/bin/env bash
#
# test-round-ami-stale.sh — the stale-AMI watcher says a recipe/AMI mismatch once, says it again
# after a match has cleared it, and is silent while spill is off.
#
# tier: T1
# covers: spira/round-ami-stale.sh spira/watchers round-vm/src/cli.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-round-ami-stale.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run/watchd" "$TMP/stub"
INBOX="$TMP/inbox.log"
printf '#!/usr/bin/env bash\nif [ -e "%s/stale" ]; then echo "round AMI stale for recipe r2: newest image ami-1 is tagged recipe-sha r1"; exit 1; fi\nif [ -e "%s/blind" ]; then echo "describe-images: denied" >&2; exit 2; fi\nexit 0\n' "$TMP" "$TMP" > "$TMP/stub/round-vm"
chmod +x "$TMP/stub/round-vm"
ras() {
    local spill="$1"; shift
    tl_config SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_ROUND_VM_SPILL="$spill"
    env -i PATH="$TMP/stub:$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" bash "$HERE/round-ami-stale.sh" "$@" 2>&1
}

touch "$TMP/stale"
is "silent while spill is off" "" "$(ras '' pass)"
want "a mismatch is said with the recipe and the AMI's tag" "stale for recipe r2: newest image ami-1 is tagged recipe-sha r1" "$(ras ec2-only pass)"
want "the line reaches the inbox" "watch:round-ami-stale" "$(cat "$INBOX")"
is "the same mismatch is said once" "" "$(ras ec2-only pass)"
rm "$TMP/stale"
is "a match is quiet" "" "$(ras ec2-only pass)"
touch "$TMP/stale"
want "a mismatch after the match cleared is said again" "stale for recipe r2" "$(ras ec2-only pass)"
rm "$TMP/stale"; ras ec2-only pass >/dev/null; touch "$TMP/blind"
want "an unreadable image list is not reported as fresh" "could not read the images" "$(ras ec2-only pass)"

WF="$HERE/../.github/workflows/round-ami-build.yml"
printf 'README.md\nround-vm/ami-setup.sh\n' | bash "$HERE/round-ami-paths.sh"; is "a range touching ami-setup.sh dispatches" "0" "$?"
printf 'README.md\nqueue/src/lib.rs\n' | bash "$HERE/round-ami-paths.sh"; is "a range touching no recipe path does not" "1" "$?"
printf 'round-vm/ami-setup.sh\n' | bash "$HERE/round-ami-paths.sh" /dev/null 2>/dev/null; is "a workflow with no paths is refused, not read as no match" "2" "$?"
printf 'x\n' > "$TMP/wf.yml"; printf 'on:\n  push:\n    paths:\n      - fixture/only.sh\n  schedule:\n    - cron: "1 1 * * 1"\n' > "$TMP/wf.yml"
printf 'fixture/only.sh\n' | bash "$HERE/round-ami-paths.sh" "$TMP/wf.yml"; is "the list comes from the workflow file" "0" "$?"
printf 'round-vm/ami-setup.sh\n' | bash "$HERE/round-ami-paths.sh" "$TMP/wf.yml"; is "a path the workflow no longer lists does not match" "1" "$?"
