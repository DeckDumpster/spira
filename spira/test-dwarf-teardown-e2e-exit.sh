#!/usr/bin/env bash
#
# test-dwarf-teardown-e2e-exit.sh — the operator-wait rows, which need the real work broker and
# mail. The charge, pre-session, decision-blocked, yield-headless, exit-code and ledger-segment
# rows are whole-run tests in dwarf/src/tests.rs (`cargo test -p dwarf`).
#
# SERVER-MODE bd: attempts_of/requeues_of read the events table via `bd sql`, which embedded
# mode refuses (testdb.sh).
#
# defect: sp-egge2 sp-ne93n sp-l7f5 sp-214 sp-ywlti sp-iu10 sp-2a4hd sp-wnsks
# tier: T3
# covers: dwarf/src/* spira/lib.sh mail/src/* spira-lc/src/work.rs work/* cockpit/ops/src/resolve.rs cockpit/ops/src/resolve_main.rs
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90   # setup builds real git/dolt fixtures; a loaded VM doubles it

# testdb-mode: server — attempts_of/requeues_of read the events table via bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/full-dwarf-fixture.sh"

fa_setup teardownexit || exit 77
trap 'fa_teardown' EXIT INT TERM
. "$HERE/testlib/teardown-e2e.sh"

echo "test-dwarf-teardown-e2e-exit.sh"

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
# lc_dwarf_mirror stand-in is taken off PATH and the dwarf claims, renews, reads and releases
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
lc_content_shim "$FA_TMP/lcshim"; export PATH="$FA_TMP/lcshim:$PATH"
for _ in $(seq 1 50); do [ -S "$OW_SOCK" ] && break; sleep 0.1; done
[ -S "$OW_SOCK" ] || bail "spira-lc serve never opened its socket: $(cat "$FA_TMP/serve.log")"
dwarf_fixture_agent "$FA_BIN/claude"   # re-capture PATH: the model reaches `work`, not the stand-in

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
fa_run_dwarf >/dev/null
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
# way any answer arrives: a reply with In-Reply-To, through `mail sendmail`.  sendmail only delivers; `mail reconcile` applies.
ow_qid="$([ -n "$ow_mail" ] && hdr_of "$ow_mail" Message-ID)"
printf 'From: Concierge <concierge@spira>\nSubject: Re: fixture question\nIn-Reply-To: %s\nMessage-ID: <ow-answer-1@spira>\n\nProceed without waiting.\n' "$ow_qid" \
    | mail sendmail > "$FA_TMP/sendmail.out" 2>&1
is   "the reply was delivered" "0" "$?"
mail reconcile > "$FA_TMP/reconcile.out" 2>&1
is   "one reconciler pass applied the reply" "0" "$?"
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
    | SPIRA_WORK_BEAD_ID=sp-ow2 SPIRA_RUNE=builder work ask --subject "rotate the fixture deploy key" --kind question --default no --class permissions --body-file - \
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
