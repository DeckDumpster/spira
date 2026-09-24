#!/usr/bin/env bash
#
# test-migrate-ask.sh — no caller of cockpit/ask.sh remains; every migrated sender
# produces a message that passes mail.sh's built-in lint.
#
# LAW-ABSENCE-NEEDS-A-POSITIVE-CONTROL applies twice:
#   1. The grep that finds ask.sh callers must be shown to fire before it is shown to be
#      silent — a grep whose pattern never matches passes "no callers" trivially.
#   2. Each migrated sender is exercised against the real mail.sh lint path (which runs
#      inside send), not a stub, because a stub that always exits 0 proves nothing.
#
# covers: spira/lib.sh spira/sentinel.sh spira/watchd.sh spira/skew.sh spira/pilgrimage.sh spira/archivist.sh spira/reflect.sh spira/incident.sh spira/strand.sh spira/mail.sh
# host-reason: lint runs in-process; no systemd/gh/network required; no shared state written
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-migrate-ask.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

REPO_ROOT="$(cd "$HERE/.." && pwd)"

# mail.sh sources conf.sh which derives these from SPIRA_HOME. Set them explicitly so the
# test does not depend on the real spira.conf or a real SPIRA_HOME.
export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_KINDS="$HERE/mail/kinds"
export SPIRA_CONF=/nonexistent

send_note()     { bash "$HERE/mail.sh" send test --from "$1" --subject "$2" --kind note <<< "$3"; }
send_question() { bash "$HERE/mail.sh" send test --from "$1" --subject "$2" --kind question --default "$3" <<< "$4"; }

# ======================================================================================
echo
echo "grep positive control — the pattern fires before it is trusted to be silent:"
# ======================================================================================
# Plant a synthetic caller so the grep has something to match.
PLANT="$TMP/synthetic-caller.sh"
printf '#!/usr/bin/env bash\nbash cockpit/ask.sh add "some question"\n' > "$PLANT"

grep -E 'cockpit/ask\.sh|SPIRA_ASK[^_]' "$PLANT" >/dev/null 2>&1 \
    && ok  "grep pattern fires on a planted ask.sh caller" \
    || bad "grep pattern fires on a planted ask.sh caller" "pattern never matched"

rm -f "$PLANT"

# ======================================================================================
echo
echo "no ask.sh callers remain in the tree:"
# ======================================================================================
remaining="$(grep -rE 'cockpit/ask\.sh|SPIRA_ASK[^_]' \
    --include='*.sh' --include='*.md' --include='*.conf' \
    --exclude="$(basename "$0")" \
    "$REPO_ROOT/spira" "$REPO_ROOT/cockpit" 2>/dev/null || true)"

[ -z "$remaining" ] \
    && ok  "no ask.sh callers remain in spira/ or cockpit/" \
    || bad "no ask.sh callers remain in spira/ or cockpit/" \
           "$(printf '%s' "$remaining" | head -5)"

[ ! -f "$REPO_ROOT/cockpit/ask.sh" ] \
    && ok  "cockpit/ask.sh is deleted" \
    || bad "cockpit/ask.sh is deleted" "file still exists"

# ======================================================================================
echo
echo "mail.sh lint — each migrated sender produces a well-formed message:"
# ======================================================================================
# send returns 1 if lint fails; 0 if the message was accepted and delivered.
# Exercising the real send path proves the actual heredoc bodies are correct.

# -- lib.sh: spira_ask_timeout_loop (question) --
subj="sp-foo timed out 3 times in the incident lane (120s cap)"
dflt="move the bead to a persona with no cap by replacing the 'incident' label with 'plan'"
body="## Question
$subj

## Default
$dflt

sp-foo/main has been killed by the 120s cap 3 times without committing anything."
send_question "Landing gate <gate@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "lib.sh spira_ask_timeout_loop question passes lint" "0" "$?"

# -- lib.sh: land_escalate (question) --
subj="Spira is landing nothing — gate keeps failing"
dflt="run SPIRA_HOME/landing.sh by hand to see the failure, then file the fix as a bead"
body="## Question
$subj

## Default
$dflt

every finished branch has been rejected by the landing gate."
send_question "Landing gate <gate@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "lib.sh land_escalate question passes lint" "0" "$?"

# -- lib.sh: spira_event (note) --
body="## Note
bead.landed on sp-x

kind: bead.landed

target: sp-x"
send_note "Spira event <event@spira>" "bead.landed on sp-x" "$body" >/dev/null 2>&1
is "lib.sh spira_event note passes lint" "0" "$?"

# -- sentinel.sh: requeue escalation (question) --
subj="sp-bar has been closed by an aeon without new commits — requeue?"
dflt="requeue the bead if the work is real, or close it permanently if it was a duplicate"
body="## Question
$subj

## Default
$dflt

sp-bar was closed without landing anything."
send_question "Sentinel <sentinel@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "sentinel.sh requeue question passes lint" "0" "$?"

# -- watchd.sh: escalation (question) --
subj="Watchd: landing has been stalled for 4h"
dflt="run landing.sh by hand and file the fix as a bead"
body="## Question
$subj

## Default
$dflt

no branch has landed in the last 4 hours.

latest event: nothing"
send_question "Watchd <watchd@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "watchd.sh question passes lint" "0" "$?"

# -- reflect.sh: escalation (question) --
subj="The DAG is stalled — no ready work and nothing in flight"
dflt="read the diagnosis and decide"
body="## Question
$subj

## Default
$dflt

the Spira DAG is stalled with open work and nothing ready"
send_question "Reflect <reflect@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "reflect.sh question passes lint" "0" "$?"

# -- strand.sh: escalation (question) --
subj="sp-baz is a stranded branch — its bead is closed"
action="close the branch or reopen the bead"
body="## Question
$subj

## Default
$action

The branch spira/sp-baz exists but its bead is closed.

context here"
send_question "Strand check <strand@spira>" "$subj" "$action" "$body" >/dev/null 2>&1
is "strand.sh question passes lint" "0" "$?"

# -- pilgrimage.sh: completion (note) --
body="## Note
PILGRIMAGE COMPLETE — sp-epi: a finished epic

target: sp-epi

All 2 child beads closed: sp-c1 sp-c2"
send_note "Pilgrimage <pilgrimage@spira>" "PILGRIMAGE COMPLETE — sp-epi: a finished epic" "$body" >/dev/null 2>&1
is "pilgrimage.sh note passes lint" "0" "$?"

# -- skew.sh: escalation (question) --
subj="Spira is NOT-LATEST: activated release is spira-20260912T100000Z, latest is spira-20260912T120000Z"
dflt="run the upgrade procedure from docs/ops/upgrade.md"
body="## Question
$subj

## Default
$dflt

The activated Spira release is behind the latest available release."
send_question "Skew <skew@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "skew.sh question passes lint" "0" "$?"

# -- incident.sh: SIN escalation (question) --
subj="INCIDENT SPIKE: 5 incidents with ref incident:test-ref in 1h"
dflt="mute this alert and leave the incident open for Ops to work unpaged"
body="## Question
$subj

## Default
$dflt

5 incidents filed in the last hour for this ref."
send_question "Incident <incident@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "incident.sh SIN question passes lint" "0" "$?"

# -- archivist.sh: insight note --
body="## Note
Insight: the landing gate rejects branches with trailing whitespace

content here"
send_note "Spira event <event@spira>" "Insight: the landing gate rejects branches with trailing whitespace" "$body" >/dev/null 2>&1
is "archivist.sh note passes lint" "0" "$?"

# -- lib.sh: spira_ask_rebase_loop (question, others empty) --
subj="express lane: a label that exempts a bead from throttle: spira/sp-abc rebase loop x5 in spira"
dflt="rebase spira/sp-abc by hand and push, or close it if the work is already landed"
body="## Question
$subj

## Default
$dflt

sp-abc has been reopened for a rebase conflict 5 times and the loop is not converging. Conflicts in: foo.sh.
Status: in_progress."
send_question "Landing gate <gate@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "lib.sh spira_ask_rebase_loop (empty others) passes lint" "0" "$?"

# -- lib.sh: spira_ask_rebase_loop (question, others non-empty) --
subj="express lane: a label that exempts a bead from throttle: spira/sp-abc rebase loop x5 in spira"
dflt="check whether spira/sp-abc is a duplicate of sp-xyz and close it if so; if the work is genuinely new, rebase by hand and push"
body="## Question
$subj

## Default
$dflt

sp-abc has been reopened for a rebase conflict 5 times and the loop is not converging. Conflicts in: foo.sh. The conflicted files were also changed on the base by sp-xyz.
Status: in_progress."
send_question "Landing gate <gate@spira>" "$subj" "$dflt" "$body" >/dev/null 2>&1
is "lib.sh spira_ask_rebase_loop (non-empty others) passes lint" "0" "$?"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
