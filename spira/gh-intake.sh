#!/usr/bin/env bash
#
# gh-intake.sh — ingest public GitHub issues as beads, one way, into a live partition.
#
#   gh-intake.sh [--dry-run]
#
# WHY THIS EXISTS. Issues filed against the public repository are reports from
# somewhere else — another machine, a clean deploy that finds what a developed-on
# one cannot. They are worth working, and the loop only works beads.
#
# NO CREDENTIAL, AND THAT IS THE POINT. The tracker is public: anyone may read it
# without authenticating. A token here would exist only to satisfy a client
# library, and a token on the box is one the next caller can misuse — a sync run
# without --pull-only would publish internal working notes and the overseer's own
# judgement to a public issue list, which law-beads-is-never-public forbids and
# which cannot be undone. With no credential anywhere in this path, "the bridge
# cannot write to GitHub" stops being a property to probe and becomes a fact
# about the machine. That is why this talks to the API directly rather than
# through a sync client that authenticates unconditionally.
#
# STAMPING IS THE POINT, NOT THE FETCH. A bead with no labels matches no fayth
# predicate, so no aeon can ever claim it: filed and unreachable, which is
# neither of the two states a filed bead may be in (law-filed-bead-queued-xor-
# escalated — 130 beads once sat exactly there across three machines). Every
# ingested bead is created directly into a partition a fayth selects, and the run
# FAILS if any ingested bead is left outside one.
#
# THE POST-CHECK ASSERTS WHAT REMAINS, not what was done. "Created zero" is what a
# working run on an empty inbox looks like and equally what a broken query, a
# renamed label or a changed ref format looks like — and the broken one reads as
# all-clear. "Zero remain unstamped" is true only in the first case.
#
# IDEMPOTENT BY EXTERNAL REF. Re-running ingests nothing twice: the ref
# github:<owner>/<repo>#<number> is the join key, matched against the store
# before anything is created.
#
# WHAT IT NEVER DOES. It does not close, comment on, label or otherwise write to
# the GitHub issue. The tracker is an inbox; the bead is the work item. Keeping
# them in step in the other direction would need a credential, which is the thing
# this file exists to do without.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
. "$HERE/lib.sh"

DRY=0
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY=1; shift ;;
        -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
        *) printf 'gh-intake: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

BD="${SPIRA_BD:-bd}"
DB="${SPIRA_DB:?SPIRA_DB is not set}"
REPO="${SPIRA_GH_INTAKE_REPO:-}"
SCOPE="${SPIRA_SCOPE_LABEL:-spira}"
LANE="${SPIRA_PLAN_LABEL:-plan}"
# WHICH REPOSITORY THE WORK IS IN. aeon.sh resolves a bead's repo:<name> label
# through the repo-map to decide where to cut a worktree. A bead that names none,
# or names one the map cannot resolve, is labelled needs-ryan and parked on first
# claim — it sits open and unclaimable until a human fixes it by hand. Ingesting
# without this files work guaranteed to stall at the moment it is picked up, once
# per issue. The default is the tracker's own repository name, which is right
# whenever the issues are about the code they are filed against.
BEAD_REPO="${SPIRA_GH_INTAKE_BEAD_REPO:-${REPO##*/}}"
API="${SPIRA_GH_INTAKE_API:-https://api.github.com}"

die() { printf 'gh-intake: %s\n' "$1" >&2; exit 1; }
log() { printf 'gh-intake: %s\n' "$1"; }

[ -n "$REPO" ] || die "SPIRA_GH_INTAKE_REPO is not set — nothing says which tracker to ingest from"

# RESOLVED BEFORE ANYTHING IS CREATED, not discovered by an aeon later. This is the
# one precondition whose failure is invisible at ingest time and expensive after:
# the beads are filed, they look correct, and each one parks the first time an aeon
# reaches it.
_rr="$(repo_root "$BEAD_REPO" 2>/dev/null)" || _rr=""
if [ -z "$_rr" ] || [ ! -e "$_rr/.git" ]; then
    die "repo:$BEAD_REPO does not resolve to a checkout through $SPIRA_REPO_MAP.
       Every ingested bead would be parked by aeon.sh on first claim and left for a
       human. Add $BEAD_REPO to the repo-map, or set SPIRA_GH_INTAKE_BEAD_REPO to a
       name that resolves."
fi
log "beads will be filed against repo:$BEAD_REPO ($_rr)"

# ── what the store already holds ─────────────────────────────────────────────
# Matching on the external ref rather than on a title is what makes a re-run
# ingest nothing twice: a title can be edited on either side, a ref cannot.
_ingested() {
    "$BD" -C "$DB" list --json 2>/dev/null | python3 -c '
import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
rows=d if isinstance(d,list) else d.get("issues",d.get("data",[]))
for r in rows:
    ref=(r.get("external_ref") or "")
    if not ref.lower().startswith("github:"): continue
    print(r.get("id",""), ref, ",".join(r.get("labels") or []))
'
}
_total() { "$BD" -C "$DB" list --json 2>/dev/null | python3 -c '
import sys,json
try: d=json.load(sys.stdin)
except Exception: print(0); sys.exit(0)
rows=d if isinstance(d,list) else d.get("issues",d.get("data",[]))
print(len(rows))'; }

before_total="$(_total)"
[ "$before_total" -gt 0 ] || die "the store reports zero beads — the query is broken, not the store empty"
known="$(_ingested | awk '{print $2}')"
log "store holds $before_total bead(s); $(printf '%s' "$known" | grep -c . || true) already ingested"

# ── fetch ────────────────────────────────────────────────────────────────────
# Unauthenticated, which GitHub rate-limits to 60 requests an hour. An ingest
# costs one request per hundred issues, so the limit is not a constraint — but a
# 403 for exhausting it and a 403 for anything else look alike, so it is named
# rather than folded into a generic failure.
# ONE FILE PER PAGE, never one concatenated stream. The API pretty-prints its
# JSON, so a page spans many lines; appending pages to a single file and parsing
# it a line at a time finds no complete document and reports an empty tracker —
# which is indistinguishable from a wrong repository name.
FEED="$(mktemp -d)"; trap 'rm -rf "$FEED"' EXIT INT TERM
page=1
while :; do
    body="$(curl -sS --max-time 30 \
        "$API/repos/$REPO/issues?state=open&per_page=100&page=$page" 2>/dev/null)" || \
        die "could not reach $API — no issues were read"
    n="$(printf '%s' "$body" | python3 -c '
import sys,json
try: d=json.load(sys.stdin)
except Exception: print("ERR"); sys.exit(0)
if isinstance(d,dict):
    print("MSG:"+str(d.get("message",""))); sys.exit(0)
print(len(d))' )"
    case "$n" in
        ERR)       die "the API returned something that is not JSON — refusing to guess at it" ;;
        MSG:*)     die "the API refused: ${n#MSG:}" ;;
    esac
    printf '%s' "$body" > "$FEED/page-$page.json"
    [ "$n" -ge 100 ] || break
    page=$((page+1))
    [ "$page" -le 20 ] || die "more than 2000 open issues — refusing to page further"
done

# PULL REQUESTS ARE NOT ISSUES. The issues endpoint returns both; a pull request
# carries a pull_request key and nothing else distinguishes it. Ingesting them
# files a bead for every pull request ever opened.
mapfile -t ROWS < <(python3 - "$FEED" "$REPO" <<'PY'
import sys,json,os,glob
feed,repo=sys.argv[1],sys.argv[2]
out=[]
for f in sorted(glob.glob(os.path.join(feed,"page-*.json"))):
    try: d=json.load(open(f))
    except Exception: continue
    if not isinstance(d,list): continue
    for i in d:
        if "pull_request" in i: continue
        out.append((i["number"], i.get("title") or "", i.get("body") or ""))
for n,t,b in sorted(out):
    print(json.dumps({"ref":"github:%s#%d"%(repo,n),"title":t,"body":b}))
PY
)
log "fetched ${#ROWS[@]} open issue(s) from $REPO"
[ "${#ROWS[@]}" -gt 0 ] || die "the tracker reported no open issues — that is possible, but it is also what a wrong repository name looks like. Check SPIRA_GH_INTAKE_REPO=$REPO"

# ── create what is missing ───────────────────────────────────────────────────
created=0; skipped=0
for row in "${ROWS[@]}"; do
    ref="$(printf '%s' "$row" | python3 -c 'import sys,json;print(json.load(sys.stdin)["ref"])')"
    if printf '%s\n' "$known" | grep -qxF "$ref"; then
        skipped=$((skipped+1)); continue
    fi
    title="$(printf '%s' "$row" | python3 -c 'import sys,json;print(json.load(sys.stdin)["title"])')"
    if [ "$DRY" -eq 1 ]; then
        printf '  would create: %s  %s\n' "$ref" "${title:0:70}"
        created=$((created+1)); continue
    fi
    # CREATED INTO THE PARTITION, not created and then labelled. A bead that
    # exists for even one sentinel pass without its labels is a bead the loop has
    # already declined to claim.
    printf '%s' "$row" | python3 -c '
import sys,json
d=json.load(sys.stdin)
print("Ingested from %s\n\n%s" % (d["ref"], d["body"]))' \
      | "$BD" -C "$DB" create "$title" \
            --external-ref "$ref" \
            --labels "$SCOPE,$LANE,repo:$BEAD_REPO" \
            -t bug \
            --body-file - >/dev/null 2>&1 \
        && created=$((created+1)) \
        || log "WARNING: could not create a bead for $ref"
done
log "created $created, already present $skipped"

# ── the post-check ───────────────────────────────────────────────────────────
if [ "$DRY" -eq 0 ]; then
    left="$(_ingested | awk -v s="$SCOPE" '{ n=split($3,a,","); hit=0; for(i=1;i<=n;i++) if(a[i]==s) hit=1; if(!hit) print $1" "$2 }')"
    n_left="$(printf '%s' "$left" | grep -c . || true)"
    if [ "$n_left" -gt 0 ]; then
        printf 'gh-intake: %s ingested bead(s) carry no %s label and no fayth can claim them:\n' \
            "$n_left" "$SCOPE" >&2
        printf '%s\n' "$left" >&2
        die "ingest filed work that nothing will pick up"
    fi
fi

log "ok — $REPO ingested into ${SCOPE},${LANE}"
