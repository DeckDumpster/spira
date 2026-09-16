#!/usr/bin/env bash
#
# rule.sh — enact, amend, or retire a statute in one command.
#
#   rule.sh enact <slug> "<statute text>"    write it, then synthesise it into the wiki
#   rule.sh retire <slug>                    remove it
#   rule.sh list                             what is in force
#   rule.sh show <slug>                      one statute's full text
#
# `<slug>` is written without the `law-` prefix; it is added for you.
#
# WHY THIS IS A COMMAND AND NOT A CHECKLIST
# -----------------------------------------
# Enacting a statute is two steps — write it to the statute book, regenerate the wiki
# page that is its only git-backed copy — and a rule that depends on remembering a second
# step is a resolution, not a mechanism. The standing lesson here is that when a rule is
# discovered the deliverable is a guard or a command, never a note.
#
# ONE DATABASE. Statutes live in the Spira beads database and nowhere else; that is the
# store every aeon reads its memories from at summon. There is no propagation step,
# because there is nothing to propagate to. `SPIRA_DB` overrides the path, as it does for
# every tool in this repo.
#
# WHEN TO REACH FOR IT
# --------------------
# When the operator answers an escalation, that answer is a verdict. Ask whether it
# generalises; if it does, enact it in the same session with the case that produced it
# as the trailing citation, so the rule carries its own history. The escalation queue should
# be producing law, not just draining — a class of question that keeps coming back is a
# missing statute.
#
# Statutes are read by every agent on every session, so write them to be read a thousand
# times: one paragraph, imperative, ~70 words, the scar as a single clause rather than a
# narrative of how we got here.
set -uo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")/spira" && pwd -P)/conf.sh"
export BEADS_NO_AUTO_IMPORT=1
DB="$SPIRA_DB"

usage() { sed -n '3,10p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }

# SYNTHESIS IS REQUIRED, NOT OPTIONAL. A missing or non-executable hook is an error: if you
# are running `rule.sh enact`, you expect the wiki page to be regenerated. Silently succeeding
# when it cannot is how the operator is told "Statute is live" while the page sits stale —
# observed verbatim when law-synth.sh failed with "Argument list too long" and rule.sh still
# printed the success banner (sp-p0xyt). The hook is CONFIGURED in spira.conf as SPIRA_WIKI_HOOK;
# an operator without a wiki checkout should not be running rule.sh enact in that environment.
synth() {
    local hook="$SPIRA_WIKI_HOOK"
    if [ -z "${hook:-}" ]; then
        echo "rule: SPIRA_WIKI_HOOK is not set — statute NOT regenerated in wiki." >&2
        echo "      Set SPIRA_WIKI_HOOK in spira.conf to the path of .claude/law-synth.sh." >&2
        return 1
    fi
    if [ ! -x "$hook" ]; then
        echo "rule: SPIRA_WIKI_HOOK='$hook' is not executable — statute NOT regenerated in wiki." >&2
        return 1
    fi
    "$hook"
}
slugify() { printf 'law-%s' "${1#law-}"; }

# A missing `.beads` is a real fault to report, never a reason to quietly address whatever
# database the working directory happens to resolve to.
[ -d "$DB/.beads" ] || { echo "rule: $DB has no .beads — refusing to guess a database" >&2; exit 1; }

case "${1:-}" in

enact)
    [ $# -ge 3 ] || usage
    key="$(slugify "$2")"; shift 2
    text="$*"
    words=$(wc -w <<<"$text")
    if [ "$words" -gt 130 ]; then
        echo "rule: refusing — ${words} words. A statute is one paragraph (~70 words);" >&2
        echo "      every agent pays this context on every session. Put the case history" >&2
        echo "      in the wiki and keep the scar here as a single clause." >&2
        exit 1
    fi
    bd -C "$DB" remember --key "$key" "$text" >/dev/null || {
        echo "rule: failed to write $key to the statute book at $DB" >&2; exit 1; }
    rm -f "${SPIRA_MEMORIES_CACHE:-}" 2>/dev/null || true
    echo "enacted $key (${words} words)"
    if synth; then
        echo
        echo "Statute is live in every agent session at its next summon."
        echo "Commit wiki/notes/common-law.md to replicate it off this box."
    else
        echo >&2
        echo "Statute IS in the book — the database write succeeded." >&2
        echo "The wiki page was NOT regenerated. Fix the hook and re-run rule.sh enact." >&2
        exit 1
    fi
    ;;

retire)
    [ $# -eq 2 ] || usage
    key="$(slugify "$2")"
    bd -C "$DB" memories --json 2>/dev/null \
      | python3 -c 'import json,sys;d=json.load(sys.stdin);sys.exit(0 if sys.argv[1] in d else 1)' "$key" || {
        echo "rule: no statute '$key' in the statute book at $DB" >&2; exit 1; }
    bd -C "$DB" forget "$key" >/dev/null 2>&1 && echo "forgot $key"
    rm -f "${SPIRA_MEMORIES_CACHE:-}" 2>/dev/null || true
    if synth; then
        echo
        echo "Retired. Do not leave a retired statute standing with a correction attached —"
        echo "that is the same defect as a correction banner on a stale page."
    else
        echo >&2
        echo "Statute IS removed from the book — the database write succeeded." >&2
        echo "The wiki page was NOT regenerated. Fix the hook and re-run rule.sh retire." >&2
        exit 1
    fi
    ;;

list)
    bd -C "$DB" memories --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
laws = {k: v for k, v in sorted(d.items()) if k.startswith("law-") and isinstance(v, str)}
for k, v in laws.items():
    print(f"  {k:<44} {len(v.split()):>3}w  {v.split(".")[0][:64]}")
print(f"\n{len(laws)} statutes in force")'
    ;;

show)
    [ $# -eq 2 ] || usage
    key="$(slugify "$2")"
    # `recall` only. The first version fell through to `bd remember "$key"` when recall
    # found nothing, which is a WRITE command reached by mistyping a slug in a read.
    bd -C "$DB" recall "$key" 2>/dev/null || {
        echo "rule: no statute '$key' — \`rule.sh list\` shows what is in force" >&2; exit 1; }
    ;;

*) usage ;;
esac
