#!/usr/bin/env bash
# test-mail-real-senders.sh — real harness emitters pass mail.sh's own lint (gap G-05,
# UC-operator-channel-05).
#
# test-migrate-ask.sh used to lint hand-copied sender bodies, so a regression in a real
# emitter's actual message construction went unseen (it tested copies, not the senders).
# This calls each emitter's real code — not a re-typed body — with mail.sh pointed at a
# scratch Maildir and with no lint override, so the running message actually clears
# mail.sh's send path.
#
# COVERAGE: land_escalate (lib.sh) is a standalone function reachable without standing up
# a database or systemd — sourced directly. watchd and skew are both compiled binaries now
# (sp-07yxy's watchd, sp-yyk47's skew) — there is no source text left to sed or source a
# function body from, so each is driven through its own real CLI instead: watchd's notify
# path directly, skew's through a real `skew check --escalate` against a minimal
# NOT-LATEST fixture.
#
# incident.sh's SIN escalation is driven through incident-stub-bd.py (a genuinely stateful
# fake bd, not a canned response — test-sin-exempt.sh already established that it reproduces
# the create-then-recur sequence faithfully) rather than a real bd store: what this suite
# checks is that the message incident.sh builds clears mail.sh's own lint, which needs the
# real mail.sh, not a real database. Standing up a real recurrence count is test-sin-exempt.sh
# and test-incident-recur-cause.sh's job, not this one's.
#
# archivist's sweep, driven for real over a fabricated transcript that ctx-meter.sh measures
# for real, with SPIRA_AGENT stubbed to do exactly what the real archivist's own brief
# (chamber/archivist.md) tells it to: call `archivist mark <session> archiving <n>` as it
# files something. A sweep that crosses the top band sends no per-session note of its own — the
# daily digest is the only path to the operator — so nothing here should ever reach the mailbox.
#
# tier: T2
# covers: spira/lib.sh watchd/* skew/src/* spira/incident.sh archivist/src/* spira/ctx-meter.sh spira/incident-stub-bd.py spira/mail.sh incident/* UC-operator-channel-05
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=/nonexistent
export SPIRA_ID_PREFIX="sp"
export SPIRA_ASK_LABEL="needs-operator"

# A stub bd: every emitter below only needs "is there already an open ask with this
# subject" to answer no, so a send is always attempted for real.
STUB_BD="$TMP/bd-stub.sh"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '[]\n'
STUB
chmod +x "$STUB_BD"
export SPIRA_BD="$STUB_BD"
export SPIRA_DB="$TMP/db"

unread() { mail.sh count operator 2>/dev/null; }

echo
echo "lib.sh: land_escalate (question)"

. "$HERE/lib.sh"
export SPIRA_LAND_ESCALATE_EVERY=0
before="$(unread)"
land_escalate "gate keeps failing in the real emitter test" \
    "every finished branch has been rejected by the landing gate." >/dev/null 2>&1
after="$(unread)"
is "land_escalate delivers exactly one message" "$((before + 1))" "$after"
msg="$(ls -t "$SPIRA_MAIL/operator/new" 2>/dev/null | head -1)"
body="$(cat "$SPIRA_MAIL/operator/new/$msg" 2>/dev/null)"
want "land_escalate message names the reason"  "gate keeps failing"  "$body"
want "land_escalate message carries a Default"  "## Default"          "$body"

echo
echo "watchd: notify's escalate/ask (question)"

# sp-48f6g: watchd.sh rewritten to the compiled binary `watchd`; _wd_ask no longer exists to
# source and call directly. Drives the real escalation path end to end instead: a one-line
# manifest, a planted backlog old enough to fire on the first pass, `watchd notify` run for
# real against this scratch SPIRA_RUN/SPIRA_MAIL/SPIRA_BD — the same "real sender, real
# mail.sh" contract every other emitter in this suite is held to.
command -v watchd >/dev/null 2>&1 || bail "watchd is not on PATH"
WD_WATCHERS="$TMP/watchd-watchers"
printf 'realsender|log|%s/realsender.log\n' "$SPIRA_RUN" > "$WD_WATCHERS"
mkdir -p "$SPIRA_RUN/watchd"
printf '[2000-01-01T00:00:00Z] FAIL planted by the real-sender test\n' > "$SPIRA_RUN/realsender.log"
before="$(unread)"
SPIRA_WATCHERS="$WD_WATCHERS" SPIRA_NOTIFY_AGE=1 SPIRA_ACTIONABLE='FAIL' watchd notify >/dev/null 2>&1
after="$(unread)"
is "watchd notify delivers exactly one message" "$((before + 1))" "$after"

echo
echo "skew: escalate (question)"

# `skew` is a compiled binary now (sp-yyk47): its `escalate()` is no longer bash source to
# extract with sed (that technique needed skew.sh's own text; a binary has none to read).
# Instead, drive the real `skew check --escalate` through a real NOT-LATEST scenario — the
# same minimal fixture test-skew-escalate.sh builds for its own coverage of escalate()'s
# error handling — so this suite still exercises the real send path against the real
# mail.sh, with no lint override, the property this suite exists to prove.
SKEW_REPO="$TMP/skew-repo"
git init -q -b main "$SKEW_REPO"
git -C "$SKEW_REPO" config user.email t@t; git -C "$SKEW_REPO" config user.name t
git -C "$SKEW_REPO" commit -q --allow-empty -m c1
C1="$(git -C "$SKEW_REPO" rev-parse HEAD)"
git -C "$SKEW_REPO" tag -a "spira-release-spira-20260101T000000Z" "$C1" -m v1
git -C "$SKEW_REPO" commit -q --allow-empty -m c2
C2="$(git -C "$SKEW_REPO" rev-parse HEAD)"
git -C "$SKEW_REPO" tag -a "spira-release-spira-20260102T000000Z" "$C2" -m v2
SKEW_RELEASES="$TMP/skew-releases"
mkdir -p "$SKEW_RELEASES/spira-20260101T000000Z"
printf 'commit %s\ntimestamp 20260101T000000Z\n' "$C1" > "$SKEW_RELEASES/spira-20260101T000000Z/MANIFEST"
ln -s "spira-20260101T000000Z" "$SKEW_RELEASES/current"   # activates the OLDER tag -> NOT-LATEST
SKEW_RUN="$TMP/skew-run"; mkdir -p "$SKEW_RUN"

before="$(unread)"
env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$HERE" SPIRA_REPO="$SKEW_REPO" \
    SPIRA_RUN="$SKEW_RUN" SPIRA_RELEASES="$SKEW_RELEASES" \
    SPIRA_MAIL="$SPIRA_MAIL" \
    SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    skew check --escalate >/dev/null 2>&1
after="$(unread)"
is "skew escalate delivers exactly one message" "$((before + 1))" "$after"

echo
echo "incident.sh: SIN escalation (question)"

INC=incident.sh   # invoked by name on the suite's PATH (sp-gypjk)
SIN_STATE="$TMP/sin-state.json"
SIN_LOG="$TMP/sin-bd.log"
SIN_AT=2

# file_sin_incident <ref> <title> <payload> — a real incident.sh subprocess against
# incident-stub-bd.py, with the real mail.sh (SPIRA_HOME/SPIRA_MAIL from the suite-wide
# exports above) so the SIN message actually clears mail.sh's lint.
file_sin_incident() {
    local ref="$1" title="$2" payload="$3"
    printf '%s' "$payload" | \
        env SPIRA_BD="$HERE/incident-stub-bd.py" \
        STUB_BD_STATE="$SIN_STATE" STUB_BD_LOG="$SIN_LOG" \
        SPIRA_DB="fakedb" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/run/real-sender-sin.lock" \
        SPIRA_INCIDENT_REPO=real-sender-fixture \
        SPIRA_SIN_AT="$SIN_AT" \
        "$INC" file "$title" - 2>/dev/null
}

ref="incident:real-sender-sin-test"
before="$(unread)"
# SIN_AT counts RECURRENCES, not sightings: the filing that creates the bead is recurrence 0,
# so reaching SIN_AT recurrences takes one create plus SIN_AT recurring filings (test-sin-exempt.sh).
for i in $(seq 1 "$(( SIN_AT + 1 ))"); do
    file_sin_incident "$ref" "real sender SIN test" "payload $i" >/dev/null
done
after="$(unread)"
is "incident.sh SIN escalation delivers exactly one message" "$((before + 1))" "$after"
msg="$(ls -t "$SPIRA_MAIL/operator/new" 2>/dev/null | head -1)"
body="$(cat "$SPIRA_MAIL/operator/new/$msg" 2>/dev/null)"
want "incident.sh SIN message carries a Default"     "## Default"  "$body"
want "incident.sh SIN message names the recurrence"  "recurred"    "$body"

echo
echo "archivist: sweep over the top band sends no per-session note"

ARC_SH=archivist   # invoked by name on the suite's PATH (sp-gypjk)
mkdir -p "$TMP/arc-chamber" "$TMP/arc-run" "$TMP/arc-projects/-test-project"
cp "$HERE/chamber/archivist.md" "$TMP/arc-chamber/"

# mkarctranscript <path> <turns> <ctx> — same synthetic shape test-archivist uses, so
# ctx-meter.sh measures a real (if fabricated) transcript rather than a hand-typed SP_CTX_*.
mkarctranscript() {
    local tp="$1" n="$2" ctx="$3" i per
    per=$(( ctx / (n > 0 ? n : 1) ))
    : > "$tp"
    for i in $(seq 1 "$n"); do
        printf '{"type":"assistant","message":{"id":"m-%d","usage":{"input_tokens":%d,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":10}}}\n' \
            "$i" "$per" >> "$tp"
    done
}
# 50 turns at 1.1M tokens crosses every band ("over"), and 50 turns of drift against the
# non-default SPIRA_ARCHIVIST_EVERY=10 below clears the sweep trigger with room to spare.
mkarctranscript "$TMP/arc-projects/-test-project/sess-realsender.jsonl" 50 1100000

# THE STUB DOES EXACTLY WHAT chamber/archivist.md TELLS A REAL ARCHIVIST TO DO: call
# `archivist mark <session> archiving <n>` as it files something. That write is what
# turns items_filed from 0 to 1 — archive() deliberately never parses the model's own prose
# for a count (see archivist's ITEMS COMES FROM THE STATE FILE comment).
STUB_ARC_CLAUDE="$TMP/stub-archivist-claude"
cat > "$STUB_ARC_CLAUDE" <<STUBEOF
#!/usr/bin/env bash
cat >/dev/null
"$ARC_SH" mark sess-realsender archiving 1
exit 0
STUBEOF
chmod +x "$STUB_ARC_CLAUDE"

before="$(unread)"
SPIRA_RUN="$TMP/arc-run" SPIRA_TOKEN_PROJECTS="$TMP/arc-projects" \
    SPIRA_CTX_WARN=200000 SPIRA_CTX_HIGH=400000 SPIRA_CTX_LIMIT=1000000 \
    SPIRA_ARCHIVIST_EVERY=10 SPIRA_ARCHIVIST_PER_PASS=1 SPIRA_ARCHIVIST_TIMEOUT=10 \
    SPIRA_CHAMBER="$TMP/arc-chamber" SPIRA_AGENT="$STUB_ARC_CLAUDE" \
    "$ARC_SH" sweep >/dev/null 2>&1
after="$(unread)"

# THE SWEEP ITSELF WORKED: the fabricated session crossed the top band and archive()
# recorded it filed.
state="$(sed -n 's/^state=//p' "$TMP/arc-run/archivist/sess-realsender.state" 2>/dev/null)"
items="$(sed -n 's/^items_filed=//p' "$TMP/arc-run/archivist/sess-realsender.state" 2>/dev/null)"
is "archivist sweep records the session safe to clear" "safe" "$state"
is "archivist sweep records the item the stub filed"   "1"    "$items"

# NOTHING REACHES THE OPERATOR from this sweep: no items were queued for the digest (the
# stub only calls `mark`, never `record`), so digest_send has nothing to send, and the
# sweep itself sends no note of its own now that the per-session push is gone.
is "archivist sweep sends no mail with nothing queued for the digest" "$before" "$after"

# THE LINT ITSELF STILL REFUSES THAT SHAPE: a `--kind note` from archivist@spira with no
# `--digest` is exactly the ad hoc per-finding note mail.sh's guard exists to stop. This is
# what G-05 exists to catch, so the guard is worth pinning directly even though nothing in
# archivist sends this shape any more.
lint_err="$(printf 'body' | mail.sh send operator \
    --from "Archivist <archivist@spira>" --subject "isolated repro" --kind note 2>&1 >/dev/null)"
rc=$?
is "the isolated repro also fails" "1" "$rc"
want "the isolated repro names the archivist-note guard" "archivist note refused" "$lint_err"

tl_summary
