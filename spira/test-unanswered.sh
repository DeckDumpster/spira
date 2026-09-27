#!/usr/bin/env bash
# test-unanswered.sh — unanswered.py: whose-turn detection, tie-breaking, and the fan-out bound.
#
# THE DEFECT (sp-xsl8i). cockpit/unanswered.sh fanned out one `bd comments <id> --json`
# subprocess per candidate bead — 14.6s against 831 production attention beads. unanswered.py
# reads every comment on every candidate in ONE `bd sql` query instead; embedded bd cannot run
# `bd sql` at all (testdb.sh's own header), so — same as test-answers.sh, which stubs the
# equally-unsupported `history --events` — this suite stubs `bd` rather than using testdb.sh.
#
# tier: T1
# covers: spira/unanswered.py cockpit/unanswered.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-unanswered.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

ASK_LABEL="needs-attention"   # pinned to a non-default (law-gates-run-in-a-clean-environment)
HUMAN="testop"

# THE STUB. unanswered.py's comment_threads() issues one `bd sql --json 'SELECT ... FROM
# comments WHERE issue_id IN (...) ...'` query. This stub answers it from a caller-supplied
# fixture file, keyed by id, rather than a fixed reply — the whose-turn logic needs threads
# that differ per bead (operator last vs. someone else last vs. a same-second tie).
CALLS="$TMP/calls.log"
FIXTURE_COMMENTS="$TMP/comments.json"   # {id: [ {author, text, created_at}, ... ], ...}
STUB_BD="$TMP/bd"
cat > "$STUB_BD" <<STUBEOF
#!/usr/bin/env bash
echo "\$*" >> "$CALLS"
for arg; do
    case "\$arg" in
        *"FROM comments"*)
            python3 - "$FIXTURE_COMMENTS" "\$arg" <<'PY'
import json, re, sys
fixture = json.load(open(sys.argv[1]))
ids = re.findall(r"'([^']+)'", sys.argv[2])
rows = []
n = 0
for ident in ids:
    for c in fixture.get(ident, []):
        n += 1
        rows.append({"issue_id": ident, "id": "c-%d" % n, "author": c["author"],
                     "text": c["text"], "created_at": c["created_at"]})
print(json.dumps(rows))
PY
            exit 0
            ;;
    esac
done
echo '[]'
STUBEOF
chmod +x "$STUB_BD"

run_unanswered() {   # run_unanswered <fixture-issues-json> -> stdout+stderr
    printf '%s' "$1" | python3 "$HERE/unanswered.py" \
        "bd=$STUB_BD" "db=unused" "ask_label=$ASK_LABEL" "human=$HUMAN" 2>&1
}

issue_row() {   # issue_row <id> <labels-csv> <comment_count>
    printf '{"id":"%s","title":"t %s","labels":[%s],"comment_count":%s}' \
        "$1" "$1" "$(printf '"%s"' "$2" | sed 's/,/","/g')" "$3"
}

# ==========================================================================
# UC-1: an open obligation — his last word on the thread is the newest — is
# reported; a thread where somebody else answered him is not.
# ==========================================================================
echo
echo "UC-1: open obligation reported, answered thread silent (positive + negative control)"

WAITING="sp-tu-waiting"
ANSWERED_ID="sp-tu-answered"
cat > "$FIXTURE_COMMENTS" <<JSON
{
  "$WAITING": [
    {"author": "testop", "text": "please look at this", "created_at": "2026-09-20T10:00:00Z"}
  ],
  "$ANSWERED_ID": [
    {"author": "testop", "text": "please look at this too", "created_at": "2026-09-20T09:00:00Z"},
    {"author": "aeon", "text": "done", "created_at": "2026-09-20T09:05:00Z"}
  ]
}
JSON
FIXTURE="[$(issue_row "$WAITING" "$ASK_LABEL,overseer" 1),$(issue_row "$ANSWERED_ID" "$ASK_LABEL,overseer" 2)]"

out="$(run_unanswered "$FIXTURE")"
want   "an open obligation is reported"        "$WAITING"     "$out"
nowant "an answered thread is not reported"    "$ANSWERED_ID"  "$out"

# ==========================================================================
# UC-2: a same-second tie between the operator and someone else resolves
# toward "he is owed a reply" — a thread wrongly listed costs a glance, a
# thread wrongly dropped is the silence this file exists to end.
# ==========================================================================
echo
echo "UC-2: a same-second tie resolves toward reporting the thread"

TIE="sp-tu-tie"
cat > "$FIXTURE_COMMENTS" <<JSON
{
  "$TIE": [
    {"author": "aeon", "text": "status update", "created_at": "2026-09-20T11:00:00Z"},
    {"author": "testop", "text": "ok, and also this", "created_at": "2026-09-20T11:00:00Z"}
  ]
}
JSON
out="$(run_unanswered "[$(issue_row "$TIE" "insight" 2)]")"
want "a same-second tie with the operator in it is reported" "$TIE" "$out"

# ==========================================================================
# UC-3 (sp-xsl8i) — the fan-out is gone: hundreds of candidates cost a FIXED
# number of bd calls, not one bd comments call per bead. Count calls, not
# wall time, per the bead's own done-when.
# ==========================================================================
echo
echo "UC-3: hundreds of candidates cost a bounded number of bd calls, not one per bead"

N=300
rows=""
comments_json="{"
for i in $(seq 1 "$N"); do
    id="sp-tu-bulk$i"
    rows="$rows$(issue_row "$id" "insight" 1),"
    comments_json="$comments_json\"$id\":[{\"author\":\"testop\",\"text\":\"go\",\"created_at\":\"2026-09-20T12:00:00Z\"}],"
done
comments_json="${comments_json%,}}"
printf '%s' "$comments_json" > "$FIXTURE_COMMENTS"
BIG_FIXTURE="[${rows%,}]"

: > "$CALLS"
out_big="$(run_unanswered "$BIG_FIXTURE")"

ncalls=$(grep -c . "$CALLS" 2>/dev/null || echo 0)
[ "${ncalls:-0}" -le 2 ] \
    && ok "unanswered.py issues a bounded number of bd calls for $N candidates (got $ncalls)" \
    || bad "unanswered.py issues a bounded number of bd calls for $N candidates" \
           "got $ncalls calls, wanted <= 2"

want "positive control: a bulk candidate is still reported" "sp-tu-bulk1" "$out_big"

tl_summary
