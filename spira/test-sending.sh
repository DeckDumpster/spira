#!/usr/bin/env bash
#
# test-sending.sh — the Sending gives every spira/* branch exactly one disposition, and
#   the extracted send_disposition decides it without touching a ref, a worktree, or bd.
#
#   ./test-sending.sh
#
# WHY THIS EXISTS (sp-rg46a). Six suites and half of a seventh each built their own
# testdb, bare remote and clone just to run one sending.sh pass over one or two branches:
# test-sending-{unlanded-guard,certified-guard,content-label,empty-commit,landstate-assert,
# squash-merged}.sh and the sending half of test-content-landed-empty-branch.sh — 50s of
# duplicate fixture cost for facts about the same function. This suite is their
# replacement: one fixture, one pass, a row per disposition, driven by a stub bd instead
# of a throwaway database (sending.sh reaches bd only through `bdjson show` and
# `bdq label add/remove`, so a stub answering those two shapes is the seam contract, not a
# model that can drift — the same argument test-held.sh already made for held.sh).
#
# A CONFIRMED DEAD PATH. content_landed returns true for ANY branch with zero commits
# ahead of the base, because "zero ahead" and "is an ancestor of the base" are the same
# fact (sp-bf31a) — so the FAST-FORWARD/non-code-delivers/open-zero-ahead arms deeper in
# send_disposition, all gated on `ahead == 0`, can never run: content_landed already
# returned true and sent the branch out through the content-landed arm first. This is not
# new here — test-sending-closed-reap.sh already documents the same shape for the
# non-code-delivers and superseded-empty cases ("SENT, not REAPED, since the [...] arm is
# never reached") — so this suite tests what ahead=0 branches actually do (SEND
# content-landed, unlabelled) rather than asserting a verdict the code cannot produce.
#
# THE SOURCE-GUARD THIS SUITE NEEDED. sending.sh ran a live sweep as a side effect of
# being sourced. Two cases here — the mid-send HELD recheck and the FAILED/exit-status
# path — call send_disposition and send_branch directly, so sending.sh gained the same
# `BASH_SOURCE[0] != $0` guard landing.sh and pilgrimage.sh already carry.
#
# defect: sp-mqsl, sp-e5ow0, sp-796o, sp-kq8l, sp-bjzj, sp-3gih, sp-hl92, sp-1smg
# tier: T2
# covers: spira/sending.sh spira/lib.sh UC-landed-audit-reaping-14 UC-landed-audit-reaping-16 UC-landed-audit-reaping-17 UC-landed-audit-reaping-18 UC-landed-audit-reaping-19 UC-landed-audit-reaping-20 UC-landed-audit-reaping-21 UC-landed-audit-reaping-22
# hermetic-ok: stub bd (a JSON-file-per-id fixture), a stub gh, and local git repos — no
#   database, no systemd, no real network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export SPIRA_CONF="$TMP/no-such-conf"

# --------------------------------------------------------------------------------------
# THE STUB BD. sending.sh's only bd seams are `bdjson show <id>` (read) and
# `bdq label add/remove <id> <label>` (write). One JSON object per id, under
# $STUB_BEADS_DIR/<id>.json; label add/remove mutate it in place, so the content-landed
# and branch: label assertions read back what sending.sh actually wrote — a real seam
# contract, not a canned answer (law-a-control-that-cannot-check-must-refuse's positive
# twin: a stub that cannot be written to cannot prove a write happened).
# --------------------------------------------------------------------------------------
STUB_BEADS_DIR="$TMP/beads"; mkdir -p "$STUB_BEADS_DIR"
STUB_BD="$TMP/bd-stub"
cat > "$STUB_BD" <<'EOF'
#!/usr/bin/env bash
dir="${STUB_BEADS_DIR:?}"
cmd=""; skip_next=0; argv=()
for a in "$@"; do
    if [ "$skip_next" = 1 ]; then skip_next=0; continue; fi
    case "$a" in
        -C)          skip_next=1 ;;
        --json)      ;;
        show|label)  cmd="$a" ;;
        *)           argv+=("$a") ;;
    esac
done
case "$cmd" in
    show)
        f="$dir/${argv[0]:-}.json"
        if [ -f "$f" ]; then printf '['; cat "$f"; printf ']\n'; else printf '[]\n'; fi
        ;;
    label)
        f="$dir/${argv[1]:-}.json"
        [ -f "$f" ] || exit 0
        python3 - "$f" "${argv[0]:-}" "${argv[2]:-}" <<'PY'
import json, sys
f, sub, lbl = sys.argv[1], sys.argv[2], sys.argv[3]
d = json.load(open(f))
labs = d.get("labels") or []
if sub == "add" and lbl not in labs: labs.append(lbl)
if sub == "remove" and lbl in labs: labs.remove(lbl)
d["labels"] = labs
json.dump(d, open(f, "w"))
PY
        ;;
esac
EOF
chmod +x "$STUB_BD"
export SPIRA_BD="$STUB_BD" STUB_BEADS_DIR SPIRA_DB="$TMP/no-such-db"

bead() {   # bead <id> <status> [dependencies-json] [labels-json] -> writes the fixture row
    local id="$1" status="$2" deps="${3:-[]}" labs="${4:-[]}"
    printf '{"id":"%s","status":"%s","labels":%s,"dependencies":%s}' \
        "$id" "$status" "$labs" "$deps" > "$STUB_BEADS_DIR/$id.json"
}
labels_of() { python3 -c '
import json, sys
try: d = json.load(open(sys.argv[1]))
except Exception: sys.exit(0)
print(",".join(d.get("labels") or []))' "$STUB_BEADS_DIR/$1.json" 2>/dev/null; }
has_label() { case ",$(labels_of "$1")," in *",$2,"*) return 0 ;; *) return 1 ;; esac; }

# --------------------------------------------------------------------------------------
# THE GIT FIXTURE. One repository, one bare remote, one repo-map with two rows: the home
# repo (resolvable base, real remote) and a second, unresolvable-ref repo for the SKIP
# case (UC-21) — both swept in the SAME sending.sh invocation.
# --------------------------------------------------------------------------------------
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$RUN/landstate"
export SPIRA_RUN="$RUN" SPIRA_REAPLOG="$RUN/reap.log"

OTHER="$TMP/other"; git init -q -b main "$OTHER"   # no remote, no commit: base unresolvable

HOME_REPO="$(basename "$REPO")"
printf '%s | %s | push | main | |\nother | %s | push | | |\n' "$HOME_REPO" "$REPO" "$OTHER" \
    > "$TMP/repo-map"

STATUS_FILE="$TMP/status-from"

sending() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$STUB_BD" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" SPIRA_GH="${SPIRA_GH:-$TMP/no-such-gh}" \
    SPIRA_REPO_MAP="$TMP/repo-map" \
        bash "$HERE/sending.sh" --no-fetch --status-from "$STATUS_FILE" "$@" 2>&1
}
branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/$1" 2>/dev/null; }

# ---- fixture branches ----------------------------------------------------------------

# sp-cl0: content-landed via the ancestor shortcut, ahead=0 — the branch IS the base tip.
# No commits of its own, so no content-landed label (UC-17).
git -C "$REPO" branch spira/sp-cl0 main
bead sp-cl0 open

# sp-cl1: content-landed via merge-tree equality, ahead=1 — an empty commit that names the
# bead but changes no files (sp-kq8l). Ancestry alone would refuse this (the branch is not
# reachable from origin/main); content_landed approves it because merging changes nothing.
# Landstate present, so no ASSERT line.
git -C "$REPO" checkout -q -b spira/sp-cl1 main
git -C "$REPO" commit -q --allow-empty -m "sp-cl1: review only, no file changes"
git -C "$REPO" checkout -q main
bead sp-cl1 closed
printf 'LANDED %s %s\n' "$(git -C "$REPO" rev-parse spira/sp-cl1)" "$(date +%s)" \
    > "$RUN/landstate/sp-cl1"

# sp-clnoassert: same shape as sp-cl1, but with NO landstate record — the ASSERT log line
# (sp-qj8n) must fire, and the send must proceed anyway.
git -C "$REPO" checkout -q -b spira/sp-clnoassert main
git -C "$REPO" commit -q --allow-empty -m "sp-clnoassert: review only"
git -C "$REPO" checkout -q main
bead sp-clnoassert closed

# sp-supsafe: superseded, and the base already holds a conflicting version of its one
# commit's content — merge-tree conflicts, so nothing of this branch's is missing from the
# base. REAP superseded-safe.
git -C "$REPO" checkout -q -b spira/sp-supsafe main
printf 'branch version\n' > "$REPO/shared-sup.txt"
git -C "$REPO" add shared-sup.txt && git -C "$REPO" commit -q -m "sp-supsafe: work"
git -C "$REPO" checkout -q main
printf 'base version\n' > "$REPO/shared-sup.txt"
git -C "$REPO" add shared-sup.txt && git -C "$REPO" commit -q -m "base: conflicting change"
bead sp-supsafe closed '[{"issue_id":"sp-supsafe","depends_on_id":"sp-succ","type":"supersedes"}]'

# sp-supunsafe: superseded, but its one commit adds content the base does not have and
# merge-tree succeeds cleanly (no conflict) — KEEP superseded-unsafe, and its worktree
# (attached below) must be freed even though the branch survives.
git -C "$REPO" checkout -q -b spira/sp-supunsafe main
printf 'unique content the base never got\n' > "$REPO/sp-supunsafe.txt"
git -C "$REPO" add sp-supunsafe.txt && git -C "$REPO" commit -q -m "sp-supunsafe: unique work"
git -C "$REPO" checkout -q main
bead sp-supunsafe closed '[{"issue_id":"sp-supunsafe","depends_on_id":"sp-succ","type":"supersedes"}]'
git -C "$REPO" worktree add -q "$RUN/worktree/sp-supunsafe" spira/sp-supunsafe >/dev/null 2>&1

# sp-sq: squash-merged. The PR's headRefOid equals the branch's current tip; the base moved
# on past the squash point, so content_landed is false. REAP squash-merged.
git -C "$REPO" checkout -q -b spira/sp-sq main
printf 'line1\n' > "$REPO/shared-sq.txt"
git -C "$REPO" add shared-sq.txt && git -C "$REPO" commit -q -m "sp-sq: commit A"
printf 'line1\nline2\n' > "$REPO/shared-sq.txt"
git -C "$REPO" add shared-sq.txt && git -C "$REPO" commit -q -m "sp-sq: commit B"
SQ_TIP="$(git -C "$REPO" rev-parse spira/sp-sq)"
git -C "$REPO" checkout -q main
printf 'line1\nline2\n' > "$REPO/shared-sq.txt"
git -C "$REPO" add shared-sq.txt && git -C "$REPO" commit -q -m "sp-sq: squash-merge (#1)"
printf 'line1\nline2\nline3\n' > "$REPO/shared-sq.txt"
git -C "$REPO" add shared-sq.txt && git -C "$REPO" commit -q -m "unrelated: advance shared-sq.txt"
bead sp-sq closed

# sp-otherpr: a batch commit named this bead on the base (landed()=true), but the base has
# since diverged so content_landed is false; every commit on the branch is already
# patch-equivalent upstream (git cherry finds nothing unapplied). SEND other-pr.
git -C "$REPO" checkout -q -b spira/sp-otherpr main
printf 'v1\n' > "$REPO/shared-otherpr.txt"
git -C "$REPO" add shared-otherpr.txt && git -C "$REPO" commit -q -m "sp-otherpr: add content"
git -C "$REPO" checkout -q main
printf 'v1\n' > "$REPO/shared-otherpr.txt"
git -C "$REPO" add shared-otherpr.txt && git -C "$REPO" commit -q -m "spira: land sp-otherpr"
printf 'v2\n' > "$REPO/shared-otherpr.txt"
git -C "$REPO" add shared-otherpr.txt && git -C "$REPO" commit -q -m "unrelated: advance further"
bead sp-otherpr closed

# sp-cherry: landed() is true from an OLD naming commit, but a commit was added to the
# branch AFTER that landing — git cherry finds it unapplied ('+'). KEEP cherry-unapplied.
git -C "$REPO" checkout -q -b spira/sp-cherry main
printf 'v1\n' > "$REPO/shared-cherry.txt"
git -C "$REPO" add shared-cherry.txt && git -C "$REPO" commit -q -m "sp-cherry: add content"
git -C "$REPO" checkout -q main
printf 'v1\n' > "$REPO/shared-cherry.txt"
git -C "$REPO" add shared-cherry.txt && git -C "$REPO" commit -q -m "spira: land sp-cherry"
printf 'v2\n' > "$REPO/shared-cherry.txt"
git -C "$REPO" add shared-cherry.txt && git -C "$REPO" commit -q -m "unrelated: advance further"
git -C "$REPO" checkout -q spira/sp-cherry
printf 'a new, never-landed change\n' > "$REPO/sp-cherry-extra.txt"
git -C "$REPO" add sp-cherry-extra.txt && git -C "$REPO" commit -q -m "sp-cherry: one more commit, after landing"
git -C "$REPO" checkout -q main
bead sp-cherry closed

# sp-unlanded: real, unique content; no landing record. KEEP unlanded.
git -C "$REPO" checkout -q -b spira/sp-unlanded main
printf 'genuinely unlanded work\n' > "$REPO/sp-unlanded.txt"
git -C "$REPO" add sp-unlanded.txt && git -C "$REPO" commit -q -m "sp-unlanded: real work"
git -C "$REPO" checkout -q main
bead sp-unlanded open

# sp-noone: real, unique content; no bd record at all. UNADOPTED.
git -C "$REPO" checkout -q -b spira/sp-noone main
printf 'orphaned content\n' > "$REPO/sp-noone.txt"
git -C "$REPO" add sp-noone.txt && git -C "$REPO" commit -q -m "sp-noone: no bead names this"
git -C "$REPO" checkout -q main

# sp-held: a live claim via the status seam. HELD, at the top of the loop, before
# send_disposition is ever called.
git -C "$REPO" checkout -q -b spira/sp-held main
printf 'work in flight\n' > "$REPO/sp-held.txt"
git -C "$REPO" add sp-held.txt && git -C "$REPO" commit -q -m "sp-held: an aeon is still here"
git -C "$REPO" checkout -q main
bead sp-held in_progress

printf 'sp-held\tin_progress\n' > "$STATUS_FILE"

# sp-orphan: a worktree registered and present, whose branch ref is removed by hand — the
# shape PASS 2 exists for, however it arises in production. `update-ref -d` is plumbing:
# unlike `git branch -D`, it does not refuse a ref a worktree has checked out.
git -C "$REPO" worktree add -q -b spira/sp-orphan "$RUN/worktree/sp-orphan" main >/dev/null 2>&1
git -C "$REPO" update-ref -d refs/heads/spira/sp-orphan

# THE LEGACY TREE. A detached worktree at the unsuffixed .landing path — retired
# unconditionally on every pass now that the per-repository form exists.
git -C "$REPO" worktree add -q --detach "$RUN/worktree/.landing" main >/dev/null 2>&1

# One live remote branch, to prove the Sending deletes it too (UC-19) — sp-cl1 is going to
# be SENT, so give it a remote counterpart before the pass.
git -C "$REPO" push -q origin spira/sp-cl1
# Give it a branch: label too, to prove the label is dropped once the ref is verifiably
# gone (law-branch-affinity-is-recorded).
bead sp-cl1 closed '[]' '["branch:spira/sp-cl1"]'
printf 'LANDED %s %s\n' "$(git -C "$REPO" rev-parse spira/sp-cl1)" "$(date +%s)" \
    > "$RUN/landstate/sp-cl1"
# The aeon session log must be KEPT after the send (CHECK 5 depends on its existence).
touch "$RUN/sp-cl1.log"

echo "test-sending.sh"

# ---- fixture confirmation -------------------------------------------------------------
# shellcheck disable=SC1090
. "$HERE/lib.sh"
echo
echo "fixture confirmation:"
if content_landed "$REPO" spira/sp-cl0 origin/main; then
    ok "sp-cl0: content_landed true via the ancestor shortcut (ahead=0)"
else
    bad "sp-cl0: content_landed true via the ancestor shortcut" "returned non-zero — fixture is wrong"
fi
if git -C "$REPO" merge-base --is-ancestor spira/sp-cl1 origin/main 2>/dev/null; then
    bad "sp-cl1: ancestry alone would NOT catch this (fixture is wrong)" "branch IS an ancestor"
else
    ok "sp-cl1: ancestry alone refuses it (the defect content_landed exists to fix)"
fi
if content_landed "$REPO" spira/sp-cl1 origin/main; then
    ok "sp-cl1: content_landed approves the empty-commit branch"
else
    bad "sp-cl1: content_landed approves the empty-commit branch" "returned non-zero"
fi
if content_landed "$REPO" spira/sp-sq origin/main; then
    bad "sp-sq: content_landed must be false (base diverged past the squash)" "returned 0"
else
    ok "sp-sq: content_landed correctly false"
fi
if landed sp-otherpr "$REPO" 2>/dev/null; then
    ok "sp-otherpr: landed() finds the naming commit"
else
    bad "sp-otherpr: landed() finds the naming commit" "returned non-zero"
fi
if landed sp-cherry "$REPO" 2>/dev/null; then
    ok "sp-cherry: landed() finds the (stale) naming commit"
else
    bad "sp-cherry: landed() finds the (stale) naming commit" "returned non-zero"
fi

# ---- the pass ---------------------------------------------------------------------------
echo
echo "one sending pass, every disposition:"
out="$(sending)"
rc=$?
printf '%s\n' "$out" >&2

# SEND / content-landed
want   "sp-cl0 is SENT"                     "SENT sp-cl0"          "$out"
is     "sp-cl0 branch is gone"              1 "$(branch_exists spira/sp-cl0; echo $?)"
is     "sp-cl0 is NOT labelled content-landed (ahead=0)" no "$(has_label sp-cl0 content-landed && echo yes || echo no)"

want   "sp-cl1 is SENT"                     "SENT sp-cl1"          "$out"
is     "sp-cl1 branch is gone"              1 "$(branch_exists spira/sp-cl1; echo $?)"
is     "sp-cl1 IS labelled content-landed (ahead=1)" yes "$(has_label sp-cl1 content-landed && echo yes || echo no)"
nowant "no ASSERT line for sp-cl1 (landstate present)" "ASSERT sp-cl1" "$out"

want   "sp-clnoassert is SENT"              "SENT sp-clnoassert"   "$out"
want   "ASSERT fires for sp-clnoassert (no landstate)" "ASSERT sp-clnoassert" "$out"
is     "sp-clnoassert branch is gone despite the ASSERT" 1 "$(branch_exists spira/sp-clnoassert; echo $?)"

# REAP / superseded
want   "sp-supsafe is REAPED"               "REAPED sp-supsafe"    "$out"
is     "sp-supsafe branch is gone"          1 "$(branch_exists spira/sp-supsafe; echo $?)"

want   "sp-supunsafe is KEPT"               "KEEP   sp-supunsafe"  "$out"
nowant "sp-supunsafe is not reaped"         "REAPED sp-supunsafe"  "$out"
is     "sp-supunsafe branch still exists"   0 "$(branch_exists spira/sp-supunsafe; echo $?)"
if git -C "$REPO" worktree list --porcelain 2>/dev/null | grep -q "worktree $RUN/worktree/sp-supunsafe$"; then
    bad "sp-supunsafe's worktree is freed even though the branch is kept" "still registered"
else
    ok "sp-supunsafe's worktree is freed even though the branch is kept"
fi

# REAP / squash-merged
want   "sp-sq is REAPED"                    "REAPED sp-sq"         "$out"
is     "sp-sq branch is gone"               1 "$(branch_exists spira/sp-sq; echo $?)"

# SEND / other-pr, KEEP / cherry-unapplied
want   "sp-otherpr is SENT"                 "SENT sp-otherpr"      "$out"
is     "sp-otherpr branch is gone"          1 "$(branch_exists spira/sp-otherpr; echo $?)"

want   "sp-cherry is KEPT"                  "KEEP   sp-cherry"     "$out"
nowant "sp-cherry is not sent"              "SENT sp-cherry"       "$out"
is     "sp-cherry branch still exists"      0 "$(branch_exists spira/sp-cherry; echo $?)"

# KEEP / unlanded, UNADOPTED
want   "sp-unlanded is KEPT"                "KEEP   sp-unlanded"   "$out"
is     "sp-unlanded branch still exists"    0 "$(branch_exists spira/sp-unlanded; echo $?)"

want   "sp-noone is UNADOPTED"              "UNADOPTED sp-noone"   "$out"
is     "sp-noone branch still exists"       0 "$(branch_exists spira/sp-noone; echo $?)"

# HELD
want   "sp-held is HELD"                    "HELD   sp-held"       "$out"
is     "sp-held branch still exists"        0 "$(branch_exists spira/sp-held; echo $?)"

# Pass 2 — orphaned worktree
want   "sp-orphan's worktree is SENT (orphaned, branch already gone)" "SENT sp-orphan" "$out"
if git -C "$REPO" worktree list --porcelain 2>/dev/null | grep -q "worktree $RUN/worktree/sp-orphan$"; then
    bad "sp-orphan's worktree is removed" "still registered"
else
    ok "sp-orphan's worktree is removed"
fi

# The legacy tree
want "the legacy .landing tree is RETIRED" "RETIRED .landing" "$out"
[ -e "$RUN/worktree/.landing" ] \
    && bad "the legacy .landing tree is gone" "still present" \
    || ok "the legacy .landing tree is gone"

# The unresolvable-ref repo
want "the unresolvable-ref repo is SKIPPED, naming the cause" "cannot resolve the ref it lands on" "$out"

# UC-19: remote branch delete, branch: label removal, aeon log kept.
if git -C "$REPO" ls-remote --exit-code "$REMOTE" "refs/heads/spira/sp-cl1" >/dev/null 2>&1; then
    bad "the remote copy of spira/sp-cl1 is deleted too" "still on the remote"
else
    ok "the remote copy of spira/sp-cl1 is deleted too"
fi
is "the branch: label is removed once the ref is verifiably gone" no \
    "$(has_label sp-cl1 branch:spira/sp-cl1 && echo yes || echo no)"
[ -f "$RUN/sp-cl1.log" ] \
    && ok "the aeon session log is kept" \
    || bad "the aeon session log is kept" "was deleted"

is "the pass exits 0 when nothing failed" 0 "$rc"

# ---- DRY RUN — a second, isolated fixture: nothing above should have run twice ----------
echo
echo "dry-run changes nothing:"
DREPO="$TMP/dry-repo"; DREMOTE="$TMP/dry-remote.git"; DRUN="$TMP/dry-run"
git init -q --bare -b main "$DREMOTE"
git init -q -b main "$DREPO"
git -C "$DREPO" commit -q --allow-empty -m base
git -C "$DREPO" remote add origin "$DREMOTE"
git -C "$DREPO" push -q origin main
git -C "$DREPO" remote set-head origin main
mkdir -p "$DRUN/worktree" "$DRUN/landstate"
git -C "$DREPO" checkout -q -b spira/sp-dry main
git -C "$DREPO" commit -q --allow-empty -m "sp-dry: review only"
git -C "$DREPO" checkout -q main
bead sp-dry closed
DHOME="$(basename "$DREPO")"
printf '%s | %s | push | main | |\n' "$DHOME" "$DREPO" > "$TMP/dry-repo-map"

dry_out="$(SPIRA_HOME="$HERE" SPIRA_RUN="$DRUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$STUB_BD" \
    SPIRA_REPO="$DREPO" SPIRA_HOME_REPO="$DHOME" \
    SPIRA_REPO_MAP="$TMP/dry-repo-map" \
        bash "$HERE/sending.sh" --dry-run --no-fetch 2>&1)"
want "dry-run reports WOULD, not SENT"  "WOULD  sp-dry  send branch" "$dry_out"
nowant "dry-run never reports SENT"     "SENT sp-dry"                "$dry_out"
is "dry-run leaves the branch in place" 0 "$(git -C "$DREPO" show-ref --verify -q refs/heads/spira/sp-dry; echo $?)"
is "dry-run does not write the content-landed label" no \
    "$(has_label sp-dry content-landed && echo yes || echo no)"

# ---- mid-send HELD — send_branch's own recheck, not the top-of-loop witness ------------
#
# Calling send_branch directly (rather than through sweep_repo) exercises ONLY its
# internal recheck: a branch that was clear at the top of the loop but is claimed by the
# time the Sending is about to act on it must still be refused (UC-16). The source guard
# added to sending.sh for this suite is what makes this callable at all.
echo
echo "mid-send HELD — send_branch's own recheck:"
MREPO="$TMP/mid-repo"; MREMOTE="$TMP/mid-remote.git"; MRUN="$TMP/mid-run"
git init -q --bare -b main "$MREMOTE"
git init -q -b main "$MREPO"
git -C "$MREPO" commit -q --allow-empty -m base
git -C "$MREPO" remote add origin "$MREMOTE"
git -C "$MREPO" push -q origin main
git -C "$MREPO" remote set-head origin main
mkdir -p "$MRUN/worktree"
git -C "$MREPO" branch spira/sp-mid main

(
    export SPIRA_RUN="$MRUN" SPIRA_REAPLOG="$MRUN/reap.log" SPIRA_HOME="$HERE"
    # shellcheck disable=SC1090
    . "$HERE/sending.sh"
    spira_status_seam - <<'SEAM'
sp-mid	in_progress
SEAM
    REPO="$MREPO"; LANDREF="origin/main"; REPONAME="mid-repo"
    out="$(send_branch sp-mid spira/sp-mid 2>&1)"
    printf '%s\n' "$out"
)  > "$TMP/mid-out" 2>&1
mid_out="$(cat "$TMP/mid-out")"
want "send_branch refuses a bead claimed since the top-of-loop check" "HELD   sp-mid" "$mid_out"
want "the refusal names it a mid-send recheck" "(mid-send)" "$mid_out"
is "the branch survives the mid-send HELD" 0 \
    "$(git -C "$MREPO" show-ref --verify -q refs/heads/spira/sp-mid; echo $?)"

# ---- FAILED and a non-zero exit status --------------------------------------------------
#
# A worktree whose git state cannot be read (its .git file corrupted) makes salvage fail,
# which makes spira_destroy_worktree fail, which is send_branch's FAILED path (UC-21).
echo
echo "FAILED — a worktree that cannot be salvaged, and the exit status that follows:"
FREPO="$TMP/fail-repo"; FREMOTE="$TMP/fail-remote.git"; FRUN="$TMP/fail-run"
git init -q --bare -b main "$FREMOTE"
git init -q -b main "$FREPO"
git -C "$FREPO" commit -q --allow-empty -m base
git -C "$FREPO" remote add origin "$FREMOTE"
git -C "$FREPO" push -q origin main
git -C "$FREPO" remote set-head origin main
mkdir -p "$FRUN/worktree"
git -C "$FREPO" checkout -q -b spira/sp-fail main
git -C "$FREPO" commit -q --allow-empty -m "sp-fail: review only, lands cleanly"
git -C "$FREPO" checkout -q main
git -C "$FREPO" worktree add -q "$FRUN/worktree/sp-fail" spira/sp-fail >/dev/null 2>&1
# Corrupt the worktree's link to the repository — `git -C <w> status` now fails, so
# salvage refuses rather than guessing the tree is clean.
echo 'gitdir: /nonexistent' > "$FRUN/worktree/sp-fail/.git"
bead sp-fail closed
FHOME="$(basename "$FREPO")"
printf '%s | %s | push | main | |\n' "$FHOME" "$FREPO" > "$TMP/fail-repo-map"

fail_out="$(SPIRA_HOME="$HERE" SPIRA_RUN="$FRUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$STUB_BD" \
    SPIRA_REPO="$FREPO" SPIRA_HOME_REPO="$FHOME" \
    SPIRA_REPO_MAP="$TMP/fail-repo-map" \
        bash "$HERE/sending.sh" --no-fetch 2>&1)"
fail_rc=$?
want "sp-fail is reported FAILED" "FAILED sp-fail" "$fail_out"
wantrc "the pass exits non-zero when failed > 0" 1 "$fail_rc"
want "the reap log records why" "salvage failed" "$(cat "$FRUN/reap.log" 2>/dev/null)"

tl_summary
