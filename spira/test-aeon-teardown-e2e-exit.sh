#!/usr/bin/env bash
#
# test-aeon-teardown-e2e-exit.sh — yield-headless, exit-code and operator-wait teardown rows
# One of three parts of the aeon teardown wiring proof, split so no part nears the suite wall
# bound: test-aeon-teardown-e2e.sh holds the charge and pre-session rows, -wired the decision
# and ledger rows, -exit the yield, exit-code and operator-wait rows. Each builds the shared
# full-aeon fixture once and resets it between rows.
#
# SERVER-MODE bd: attempts_of/requeues_of read the events table via `bd sql`, which embedded
# mode refuses (testdb.sh).
#
# defect: sp-egge2 sp-ne93n sp-l7f5 sp-214 sp-ywlti sp-iu10 sp-2a4hd sp-wnsks
# tier: T3
# covers: aeon/src/* spira/lib.sh mail/src/* spira-lc/src/work.rs work/* cockpit/ops/src/resolve.rs cockpit/ops/src/resolve_main.rs
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# testdb-mode: server — attempts_of/requeues_of read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/full-aeon-fixture.sh"

fa_setup teardownexit || exit 77
trap 'fa_teardown' EXIT INT TERM
. "$HERE/testlib/teardown-e2e.sh"

echo "test-aeon-teardown-e2e-exit.sh"

# ==========================================================================================
echo
echo "ROW: yield-headless — ledger status and bead note, wired end to end"
# ==========================================================================================
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build"}}]}}\n'
printf '{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"The build is running. I will wait for the background task notification to continue."}],"stop_reason":"end_turn"}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":5000,"num_turns":2,"total_cost_usd":0.01}\n'
exit 0
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-yh-1; fa_run_aeon >/dev/null
want "ledger records yield-headless"      "status=yield-headless" "$(fa_ledger_line sp-yh-1)"
want "bead note mentions yield-headless"  "background task notification" "$(fa_notes sp-yh-1)"

# ==========================================================================================
echo
echo "ROW: exit code, bead mode — closed bead exits 0 regardless of claude's own rc"
# ==========================================================================================
# THE DEFECT THIS TESTS. ops and qa run as named systemd units. A named unit enters FAILED
# when its ExecStart exits non-zero — an alert that is always firing is one nobody reads
# (law-alerts-must-be-actionable) — so a session that did the work and closed the bead must
# not fail the unit just because the claude CLI's own exit code was a stray non-zero.
cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
cat /dev/stdin > /dev/null 2>&1
id="${BEAD_ID:-}"   # the bound bead (sp-v62vn: bd status no longer reads in_progress)
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 0)"
SHIM
chmod +x "$FA_BIN/claude"

fa_reset; fa_seed sp-ex-2
printf 1 > "$FA_TMP/shim-rc"
rc="$(fa_run_aeon)"
# Since sp-v62vn the session is restricted, and its hand-on (the shim's close, which the
# lifecycle stand-in reads as `work submit`) is the submitted disposition, not teardown's
# closed branch: the bd-close conversion to open+spira-submitted is gone with the branch.
# The ledger's real rc is NOT (UC-aeon-execution-18): the submitted exit recorded the
# aeon's own 0 until the submitted branch ledgered the model's rc itself.
is "aeon exits 0 despite claude rc=1 (the fix)" "0" "$rc"
want "ledger still records the real rc" "rc=1" "$(fa_ledger_line sp-ex-2)"
want "and records the submitted status" "status=submitted" "$(fa_ledger_line sp-ex-2)"
# The positive control for this UC (bead not closed, claude rc=1, aeon exits non-zero) is
# the "session did not close" row of test-aeon-teardown-e2e.sh (sp-rq-2).

# ROW DELETED — FAYTH_GRAPH_ONLY's close standing unconverted (sp-wnsks) was a rule inside
# teardown's closed branch, which no session reaches since sp-v62vn (every session is
# restricted and hands its bead on through the work verbs).

# ==========================================================================================
echo
echo "ROW: exit code, sweep mode — claude rc=1 but ran still exits 0"
# ==========================================================================================
cat > "$FA_HOME/chamber/sweeper.fayth" <<SFAYTH
FAYTH_NAME=sweeper
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
SFAYTH
printf 'sweep {{BEAD_ID}}\n{{PARK}}\n' > "$FA_HOME/chamber/sweeper.md"
# persona.sweeper.model: aeon::conf::persona_model refuses outright when a fayth's model
# is undeclared (no built-in fallback, per Ryan 2026-10-05) — the complete fixture declares
# every REAL persona's model but has never heard of this suite's own "sweeper" fayth.
# tl_config only knows the SPIRA_FOO -> spira.foo mapping, not [persona.*] tables, so this
# sets the dotted path directly.
spira-config set persona.sweeper.model claude-sonnet-5-5 "$_TL_CONF_OVERRIDE" >/dev/null \
    || bail "could not declare persona.sweeper.model"

cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":2000,"num_turns":1,"total_cost_usd":0.001}\n'
exit "$(cat "$TMP/shim-rc" 2>/dev/null || echo 0)"
SHIM
chmod +x "$FA_BIN/claude"
printf 1 > "$FA_TMP/shim-rc"
sweep_rc="$(aeon --home "$SPIRA_HOME" sweeper --sweep --prompt "check pipeline" > "$FA_TMP/sweep-out" 2>&1; echo $?)"
[ "$sweep_rc" = 0 ] || sed 's/^/# /' "$FA_TMP/sweep-out" 2>/dev/null
is "sweep with claude rc=1 but ran exits 0 (ops/qa sweep fix)" "0" "$sweep_rc"
# The positive control for this UC (a refused sweep — no tool calls — exits non-zero so a
# real ops failure stays visible) is test-aeon-sweep.sh's instead, which already builds the
# lighter sweep-only fixture this control needs and does not touch this file's 60s cap.

# ==========================================================================================
echo
echo "ROW: operator-wait — the model asks through work ask, the ask hold releases it, no attempt charged"
# ==========================================================================================
# THE DEFECT THIS GUARDS (sp-v62vn follow-up). Teardown released a session as operator-wait
# only on the `<bead>.operator-wait` marker `mail` wrote, stamped with SESSION_EPOCH. Every
# session is restricted now — its PATH holds only `work`, no `mail` — so no session could be
# released operator-wait at all, and a question to the operator was charged as an attempt.
# The model asks through `work ask`; the broker delivers the question to the operator and
# places an `ask` hold on the bound bead's lifecycle row, and teardown reads that hold.
#
# A REAL LIFECYCLE SERVICE for this row only (testlib/lc-fixture.sh + spira-lc serve, as
# test-submitted-lands.sh): the hold is the broker's own Hold event on a real row, so the
# lc_aeon_mirror stand-in is taken off PATH and the aeon claims, renews, reads and releases
# through the same service. Last in the file so no earlier row sees SPIRA_LC_*.
. "$HERE/testlib/lc-fixture.sh"
OW_SERVE_PID=""
trap '[ -n "$OW_SERVE_PID" ] && kill "$OW_SERVE_PID" >/dev/null 2>&1; lcfix_down; fa_teardown' EXIT INT TERM
PATH="${PATH//"$FA_TMP/lcm:"/}"; export PATH
lcfix_up || bail "could not build a lifecycle fixture"
OW_SOCK="$FA_TMP/lc.sock"
tl_config SPIRA_LC_SOCKET="$OW_SOCK"
spira-lc serve "$OW_SOCK" > "$FA_TMP/serve.log" 2>&1 &
OW_SERVE_PID=$!
for _ in $(seq 1 50); do [ -S "$OW_SOCK" ] && break; sleep 0.1; done
[ -S "$OW_SOCK" ] || bail "spira-lc serve never opened its socket: $(cat "$FA_TMP/serve.log")"
aeon_fixture_agent "$FA_BIN/claude"   # re-capture PATH: the model reaches `work`, not the stand-in

cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"work ask"}}]}}\n'
work ask --subject "fixture question" --kind question --default "proceed without waiting" > "$TMP/ask.out" 2>&1
printf '%s' "$?" > "$TMP/ask.rc"
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$FA_BIN/claude"
fa_reset; fa_seed sp-ow2   # sp-<alnum>: `work` refuses a binding with a hyphen in the id
lcfix_seed sp-ow2 READY || bail "could not seed sp-ow2's lifecycle row"
fa_run_aeon >/dev/null
is   "the model's work ask was applied by the broker" "0" "$(cat "$FA_TMP/ask.rc" 2>/dev/null || echo missing)"
[ "$(cat "$FA_TMP/ask.rc" 2>/dev/null)" = 0 ] || sed 's/^/# /' "$FA_TMP/ask.out" 2>/dev/null
ow_row() {   # ow_row <id> -> "<STATE> <holds,...|->"
    spira-lc show "$1" 2>/dev/null | python3 -c '
import sys, json
b = json.load(sys.stdin)["bead"]
h = b.get("holds") or []
h = json.loads(h) if isinstance(h, str) else h   # the row stores holds as a JSON text column
print(b["state"], ",".join(h) or "-")' 2>/dev/null
}
hdr_of() {   # hdr_of <file> <header> -> the header's value
    awk -v h="$2" '/^[[:space:]]*$/ { exit } tolower($0) ~ "^" tolower(h) ":" { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }' "$1"
}
is   "the row: released to READY, still carrying the ask hold until the operator answers" "READY ask" "$(ow_row sp-ow2)"
notes_ow="$(fa_notes sp-ow2)"
want   "note says the session asked the operator" "asked the operator" "$notes_ow"
want   "note says No attempt charged"             "No attempt charged" "$notes_ow"
nowant "note does not say Unlanded"               "Unlanded"           "$notes_ow"
want   "ledger says operator-wait" "operator-wait" "$(fa_ledger_line sp-ow2)"
is     "no attempt charged" "0" "$(count_of sp-ow2)"
# THE BROKER'S ASK IS CLASSIFIED (sp-v62vn follow-up): the question declares no escalation
# class, so mail routes it to the concierge, never the operator. Before, the broker called
# mail without BEAD_ID, mail's class check never ran, and this landed in operator/.
ow_mail="$(grep -l '^Subject: fixture question' "$SPIRA_MAIL"/concierge/new/* "$SPIRA_MAIL"/concierge/cur/* 2>/dev/null | head -1)"
is   "the unclassed question reached the concierge's mailbox" "yes" "$([ -n "$ow_mail" ] && echo yes || echo no)"
is   "and not the operator's" "" "$(grep -l '^Subject: fixture question' "$SPIRA_MAIL"/operator/new/* "$SPIRA_MAIL"/operator/cur/* 2>/dev/null)"
is   "the ask mail names the work bead it is about" "sp-ow2" "$([ -n "$ow_mail" ] && hdr_of "$ow_mail" X-Spira-Work-Bead)"
is   "and no marker file was involved" "no" "$([ -e "$SPIRA_RUN/sp-ow2.operator-wait" ] && echo yes || echo no)"

# ==========================================================================================
echo
echo "ROW: the answer lifts the ask hold — a reply to the ask mail through mail sendmail"
# ==========================================================================================
# THE DEFECT THIS GUARDS (sp-v62vn follow-up): only `spira-lc reply` / `withdraw-ask` lift an
# ask hold, and nothing that carries an answer emitted one — so a bead whose question was
# answered stayed held, unclaimable, forever. The concierge answers the routed question the
# way any answer arrives: a reply with In-Reply-To, through `mail sendmail`.
ow_qid="$([ -n "$ow_mail" ] && hdr_of "$ow_mail" Message-ID)"
printf 'From: Concierge <concierge@spira>\nSubject: Re: fixture question\nIn-Reply-To: %s\nMessage-ID: <ow-answer-1@spira>\n\nProceed without waiting.\n' "$ow_qid" \
    | mail sendmail > "$FA_TMP/sendmail.out" 2>&1
is   "the reply was delivered" "0" "$?"
is   "the row: READY with no hold — claimable again" "READY -" "$(ow_row sp-ow2)"
if spira-lc held sp-ow2 ask >/dev/null 2>&1; then bad "spira-lc held sp-ow2 ask: no longer held" "still held"; else ok "spira-lc held sp-ow2 ask: no longer held"; fi
want "the answer is on the work bead for the next session" "Proceed without waiting." "$(fa_notes sp-ow2)"
is   "the routed question's reply closed no bead (the work bead is still open in bd)" "open" "$(fa_field sp-ow2 status)"

# ==========================================================================================
echo
echo "ROW: a question closed without an answer withdraws the ask — resolve on the ask bead"
# ==========================================================================================
# A CLASSED question goes to the operator and files a tracking (ask) bead naming the work
# bead (work-bead:<id>). The concierge resolving that ask bead itself — moot, or established
# without him — is the question closed with no answer: withdraw-ask, not reply.
printf '## Question\nMay the fixture rotate its deploy key?\n\n## Default\nno\n\n## Class basis\nthe deploy key is a credential only the operator holds\n' \
    | SPIRA_WORK_BEAD_ID=sp-ow2 SPIRA_FAYTH=builder work ask --subject "rotate the fixture deploy key" --kind question --default no --class permissions --body-file - \
    > "$FA_TMP/ask2.out" 2>&1
is   "a classed work ask is applied" "0" "$?"
is   "the row is held again" "READY ask" "$(ow_row sp-ow2)"
ow_mail2="$(grep -l '^Subject: rotate the fixture deploy key' "$SPIRA_MAIL"/operator/new/* "$SPIRA_MAIL"/operator/cur/* 2>/dev/null | head -1)"
is   "the classed question reached the operator" "yes" "$([ -n "$ow_mail2" ] && echo yes || echo no)"
ow_ask="$([ -n "$ow_mail2" ] && hdr_of "$ow_mail2" X-Spira-Bead)"
want "its ask bead carries the work bead's label" "work-bead:sp-ow2" "$(fa_labels "$ow_ask")"
tl_config COCKPIT_DB="$SPIRA_DB"
resolve "$ow_ask" "moot: the fixture's key never needed rotating" > "$FA_TMP/resolve.out" 2>&1
is   "resolve closed the ask bead" "0" "$?"
want "resolve reports the ask resolved (close itself lifts the hold)" "resolved" "$(cat "$FA_TMP/resolve.out")"
is   "the row: READY with no hold" "READY -" "$(ow_row sp-ow2)"
want "and the lift was a withdraw, not a reply" "AskWithdrawn" "$(spira-lc history sp-ow2 2>/dev/null)"

tl_summary
