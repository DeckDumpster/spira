#!/usr/bin/env bash
#
# inventory.sh — refuse to ship one operator's infrastructure.
#
#   inventory.sh                 scan every tracked file; exit 1 naming each offender
#   inventory.sh --scan <file>   scan one file; print the offending tokens, one per line
#   inventory.sh --patterns      print the pattern list and exit
#
# WHAT THIS IS FOR. This repository is meant to be cloned by someone else. A comment naming
# a repository, a deploy path, a host or a person teaches their agent to reason about
# infrastructure that does not exist, and sometimes to act on it. The rule and the trap are
# what make the code worth reading; the case history behind them belongs in the operator's
# own notes.
#
# WHY A PROGRAM AND NOT A HABIT. A sanitising pass done by hand is done once. This runs from
# the landing gate on every branch, so the next comment that names a box is refused at the
# moment it is written rather than found by a reader who already believed it.
#
# WHAT IT LOOKS FOR is structural, so it needs no list of the operator's own names:
#
#   an absolute path rooted in a home or workspace directory   /home/<user>/…  /Users/<u>/…
#                                                              /workspaces/…
#   an e-mail address that is neither a reserved example domain (RFC 2606 / RFC 6761) nor
#   the `git@host` SSH remote form
#   a provenance mark naming a person and a date               (per <Name>, YYYY-MM-DD)
#
# AND WHATEVER THE OPERATOR ADDS. `inventory-deny`, one extended-regex per line, `#`
# comments. It ships with no entries: your repository and host names are yours to name, and
# a shipped deny-list of somebody else's names is itself the inventory it is hunting.
#
# COMMENTS ARE SCANNED, NOT STRIPPED. Every occurrence this was written to catch was in a
# comment; a version that stripped them first passed a tree that named seven repositories,
# a host and a person across ninety lines.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
# `||` binds looser than `&&`, so the fallback needs its own subshell or the git branch and
# the cd branch run as one expression and the wrong one wins.
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"
DENY="${SPIRA_INVENTORY_DENY:-$HERE/inventory-deny}"

# Reserved for documentation by RFC 2606 and RFC 6761, plus the harness's own synthetic
# author. Anything else is somebody's real address.
# `git@host` is the SSH remote form, not a person, and it is how every remote in every
# example is written.
# A SYSTEMD TEMPLATE INSTANCE IS NOT AN ADDRESS. `unit@instance.service` has the shape of
# one exactly, and the harness renders a unit per watcher that way — so without this every
# file that names an instance would be refused as though it carried somebody's mail. The
# exemption is on the unit SUFFIX, which no mail domain has.
EXEMPT_MAIL='^git@|@example\.(com|net|org|invalid)|@[A-Za-z0-9_.-]+\.invalid$|@(example|test|invalid|localhost)$|@spira\.local'
EXEMPT_MAIL="$EXEMPT_MAIL"'|@[A-Za-z0-9_.-]*\.(service|timer|socket|target|path|mount|slice|scope|swap|device)$'

patterns() {
    cat <<'PAT'
/home/[a-z][a-z0-9_.-]*/
/Users/[A-Za-z][A-Za-z0-9_.-]*/
/workspaces/
\(per [A-Z][a-z]+, [0-9]{4}-[0-9]{2}-[0-9]{2}
PAT
    [ -f "$DENY" ] && sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$DENY"
    return 0
}

# Computed once here; scan() uses it directly so the subshell+paste runs once, not per file.
_pat="$(patterns | paste -sd'|' -)"

# scan <file> -> the offending tokens, one per line. Exit 0 either way; the caller decides
# what an offender means. Mail is matched separately because the exemption is a subtraction.
scan() {
    local f="$1"
    { [ -n "$_pat" ] && grep -ohE "$_pat" "$f" 2>/dev/null
      grep -ohE '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' "$f" 2>/dev/null \
        | grep -vE "$EXEMPT_MAIL"
    } | sort -u
}

case "${1:-}" in
--patterns) patterns; exit 0 ;;
--scan)     scan "${2:?--scan needs a file}"; exit 0 ;;
esac

git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || {
    echo "inventory: $ROOT is not a git repository — nothing to scan" >&2; exit 3; }

mapfile -t tracked < <(git -C "$ROOT" ls-files)
[ "${#tracked[@]}" -gt 0 ] || { echo "inventory: nothing is tracked — refusing to report clean" >&2; exit 3; }
# Tracked files plus untracked non-ignored files: a violation in a file not yet
# committed is still a violation (sp-hm2vw).
mapfile -t files < <(
    printf '%s\n' "${tracked[@]}"
    git -C "$ROOT" ls-files --others --exclude-standard
)

# Batch-find the candidate set: files that may carry an offender.
# Two grep -l invocations across the whole tree replace 850+ per-file subshell forks.
declare -A _cands
if [ -n "$_pat" ]; then
    while IFS= read -r abs; do
        _cands["${abs#$ROOT/}"]=1
    done < <(printf '%s\0' "${files[@]/#/$ROOT/}" | xargs -0 grep -lE "$_pat" -- 2>/dev/null)
fi
while IFS= read -r abs; do
    _cands["${abs#$ROOT/}"]=1
done < <(printf '%s\0' "${files[@]/#/$ROOT/}" \
    | xargs -0 grep -lE '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' -- 2>/dev/null)

bad=0
_offenders=()
for f in "${!_cands[@]}"; do
    # Three files are exempt, and all three for the same reason: their content IS the
    # offender list. This file carries the patterns as string literals, the deny-list is
    # nothing but tokens to refuse, and the suite has to plant one of each shape to prove the
    # fence can go red. A fence that flags itself is a fence somebody deletes.
    case "$f" in
        */inventory.sh|inventory.sh|*/inventory-deny|inventory-deny) continue ;;
        */test-inventory.sh|test-inventory.sh) continue ;;
    esac
    [ -f "$ROOT/$f" ] || continue
    hits="$(scan "$ROOT/$f")"
    [ -n "$hits" ] || continue
    bad=1
    _offenders+=("$f")
    printf '%s\n' "$f"
    printf '%s\n' "$hits" | sed 's/^/    /'
done

if [ "$bad" = 0 ]; then
    printf 'inventory: clean — %d tracked file(s) name no operator infrastructure\n' "${#files[@]}"
    exit 0
fi
cat >&2 <<'WHY'

REFUSED by inventory.sh — the files above name one operator's infrastructure.

This repository is meant to be cloned. Keep the rule and the trap, which are why the code
is correct; move the case history — the path, the host, the repository name, the person and
the date — to wherever your own notes live, and cite it from there.

If a token above is genuinely generic, the deny-list and the pattern list are both editable;
a fence is a polite refusal, not a wall.
WHY
printf '%s\n' "${_offenders[@]}"
exit 1
