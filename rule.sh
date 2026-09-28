#!/usr/bin/env bash
#
# rule.sh — enact, amend, or retire a statute in one command.
#
#   rule.sh enact <slug> "<statute text>" [--dry-run]   write it, then synthesise it into the wiki
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

SPIRA_HOME_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/spira" && pwd -P)"
. "$SPIRA_HOME_DIR/conf.sh"
export BEADS_NO_AUTO_IMPORT=1
DB="$SPIRA_DB"

usage() { sed -n '3,10p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }

# Commits wiki/notes/common-law.md itself, immediately after synth() regenerates it, naming
# only that path under a `law: enact <key>` / `law: retire <key>` message — so it never sits
# dirty for some other actor's exit to sweep up under the wrong authorship (sp-4fl2e).
# Best-effort: no SPIRA_WIKI, or nothing to stage (synth wrote no change), are not failures.
#   0 = committed   1 = skipped (no wiki checkout)   2 = commit attempt failed
commit_common_law() {
    local verb="$1" key="$2"
    [ -n "${SPIRA_WIKI:-}" ] && [ -d "$SPIRA_WIKI/.git" ] || return 1
    printf 'wiki/notes/common-law.md\n' \
        | bash "$SPIRA_HOME_DIR/wiki-commit.sh" "$SPIRA_WIKI" "law: $verb $key" \
        || return 2
    return 0
}

# SYNTHESIS IS REQUIRED, NOT OPTIONAL. A missing or non-executable hook is an error: if you
# are running `rule.sh enact`, you expect the wiki page to be regenerated. Silently succeeding
# when it cannot is how the operator is told "Statute is live" while the page sits stale —
# observed verbatim when law-synth.sh failed with "Argument list too long" and rule.sh still
# printed the success banner (sp-p0xyt). The default hook is the harness's own
# spira/law-synth.sh, which itself no-ops when SPIRA_WIKI is unset; SPIRA_WIKI_HOOK in
# spira.conf overrides it only when some other regeneration is wanted.
synth() {
    local hook="${SPIRA_WIKI_HOOK:-}"
    [ -n "$hook" ] || hook="$SPIRA_HOME_DIR/law-synth.sh"
    if [ ! -x "$hook" ]; then
        echo "rule: hook '$hook' is not executable — statute NOT regenerated in wiki." >&2
        return 1
    fi
    "$hook"
}
slugify() { printf 'law-%s' "${1#law-}"; }

# memories_json -> bd's memories --json on stdout, or exit 1 with bd's own stderr text
# reported and named against $DB. A down database and a genuinely empty statute book both
# used to reach here as empty stdin, and json.load could not tell them apart — it raised on
# the former and would have been correct to return {} on the latter (sp-n93br). Checking
# bd's exit status here, before anything is handed to python3, keeps the two distinguishable.
memories_json() {
    local out
    out="$(bd -C "$DB" memories --json 2>&1)" || {
        echo "rule: cannot reach the statute book at $DB: $out" >&2
        exit 1
    }
    printf '%s' "$out"
}

# A missing `.beads` is a real fault to report, never a reason to quietly address whatever
# database the working directory happens to resolve to.
[ -d "$DB/.beads" ] || { echo "rule: $DB has no .beads — refusing to guess a database" >&2; exit 1; }

case "${1:-}" in

enact)
    [ $# -ge 3 ] || usage
    key="$(slugify "$2")"; shift 2

    # Every remaining argument used to be joined into the statute text with no check, so a
    # stray flag (a mistyped --dry-run) became law silently. Reject anything that isn't the
    # one quoted text argument, or the one flag this command implements, before touching
    # the book (sp-dnrjw).
    dry_run=0 text="" text_set=0
    for arg in "$@"; do
        case "$arg" in
            --dry-run) dry_run=1 ;;
            -*)
                echo "rule: enact does not take '$arg' — usage: rule.sh enact <slug> \"<text>\" [--dry-run]" >&2
                exit 1
                ;;
            *)
                [ "$text_set" -eq 0 ] || {
                    echo "rule: enact takes one quoted text argument; '$arg' is a second one — quote the whole statute text" >&2
                    exit 1
                }
                text="$arg"; text_set=1
                ;;
        esac
    done
    [ "$text_set" -eq 1 ] || usage

    words=$(wc -w <<<"$text")
    if [ "$words" -gt 130 ]; then
        echo "rule: refusing — ${words} words. A statute is one paragraph (~70 words);" >&2
        echo "      every agent pays this context on every session. Put the case history" >&2
        echo "      in the wiki and keep the scar here as a single clause." >&2
        exit 1
    fi

    # An overwrite is otherwise invisible: bd remember --key replaces silently, with no undo.
    # Printing both texts makes the prior statute recoverable from the transcript even when
    # nothing else survives (sp-dnrjw).
    if prior="$(bd -C "$DB" recall "$key" 2>/dev/null)"; then
        echo "rule: '$key' already exists — this enact overwrites it."
        echo "--- current ---"; echo "$prior"
        echo "--- new ---"; echo "$text"
        echo "---"
    fi

    if [ "$dry_run" -eq 1 ]; then
        echo "DRY RUN: would enact $key (${words} words). Nothing written."
        exit 0
    fi

    remember_err="$(bd -C "$DB" remember --key "$key" "$text" 2>&1 >/dev/null)" || {
        echo "rule: failed to write $key to the statute book at $DB: $remember_err" >&2; exit 1; }
    rm -f "${SPIRA_MEMORIES_CACHE:-}" 2>/dev/null || true
    echo "enacted $key (${words} words)"
    if synth; then
        echo
        echo "Statute is live in every agent session at its next summon."
        commit_common_law enact "$key"; _cc_rc=$?
        case "$_cc_rc" in
            0) echo "wiki/notes/common-law.md committed (law: enact $key)." ;;
            1) echo "Commit wiki/notes/common-law.md to replicate it off this box." ;;
            *) echo "wiki/notes/common-law.md commit FAILED — commit it manually." >&2 ;;
        esac
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
    json="$(memories_json)" || exit 1
    printf '%s' "$json" \
      | python3 -c 'import json,sys;d=json.load(sys.stdin);sys.exit(0 if sys.argv[1] in d else 1)' "$key" || {
        echo "rule: no statute '$key' in the statute book at $DB" >&2; exit 1; }
    forget_err="$(bd -C "$DB" forget "$key" 2>&1 >/dev/null)" && echo "forgot $key" || {
        echo "rule: failed to forget $key: $forget_err" >&2; exit 1; }
    rm -f "${SPIRA_MEMORIES_CACHE:-}" 2>/dev/null || true
    if synth; then
        echo
        echo "Retired. Do not leave a retired statute standing with a correction attached —"
        echo "that is the same defect as a correction banner on a stale page."
        commit_common_law retire "$key"; _cc_rc=$?
        case "$_cc_rc" in
            0) echo "wiki/notes/common-law.md committed (law: retire $key)." ;;
            1) echo "Commit wiki/notes/common-law.md to replicate it off this box." ;;
            *) echo "wiki/notes/common-law.md commit FAILED — commit it manually." >&2 ;;
        esac
    else
        echo >&2
        echo "Statute IS removed from the book — the database write succeeded." >&2
        echo "The wiki page was NOT regenerated. Fix the hook and re-run rule.sh retire." >&2
        exit 1
    fi
    ;;

list)
    json="$(memories_json)" || exit 1
    printf '%s' "$json" | python3 -c '
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
    # bd's own stderr is surfaced rather than replaced: it already says "no memory with
    # that key" or names the real connection failure, and swallowing it collapsed both
    # into the same misleading "no statute" line (sp-n93br).
    out="$(bd -C "$DB" recall "$key" 2>&1)" || {
        echo "rule: $out" >&2
        echo "rule: \`rule.sh list\` shows what is in force" >&2
        exit 1
    }
    printf '%s\n' "$out"
    ;;

*) usage ;;
esac
