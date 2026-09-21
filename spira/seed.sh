#!/usr/bin/env bash
#
# seed.sh — write the shipped statutes into this installation's database.
#
#   seed.sh                 write every statute not already in force
#   seed.sh --dry-run       say what it would write, change nothing
#   seed.sh --force <slug>  overwrite one statute that is already in force
#   seed.sh --force <slug> --dry-run   say which text would replace which
#   seed.sh --list          what ships, and whether each is in force here
#
# WHY THIS EXISTS. Statutes live in the beads KV store, which is per-installation. The
# harness ships the mechanism; without this step a fresh install carries none of the law
# that makes it behave, and every rule in statutes/ has to be rediscovered the expensive
# way. Run it once at install, and again after a pull that adds statutes.
#
# IT NEVER OVERWRITES. A key already in the database is skipped and said so, because an
# operator who amended a statute meant it, and a seeder that reverts local law on every
# upgrade is worse than one that never runs.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

DIR="$SPIRA_HOME/statutes"
spira_require bd || exit 1
[ -d "$DIR" ] || { echo "seed: no statutes directory at $DIR" >&2; exit 1; }
# A missing `.beads` is a real fault to report, never a reason to quietly write whatever
# database the working directory happens to resolve to.
[ -d "$SPIRA_DB/.beads" ] || {
    echo "seed: $SPIRA_DB has no .beads — refusing to guess a database." >&2
    echo "      Set SPIRA_DB in ${SPIRA_CONF_FILE:-spira.conf}, or run: bd -C $SPIRA_DB init" >&2
    exit 1; }

# Every key in force, one per line. Asked once: `bd recall` per statute is a round trip each,
# and this runs over twenty of them.
in_force() {
    bd -C "$SPIRA_DB" memories --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for k in sorted(d): print(k)'
}

# FLAGS COMPOSE, and they are parsed in a LOOP rather than as one `case` on $1. Written as a
# case, `--force <slug> --dry-run` matched `--force` and then ignored the rest, so a run that
# said "dry" overwrote a statute in the live database — the exact damage --force exists to
# make deliberate. An override must be hard to reach by accident, and a flag that is silently
# discarded is the easiest accident there is.
DRY=0; FORCE=0; LIST=0; ONLY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY=1; shift ;;
        --list)    LIST=1; shift ;;
        --force)   FORCE=1; ONLY="${2:-}"
                   [ -n "$ONLY" ] || { echo "seed: --force needs a slug" >&2; exit 1; }
                   ONLY="law-${ONLY#law-}"; shift 2 ;;
        *)         sed -n '3,9p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
    esac
done
MODE=write
[ "$FORCE" = 1 ] && MODE=force
[ "$DRY"   = 1 ] && MODE=dry      # dry wins over force: saying both means show me
[ "$LIST"  = 1 ] && MODE=list

HELD="$(in_force)"
wrote=0; skipped=0; failed=0
for f in "$DIR"/law-*.txt; do
    [ -f "$f" ] || continue
    key="$(basename "$f" .txt)"
    [ -n "$ONLY" ] && [ "$key" != "$ONLY" ] && continue
    held=0; grep -qxF "$key" <<< "$HELD" && held=1

    if [ "$MODE" = list ]; then
        printf '  %-46s %s\n' "$key" "$([ "$held" = 1 ] && echo 'in force' || echo '-')"
        continue
    fi
    if [ "$held" = 1 ] && [ "$FORCE" != 1 ]; then
        skipped=$((skipped+1)); continue
    fi
    if [ "$MODE" = dry ]; then
        printf '  would %s %s (%s words)\n' \
            "$([ "$held" = 1 ] && echo 'OVERWRITE' || echo 'write')" "$key" "$(wc -w < "$f")"
        wrote=$((wrote+1)); continue
    fi
    # THE TEXT GOES IN AS ONE ARGUMENT READ FROM THE FILE, never interpolated into a
    # double-quoted string: backticks and $( ) in a statute are command substitution, and a
    # statute is prose about shell (law-commit-messages-via-stdin).
    if _bd_err="$(bd -C "$SPIRA_DB" remember --key "$key" "$(cat "$f")" 2>&1 >/dev/null)"; then
        printf '  wrote %s\n' "$key"; wrote=$((wrote+1))
    else
        printf '  FAILED %s\n' "$key" >&2
        [ -n "$_bd_err" ] && printf '    %s\n' "$(printf '%s\n' "$_bd_err" | head -1)" >&2
        failed=$((failed+1))
    fi
    unset _bd_err
done

[ "$MODE" = list ] && exit 0
printf '\n%d %s, %d already in force, %d failed\n' \
    "$wrote" "$([ "$MODE" = dry ] && echo 'would be written' || echo written)" "$skipped" "$failed"
[ "$failed" -eq 0 ] || exit 1
[ "$MODE" = dry ] && exit 0
[ "$wrote" -gt 0 ] && echo "Live in every agent session at its next summon."
exit 0
