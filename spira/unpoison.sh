#!/usr/bin/env bash
#
# unpoison.sh — clear spira-poison from a bead so it STAYS cleared, and prove it did.
#
#   unpoison.sh --bead <id> [--bead <id>...] --cause "<why the poison was wrong>" [--watch] [--dry-run]
#
# WHY THIS EXISTS. Clearing a poison by hand failed a different way every time. Removing the
# label alone left the attempt count at the threshold, so sentinel CHECK 4 re-poisoned the
# bead on its next pass (2026-09-26, six beads, minutes later). attempts.sh deadlocked had the
# same hole. Crediting attempts by hand worked only when every step was remembered. This is
# the one path: it writes the poison.cleared floor the attempt count is measured from
# (sp-qd2ul), resets the poison-ask history, removes the label (and, sp-ki12s: releases the
# matching lifecycle hold, dual-written alongside it until sp-i2m7y's CHECK 4 rewrite lands),
# records why, resolves the "change the approach or drop it?" ask — and then VERIFIES, with
# the same functions CHECK 4 uses, that the next pass will not put it back.
#
#   --cause    required. What made the poison wrong: the evidence, not an opinion ("every
#              charged session ended waiting for a background batch — yield-headless").
#   --watch    after clearing, wait for one complete sentinel pass and confirm the label is
#              still off (bounded; exit 1 if it came back or no pass completed).
#   --dry-run  show what would be done; change nothing.
#
# REFUSES: a bead that is neither poisoned nor at the attempt threshold (nothing to clear), a
# bead an aeon currently holds (in_progress with an assignee — let it finish), and a missing
# --cause. NAMED ARGUMENTS ONLY.
#
# EXIT  0 every bead cleared and verified · 1 a bead could not be cleared or did not verify ·
#       2 usage.
set -uo pipefail
. "$(dirname "$0")/lib.sh"
. "$(dirname "$0")/lc.sh"

usage() {
    sed -n '4,25p' "$0" | sed 's/^# \{0,1\}//'
}

IDS=(); CAUSE=""; WATCH=0; DRY=0
while [ $# -gt 0 ]; do
    case "$1" in
        --bead)    IDS+=("${2:?--bead needs a bead id}"); shift ;;
        --cause)   CAUSE="${2:?--cause needs text}"; shift ;;
        --watch)   WATCH=1 ;;
        --dry-run) DRY=1 ;;
        -h|--help) usage; exit 0 ;;
        -*)        printf 'unpoison.sh: unknown flag %s\n' "$1" >&2; exit 2 ;;
        *)         printf 'unpoison.sh: unexpected argument "%s" — named arguments only (--bead, --cause).\n' "$1" >&2; exit 2 ;;
    esac
    shift
done
[ "${#IDS[@]}" -gt 0 ] || { printf 'unpoison.sh: --bead is required\n' >&2; exit 2; }
[ -n "$CAUSE" ]        || { printf 'unpoison.sh: --cause is required — say why the poison was wrong\n' >&2; exit 2; }

POISON_LABEL=spira-poison
P_AT="${POISON_AT:-3}"

field() {  # field <json> <python-expr over b>
    printf '%s' "$1" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print(""); raise SystemExit
b = (d if isinstance(d, list) else [d])[0] if d else {}
print(eval(sys.argv[1]))' "$2" 2>/dev/null
}

rc_all=0
for id in "${IDS[@]}"; do
    js="$(bdjson show "$id" 2>/dev/null)"
    status="$(field "$js" 'b.get("status","")')"
    if [ -z "$status" ]; then printf 'FAIL %s: no such bead\n' "$id"; rc_all=1; continue; fi
    labels="$(field "$js" '",".join(b.get("labels") or [])')"
    assignee="$(field "$js" 'b.get("assignee") or ""')"
    n="$(attempts_of "$id")"
    poisoned=0; case ",$labels," in *",$POISON_LABEL,"*) poisoned=1 ;; esac

    if [ "$poisoned" = 0 ] && [ "${n:-0}" -lt "$P_AT" ]; then
        printf 'SKIP %s: not poisoned and attempts %s < %s — nothing to clear\n' "$id" "$n" "$P_AT"; continue
    fi
    if [ "$status" = in_progress ] && [ -n "$assignee" ]; then
        printf 'FAIL %s: held by %s (in_progress) — let that aeon finish, then clear\n' "$id" "$assignee"; rc_all=1; continue
    fi
    if [ "$DRY" = 1 ]; then
        printf 'WOULD %s: attempts %s, poisoned=%s — write poison.cleared, reset ask history, remove %s, note, resolve ask\n' \
            "$id" "$n" "$poisoned" "$POISON_LABEL"; continue
    fi

    # 1. The floor the attempt count is measured from (sp-qd2ul). Written FIRST, so a CHECK 4
    #    pass racing this script sees the reset count before it sees the missing label.
    # The event value goes into SQL verbatim; a quote in the cause broke the write and left the
    # count at the threshold (caught by step 6 on its first real run). The full cause still
    # goes into the note below; the event carries a quote-free, bounded form.
    bump_poison_cleared "$id" "$(printf '%s' "$CAUSE" | tr -d "'\"\\\\" | cut -c1-200)"
    # 2. The ask-dedup history, so a genuine future poisoning is asked about again.
    poison_asked_clear "$id"
    # 3. The label. KEPT AS A DUAL WRITE WITH lc_unhold, not replaced (sp-ki12s): CHECK 4
    # still dual-writes the label too (see its own note) because dispatchable_open excludes
    # `spira-poison` directly — removing it only here, and only eventually via the stale-
    # poison-clear scan's next pass, would leave a bead the operator just unpoisoned
    # excluded from dispatch for up to a full sentinel interval. Converting that exclusion
    # to read the lifecycle hold instead is sp-i2m7y's job, in the same cutover round.
    bdq label remove "$id" "$POISON_LABEL" >/dev/null 2>&1 || true
    lc_unhold "$id" poison unpoison.sh || true
    # 4. Why — the next aeon reads this.
    bdq note "$id" "Poison cleared by unpoison.sh (attempts were $n): $CAUSE" >/dev/null 2>&1 || true
    # 5. The operator ask this poisoning raised ("… — change the approach or drop it?").
    asks="$(bdjson list --status open --limit 0 --label "${SPIRA_ASK_LABEL:?}" 2>/dev/null \
        | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
for b in (d if isinstance(d, list) else [d]):
    if ("bead " + sys.argv[1] + " —") in (b.get("title") or ""): print(b["id"])' "$id" 2>/dev/null)"
    for a in $asks; do
        bdq close "$a" --reason "Resolved by unpoison.sh: $id's poison was cleared — $CAUSE" >/dev/null 2>&1 \
            && printf '     resolved ask %s\n' "$a"
    done

    # 6. VERIFY with CHECK 4's own decision function: the next pass must not put the label
    # back, AND the lifecycle hold (dual-written alongside it — see step 3) must be gone
    # everywhere it can be observed at all.
    js2="$(bdjson show "$id" 2>/dev/null)"
    labels2="$(field "$js2" '",".join(b.get("labels") or [])')"
    n2="$(attempts_of "$id")"
    decision="$(check4_decide "${n2:-0}" "$(requeues_of "$id")" "$(reclaims_of "$id")" "$labels2" 2>/dev/null)"
    bad=""
    case ",$labels2," in *",$POISON_LABEL,"*) bad="$bad label-still-present" ;; esac
    if lc_holds "$id" 2>/dev/null | grep -qx poison; then bad="$bad lifecycle-hold-still-present"; fi
    [ "${n2:-99}" -lt "$P_AT" ] || bad="$bad attempts-still-$n2"
    case " $decision " in *' poison '*) bad="$bad check4-would-repoison" ;; esac
    if [ -n "$bad" ]; then
        printf 'FAIL %s: did not verify:%s (attempts %s, check4=%s)\n' "$id" "$bad" "$n2" "$decision"; rc_all=1
    else
        printf 'OK   %s: cleared — attempts %s -> %s, check4 decides "%s"\n' "$id" "$n" "$n2" "${decision:-none}"
    fi
done

# --watch: one full sentinel pass must complete and leave every cleared bead unpoisoned.
if [ "$WATCH" = 1 ] && [ "$DRY" = 0 ] && [ "$rc_all" = 0 ]; then
    log_f="$SPIRA_RUN/sentinel.log"
    start="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'watch: waiting for a sentinel pass that starts after %s (up to 25 min)...\n' "$start"
    seen=0
    for _ in $(seq 150); do
        # a pass is complete once a "state:" line (pass start) AND a later "CHECK5" summary appear
        if awk -v s="$start" '$1 > s' "$log_f" 2>/dev/null | grep -q 'CHECK5:'; then seen=1; break; fi
        sleep 10
    done
    if [ "$seen" = 0 ]; then printf 'FAIL watch: no sentinel pass completed in 25 min\n'; exit 1; fi
    for id in "${IDS[@]}"; do
        l="$(field "$(bdjson show "$id" 2>/dev/null)" '",".join(b.get("labels") or [])')"
        case ",$l," in
            *",$POISON_LABEL,"*) printf 'FAIL watch %s: re-poisoned by the pass\n' "$id"; rc_all=1 ;;
            *)                   printf 'OK   watch %s: still clear after a full sentinel pass\n' "$id" ;;
        esac
    done
fi
exit "$rc_all"
