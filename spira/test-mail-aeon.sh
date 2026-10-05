#!/usr/bin/env bash
#
# test-mail-aeon.sh — aeon mail lifecycle: bead.sh amend mails the live aeon, and the
# mailbox is removed when the aeon exits. The hook and address-routing halves (which need
# no bead store) live in test-mail-aeon-hook.sh (coverage-map row 11, DEMOTE-TO-T2).
#
# Acceptance criteria (each seen red first):
#   (b) bead.sh amend on an in_progress bead with a live aeon mails the change
#   (c) amend on an open bead sends nothing and succeeds
#   (d) mailbox created while the aeon runs, then gone after the aeon exits (positive
#       control added per coverage-map row 12: the stub agent records that it observed the
#       mailbox before closing the bead, so "gone after" is not indistinguishable from
#       "never existed")
#
# (b), (c): bead.sh amend tested with real db and a fake pidfile (live = bash $$).
# (d): aeon.sh run with a stub agent that immediately closes the bead.
#
# tier: T3
# covers: spira/bead.sh mail/src/* aeon/src/* UC-operator-channel-12
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

. "$HERE/testdb.sh"
testdb_require test-mail-aeon
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up mailaeon || { echo "test-mail-aeon: could not build fixture db"; exit 1; }
# bdq is a lib.sh function; define a thin wrapper so test-level calls reach the fixture db.
bdq() { BD_IGNORE_SCHEMA_SKEW=1 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber" "$SPIRA_HOME/hooks"
# conf.d IS COPIED IN (matching test-aeon-sweep.sh, test-aeon-world-stop.sh, ...): aeon's
# own in-process config registry (spira_config::resolve, aeon::conf::merge_resolved_config)
# derives conf.d from THIS --home and now REFUSES to start if it is missing (sp-1cdgq) --
# a --home with no conf.d used to resolve silently to nothing instead of refusing.
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
printf '. "%s/lib.sh"\n' "$HERE" > "$SPIRA_HOME/lib.sh"   # the aeon binary sources <home>/lib.sh; this is the real one, as aeon.sh sourced it
# conf.d/conf-gen.sh ALSO HAVE TO BE HERE, not just reachable through lib.sh's source chain:
# the aeon binary's own in-process config resolution (spira_config::resolve, wave 4.8)
# reads conf.d/<KEY> straight off this literal SPIRA_HOME (home.join("conf.d")), never
# through a symlink or source chain (that is `default_conf_d`'s own doc). Every other
# fixture driving the real `aeon` binary already carries this copy (test-aeon-resume.sh,
# test-aeon-chamber-overlay.sh, ...); without it, every generic registry key — SPIRA_MAIL
# among them — silently resolves to "" because its conf.d/SPIRA_MAIL file is never found,
# so part (d) below created no mailbox for the stub to see.
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""   # prevent reading a real spira.conf


# mail.sh is gone (sp-ooh1k): bead.sh now calls the compiled `mail` binary by bare name, and
# the suite's PATH (testenv --with-bins) already carries it — no fixture copy needed.
cp "$HERE/hooks/aeon-mail-deliver.sh" "$SPIRA_HOME/hooks/"
# The aeon arms its worktree through worktree-hooks.sh (the tree's, by name), which composes
# <home>/hooks/pre-commit into the worktree's hook: the fixture home carries a pass-through
# one, since the canonical fences are not what this suite is about.
printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRA_HOME/hooks/pre-commit"; chmod +x "$SPIRA_HOME/hooks/pre-commit"

# ==========================================================================
# bead.sh amend — (b) and (c)
# ==========================================================================
echo
echo "bead.sh amend — in_progress bead with live aeon (b)"

export SPIRA_REPO_MAP="$TMP/repo-map"
printf '' > "$SPIRA_REPO_MAP"   # empty; amend does not need it

run_bead() { SPIRA_HOME="$SPIRA_HOME" SPIRA_MAIL="$SPIRA_MAIL" SPIRA_RUN="$SPIRA_RUN" \
             bead.sh "$@"; }

# The amended beads are FIXTURE STATE, declared as data — one held in_progress by an aeon,
# one open — never claimed or closed through bd around the lifecycle machine (sp-hyo5e).
# Neither carries the builder's partition label, so (d)'s aeon cannot claim them.
fixture_beads() {   # fixture_beads <status-of-sp-amend2> <status-of-sp-amend3>
    testdb_seed <<JSONL
{"id":"sp-amend2","title":"Test amend bead","status":"$1","assignee":"aeon-builder","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL:-spira}","repo:fixture"],"updated_at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)"}
{"id":"sp-amend3","title":"Test no-aeon bead","status":"$2","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL:-spira}","repo:fixture"],"updated_at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)"}
JSONL
}
fixture_beads in_progress open || { bad "bead.sh amend: could not seed the test beads" ""; tl_summary; exit 1; }
BID2=sp-amend2
is "the fixture holds the amended bead in_progress" in_progress \
    "$(bdq show "$BID2" --json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0])["status"])' 2>/dev/null)"

FAKE_PID=$$
FAKE_PF="$SPIRA_RUN/aeon-builder-$BID2.pid"
echo "$FAKE_PID" > "$FAKE_PF"

mkdir -p "$SPIRA_MAIL/aeon-$BID2/new" "$SPIRA_MAIL/aeon-$BID2/cur" "$SPIRA_MAIL/aeon-$BID2/tmp"

amend_rc=0
run_bead amend "$BID2" --note "Scope expanded: add acceptance tests" >/dev/null 2>&1 || amend_rc=$?
is  "SEEN RED (b): amend exits 0 with live aeon" 0 "$amend_rc"
mbx_new="$(ls "$SPIRA_MAIL/aeon-$BID2/new" 2>/dev/null | wc -l | tr -d ' ')"
is   "SEEN RED (b): amend delivers mail to aeon mailbox"  "1"  "$mbx_new"

msg_body="$(cat "$SPIRA_MAIL/aeon-$BID2/new"/* 2>/dev/null)"
want  "SEEN RED (b): mail contains the note text"  "Scope expanded"  "$msg_body"

rm -f "$FAKE_PF"
rm -rf "$SPIRA_MAIL/aeon-$BID2"

echo
echo "bead.sh amend — open bead with no live aeon (c)"

BID3=sp-amend3

amend3_rc=0
run_bead amend "$BID3" --note "No aeon watching" >/dev/null 2>&1 || amend3_rc=$?
is    "SEEN GREEN (c): amend exits 0 with no live aeon"   0 "$amend3_rc"
is     "SEEN GREEN (c): no mailbox created"  "" \
    "$(ls "$SPIRA_MAIL/aeon-$BID3/new" 2>/dev/null | tr '\n' ' ' | tr -d ' ')"

# ==========================================================================
# (d) mailbox created while the aeon runs, removed after the aeon exits
# ==========================================================================
# Out of (d)'s way — its stub agent works the first in_progress bead. The fixture rows are
# removed outright (test cleanup, not a lifecycle transition): a re-seed as closed loses to
# the amend's own newer updated_at, and the upsert is skipped.
bdq delete sp-amend2 sp-amend3 --force >/dev/null 2>&1
is "the amend fixtures are gone before (d)" "" \
    "$(bdq list --json 2>/dev/null | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; print(" ".join(x["id"] for x in r if x["id"] in ("sp-amend2","sp-amend3")))' 2>/dev/null)"

echo
echo "(d) mailbox seen during the run, then gone after the aeon exits"

AEON_ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$AEON_ORIGIN"
AEON_REPO="$TMP/repo"; git clone -q "$AEON_ORIGIN" "$AEON_REPO" 2>/dev/null
git -C "$AEON_REPO" config user.email t@t; git -C "$AEON_REPO" config user.name t
printf 'seed\n' > "$AEON_REPO/f"
git -C "$AEON_REPO" add f
git -C "$AEON_REPO" commit -qm seed
git -C "$AEON_REPO" push -q origin main 2>/dev/null

printf 'fixture | %s | push | origin/main | |\n' "$AEON_REPO" > "$SPIRA_HOME/repo-map"
export SPIRA_REPO_MAP="$SPIRA_HOME/repo-map"

cat > "$SPIRA_HOME/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}partition:${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"
# sp-mve9i: the aeon reads its bead's state from the lifecycle row, never bd status; the
# shim's bd close is told to it in lifecycle terms (testlib.sh lc_aeon_mirror).
lc_aeon_mirror "$TMP/lcm"; export PATH="$TMP/lcm:$PATH"
command -v aeon >/dev/null 2>&1 \
    || { bad "(d): aeon is not on PATH" ""; tl_summary; exit 1; }

export MAILBOX_SEEN_MARKER="$TMP/mailbox-seen"

cat > "$BIN/claude" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null  # drain prompt
id="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
[ -n "$id" ] || exit 1
# POSITIVE CONTROL (row 12): the mailbox must exist WHILE the aeon runs, before it is
# checked for absence after — otherwise "gone" is indistinguishable from "never made".
[ -d "$SPIRA_MAIL/aeon-$id" ] && touch "${MAILBOX_SEEN_MARKER:-/dev/null}"
printf 'work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id: done" >/dev/null 2>&1
BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}\n'
STUB
chmod +x "$BIN/claude"

BID4="$(bdq create "Test mailbox cleanup bead" -l "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}partition:${SPIRA_PLAN_LABEL:-plan},repo:fixture" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0])["id"])' 2>/dev/null)"
[ -n "$BID4" ] || { bad "(d): could not file test bead" ""; tl_summary; exit 1; }

aeon_rc=0
SPIRA_HOME="$SPIRA_HOME" SPIRA_RUN="$SPIRA_RUN" SPIRA_MAIL="$SPIRA_MAIL" \
SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}" \
SPIRA_AGENT="$BIN/claude" SPIRA_CONF="" \
GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
    aeon --home "$SPIRA_HOME" builder >/dev/null 2>&1 || aeon_rc=$?

status="$(bdq show "$BID4" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,dict) else d[0]; print(d.get("status",""))' 2>/dev/null)"
labels="$(bdq show "$BID4" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,dict) else d[0]; print(",".join(d.get("labels") or []))' 2>/dev/null)"
# The stub closes a task bead; aeon.sh's teardown converts that close to open +
# spira-submitted (sp-qsona) — only the landing pass closes a work bead.
is   "SEEN RED (d): bead was closed by the stub aeon (converted to submitted)"  "open"  "$status"
want "SEEN RED (d): carrying the submitted label" "spira-submitted" "$labels"

if [ -f "$MAILBOX_SEEN_MARKER" ]; then
    ok "POSITIVE CONTROL (d): the mailbox existed while the aeon ran"
else
    bad "POSITIVE CONTROL (d): the mailbox existed while the aeon ran" \
        "stub never saw it — 'gone after' below would be vacuous"
fi

mbx_path="$SPIRA_MAIL/aeon-$BID4"
if [ -d "$mbx_path" ]; then
    bad "(d): mailbox still present after aeon exited" "$mbx_path"
else
    ok "(d): mailbox gone after aeon exited"
fi

tl_summary
