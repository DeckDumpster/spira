#!/usr/bin/env bash
#
# test-amend-live.sh — acknowledged amendments: the PreToolUse hook blocks until `work ack`,
# bead.sh amend --ack records accept/decline, --enforce slays and requeues with the body merged.
#
# tier: T2
# covers: spira/bead.sh spira/hooks/dwarf-amend-block.sh bead/src/main.rs spira-config/src/amendment.rs spira-lc/src/work.rs work/src/lib.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"

echo "test-amend-live.sh"

testdb_require test-amend-live
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up amend_live || { printf 'test-amend-live: could not build fixture database\n' >&2; exit 1; }

testdb_seed <<'JSONL'
{"id":"sp-aml-a","title":"live target a","description":"ORIGINAL-A","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-aml-b","title":"live target b","description":"ORIGINAL-B","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

MAIL="$TMP/mail"
mkdir -p "$MAIL/dwarf-sp-aml-a/new" "$MAIL/dwarf-sp-aml-b/new" "$TMP/bin" "$TMP/run"
cat > "$TMP/bin/slay" <<'S'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SLAY_LOG"
S
cat > "$TMP/bin/mail" <<'S'
#!/usr/bin/env bash
cat >/dev/null
S
chmod +x "$TMP/bin/slay" "$TMP/bin/mail"

record() { # id bead state deadline text
    mkdir -p "$MAIL/dwarf-$2/amend"
    printf '{"id":"%s","bead":"%s","amender":"concierge","text":"%s","created":1000,"deadline":%s,"state":"%s","reason":""}' \
        "$1" "$2" "$5" "$4" "$3" > "$MAIL/dwarf-$2/amend/$1.json"
}

run_bead() {
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_RUN="$TMP/run" SPIRA_MAIL="$MAIL"
    OUT="$(SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" SPIRA_NOW="${NOW:-5000}" SLAY_LOG="$TMP/slay.log" PATH="$TMP/bin:$PATH" bead.sh amend "$@" 2>&1)"
    RC=$?
}
desc_of() {
    timeout 5 "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("description",""))'
}
hook() {
    SPIRA_MAIL="$MAIL" BEAD_ID="$1" "$HERE/hooks/dwarf-amend-block.sh" <<<"$2"
}

echo
echo "T1: the hook blocks every tool call while an amendment is pending, and names it"
record am-aa sp-aml-a pending 9000 "the fix is already built at 709cfac"
want "blocks a Bash call" '"decision": "block"' "$(hook sp-aml-a '{"tool_name":"Bash","tool_input":{"command":"ls"}}')"
want "names the amendment" "am-aa" "$(hook sp-aml-a '{"tool_name":"Read","tool_input":{"file_path":"/x"}}')"
want "shows its text" "709cfac" "$(hook sp-aml-a '{"tool_name":"Read","tool_input":{"file_path":"/x"}}')"
nowant "lets the ack through" "block" "$(hook sp-aml-a '{"tool_name":"Bash","tool_input":{"command":"work ack am-aa --accept"}}')"
nowant "lets a quoted decline through" "block" "$(hook sp-aml-a '{"tool_name":"Bash","tool_input":{"command":"work ack am-aa --decline \"no; wrong repo\""}}')"
want "refuses an ack chained to another command" "block" "$(hook sp-aml-a '{"tool_name":"Bash","tool_input":{"command":"work ack am-aa --accept; ls"}}')"
want "refuses an ack of another id" "block" "$(hook sp-aml-a '{"tool_name":"Bash","tool_input":{"command":"work ack am-zz --accept"}}')"
nowant "another bead is untouched" "block" "$(hook sp-aml-b '{"tool_name":"Bash","tool_input":{"command":"ls"}}')"

echo
echo "T2: accept records, merges the text into the description, unblocks"
run_bead sp-aml-a --ack am-aa --accept
wantrc "accept ok" "0" "$RC"
want "state is accepted" '"state":"accepted"' "$(cat "$MAIL/dwarf-sp-aml-a/amend/am-aa.json")"
want "text merged into the description" "709cfac" "$(desc_of sp-aml-a)"
want "original kept" "ORIGINAL-A" "$(desc_of sp-aml-a)"
nowant "hook no longer blocks" "block" "$(hook sp-aml-a '{"tool_name":"Bash","tool_input":{"command":"ls"}}')"
run_bead sp-aml-a --ack am-aa --accept
wantrc "a second answer is refused" "1" "$RC"

echo
echo "T3: decline records the reason, leaves the description alone, and needs a reason"
record am-bb sp-aml-b pending 9000 "switch approach"
run_bead sp-aml-b --ack am-bb --decline
wantrc "decline without a reason is a usage error" "2" "$RC"
run_bead sp-aml-b --ack am-bb --decline "pinned dependency would break"
wantrc "decline ok" "0" "$RC"
want "reason recorded" "pinned dependency would break" "$(cat "$MAIL/dwarf-sp-aml-b/amend/am-bb.json")"
nowant "description untouched" "switch approach" "$(desc_of sp-aml-b)"
nowant "a declined amendment no longer blocks the dwarf" "block" "$(hook sp-aml-b '{"tool_name":"Bash","tool_input":{"command":"ls"}}')"

echo
echo "T4: --enforce refuses inside the window and after an accept; slays and requeues after a decline or the deadline"
record am-cc sp-aml-a pending 9000 "second amendment"
NOW=5000 run_bead --enforce am-cc
wantrc "inside the window" "1" "$RC"
want "says why" "ack window" "$OUT"
NOW=5000 run_bead --enforce am-aa
wantrc "accepted amendment" "1" "$RC"
[ ! -s "$TMP/slay.log" ] && ok "nothing slain by a refused enforce" || bad "nothing slain by a refused enforce"
NOW=9001 run_bead --enforce am-cc
wantrc "past the deadline" "0" "$RC"
want "slays the holder, reopening the bead" "--bead sp-aml-a" "$(cat "$TMP/slay.log")"
want "reopens" "--reopen" "$(cat "$TMP/slay.log")"
want "amendment merged into the body" "second amendment" "$(desc_of sp-aml-a)"
want "recorded as enforced" '"state":"enforced"' "$(cat "$MAIL/dwarf-sp-aml-a/amend/am-cc.json")"
NOW=9001 run_bead --enforce am-cc
wantrc "enforcing twice is refused" "1" "$RC"
: > "$TMP/slay.log"
NOW=5000 run_bead --enforce am-bb
wantrc "a decline is enforceable at once" "0" "$RC"
want "slays on a decline" "--bead sp-aml-b" "$(cat "$TMP/slay.log")"

echo
echo "T5: --live with no live dwarf falls back to the advisory amend and records nothing"
run_bead sp-aml-b --live --note "advisory only"
wantrc "advisory ok" "0" "$RC"
want "says so" "no live dwarf" "$OUT"
ls "$MAIL/dwarf-sp-aml-b/amend" | grep -c . | grep -q '^1$' && ok "no new record" || bad "no new record"

echo
echo "T6: --live refuses what it cannot hold for acknowledgement"
run_bead sp-aml-b --live --priority 1 --note x
wantrc "priority with --live" "2" "$RC"
run_bead sp-aml-b --live
wantrc "no text" "2" "$RC"
run_bead sp-aml-b --ack-within 5m --note x
wantrc "--ack-within without --live" "2" "$RC"

tl_summary
