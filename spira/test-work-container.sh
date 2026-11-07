#!/usr/bin/env bash
#
# test-work-container.sh — the dwarf semantic layer end to end: a real spira_lifecycle
# (its own throwaway dolt sql-server), a real bd (testdb.sh), and real mail, driven
# through the built `work` client over `spira-lc serve`'s socket exactly as a dwarf's
# restricted environment would reach it.
#
# WHAT THIS PROVES (acceptance criteria): each verb emits exactly the documented event or
# filing, and a verb naming any bead other than the bound one is refused even when a real
# socket answers (test-work-crate.sh already proves the refusal never needs one).
#
# tier: T2
# covers: work/* spira-lc/src/work.rs spira-lc/src/main.rs spira-lc/src/bd.rs spira/bead.sh mail/src/*
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

# round 4 fix (pattern 6): SPIRA_RUNES_DIR no longer derives from SPIRA_HOME — without it,
# bead.sh file --for builder cannot find runes/builder.rune ("no such persona: builder").
tl_config SPIRA_RUNES_DIR="$HERE/runes"

. "$HERE/conf.sh"

. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require "test-work-container.sh"

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
printf '## Build\n\nA fixture change in spira/x.\n' > "$TMP/build.md"
SERVE_PID=""

cleanup() {
    [ -n "$SERVE_PID" ] && kill "$SERVE_PID" >/dev/null 2>&1
    lcfix_down
    testdb_drop >/dev/null 2>&1
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

testdb_up "test-work-container"

# A real mailbox root and kind set, never the operator's real maildir.
export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_KINDS="$TMP/kinds"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
# round 5 fix (pattern 7): SPIRA_RUN was never declared — mail's repeat-guard state file
# resolved to the complete fixture's placeholder /fixture/userhome/spira/run, which this
# process cannot create ("blocked"/"superseded-by" are the first cases to need it).
# SPIRA_MAIL_MUTE: the complete fixture declares it true (pattern 2); muted delivery lands
# in cur/<id>:2,S pre-marked read (mail/src/maildir.rs mail_deliver), not new/ — this suite's
# "an ask landed in the operator's mailbox" checks literally count new/, so it needs its own
# unmuted declaration or every ask silently "delivers" into cur and the count never moves.
tl_config SPIRA_MAIL="$SPIRA_MAIL" SPIRA_MAIL_KINDS="$SPIRA_MAIL_KINDS" SPIRA_RUN="$SPIRA_RUN" \
    SPIRA_MAIL_MUTE=0
mkdir -p "$SPIRA_MAIL_KINDS"
cp -r "$HERE/mail/kinds/." "$SPIRA_MAIL_KINDS/"

# A repo-map fixture, never the host's real one: repo-map.example (the shipped fallback)
# names no repo this suite could use, and the real map is host-specific inventory.
# Empty lanes column admits every lane, so builder's plan partition is always granted.
cat > "$TMP/repo-map" <<MAP
testrepo | $TMP | push | origin/main | | |
landrepo | $TMP/landrepo | push | main | | |
MAP
export SPIRA_REPO_MAP="$TMP/repo-map"
tl_config SPIRA_REPO_MAP="$SPIRA_REPO_MAP"

# Same pin as test-lifecycle-container.sh: this suite runs inside testenv-batch.sh's own
# podman exec, which sets its own CARGO_TARGET_DIR.
LC_BIN="$(command -v spira-lc 2>/dev/null)"; [ -n "$LC_BIN" ] || { echo "spira-lc is not on PATH (the tree's build provides it)" >&2; exit 1; }
WORK_BIN="$(command -v work 2>/dev/null)"; [ -n "$WORK_BIN" ] || { echo "work is not on PATH (the tree's build provides it)" >&2; exit 1; }

# `work` is the lifecycle machine's door.
lcfix_up || bail "the control store did not come up"

# A filed bead has its row already (bead.sh creates it); this makes one for a fixture bead that
# was not filed, and leaves an existing one as it is.
seed_bead() {   # seed_bead <bead-id>
    "$LC_BIN" create-bead "$1" --ready >/dev/null 2>&1
}

# lc_working <bead-id> <holder> [stack-json] [persona] — the row a real Claim would leave.
lc_working() {
    LCFIX_HOLDER="$2" LCFIX_STACK="${3:-}" LCFIX_PERSONA="${4:-}" lcfix_claim "$1" || bail "could not claim $1 as $2"
}

# lc_certified <bead-id> <tip> — a CERTIFIED row at <tip>.
lc_certified() {
    lcfix_claim "$1" && lcfix_event "$1" WORKING "{\"Submit\":{\"tip\":\"$2\"}}" \
        && lcfix_event "$1" SUBMITTED "{\"GatePass\":{\"tip\":\"$2\",\"gate_key\":\"fixture-gate-key\"}}" \
        || bail "could not certify $1"
}

# lc_row <bead-id> — the row as compact JSON, so a case matches "state":"X".
lc_row() {
    _lcfix_show "$1" | python3 -c '
import json, sys
b = (json.load(sys.stdin) or {}).get("bead") or {}
print(json.dumps(b, separators=(",", ":")))'
}

# lc_events <bead-id> — how many events the bead's history holds.
lc_events() { "$LC_BIN" history "$1" 2>/dev/null | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))'; }

OLDTIP=1111111111111111111111111111111111111111
NEWTIP=2222222222222222222222222222222222222222

SOCK="$TMP/spira-lc.sock"
tl_config SPIRA_LC_SOCKET="$SOCK"
SPIRA_LC_SOCKET="$SOCK" "$LC_BIN" serve "$SOCK" >"$TMP/serve.log" 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 50); do
    [ -S "$SOCK" ] && break
    sleep 0.1
done
[ -S "$SOCK" ] || bail "spira-lc serve never created its socket: $(cat "$TMP/serve.log")"
export SPIRA_LC_SOCKET="$SOCK"
tl_config SPIRA_LC_SOCKET="$SOCK"

# ── one real bd bead, filed through bead.sh's own contract, and its lifecycle twin ───
BID="$(bead.sh file "dwarf semantic layer container-tier fixture" --for builder --repo testrepo --body-file "$TMP/build.md" 2>"$TMP/file.err")"
[ -n "$BID" ] || bail "bead.sh file did not return an id: $(cat "$TMP/file.err")"
seed_bead "$BID"

work_as() {   # work_as <bead-id> <verb> [args...]
    local bid="$1"; shift
    SPIRA_WORK_BEAD_ID="$bid" SPIRA_RUNE="builder" SPIRA_LC_SOCKET="$SOCK" "$WORK_BIN" "$@"
}

# ── show: reads bd content ────────────────────────────────────────────────────────────
out="$(work_as "$BID" show 2>&1)"; rc=$?
is   "show: exits 0"                 "0"                                              "$rc"
want "show: bd's own title is in it" "dwarf semantic layer container-tier fixture"     "$out"

# ── note: a plain bd note, no lifecycle event ────────────────────────────────────────
before_events="$(lc_events "$BID")"
out="$(work_as "$BID" note "left by the container-tier suite" 2>&1)"; rc=$?
is "note: exits 0" "0" "$rc"
note_text="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$BID" 2>&1)" # batch-job: fixture bd call against the suite's throwaway store
want "note: text landed on the bd bead" "left by the container-tier suite" "$note_text"
after_events="$(lc_events "$BID")"
is "note: emits no lifecycle event (bd, non-lifecycle, per design §3.5)" "$before_events" "$after_events"

PRE_WT="$TMP/ready-worktree"
mkdir -p "$PRE_WT"
git -C "$PRE_WT" init -q
git -C "$PRE_WT" -c user.email=dwarf@spira.local -c user.name=dwarf commit -q --allow-empty -m "fixture commit"
out="$(cd "$PRE_WT" && work_as "$BID" submit 2>&1)"; rc=$?
wantrc "submit from READY: refused (exit 3), never a silent exit 0" "3" "$rc"
want "submit from READY: the refusal names READY" "READY" "$out"
want "submit from READY: the refusal names the way out" "claim it first" "$out"
row="$(lc_row "$BID")"
want "submit from READY: the row stays READY" "READY" "$row"

# submit is legal only from WORKING (lifecycle/src/bead.rs) — seed_bead leaves BID at
# READY, same as every other fixture bead here; move it to WORKING first.
lc_working "$BID" dwarf-test

# ── submit: bead Submit(tip), tip read from the worktree, never taken as an argument ──
# A throwaway repo, never $REPO: $REPO here is testenv-batch.sh's own bind-mounted worktree,
# whose .git gitlink points at a commondir that exists on the host, not inside this
# container, so `git rev-parse HEAD` against it fails closed with no tip at all.
DWARF_WT="$TMP/dwarf-worktree"
mkdir -p "$DWARF_WT"
git -C "$DWARF_WT" init -q
git -C "$DWARF_WT" -c user.email=dwarf@spira.local -c user.name=dwarf commit -q --allow-empty -m "fixture commit"
real_tip="$(git -C "$DWARF_WT" rev-parse HEAD)"
out="$(cd "$DWARF_WT" && work_as "$BID" submit 2>&1)"; rc=$?
is "submit: exits 0 (applied)" "0" "$rc"
row="$(lc_row "$BID")"
want "submit: state is SUBMITTED" "SUBMITTED" "$row"
want "submit: tip is the worktree's real HEAD, not something the dwarf could type" "$real_tip" "$row"

# ── stacked dependents: base_withdrawn leaves WORKING in place and tells its holder;
# submit is refused until the stack is current (design stacked-dependents-2026-09-28 §1,
# sp-falao's own acceptance: "A reworked while B WORKING -> B's submit refused until it
# rebases") ────────────────────────────────────────────────────────────────────────────
PREREQ="sp-prereqfx"
seed_bead "$PREREQ"
lc_certified "$PREREQ" "$OLDTIP"

STK="$(bead.sh file "dwarf semantic layer: stacked-base fixture" --for builder --repo testrepo --body-file "$TMP/build.md")"
seed_bead "$STK"
lc_working "$STK" dwarf-stacked "{\"$PREREQ\":\"$OLDTIP\"}"

before_note="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$STK" 2>&1)" # batch-job: fixture bd call against the suite's throwaway store
out="$("$LC_BIN" event bead "$STK" --expect WORKING --version 1 --actor test --kind "{\"BaseWithdrawn\":{\"prereq\":\"$PREREQ\",\"tip\":\"$OLDTIP\"}}" 2>&1)"; rc=$?
is "base_withdrawn on a WORKING dependent: applied (exit 0)" "0" "$rc"
row="$(lc_row "$STK")"
want "base_withdrawn: the dependent stays WORKING, not sent to REWORK" "\"state\":\"WORKING\"" "$row"
want "base_withdrawn: the reason names the withdrawn prerequisite and tip" "base_withdrawn: $PREREQ $OLDTIP" "$row"
after_note="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$STK" 2>&1)" # batch-job: fixture bd call against the suite's throwaway store
[ "$after_note" != "$before_note" ] && ok "base_withdrawn: the WORKING holder is told (a note landed on the bead)" \
    || bad "base_withdrawn: the WORKING holder is told (a note landed on the bead)" "bd show did not change"
want "base_withdrawn: the note names the holder" "dwarf-stacked" "$after_note"

# The prerequisite is re-cut at a new tip and certified there, so the stack names a tip it left.
"$LC_BIN" resubmit "$PREREQ" "$NEWTIP" >/dev/null 2>&1 && "$LC_BIN" certify "$PREREQ" "$NEWTIP" pass fixture-key >/dev/null 2>&1 \
    || bail "could not re-cut $PREREQ at its new tip"
out="$(cd "$DWARF_WT" && work_as "$STK" submit 2>&1)"; rc=$?
is "submit while the stack still names a withdrawn tip: refused (exit 3)" "3" "$rc"
want "submit refusal: tells the dwarf to rebase onto the current stack" "rebase" "$out"

# once the stack is current again (the dwarf rebased and the claim's stack now names the
# prerequisite's real tip), submit succeeds exactly as it does for an unstacked bead.
STK2="$(bead.sh file "dwarf semantic layer: restacked fixture" --for builder --repo testrepo --body-file "$TMP/build.md")"
seed_bead "$STK2"
lc_working "$STK2" dwarf-stacked "{\"$PREREQ\":\"$NEWTIP\"}"
out="$(cd "$DWARF_WT" && work_as "$STK2" submit 2>&1)"; rc=$?
is "submit once the stack matches the prerequisite's current tip: applied (exit 0)" "0" "$rc"

# ── done needs WORKING, not SUBMITTED — reset via a fresh bead for the rest ──────────
DID="$(bead.sh file "dwarf semantic layer: done/blocked/split fixture" --for builder --repo testrepo --body-file "$TMP/build.md")"
seed_bead "$DID"
lc_working "$DID" dwarf-test

out="$(work_as "$DID" done --delivers "a document at wiki/x" 2>&1)"; rc=$?
is "builder done: refused (exit 3), its rune declares no DONE exit" "3" "$rc"
want "builder done: the refusal names the rune and its exits" "SUBMITTED ask park hold" "$out"
# A row's persona is set by its claim: the spike's own bead is claimed as a spike.
DID_SPIKE="$(bead.sh file "dwarf semantic layer: spike done fixture" --for builder --repo testrepo --body-file "$TMP/build.md")"
seed_bead "$DID_SPIKE"
lc_working "$DID_SPIKE" dwarf-test "" spike
DID="$DID_SPIKE"
out="$(work_as "$DID" done --delivers "a document at wiki/x" 2>&1)"; rc=$?
[ "$rc" = 0 ] || printf "%s\n" "$out" | sed "s/^/# done: /"
is "done: exits 0 (applied)" "0" "$rc"
row="$(lc_row "$DID")"
want "done: state is DONE" "\"state\":\"DONE\"" "$row"
want "done: reason carries the delivers evidence" "a document at wiki/x" "$row"

# ── groomer done: Done only on a bead carrying the groom-trigger marker ──────────────
groom_as() { SPIRA_WORK_BEAD_ID="$1" SPIRA_RUNE="groomer" SPIRA_LC_SOCKET="$SOCK" "$WORK_BIN" "${@:2}"; }
GWORK="$(bead.sh file "groomer claimed a work bead" --for builder --repo testrepo --body-file "$TMP/build.md")"
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" label add "$GWORK" "${SPIRA_GROOMER_LABEL:-groom}" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
seed_bead "$GWORK"
lc_working "$GWORK" dwarf-groomer
out="$(groom_as "$GWORK" done --delivers "note:groom.log" 2>&1)"; rc=$?
is "groomer done on an unmarked work bead: refused (exit 3)" "3" "$rc"
row="$(lc_row "$GWORK")"
want "groomer done on an unmarked work bead: READY again" "\"state\":\"READY\"" "$row"
nowant "groomer done on an unmarked work bead: never DONE" "\"state\":\"DONE\"" "$row"
want "groomer done on an unmarked work bead: the refusal says why" "not a groom trigger" "$out"

GTRIG="$(bead.sh file "groomer trigger fixture" --for builder --repo testrepo --body-file "$TMP/build.md")"
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" label add "$GTRIG" groom-trigger >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
seed_bead "$GTRIG"
lc_working "$GTRIG" dwarf-groomer
out="$(groom_as "$GTRIG" done --delivers "note:groom.log" 2>&1)"; rc=$?
is "POSITIVE CONTROL: groomer done on a marked trigger: exits 0" "0" "$rc"
row="$(lc_row "$GTRIG")"
want "POSITIVE CONTROL: groomer done on a marked trigger: DONE" "\"state\":\"DONE\"" "$row"

# ── red-tracker done: refused; a handled red leaves WORKING only by submit or ask ───
RTL="$(. "$SPIRA_HOME/conf.sh" >/dev/null 2>&1; printf '%s' "${SPIRA_RED_TRACKER_LABEL:-spira-red-tracker}")"
RT="$(bead.sh file "publish PR 1 red for testrepo" --for builder --repo testrepo --body-file "$TMP/build.md")"
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" label add "$RT" "$RTL" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
seed_bead "$RT"
lc_working "$RT" dwarf-test
out="$(work_as "$RT" done --delivers "no code change" 2>&1)"; rc=$?
is "red-tracker done: refused (exit 3)" "3" "$rc"
row="$(lc_row "$RT")"
want "red-tracker done: still WORKING" "\"state\":\"WORKING\"" "$row"
want "red-tracker done: the refusal names the exits" "its exits are SUBMITTED ask park hold" "$out"

# ── blocked: a hold, plus an ask filed for the operator ──────────────────────────────
BLID="$(bead.sh file "dwarf semantic layer: blocked fixture" --for builder --repo testrepo --body-file "$TMP/build.md")"
seed_bead "$BLID"
before_unread="$(ls "$SPIRA_MAIL/operator/new" "$SPIRA_MAIL/concierge/new" 2>/dev/null | grep -c .)"
out="$(work_as "$BLID" blocked "which persona owns this?" --default "builder" 2>&1)"; rc=$?
[ "$rc" = 0 ] || echo "# $out" >&2
is "blocked: exits 0" "0" "$rc"
row="$(lc_row "$BLID")"
want "blocked: an ask hold is recorded" "ask" "$row"
after_unread="$(ls "$SPIRA_MAIL/operator/new" "$SPIRA_MAIL/concierge/new" 2>/dev/null | grep -c .)"
[ "$after_unread" -gt "$before_unread" ] && ok "blocked: an ask landed in the operator's mailbox" \
    || bad "blocked: an ask landed in the operator's mailbox" "count did not increase ($before_unread -> $after_unread)"

# ── file-followup: filed through bead.sh's contract, parent set by the layer ─────────
out="$(work_as "$BID" file-followup "a followup filed by the container-tier suite" --repo testrepo --body-file "$TMP/build.md" 2>&1)"; rc=$?
is "file-followup: exits 0" "0" "$rc"
new_id="$(printf '%s' "$out" | tail -n1 | tr -d '[:space:]')"
[ -n "$new_id" ] || bail "file-followup produced no new bead id"
child_json="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$new_id" --json 2>&1)" # batch-job: fixture bd call against the suite's throwaway store
want "file-followup: the new bead's parent is the bound bead, not asked for" "\"parent\": \"$BID\"" "$child_json"
nowant "file-followup: did not inherit the bound bead's branch label" "branch:" "$child_json"

out="$(work_as "$BID" split "a dumpstar piece with no repo named" 2>&1)"; rc=$?
[ "$rc" != 0 ] && ok_ref=1 || ok_ref=0
is "split without --repo: refused (nonzero)" "1" "$ok_ref"
want "split without --repo: says --repo is required" "--repo" "$out"

# ── file-followup --after-landing: unparented, blocked on the bound bead ─────────────
out="$(work_as "$BID" file-followup "a followup that needs the parent landed" --repo testrepo --body-file "$TMP/build.md" --after-landing 2>&1)"; rc=$?
is "after-landing: exits 0" "0" "$rc"
al_id="$(printf '%s' "$out" | tail -n1 | tr -d '[:space:]')"
[ -n "$al_id" ] || bail "after-landing produced no new bead id"
al_json="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$al_id" --json 2>&1)" # batch-job: fixture bd call against the suite's throwaway store
nowant "after-landing: not a child of the bound bead" "\"parent\": \"$BID\"" "$al_json"
want   "after-landing: blocked on the bound bead" "\"id\": \"$BID\"" "$al_json"
want   "after-landing: the edge is a blocking one" "\"dependency_type\": \"blocks\"" "$al_json"

# ── split: same mechanism, its own bead ───────────────────────────────────────────────
out="$(work_as "$BID" split "a split piece filed by the container-tier suite" --repo testrepo --body-file "$TMP/build.md" 2>&1)"; rc=$?
is "split: exits 0" "0" "$rc"

# ── superseded-by: a hold plus a confirmation ask, never bd supersede directly ───────
mail_count() { ls "$SPIRA_MAIL"/*/new 2>/dev/null | grep -c .; }
SBID="$(bead.sh file "dwarf semantic layer: superseded-by fixture" --for builder --repo testrepo --body-file "$TMP/build.md")"
seed_bead "$SBID"
before_all="$(mail_count)"
out="$(work_as "$SBID" superseded-by --help 2>&1)"; rc=$?
is "superseded-by --help: refused (exit 3)" "3" "$rc"
want "superseded-by --help: the refusal says it is not a bead id" "not another bead's id" "$out"
row="$(lc_row "$SBID")"
nowant "superseded-by --help: no hold applied" "manual" "$row"
is "superseded-by --help: nothing filed or mailed" "$before_all" "$(mail_count)"

SUCC="$(bead.sh file "the landed successor of the supersede fixture" --for builder --repo landrepo --body-file "$TMP/build.md")"
seed_bead "$SUCC"
git init -q -b main "$TMP/landrepo"
git -C "$TMP/landrepo" -c user.name=t -c user.email=t@t commit -q --allow-empty -m "spira: land $SUCC — the landed successor"
land_sha="$(git -C "$TMP/landrepo" rev-parse --short HEAD)"
before_op="$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | grep -c .)"
out="$(work_as "$SBID" superseded-by "$SUCC" 2>&1)"; rc=$?
[ "$rc" = 0 ] || echo "# $out" >&2
is "superseded-by: exits 0" "0" "$rc"
is "superseded-by: the confirmation ask is delivered to the operator, not rerouted" "$((before_op + 1))" "$(ls "$SPIRA_MAIL/operator/new" 2>/dev/null | grep -c .)"
ask="$(cat "$SPIRA_MAIL"/operator/new/* 2>/dev/null)"
want "superseded-by: the ask gives the successor's title" "the landed successor of the supersede fixture" "$ask"
want "superseded-by: the ask gives the successor's landing commit" "$land_sha" "$ask"
row="$(lc_row "$SBID")"
want   "superseded-by: a manual hold is recorded, not SUPERSEDED" "manual" "$row"
nowant "superseded-by: the bead itself is not moved to SUPERSEDED"   "\"state\":\"SUPERSEDED\"" "$row"
want   "superseded-by: the request names the proposed successor"    "$SUCC" "$row"

# ── acceptance criterion, at full stack: a verb naming a foreign bead is refused ─────
out="$(work_as "$BID" note "$SBID" 2>&1)"; rc=$?
is "foreign bead reference: refused (exit 3) even with a live socket and a live server" "3" "$rc"

tl_summary
