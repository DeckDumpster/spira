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
# ZERO AHEAD. spira-lc content-landed returns true for ANY branch with zero commits ahead of the
# base ("zero ahead" and "ancestor" are the same fact, sp-bf31a), so such a branch is SENT
# through the content-landed arm, with no event; there is no separate fast-forward arm.
#
# THE BINARY (sp-arpjt). sending.sh is the `sending` binary now (sending/DESIGN.md), run
# here by name from the tree's own build, end to end against the real lib.sh chokepoints.
# Its dispositions, the mid-send recheck and the OFF-mode content-landed label are its own
# unit tests; this suite is what they cannot see — the real deletions, the real machine.
#
# THE CONTENT-LANDED CASE AGAINST A REAL spira-lc (sp-i2m7y). The retired content-landed
# bd label is gone; sp-cl1 (ahead=1, merge-tree equal to the base) now proves itself by a
# real ContentOnBase event against a throwaway spira-lc/Dolt server, the same shape
# test-lc-hold.sh uses. sp-cl0 (ahead=0) is the negative control: its row must stay exactly
# where it was seeded, since the ahead-count guard means the call is never made for it.
#
# defect: sp-mqsl, sp-e5ow0, sp-796o, sp-kq8l, sp-bjzj, sp-3gih, sp-hl92, sp-1smg
# tier: T2
# covers: sending/src/* spira/lib.sh UC-landed-audit-reaping-14 UC-landed-audit-reaping-16 UC-landed-audit-reaping-17 UC-landed-audit-reaping-18 UC-landed-audit-reaping-19 UC-landed-audit-reaping-20 UC-landed-audit-reaping-21 UC-landed-audit-reaping-22
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-lc-hold.sh
#   (sp-ki12s) — testenv-batch.sh already provides the container.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

TMP="$(mktemp -d)"
LC_SERVER_PID=""
trap '[ -n "$LC_SERVER_PID" ] && kill "$LC_SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export SPIRA_CONF="$TMP/no-such-conf"

# --------------------------------------------------------------------------------------
# A THROWAWAY spira-lc/Dolt SERVER, the same shape test-lc-hold.sh and
# test-check2-reaper.sh use. Exported so the `sending.sh` subprocess the
# `sending()` helper below spawns inherits it too.
# --------------------------------------------------------------------------------------
LCREPO="$(cd "$HERE/.." && pwd)"
# A literal base port, not SPIRA_LC_TESTDB_PORT's conf.sh default: conf.sh is not sourced
# yet at this point in the suite (its own SPIRA_CONF points at a no-such-conf on purpose,
# further down), so nothing has given that variable a value here.
LC_PORT=$((23309 + (RANDOM % 400)))
LC_TMP="$TMP/lc"; mkdir -p "$LC_TMP/data"
cat > "$LC_TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $LC_PORT
  max_connections: 50
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$LC_TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
"$DOLT_BIN" sql-server --config "$LC_TMP/server.yaml" > "$LC_TMP/server.log" 2>&1 &
LC_SERVER_PID=$!
lc_up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$LC_TMP" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        lc_up=1; break
    fi
    sleep 0.2
done
[ "$lc_up" = 1 ] || bail "dolt sql-server for spira_lifecycle never came up: $(cat "$LC_TMP/server.log")"
lc_root_sql() { "$DOLT_BIN" --data-dir "$LC_TMP" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls "$@"; }

command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$LC_PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$LC_TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
unset SPIRA_LC_SOCKET
spira-lc admin-apply-ddl "$LCREPO/lifecycle/schema.sql" >"$LC_TMP/schema.log" 2>&1
wantrc "spira_lifecycle schema applies cleanly" 0 $?

# Every fixture bead carries its lifecycle row (sp-mve9i): sending's claim witness reads the
# row, never bd status — bd open → READY, in_progress → WORKING, closed (the builder's submit)
# → SUBMITTED; a bead with no row is one nothing proves unclaimed, so it would be HELD.
seed_lc() {   # seed_lc <bead-id> <state>
    lc_root_sql --use-db spira_lifecycle sql -q "DELETE FROM bead WHERE bead_id = '$1'" >/dev/null 2>&1
    lc_root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('$1','$2','[]',0,0)" >/dev/null 2>&1
}
lc_row_state() { lc_root_sql --use-db spira_lifecycle sql -q "SELECT state FROM bead WHERE bead_id='$1'" -r csv 2>/dev/null | tail -1; }

# --------------------------------------------------------------------------------------
# THE STUB BD. sending.sh's remaining bd seams are `bdjson show <id>` (read) and
# `bdq label add/remove <id> <label>` (write, for branch: affinity only now — content-landed
# is a real spira-lc event, above). One JSON object per id, under $STUB_BEADS_DIR/<id>.json;
# label add/remove mutate it in place, so the branch: label assertions read back what
# sending.sh actually wrote — a real seam contract, not a canned answer
# (law-a-control-that-cannot-check-must-refuse's positive twin: a stub that cannot be
# written to cannot prove a write happened).
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
        show|label|list)  cmd="$a" ;;
        *)           argv+=("$a") ;;
    esac
done
case "$cmd" in
    list)   # the positive control (spira_db_reachable): the store lists at least one bead
        printf '[{"id":"sp-any"}]\n'
        ;;
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
# The base column stays EMPTY here on purpose: a declared `main` would resolve LANDREF to
# the bare local ref "main", which ref_remote cannot split a remote out of (no "/"), so the
# remote-branch-delete step (UC-19) would silently never fire. Leaving it unset falls
# through to rung 2 (origin/HEAD, set below), giving "origin/main".
printf '%s | %s | push | | |\nother | %s | push | | |\n' "$HOME_REPO" "$REPO" "$OTHER" \
    > "$TMP/repo-map"

STATUS_FILE="$TMP/status-from"

sending() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$STUB_BD" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" SPIRA_GH="$STUB_GH" \
    SPIRA_REPO_MAP="$TMP/repo-map" \
        command sending --no-fetch --status-from "$STATUS_FILE" "$@" 2>&1
}
branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/$1" 2>/dev/null; }

# ---- fixture branches ----------------------------------------------------------------

# sp-cl0: content-landed via the ancestor shortcut, ahead=0 — the branch IS the base tip.
# No commits of its own, so no ContentOnBase event either (UC-17) — seeded READY here (bd open,
# in lifecycle terms) as the negative control: the ahead-count guard means the call is never
# made for it.
git -C "$REPO" branch spira/sp-cl0 main
bead sp-cl0 open
seed_lc sp-cl0 READY

# sp-cl1: content-landed via merge-tree equality, ahead=1 — an empty commit that names the
# bead but changes no files (sp-kq8l). Ancestry alone would refuse this (the branch is not
# reachable from origin/main); spira-lc content-landed approves it because merging changes nothing.
git -C "$REPO" checkout -q -b spira/sp-cl1 main
git -C "$REPO" commit -q --allow-empty -m "sp-cl1: review only, no file changes"
git -C "$REPO" checkout -q main
bead sp-cl1 closed
printf 'LANDED %s %s\n' "$(git -C "$REPO" rev-parse spira/sp-cl1)" "$(date +%s)" \
    > "$RUN/landstate/sp-cl1"
seed_lc sp-cl1 SUBMITTED

# sp-clnoassert: same shape as sp-cl1, but with NO landstate record at all — CHECK 5 (the
# sentinel), not this program, owns the closed-not-landed invariant now (sp-jci6o), so the
# send must proceed regardless of whether landing.sh left a record.
git -C "$REPO" checkout -q -b spira/sp-clnoassert main
git -C "$REPO" commit -q --allow-empty -m "sp-clnoassert: review only"
git -C "$REPO" checkout -q main
bead sp-clnoassert closed
seed_lc sp-clnoassert SUBMITTED

# spira/round-54: a Concierge round-merge branch, an ancestor of main and with no bead of
# its own — the same content-landed shape as sp-clnoassert, and with no landstate record
# either, but it must never reach spira-lc content-landed or the ASSERT at all: it is not a bead
# branch (sp-dxntp).
git -C "$REPO" branch spira/round-54 main

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
seed_lc sp-supsafe SUBMITTED

# sp-supunsafe: superseded, but its one commit adds content the base does not have and
# merge-tree succeeds cleanly (no conflict) — KEEP superseded-unsafe, and its worktree
# (attached below) must be freed even though the branch survives.
git -C "$REPO" checkout -q -b spira/sp-supunsafe main
printf 'unique content the base never got\n' > "$REPO/sp-supunsafe.txt"
git -C "$REPO" add sp-supunsafe.txt && git -C "$REPO" commit -q -m "sp-supunsafe: unique work"
git -C "$REPO" checkout -q main
bead sp-supunsafe closed '[{"issue_id":"sp-supunsafe","depends_on_id":"sp-succ","type":"supersedes"}]'
seed_lc sp-supunsafe SUBMITTED
git -C "$REPO" worktree add -q "$RUN/worktree/sp-supunsafe" spira/sp-supunsafe >/dev/null 2>&1

# sp-sq: squash-merged. The PR's headRefOid equals the branch's current tip; the base moved
# on past the squash point, so spira-lc content-landed is false. REAP squash-merged.
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
seed_lc sp-sq SUBMITTED

# The `gh` stub. sending.sh's ghq wrapper (lib.sh) calls ${SPIRA_GH:-gh}; keyed by branch
# name, like test-sending-squash-merged.sh's own stub, so only sp-sq resolves to a merged
# PR at its own tip.
STUB_GH="$TMP/gh-stub"
cat > "$STUB_GH" <<GHEOF
#!/usr/bin/env bash
case "\$3" in
  spira/sp-sq) printf '%s\n' "$SQ_TIP"; exit 0 ;;
  *)           exit 1 ;;
esac
GHEOF
chmod +x "$STUB_GH"

# sp-otherpr: its lifecycle row is LANDED (a batch commit also names it on the base), but the base has
# since diverged so spira-lc content-landed is false; every commit on the branch is already
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
seed_lc sp-otherpr LANDED   # the lifecycle record has it LANDED (sp-2c1n0): that, not the subject, is landed-ness

# sp-cherry: its lifecycle row is LANDED from an OLD landing, but a commit was added to the
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
seed_lc sp-cherry LANDED

# sp-unlanded: real, unique content; no landing record. KEEP unlanded.
git -C "$REPO" checkout -q -b spira/sp-unlanded main
printf 'genuinely unlanded work\n' > "$REPO/sp-unlanded.txt"
git -C "$REPO" add sp-unlanded.txt && git -C "$REPO" commit -q -m "sp-unlanded: real work"
git -C "$REPO" checkout -q main
bead sp-unlanded open
seed_lc sp-unlanded READY

# sp-noone: real, unique content not on the base; no bd record at all. ORPHAN no-bead —
# archived at refs/archive/spira/sp-noone, never plain-deleted (sp-hwhnw).
git -C "$REPO" checkout -q -b spira/sp-noone main
printf 'orphaned content\n' > "$REPO/sp-noone.txt"
git -C "$REPO" add sp-noone.txt && git -C "$REPO" commit -q -m "sp-noone: no bead names this"
git -C "$REPO" checkout -q main
SP_NOONE_TIP="$(git -C "$REPO" rev-parse spira/sp-noone)"

# sp-stray: no bead, and the tip is already an ancestor of main — nothing on it the base
# does not already have. spira-lc content-landed's ancestor shortcut catches this before the
# bead lookup ever runs (bead or not), so it is SENT via the content-landed arm, not the
# ORPHAN one below — the "unadopted, ancestor of base -> delete" half of sp-hwhnw's split
# that was already correct.
git -C "$REPO" branch -q spira/sp-stray main

# sp-held: a live claim via the status seam. HELD, at the top of the loop, before
# send_disposition is ever called.
git -C "$REPO" checkout -q -b spira/sp-held main
printf 'work in flight\n' > "$REPO/sp-held.txt"
git -C "$REPO" add sp-held.txt && git -C "$REPO" commit -q -m "sp-held: an aeon is still here"
git -C "$REPO" checkout -q main
bead sp-held in_progress
seed_lc sp-held WORKING

printf 'sp-held\tin_progress\n' > "$STATUS_FILE"

# sp-orphan: a worktree registered and present, whose branch ref is removed by hand — the
# shape PASS 2 exists for, however it arises in production. `update-ref -d` is plumbing:
# unlike `git branch -D`, it does not refuse a ref a worktree has checked out.
git -C "$REPO" worktree add -q -b spira/sp-orphan "$RUN/worktree/sp-orphan" main >/dev/null 2>&1
git -C "$REPO" update-ref -d refs/heads/spira/sp-orphan

# A HARNESS TREE (leading dot, per-repository): never judged by PASS 2. (The retirement of
# the unsuffixed legacy .landing/.rebase trees went with sending.sh, sp-arpjt: none exists.)
git -C "$REPO" worktree add -q --detach "$RUN/worktree/.landing.$(basename "$REPO")" main >/dev/null 2>&1

# One live remote branch, to prove the Sending deletes it too (UC-19) — sp-cl1 is going to
# be SENT, so give it a remote counterpart before the pass. --no-fetch means sending.sh
# will not fetch on its own, so the local tracking ref send_branch reads must already
# exist here.
git -C "$REPO" push -q origin spira/sp-cl1
# A bare `fetch origin` (not `fetch origin spira/sp-cl1`), so the default refspec updates
# refs/remotes/origin/spira/sp-cl1 — a single named branch on the command line only
# populates FETCH_HEAD, never the tracking ref send_branch's remote-delete check reads.
git -C "$REPO" fetch -q origin
# Give it a branch: label too, to prove the label is dropped once the ref is verifiably
# gone (law-branch-affinity-is-recorded).
bead sp-cl1 closed '[]' '["branch:spira/sp-cl1"]'
printf 'LANDED %s %s\n' "$(git -C "$REPO" rev-parse spira/sp-cl1)" "$(date +%s)" \
    > "$RUN/landstate/sp-cl1"
# The aeon session log must be KEPT after the send (CHECK 5 depends on its existence).
touch "$RUN/sp-cl1.log"

# SYNC origin/main to local main's tip. LANDREF resolves to origin/main (rung 2, since the
# repo-map's base column is deliberately unset above), but every base-side commit the
# fixture branches above were built against — sp-supsafe's conflict, sp-sq's squash and
# advance, sp-otherpr's and sp-cherry's "spira: land ..." commits — was made on the LOCAL
# main only. Without this, spira-lc content-landed and landed() would judge every branch against a
# base frozen at the very first commit.
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

echo "test-sending.sh"

# ---- fixture confirmation -------------------------------------------------------------
# shellcheck disable=SC1090
. "$HERE/lib.sh"
echo
echo "fixture confirmation:"
if spira-lc content-landed "$REPO" spira/sp-cl0 origin/main; then
    ok "sp-cl0: spira-lc content-landed true via the ancestor shortcut (ahead=0)"
else
    bad "sp-cl0: spira-lc content-landed true via the ancestor shortcut" "returned non-zero — fixture is wrong"
fi
if git -C "$REPO" merge-base --is-ancestor spira/sp-cl1 origin/main 2>/dev/null; then
    bad "sp-cl1: ancestry alone would NOT catch this (fixture is wrong)" "branch IS an ancestor"
else
    ok "sp-cl1: ancestry alone refuses it (the defect spira-lc content-landed exists to fix)"
fi
if spira-lc content-landed "$REPO" spira/sp-cl1 origin/main; then
    ok "sp-cl1: spira-lc content-landed approves the empty-commit branch"
else
    bad "sp-cl1: spira-lc content-landed approves the empty-commit branch" "returned non-zero"
fi
if spira-lc content-landed "$REPO" spira/sp-sq origin/main; then
    bad "sp-sq: spira-lc content-landed must be false (base diverged past the squash)" "returned 0"
else
    ok "sp-sq: spira-lc content-landed correctly false"
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
is     "sp-cl0's spira-lc row is untouched (ahead=0, no ContentOnBase call made)" "READY" \
    "$(lc_row_state sp-cl0)"

want   "sp-cl1 is SENT"                     "SENT sp-cl1"          "$out"
is     "sp-cl1 branch is gone"              1 "$(branch_exists spira/sp-cl1; echo $?)"
is     "sp-cl1's spira-lc row moves to LANDED via a real ContentOnBase event (ahead=1)" "LANDED" \
    "$(lc_row_state sp-cl1)"

want   "sp-clnoassert is SENT"              "SENT sp-clnoassert"   "$out"
is     "sp-clnoassert branch is gone despite no landstate record" 1 "$(branch_exists spira/sp-clnoassert; echo $?)"

want   "round-54 is SKIPped before disposition ever runs" "SKIP   round-54" "$out"
nowant "no ASSERT line for a round branch (it never has a landstate)" "ASSERT round-54" "$out"
nowant "a round branch is never SENT"       "SENT round-54"        "$out"
is     "round-54 branch survives — left for the Concierge's own toolchain" 0 "$(branch_exists spira/round-54; echo $?)"

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

# ORPHAN WORK — archived, never plain-deleted (sp-hwhnw)
want   "sp-noone is ARCHIVED"               "ARCHIVED sp-noone"    "$out"
is     "sp-noone branch is gone"            1 "$(branch_exists spira/sp-noone; echo $?)"
is     "sp-noone's tip is reachable at refs/archive/spira/sp-noone" "$SP_NOONE_TIP" \
    "$(git -C "$REPO" rev-parse -q --verify refs/archive/spira/sp-noone 2>/dev/null)"
# Positive control: the orphaned commit itself is still resolvable after the pass, not
# just the ref that names it.
is     "sp-noone's commit content is still resolvable" "orphaned content" \
    "$(git -C "$REPO" show "$SP_NOONE_TIP:sp-noone.txt" 2>/dev/null)"

# UNADOPTED (bead-less, ancestor of the base) — sent via content-landed, no archive
want   "sp-stray is SENT"                   "SENT sp-stray"        "$out"
is     "sp-stray branch is gone"            1 "$(branch_exists spira/sp-stray; echo $?)"
is     "sp-stray leaves no refs/archive entry (nothing to preserve)" "" \
    "$(git -C "$REPO" rev-parse -q --verify refs/archive/spira/sp-stray 2>/dev/null)"

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

# The harness's own dot-tree is left alone
nowant "the per-repository .landing tree is never judged" ".landing" "$out"
[ -e "$RUN/worktree/.landing.$(basename "$REPO")" ] \
    && ok "the per-repository .landing tree still stands" \
    || bad "the per-repository .landing tree still stands" "it was removed"

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
seed_lc sp-dry SUBMITTED
DHOME="$(basename "$DREPO")"
printf '%s | %s | push | main | |\n' "$DHOME" "$DREPO" > "$TMP/dry-repo-map"

dry_out="$(SPIRA_HOME="$HERE" SPIRA_RUN="$DRUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$STUB_BD" \
    SPIRA_REPO="$DREPO" SPIRA_HOME_REPO="$DHOME" SPIRA_REAPLOG="$DRUN/reap.log" \
    SPIRA_REPO_MAP="$TMP/dry-repo-map" \
        command sending --dry-run --no-fetch 2>&1)"
want "dry-run reports WOULD, not SENT"  "WOULD  sp-dry  send branch" "$dry_out"
nowant "dry-run never reports SENT"     "SENT sp-dry"                "$dry_out"
is "dry-run leaves the branch in place" 0 "$(git -C "$DREPO" show-ref --verify -q refs/heads/spira/sp-dry; echo $?)"
is "dry-run does not fire a ContentOnBase event" "SUBMITTED" "$(lc_row_state sp-dry)"

# ---- mid-send HELD — the recheck immediately before a deletion is sending's own unit test
# now (sending/src/tests.rs `mid_send_hold_queue_and_failure`): with sending.sh gone there is
# no sourced send_branch for a suite to call between the loop's check and the deletion.

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
seed_lc sp-fail SUBMITTED
FHOME="$(basename "$FREPO")"
printf '%s | %s | push | main | |\n' "$FHOME" "$FREPO" > "$TMP/fail-repo-map"

fail_out="$(SPIRA_HOME="$HERE" SPIRA_RUN="$FRUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$STUB_BD" \
    SPIRA_REPO="$FREPO" SPIRA_HOME_REPO="$FHOME" SPIRA_REAPLOG="$FRUN/reap.log" \
    SPIRA_REPO_MAP="$TMP/fail-repo-map" \
        command sending --no-fetch 2>&1)"
fail_rc=$?
want "sp-fail is reported FAILED" "FAILED sp-fail" "$fail_out"
wantrc "the pass exits non-zero when failed > 0" 1 "$fail_rc"
want "the reap log records why" "salvage failed" "$(cat "$FRUN/reap.log" 2>/dev/null)"

tl_summary
