#!/usr/bin/env bash
#
# bead-sweep.sh — classify every live bead and raise each SUSPECT class as an inbox line;
# a watchd daemon.
#
#   bead-sweep.sh [--show]                       one pass, every class, to stdout
#   bead-sweep.sh watch [--interval S] [--ticks N]
#   bead-sweep.sh health
#
# Classes: OK, BLOCKED, HELD, ASK, SUSPECT. Only SUSPECT is announced: the line is printed (the
# watcher log) and appended to the Concierge inbox as "[watch:bead-sweep] <line>" the pass it
# first appears, and again only after it cleared and came back. A pass that cannot read the
# lifecycle prints "SWEEP FAILED" and writes no health record, so a blind sweep is DEGRADED
# rather than quiet.
#
# covers: spira/bead-sweep.sh spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

HEALTH_FILE="$SPIRA_RUN/watchd/bead-sweep.health"
HOME_LABEL="${SPIRA_HOME_REPO:+repo:$SPIRA_HOME_REPO}"
HOLD_MIN="${BEAD_SWEEP_HOLD_MIN:-120}"
REWORK_MIN="${BEAD_SWEEP_REWORK_MIN:-20}"
CERT_MIN="${BEAD_SWEEP_CERT_MIN:-10}"
WAIT_MIN="${BEAD_SWEEP_WAIT_MIN:-120}"
declare -A _bs_seen

_bs_classify() {
    local t="$1"
    [ -n "$HOME_LABEL" ] || { echo "SWEEP FAILED: SPIRA_HOME_REPO is not declared" >&2; return 1; }
    # batch-job: whole-store listing the sweep classifies; bounded by the daemon row cadence
    timeout 60 spira-lc list --state READY,REWORK,SUBMITTED,CERTIFIED,WORKING,OPEN,IN_DELIVERY > "$t/rows.json" 2>/dev/null || return 1
    # batch-job: whole-store listing the sweep classifies; bounded by the daemon row cadence
    timeout 90 spira-lc content list --status open,in_progress,blocked,deferred --json --limit 0 > "$t/content.json" 2>/dev/null || return 1
    # batch-job: whole-store listing the sweep classifies; bounded by the daemon row cadence
    timeout 60 spira-lc list --state LANDED,DONE,DROPPED,SUPERSEDED > "$t/term.json" 2>/dev/null || return 1
    local pr_labels="" n
    for n in $(repo_names); do [ "$(repo_land "$n")" = pr ] && pr_labels="$pr_labels repo:$n"; done
    python3 -I - "$t" "$SPIRA_ASK_LABEL" "$HOME_LABEL" "$HOLD_MIN" "$REWORK_MIN" "$CERT_MIN" "$WAIT_MIN" "$pr_labels" <<'PY'
import calendar, json, re, subprocess, sys, time
T, ask, home = sys.argv[1:4]
hold_min, rework_min, cert_min, wait_min = (int(x) for x in sys.argv[4:8])
pr_labels = set(sys.argv[8].split())
rows = {r["bead_id"]: r for r in json.load(open(T + "/rows.json"))}
content = {b["id"]: b for b in json.load(open(T + "/content.json"))}
term = {r["bead_id"]: r["state"] for r in json.load(open(T + "/term.json"))}
DONE = ("LANDED", "DONE", "DROPPED", "SUPERSEDED")
now = time.time()
REVIEW = re.compile(r"^review-by:(\d{4})-(\d{2})-(\d{2})(\S*)\s*(.*)$", re.S)
def review_by(reason):
    m = REVIEW.match(reason or "")
    if not m:
        return None
    try:
        return calendar.timegm((int(m[1]), int(m[2]), int(m[3]), 0, 0, 0))
    except (ValueError, OverflowError):
        return None
def review_text(reason):
    return REVIEW.match(reason or "")[5][:70]
def details(x):
    if isinstance(x, str):
        try:
            return details(json.loads(x))
        except ValueError:
            return []
    if isinstance(x, dict):
        return [v for k, v in x.items() if k == "detail" and isinstance(v, str)] + [d for v in x.values() for d in details(v)]
    return [d for v in x for d in details(v)] if isinstance(x, list) else []
def held_reason(i, reason):
    if review_by(reason) is not None:
        return reason
    try:
        # batch-job: one history read per manual hold whose row reason lost its review-by; bounded by the daemon row cadence
        h = json.loads(subprocess.run(["timeout", "20", "spira-lc", "history", i, "--machine", "bead"], capture_output=True, text=True).stdout or "[]")
    except (ValueError, OSError):
        return reason
    for e in reversed(h):
        if e.get("event") == "Unhold" and e.get("applied"):
            break
        if e.get("event") == "Hold" and e.get("applied"):
            for d in details(e.get("evidence")):
                if review_by(d) is not None:
                    return d
            break
    return reason
rowless = []
out ={k: [] for k in ("OK", "BLOCKED", "HELD", "ASK", "SUSPECT")}
for i, b in content.items():
    if i not in rows and i not in term and b.get("issue_type") not in ("epic", "event") and ask not in b.get("labels", []):
        out["SUSPECT"].append(f"{i} ROWLESS open bead with no lifecycle row")
        if b.get("status") == "open":
            repo = next((l[5:] for l in b.get("labels", []) if l.startswith("repo:")), "-")
            rowless.append("\t".join((i, str(b.get("priority", 2)), repo, (b.get("title") or "").replace("\t", " ").replace("\n", " "))))
for i, r in sorted(rows.items()):
    st = r["state"]
    holds = json.loads(r.get("holds") or "[]")
    labels = content.get(i, {}).get("labels", [])
    age = int((now - int(r.get("since") or r.get("updated_at") or now)) / 60)
    blk = r.get("blocked_by") or []
    if isinstance(blk, str):
        blk = json.loads(blk or "[]")
    live = [x for x in blk if term.get(x) not in DONE]
    why = (r.get("reason") or "")[:70]
    def put(k, msg):
        out[k].append(f"{i} {st} {msg}".rstrip())
    if ask in labels:
        put("ASK", "waiting on the operator")
    elif "poison" in holds:
        put("SUSPECT", f"POISONED ({why})")
    elif "ask" in holds:
        put("SUSPECT", f"ASK HOLD unanswered ({why})")
    elif "manual" in holds and (hr := held_reason(i, r.get("reason"))) and review_by(hr) is not None:
        due = review_by(hr)
        if due > now:
            put("HELD", f"manual hold until review {time.strftime('%Y-%m-%d', time.gmtime(due))}: {review_text(hr)}")
        else:
            put("SUSPECT", f"HOLD REVIEW OVERDUE since {time.strftime('%Y-%m-%d', time.gmtime(due))} ({review_text(hr)})")
    elif "manual" in holds and age > hold_min:
        put("SUSPECT", f"STALE HOLD manual, over {hold_min}m, no review-by ({why})")
    elif holds:
        put("HELD", f"holds={holds} {why}")
    elif blk and not live:
        put("SUSPECT", f"STALE EDGE blocked_by {blk} all terminal")
    elif live:
        put("BLOCKED", "by " + ",".join(live))
    elif st == "REWORK" and age > rework_min:
        put("SUSPECT", f"UNCLAIMED REWORK over {rework_min}m ({why})")
    elif st == "CERTIFIED" and home not in labels and age > cert_min:
        put("SUSPECT", f"CERTIFIED outside {home}, no batch opened")
    elif st in ("SUBMITTED", "CERTIFIED") and age > wait_min and not pr_labels & set(labels):
        put("SUSPECT", f"WAITING over {wait_min}m for a round")
    elif st == "WORKING" and int(r.get("lease_until") or 0) and int(r["lease_until"]) < now:
        put("SUSPECT", f"LEASE EXPIRED holder {str(r.get('holder'))[:28]}")
    else:
        put("OK", "")
open(T + "/rowless.tsv", "w").write("".join(r + "\n" for r in rowless))
print(f"BEAD SWEEP {len(rows)} live rows; " + ", ".join(f"{k} {len(v)}" for k, v in out.items()))
for k in ("SUSPECT", "BLOCKED", "HELD", "ASK", "OK"):
    for l in out[k]:
        print(f"{k} {l}")
PY
}

_bs_say() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:bead-sweep] %s\n' "$(date -u +%FT%TZ)" "$1" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    return 0
}

# An open bead with no lifecycle row is converged, not reported: create-bead is an ignoring
# insert, so repeating it is safe. Only a repair that fails stays a SUSPECT.
_bs_converge_rowless() {
    local f="$1" id prio repo title args
    while IFS=$'\t' read -r id prio repo title; do
        [ -n "$id" ] || continue
        args=(create-bead "$id" --priority "$prio" --title "$title")
        [ "$repo" != "-" ] && args+=(--repo "$repo")
        # batch-job: one point write per rowless bead, bounded by timeout(1)
        if timeout 30 spira-lc "${args[@]}" >/dev/null 2>&1; then
            _bs_say "REPAIRED $id open bead had no lifecycle row; created it (priority $prio, repo ${repo/#-/none})"
            REPAIRED_IDS+=("$id")
        fi
    done < "$f"
}

_bs_tick() {
    local t out rc line k id
    REPAIRED_IDS=()
    t="$(mktemp -d)"
    out="$(_bs_classify "$t")"; rc=$?
    [ "$rc" -eq 0 ] && [ -s "$t/rowless.tsv" ] && _bs_converge_rowless "$t/rowless.tsv"
    rm -rf "$t"
    if [ "$rc" -ne 0 ]; then echo "SWEEP FAILED: spira-lc unreadable"; return 1; fi
    for id in "${REPAIRED_IDS[@]}"; do out="$(grep -v "^SUSPECT $id ROWLESS" <<< "$out")"; done
    local -A now_set=()
    while IFS= read -r line; do
        case "$line" in
            SUSPECT\ *)
                now_set["$line"]=1
                [ -n "${_bs_seen[$line]:-}" ] || { _bs_seen["$line"]=1; _bs_say "$line"; } ;;
        esac
    done <<< "$out"
    for k in "${!_bs_seen[@]}"; do [ -n "${now_set[$k]:-}" ] || unset "_bs_seen[$k]"; done
    return 0
}

_bs_show() {
    local t out
    t="$(mktemp -d)"
    out="$(_bs_classify "$t")" || { rm -rf "$t"; echo "SWEEP FAILED: spira-lc unreadable"; return 2; }
    rm -rf "$t"
    printf '%s\n' "$out"
}

_bs_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "bead-sweep: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=1800 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "bead-sweep: last good pass ${age}s ago (interval ${iv}s) — blind, hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=1800 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "bead-sweep.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        _bs_tick && _bs_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        --show|-s|"") _bs_show; exit $? ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: bead-sweep.sh [--show] | watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
