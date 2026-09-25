#!/usr/bin/env bash
#
# pilgrimage.sh — epic-complete detection and notification.
#
#   pilgrimage.sh check [epic-id] [--dry-run]  detect, notify, close   (the timer entry point)
#   pilgrimage.sh watch   <epic> [addr]        subscribe (default: the operator)
#   pilgrimage.sh unwatch <epic> [addr]        unsubscribe; with no addr, silence the epic entirely
#   pilgrimage.sh status  <epic>               progress and subscribers for one pilgrimage
#   pilgrimage.sh list                         every pilgrimage in the partition
#
# WHAT THIS REPLACES
# ------------------
# `gt convoy check` and `gt convoy watch`. A convoy was a durable, NAMED, SUBSCRIBABLE unit
# of batched work. Spira gives up its cross-database tracking for free — one database makes
# that capability meaningless — but the id, the grouping and the subscription have to
# survive, because "Backlog mountain: <repository>" is something you can point at,
# watch, and report on. An epic bead already supplies the id and the grouping through its
# parent/child edges. The subscription is the part that had to be rebuilt, and it is here.
#
# DETECTION IS `bd epic status`, NOT A HAND-ROLLED QUERY
# ------------------------------------------------------
# beads already ships the predicate: `bd epic status --json` reports total_children,
# closed_children and eligible_for_close for every open epic. Reimplementing that as a
# children walk would be the read-the-manual-first mistake.
#
# WHY NOT `bd epic close-eligible`, WHICH SHIPS THE WHOLE ACTION
# --------------------------------------------------------------
# Because it takes no label filter, and Spira is a REPLICA until cutover. Measured
#: `bd epic status --eligible-only` returns 13 eligible epics in Spira, every
# one of them imported from Gas Town — four of them —
# whose children Gas Town is still working in its own databases. One unfiltered
# `close-eligible` call would close all 13 here, and the reimport at cutover would then be
# reconciling against a graph this harness had quietly falsified.
#
# The label partition is therefore load-bearing, not a convenience. SPIRA_EPIC_LABELS is
# AND-scoped exactly as the fayth's claim predicate is, so this can only ever act on beads
# that belong to Spira's own plan.
#
# NOTIFY BEFORE CLOSING
# ---------------------
# `bd epic status` reports only OPEN epics, so closing first destroys the evidence that
# the transition happened. Notification is therefore sent first and the close is gated on
# it: if a subscriber cannot be reached, the epic stays open and the next pass retries. A
# subscriber may get the notice twice; nobody gets it zero times. That bias is deliberate —
# silence is the failure mode convoy actually had, with all six convoys stranded and
# nothing saying so.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

# AND-scoped: an epic must carry ALL of these labels to be in Spira's partition.
SPIRA_EPIC_LABELS="${SPIRA_EPIC_LABELS:-spira}"
# The human edge. A completed pilgrimage is an OUTCOME — what happened — so it is emitted as
# `ask.sh note`, a closed `event` bead the panel's NOTIFICATIONS view reads. It used to be
# emitted as `ask.sh insight`, and that was the defect: an insight is what was LEARNED and
# might become law, and two announcements (sp-94h, hq-5enm) sat in that queue as outcomes
# wearing an insight's label because there was only one bin.
# An epic with no explicit watcher still notifies. `gt convoy watch` existed and almost
# nothing ever called it, so completion stayed a capability rather than an event.
# Subscription therefore defaults ON, and `unwatch <epic>` with no address is how you opt
# out. The opt-out is stored as the literal `none` rather than as an absent key, because an
# absent key cannot tell "never subscribed" apart from "deliberately silenced".
# The operator's own address, from the one key that names them, so a clone subscribes its own
# operator rather than somebody else's. It is not the literal `operator` because subscriptions
# already stored carry whatever this installation has always used, and changing the default
# would silence every epic already watched.
SPIRA_DEFAULT_WATCHERS="${SPIRA_DEFAULT_WATCHERS:-$SPIRA_OPERATOR_ACTOR}"

DRY=0
kv_get() {   # kv_get <key> -> value on stdout, rc 1 if unset
    bdq kv get "$1" --json 2>/dev/null | json_only | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
if not d.get("found"): sys.exit(1)
print(d.get("value") or "")' 2>/dev/null
}
kv_set()   { bdq kv set "$1" "$2" >/dev/null 2>&1; }
kv_clear() { bdq kv clear "$1" >/dev/null 2>&1; }

subs_key()   { printf 'spira.watch.%s' "$1"; }
subs_of() {   # subs_of <epic-id> -> comma-separated addresses; empty means silenced
    local v; v="$(kv_get "$(subs_key "$1")")" || v="$SPIRA_DEFAULT_WATCHERS"
    [ "$v" = none ] && v=""
    printf '%s' "$v"
}
landed_key() { printf 'spira.landed.%s' "$1"; }

# pilgrimage_branches_landed <epic-id> -> 0 if every child's live push-mode branch is
# in landstate as LANDED, 1 (with a log line) if any is not.
#
# THE ASSERTION THIS GUARDS AGAINST: sp-qj8n demonstrated that landing.sh could push and
# merge a branch without writing its LANDED entry to landstate — marker commits lost, the
# branch present in the repo, the bead closed, and the epic then closing over work that
# was never confirmed to have reached the base. The assertion: a branch still present in
# a push-mode repo must carry a LANDED entry. If it does not, the epic is deferred until
# the next landing pass, which either writes the entry or pushes the branch that is somehow
# still there off the base by other means.
#
# WHY ONLY PUSH-MODE REPOS. A pr-mode repo's branches are landed by GitHub, not by
# landing.sh, so there is no landstate entry to check. A hold-mode branch waits for a
# human. Only push-mode branches go through land_mark LANDED, so only those have an entry
# to assert against.
#
# WHY ONLY LIVE BRANCHES. A branch reaped long ago left no trace in the repo. The landing
# that reaped it wrote LANDED to landstate, and a wiped SPIRA_RUN is a legitimate operation
# (landstate is advisory, the database is authoritative). So the absence of a branch from
# the repo is not a problem; the presence of one without a LANDED entry is.
pilgrimage_branches_landed() {
    local epic="$1" child repo_name repo ls_file ls_state ok=1
    while IFS= read -r child; do
        [ -n "$child" ] || continue
        for repo_name in $(spira_repos); do
            repo="$(repo_root "$repo_name")" || continue
            [ "$(repo_land "$repo_name")" = push ] || continue
            git -C "$repo" show-ref --verify --quiet "refs/heads/spira/$child" 2>/dev/null || continue
            # A live push-mode branch must have a LANDED entry in landstate.
            ls_file="$SPIRA_RUN/landstate/$child"
            ls_state="$(awk '{print $1}' < "$ls_file" 2>/dev/null)"
            if [ "${ls_state:-}" != LANDED ]; then
                log "$epic: ASSERTION — spira/$child in $repo_name is live but landstate reads '${ls_state:-missing}'; deferring close until landing records it"
                ok=0
            fi
        done
    done < <(children_ids "$epic")
    [ "$ok" = 1 ]
}

# --------------------------------------------------------------------------------------
# The partition, as one query. Emits TSV: id, closed, total, complete(1|0), title.
# `bd epic status` has no --limit, so there is no paging trap here — but it also has no
# --label, which is why the filter runs in this process.
# --------------------------------------------------------------------------------------
pilgrimages() {
    bdjson epic status | SPIRA_EPIC_LABELS="$SPIRA_EPIC_LABELS" python3 -c '
import sys, json, os
want = {l.strip() for l in os.environ["SPIRA_EPIC_LABELS"].split(",") if l.strip()}
try: rows = json.load(sys.stdin)
except Exception: sys.exit(0)
for r in rows if isinstance(rows, list) else [rows]:
    e = r.get("epic") or {}
    if not want <= set(e.get("labels") or []):
        continue
    total, closed = int(r.get("total_children") or 0), int(r.get("closed_children") or 0)
    # An epic with no children is EMPTY, not complete. Treating zero-of-zero as done is how
    # a freshly created epic closes itself before anyone has decomposed it.
    done = 1 if total > 0 and closed >= total else 0
    print("\t".join([e.get("id",""), str(closed), str(total), str(done),
                     (e.get("title") or "").replace("\t", " ")]))
' 2>/dev/null
}

# What actually landed. Parsed from JSON rather than scraped from the rendered tree, whose
# header line carries the epic's own id and put the parent in its own manifest.
children_ids() {   # children_ids <epic-id>
    bdjson children "$1" | EPIC="$1" python3 -c '
import sys, json, os
try: rows = json.load(sys.stdin)
except Exception: sys.exit(0)
for r in rows if isinstance(rows, list) else [rows]:
    if r.get("id") and r["id"] != os.environ["EPIC"]:
        print(r["id"])
' 2>/dev/null
}

# --------------------------------------------------------------------------------------
# Delivery. An address is a destination, not an agent: agent-to-agent routing goes through
# beads by design, so `bead:<id>` appends a note to a bead the reader already watches.
# Returns non-zero if the notice could not be delivered — which is what holds the epic open.
# --------------------------------------------------------------------------------------
deliver() {   # deliver <addr> <epic-id> <subject> <body>
    local addr="$1" id="$2" subject="$3" body="$4"
    case "$addr" in
        "$SPIRA_OPERATOR_ACTOR"|operator|cockpit)
            [ "$DRY" = 1 ] && { log "  would notify $SPIRA_OPERATOR: $subject"; return 0; }
            "$SPIRA_HOME/mail.sh" send operator \
                --from "Pilgrimage <pilgrimage@spira>" \
                --subject "$subject" \
                --kind note <<MAILEOF >/dev/null 2>&1
## Note
$subject

target: $id

$body
MAILEOF
            ;;
        bead:*)
            [ "$DRY" = 1 ] && { log "  would note ${addr#bead:}: $subject"; return 0; }
            bdq note "${addr#bead:}" "$subject"$'\n'"$body" >/dev/null 2>&1
            ;;
        log)
            printf '%s\n%s\n' "$subject" "$body"
            ;;
        *)
            log "  unknown subscriber address '$addr' on $id — not delivered"
            return 1
            ;;
    esac
}

# ======================================================================================
# check — the timer entry point
# ======================================================================================
cmd_check() {
    local only="" acted=0
    while [ $# -gt 0 ]; do
        case "$1" in --dry-run) DRY=1 ;; *) only="$1" ;; esac; shift
    done

    while IFS=$'\t' read -r id closed total done title; do
        [ -n "${id:-}" ] || continue
        [ -z "$only" ] || [ "$only" = "$id" ] || continue

        if [ "$done" != 1 ]; then
            # Reopened by adding more issues — convoy's documented behaviour, and the reason
            # the marker is cleared rather than being a one-way latch. Without this a
            # pilgrimage that grows a second wave lands silently.
            if kv_get "$(landed_key "$id")" >/dev/null; then
                [ "$DRY" = 1 ] || kv_clear "$(landed_key "$id")"
                log "$id: reopened ($closed/$total closed) — completion marker cleared"
            fi
            continue
        fi

        kv_get "$(landed_key "$id")" >/dev/null && continue   # already announced

        # ASSERT: every live push-mode branch belonging to this epic must appear in
        # landstate as LANDED. A child branch present in a push-mode repo without a
        # LANDED entry means landing.sh did not confirm the land — the sp-qj8n defect.
        # Defer until the next landing pass writes the entry.
        if ! pilgrimage_branches_landed "$id"; then
            log "$id: deferring — not all child branches are in landstate as LANDED"
            continue
        fi

        local subject body subs failed=0
        subject="PILGRIMAGE COMPLETE — $id: $title"
        body="All $total child beads closed: $(children_ids "$id" | tr '\n' ' ')"
        subs="$(subs_of "$id")"

        log "$subject"
        for addr in $(printf '%s' "${subs:-}" | tr ',' ' '); do
            deliver "$addr" "$id" "$subject" "$body" || { failed=1; log "  DELIVERY FAILED: $addr"; }
        done

        if [ "$DRY" = 1 ]; then
            log "  would close $id and mark it announced"
            acted=$((acted+1)); continue
        fi

        if [ "$failed" = 1 ]; then
            bdq note "$id" "Pilgrimage complete, but a subscriber could not be notified. Left open deliberately; the next sentinel pass retries." >/dev/null 2>&1
            continue
        fi

        # Marker first, then close: `bd epic status` reports only open epics, so a close
        # that lands without the marker would take the transition with it.
        kv_set "$(landed_key "$id")" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        bdq close "$id" --reason "Pilgrimage complete: all $total child beads closed. Subscribers notified: ${subs:-none}." >/dev/null 2>&1
        printf '%s\n' "$subject"
        acted=$((acted+1))
    done < <(pilgrimages)

    [ "$acted" -gt 0 ] && log "check: $acted pilgrimage(s) completed"
    return 0
}

# ======================================================================================
# watch / unwatch — the subscription that convoy had and an epic bead does not
# ======================================================================================
cmd_watch() {
    local id="${1:?usage: pilgrimage.sh watch <epic-id> [addr]}" addr="${2:-$SPIRA_OPERATOR_ACTOR}" cur
    cur="$(subs_of "$id")"
    case ",${cur}," in *",$addr,"*) echo "$addr already watches $id"; return 0 ;; esac
    kv_set "$(subs_key "$id")" "${cur:+$cur,}$addr"
    echo "$addr now watches $id"
}

cmd_unwatch() {
    local id="${1:?usage: pilgrimage.sh unwatch <epic-id> [addr]}" addr="${2:-}" cur new
    if [ -z "$addr" ]; then
        kv_set "$(subs_key "$id")" none
        echo "$id silenced — completion is still detected, logged and closed, but announced to nobody"
        return 0
    fi
    cur="$(subs_of "$id")"
    new="$(printf '%s' "$cur" | tr ',' '\n' | grep -vxF "$addr" | paste -sd, -)"
    kv_set "$(subs_key "$id")" "${new:-none}"
    echo "$addr no longer watches $id"
}

# ======================================================================================
# status / list — the dashboard view. `gt convoy list` was the thing that made progress
# legible; an epic id with a bar beside it is the same answer without the extra tracker.
# ======================================================================================
cmd_list() {
    local only="${1:-}" any=0
    while IFS=$'\t' read -r id closed total done title; do
        [ -n "${id:-}" ] || continue
        [ -z "$only" ] || [ "$only" = "$id" ] || continue
        any=1; local subs
        printf '%-14s %3s/%-3s %-9s %s\n' "$id" "$closed" "$total" \
               "$([ "$done" = 1 ] && echo COMPLETE || echo running)" "$title"
        subs="$(subs_of "$id")"
        printf '               watchers: %s\n' "${subs:-none (silenced)}"
    done < <(pilgrimages)
    [ "$any" = 1 ] || echo "no open pilgrimages labelled $SPIRA_EPIC_LABELS"
}

# EXECUTABLE FROM HERE. A test sources this file to call pilgrimage_branches_landed
# directly against a stubbed children_ids/spira_repos, and sourcing it must not also run
# `check` as a side effect of the `.` (the same trap landing.sh's own guard exists for).
if [ "${BASH_SOURCE[0]}" != "$0" ]; then return 0 2>/dev/null || true; fi

case "${1:-check}" in
    check)          shift || true; cmd_check "$@" ;;
    watch)          shift; cmd_watch "$@" ;;
    unwatch)        shift; cmd_unwatch "$@" ;;
    status)         shift; cmd_list "${1:-}" ;;
    list)           cmd_list ;;
    -h|--help|help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//' ;;
    *)              die "unknown command '${1}' — try: check watch unwatch status list" ;;
esac
