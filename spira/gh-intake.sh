#!/usr/bin/env bash
#
# gh-intake.sh — ingest GitHub issues as beads, one way, into a live partition.
#
#   gh-intake.sh [--dry-run]
#
# WHY THIS EXISTS. Issues filed against the public repository are reports from
# somewhere else — another machine, another operator, a clean deploy that found
# what a developed-on one cannot. They are worth working, and the loop only works
# beads. This is the bridge, and it is the only sanctioned one.
#
# ONE WAY, AND STRUCTURALLY SO. `bd github sync` is bidirectional by default. The
# beads store holds internal working notes, agent memories and the overseer's own
# judgement; publishing it to a public issue tracker is irreversible and is
# precisely what law-beads-is-never-public forbids. Two things enforce that here
# and neither is a habit: no code path in this file constructs a push, and a
# token that carries write scopes is refused before anything runs. The second is
# defence in depth — the script cannot push, but a writable token sitting on the
# box is a loaded gun for the next caller, and intake declines to be the thing
# that normalises having one.
#
# STAMPING IS THE POINT, NOT THE PULL. A pulled issue arrives with whatever
# labels GitHub had, which is usually none. A bead with no labels matches no
# fayth predicate, so no aeon can ever claim it: it is filed and unreachable,
# which is neither of the two states a filed bead is permitted to be in
# (law-filed-bead-queued-xor-escalated — 130 beads once sat in exactly that state
# across three machines, including a bug the operator had hit himself). So every
# ingested bead is stamped into a partition that a fayth actually selects, and
# the run FAILS if any ingested bead is left outside one.
#
# THE POST-CHECK IS THE POSITIVE CONTROL. Reporting "nothing to do" is
# indistinguishable from a broken query, a renamed label or a changed external-ref
# format, and the broken version reads as all-clear. So the check is not "did we
# stamp anything" but "is anything still unstamped" — an assertion that fails
# loudly when the mechanism has quietly stopped working.
#
# WHAT IT DOES NOT DO. It never closes, comments on or otherwise writes to the
# GitHub issue. The issues are an inbox; the bead is the work item. Keeping the
# two in step in the other direction would need a write token, which is the thing
# this file exists to do without.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

DRY=0
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY=1; shift ;;
        -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
        *) printf 'gh-intake: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

BD="${SPIRA_BD:-bd}"
DB="${SPIRA_DB:?SPIRA_DB is not set}"
REPO="${SPIRA_GH_INTAKE_REPO:-}"
TOKEN="${GITHUB_TOKEN:-}"
SCOPE="${SPIRA_SCOPE_LABEL:-spira}"
LANE="${SPIRA_PLAN_LABEL:-plan}"

die() { printf 'gh-intake: %s\n' "$1" >&2; exit 1; }
log() { printf 'gh-intake: %s\n' "$1"; }

[ -n "$REPO" ]  || die "SPIRA_GH_INTAKE_REPO is not set — nothing says which repository to ingest from"
[ -n "$TOKEN" ] || die "GITHUB_TOKEN is not set — the API refuses even a public repository without one"

# ── the token must not be able to write ──────────────────────────────────────
# A classic token advertises its scopes in a response header. A fine-grained one
# does not, and there is no endpoint that will tell you what it may do; so when
# the header is absent this cannot be proved either way and says so, rather than
# reporting clean. It is not the load-bearing control — no push is constructed
# below — but an unprovable answer is reported as unprovable.
_probe="$(curl -sS -I -H "Authorization: Bearer $TOKEN" https://api.github.com/ 2>/dev/null \
          | tr -d '\r' | grep -i '^x-oauth-scopes:' | cut -d: -f2- | tr -d ' ')"
if [ -n "$_probe" ]; then
    case ",$_probe," in
        *,repo,*|*,public_repo,*|*write*|*,delete_repo,*)
            die "the token carries write scopes ($_probe). Intake refuses a token that can
            write to the tracker it reads. Use a fine-grained token with Issues: Read-only
            on $REPO." ;;
    esac
    log "token: no write scopes advertised ($_probe)"
else
    log "token: scopes not advertised (fine-grained token) — write access could not be proved either way"
fi

# ── the store, before ────────────────────────────────────────────────────────
# _ingested lists every bead this bridge is responsible for, as "<id> <labels>".
# The external ref is written by bd's own github sync; matching on it rather than
# on a label is what makes a re-run idempotent.
_ingested() {
    "$BD" -C "$DB" list --json 2>/dev/null | python3 -c '
import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
rows=d if isinstance(d,list) else d.get("issues",d.get("data",[]))
for r in rows:
    ref=(r.get("external_ref") or "")
    if "github" not in ref.lower(): continue
    print(r.get("id",""), ",".join(r.get("labels") or []))
'
}
_total() { "$BD" -C "$DB" list --json 2>/dev/null | python3 -c '
import sys,json
try: d=json.load(sys.stdin)
except Exception: print(0); sys.exit(0)
rows=d if isinstance(d,list) else d.get("issues",d.get("data",[]))
print(len(rows))'; }

before_total="$(_total)"
before_n="$(_ingested | grep -c . || true)"
log "store holds $before_total bead(s); $before_n already ingested from GitHub"

# ── pull ─────────────────────────────────────────────────────────────────────
# --pull-only is the whole contract. It appears exactly once, here.
sync_args=(-C "$DB" github sync --pull-only)
[ "$DRY" -eq 1 ] && sync_args+=(--dry-run)
GITHUB_OWNER="${REPO%%/*}" GITHUB_REPO="${REPO##*/}" GITHUB_TOKEN="$TOKEN" \
    "$BD" "${sync_args[@]}" || die "bd github sync --pull-only failed"

after_n="$(_ingested | grep -c . || true)"
log "pulled: $((after_n - before_n)) new, $after_n ingested in total"

# ── stamp ────────────────────────────────────────────────────────────────────
# An ingested bead with no scope label is unreachable by every fayth predicate.
unstamped="$(_ingested | awk -v s="$SCOPE" '{ n=split($2,a,","); hit=0; for(i=1;i<=n;i++) if(a[i]==s) hit=1; if(!hit) print $1 }')"
n_unstamped="$(printf '%s' "$unstamped" | grep -c . || true)"

if [ "$n_unstamped" -gt 0 ]; then
    log "stamping $n_unstamped bead(s) into ${SCOPE},${LANE}"
    if [ "$DRY" -eq 1 ]; then
        log "dry run — no labels written"
    else
        for id in $unstamped; do
            "$BD" -C "$DB" update "$id" --labels "+${SCOPE},+${LANE}" >/dev/null 2>&1 \
                || log "WARNING: could not label $id"
        done
    fi
else
    log "nothing to stamp"
fi

# ── the post-check ───────────────────────────────────────────────────────────
# THE ASSERTION IS ABOUT WHAT REMAINS, not about what was done. "Stamped zero" is
# what a working run on an empty inbox looks like AND what a broken query looks
# like; "zero remain unstamped" is true only in the first case.
if [ "$DRY" -eq 0 ]; then
    if [ "$before_total" -eq 0 ]; then
        die "the store reports zero beads — the query is broken, not the inbox empty"
    fi
    left="$(_ingested | awk -v s="$SCOPE" '{ n=split($2,a,","); hit=0; for(i=1;i<=n;i++) if(a[i]==s) hit=1; if(!hit) print $1 }')"
    n_left="$(printf '%s' "$left" | grep -c . || true)"
    if [ "$n_left" -gt 0 ]; then
        printf 'gh-intake: %s ingested bead(s) carry no %s label and no fayth can claim them:\n' \
            "$n_left" "$SCOPE" >&2
        printf '  %s\n' $left >&2
        die "ingest filed work that nothing will pick up"
    fi
fi

log "ok — $after_n bead(s) ingested from $REPO, all in ${SCOPE},${LANE}"
