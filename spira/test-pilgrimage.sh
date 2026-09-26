#!/usr/bin/env bash
#
# test-pilgrimage.sh — a completed pilgrimage announces itself via mail.sh.
#
# TWO TIERS:
#
#   T2 (git + files, no bd) — pilgrimage_branches_landed, called directly with
#   children_ids/spira_repos stubbed. This is the sp-qj8n assertion itself: a live
#   push-mode branch with no LANDED landstate entry blocks an epic's close. Real git
#   worktrees and real landstate files, because the assertion calls `git show-ref` and
#   reads the file — a mock of either would only prove the check can read what the test
#   wrote (law-prefer-the-real-dependency).
#
#   T3 (one bd seed, real bd) — the CLI path: announce, close, a second silent pass,
#   an unfinished epic left alone, and an epic outside Spira's partition left alone. One
#   seed carries all three epics; two `check` passes prove announce-once and idempotency
#   without a reset between them.
#
# EVERY CASE HAS ITS NEGATIVE. An epic with an open child must emit NOTHING, and the suite
# proves the check could have seen something by running the positive first — an assertion of
# absence from a probe that was never pointed at anything is indistinguishable from a pass
# (law-absence-needs-a-positive-control).
# defect: sp-obd sp-1wzp sp-qj8n
# tier: T3
# covers: spira/pilgrimage.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
eq()     { [ "$3" = "$2" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# =========================================================================================
echo "T2 — pilgrimage_branches_landed directly: git + files, children_ids/spira_repos stubbed"
# =========================================================================================
# The defect (sp-qj8n): a child's branch was present in the repo, its bead was closed, but
# landing.sh had never written LANDED to landstate — marker commits were lost. Pilgrimage
# closed the epic over this and left the branch permanently unlanded. The assertion: before
# closing an epic, every child with a live spira/* branch in a push-mode repo must appear in
# landstate as LANDED. If any is missing or non-LANDED, the close is deferred.

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export SPIRA_HOME="$HERE"
export SPIRA_CONF="$TMP/no-such-conf"
# NEVER THE REAL STORE. Nothing in this section calls bd (children_ids and spira_repos are
# both stubbed below), but sourcing pilgrimage.sh still sources conf.sh, whose schema check
# runs against $SPIRA_DB if a store exists there — a nonexistent path skips it.
export SPIRA_DB="$TMP/no-such-db"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN/landstate"

LREMOTE="$TMP/lrepo.git"
git init -q --bare -b main "$LREMOTE"
git_work="$TMP/lrepo-work"
git init -q -b main "$git_work"
git -C "$git_work" commit -q --allow-empty -m base
git -C "$git_work" remote add origin "$LREMOTE"
git -C "$git_work" push -q origin main
git -C "$git_work" fetch -q origin

LMAP="$TMP/lrepo-map"
printf 'lrepo | %s | push | origin/main | |\n' "$git_work" > "$LMAP"
export SPIRA_REPO_MAP="$LMAP"

# shellcheck disable=SC1090
. "$HERE/pilgrimage.sh"   # guarded: sourcing runs no command (see the BASH_SOURCE check)

# STUBBED: the two facts pilgrimage_branches_landed pulls from bd and the repo-map file
# resolution path. Everything else it does — show-ref, landstate reads — is real.
spira_repos()  { printf 'lrepo\n'; }
children_ids() { printf 'sp-assert-c1\nsp-assert-c2\n'; }

git -C "$git_work" worktree add -q -b "spira/sp-assert-c1" "$TMP/wt-c1" main
printf 'c1\n' > "$TMP/wt-c1/c1.txt"
git -C "$TMP/wt-c1" add -A
git -C "$TMP/wt-c1" commit -q -m "feat: sp-assert-c1 — work"

out="$(pilgrimage_branches_landed sp-assert-epic)"; rc=$?
want "a live branch with no landstate entry blocks the close" \
     "ASSERTION — spira/sp-assert-c1 in lrepo is live but landstate reads 'missing'" "$out"
eq   "and pilgrimage_branches_landed returns 1" "1" "$rc"

# A non-LANDED state (e.g. GATED) also blocks the close.
printf 'GATED abc123 1234567890 fixture\n' > "$SPIRA_RUN/landstate/sp-assert-c1"
out="$(pilgrimage_branches_landed sp-assert-epic)"; rc=$?
want "a GATED landstate entry also blocks the close" "landstate reads 'GATED'" "$out"
eq   "and still returns 1" "1" "$rc"

# Once the LANDED entry exists, the epic may close.
printf 'LANDED abc123 1234567890 lrepo\n' > "$SPIRA_RUN/landstate/sp-assert-c1"
out="$(pilgrimage_branches_landed sp-assert-epic)"; rc=$?
eq     "with a LANDED entry, pilgrimage_branches_landed returns 0" "0" "$rc"
nowant "and logs no assertion" "ASSERTION" "$out"

git -C "$git_work" worktree remove --force "$TMP/wt-c1" 2>/dev/null
git -C "$git_work" branch -q -D spira/sp-assert-c1 2>/dev/null
rm -f "$SPIRA_RUN/landstate/sp-assert-c1"

# A CHILD IN A PR-MODE REPO IS NOT CHECKED — landing.sh does not write landstate for pr.
printf 'lrepo | %s | pr | origin/main | |\n' "$git_work" > "$LMAP"
children_ids() { printf 'sp-pr-c1\n'; }
git -C "$git_work" worktree add -q -b "spira/sp-pr-c1" "$TMP/wt-pr-c1" main
printf 'pr\n' > "$TMP/wt-pr-c1/pr.txt"
git -C "$TMP/wt-pr-c1" add -A
git -C "$TMP/wt-pr-c1" commit -q -m "feat: sp-pr-c1 — work"
# No landstate entry for sp-pr-c1, and none needed.

out="$(pilgrimage_branches_landed sp-pr-epic)"; rc=$?
eq     "a pr-mode branch is not checked — returns 0 regardless of landstate" "0" "$rc"
nowant "and logs no assertion" "ASSERTION" "$out"

git -C "$git_work" worktree remove --force "$TMP/wt-pr-c1" 2>/dev/null
git -C "$git_work" branch -q -D spira/sp-pr-c1 2>/dev/null

# =========================================================================================
echo
echo "T3 — announce / close / second-pass-silent / unfinished / alien, one seed:"
# =========================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-pilgrimage
testdb_up pilgrimage || { echo "testdb_up failed"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

DB="$SPIRA_DB"
BD="${TESTDB_BD:-bd}"
# COCKPIT_DB IS EXPORTED EXPLICITLY, never left to conf.sh's default. The operator's
# spira.conf may name COCKPIT_DB, and the environment is the only source that outranks it —
# without this line a suite on his box writes its fixtures into the live database.
export COCKPIT_DB="$DB"
export SPIRA_RUN="$TMP/run2"; mkdir -p "$SPIRA_RUN"

child() {   # child <id> <status> <parent>
    printf '{"id":"%s","title":"child %s","description":"d","status":"%s","issue_type":"task","labels":["spira","plan"],"dependencies":[{"issue_id":"%s","depends_on_id":"%s","type":"parent-child"}]}\n' \
        "$1" "$1" "$2" "$1" "$3"
}
status_of() { "$BD" -C "$DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
    | python3 -c 'import json,sys;d=json.load(sys.stdin);print((d[0] if isinstance(d,list) else d).get("status") or "")'; }

MAILDIR="$TMP/maildir"
n_mails() { ls "$MAILDIR/operator/new/" 2>/dev/null | wc -l | tr -d ' '; }
mail_content() { cat "$MAILDIR/operator/new/"* 2>/dev/null; }

run() { SPIRA_DB="$DB" SPIRA_MAIL="$MAILDIR" "$HERE/pilgrimage.sh" check 2>&1; }

# One seed, three epics: a finished one (announce/close), one still going (silence), and
# one outside Spira's partition (silence). No branch/landstate for any of them, so all three
# clear the T2 assertion above trivially (no push-mode branch is live for any child).
testdb_seed <<JSONL
{"id":"sp-done","title":"a finished pilgrimage","description":"d","status":"open","issue_type":"epic","labels":["spira","plan"]}
$(child sp-d1 closed sp-done)
$(child sp-d2 closed sp-done)
{"id":"sp-part","title":"still going","description":"d","status":"open","issue_type":"epic","labels":["spira","plan"]}
$(child sp-p1 closed sp-part)
$(child sp-p2 open   sp-part)
{"id":"sp-alien","title":"someone else's epic","description":"d","status":"open","issue_type":"epic","labels":["repo:town"]}
$(child sp-a1 closed sp-alien)
JSONL

out="$(run)"
want "the run announces the finished epic"     "PILGRIMAGE COMPLETE — sp-done" "$out"
nowant "and says nothing of the unfinished one" "sp-part" "$out"
nowant "or of the one outside Spira's partition" "sp-alien" "$out"
eq   "and sent exactly one notification"       "1"       "$(n_mails)"
want "notification carries the target"         "target: sp-done"  "$(mail_content)"
want "and carries which children closed"       "sp-d1"            "$(mail_content)"

eq "the finished epic is closed once the notice is out" "closed" "$(status_of sp-done)"
eq "the unfinished epic stays open"                      "open"   "$(status_of sp-part)"
eq "the alien epic is left open for whoever owns it"     "open"   "$(status_of sp-alien)"

out="$(run)"
nowant "a second pass announces nothing — the marker holds" "PILGRIMAGE COMPLETE" "$out"
eq    "and sends no second notification"       "1"       "$(n_mails)"

tl_summary
