#!/usr/bin/env bash
#
# test-incident-decisions.sh — incident.sh's pure and stub-bd decisions
# (UC-ops-detection-remediation-01, -04, -05, -06).
#
#   ./test-incident-decisions.sh
#
# WHAT THIS SUITE HOLDS. Four properties the plan demotes out of test-incident.sh's real-bd
# fixture, because none of them needs a database at all to be true:
#
#   1. THE DEDUP DECISION (UC-01) — incident-dedup-decision.py, extracted from
#      incident.sh's _dedup_incident, is a pure function of a `bd list --json` array and a
#      target ref. Fed canned JSON directly; no bd, no Dolt.
#   2. LABEL-KEYED DEDUP IS O(1) (UC-04) — with N unrelated open beads planted, a second
#      filing of one ref must call `bd show` zero times. incident-stub-bd.py records every
#      call it receives, so the property is checked by grepping its log, not by trusting
#      the code's own claim.
#   3. THE REPO/MAIL DECISION (UC-05) — a declared SPIRA_INCIDENT_REPO is stamped via
#      set-state; an undeclared one gets needs-repo-triage and exactly one mail.
#   4. THE NOTE-SIZE DECISION (UC-06) — _recur_note_body is a pure function of the previous
#      payload's hash label and the current payload: empty when unchanged and not reopened,
#      the payload otherwise. Sourced directly and called with no bead, no bd, no file_one.
#
# STUB BD, NOT A FIXTURE DATABASE (law-gates-run-in-a-clean-environment). incident-stub-bd.py
# is a small STATEFUL fake — a second filing must see the first filing's write — not a
# canned-response mock; see its own header for why that distinction matters here.
#
# tier: T1
# covers: spira/incident.sh spira/incident-dedup-decision.py spira/incident-stub-bd.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-incident-decisions.sh"

DEDUP_PY="$HERE/incident-dedup-decision.py"
STUB_BD="$HERE/incident-stub-bd.py"
INC="$HERE/incident.sh"

decide() {  # decide <status> <fallback> <ref> <json-on-stdin>
    python3 "$DEDUP_PY" "$1" "$2" "$3"
}

# ======================================================================================
echo
echo "1. the dedup decision (incident-dedup-decision.py) — pure over a bd list --json array:"
# ======================================================================================
# POSITIVE CONTROL FIRST. Before trusting any "no match" below, a matching bead in the
# array must be found (law-absence-needs-a-positive-control).
OPEN_JSON='[{"id":"sp-a","external_ref":"incident:x","status":"open","labels":["ref:aaaa"]}]'
is "positive control: an open match is found" "open sp-a" \
    "$(printf '%s' "$OPEN_JSON" | decide open 0 "incident:x")"

is "wrong ref: no match" "" \
    "$(printf '%s' "$OPEN_JSON" | decide open 0 "incident:not-there")"

CLOSED_STATUS='[{"id":"sp-a","external_ref":"incident:x","status":"closed","labels":[]}]'
is "an open-status query ignores a closed bead" "" \
    "$(printf '%s' "$CLOSED_STATUS" | decide open 0 "incident:x")"

is "a closed-status query finds the same bead, with its closed_at" "closed sp-a 2026-01-02" \
    "$(printf '%s' '[{"id":"sp-a","external_ref":"incident:x","status":"closed","closed_at":"2026-01-02","labels":[]}]' \
        | decide closed 0 "incident:x")"

# FALLBACK MODE (sub-path B): skips any bead already carrying a ref: label — that bead was
# already checked on the label-keyed pass, so re-matching it here would just be slower, not
# wrong, but the plan's own invariant is that the fallback ONLY covers unlabeled beads.
is "fallback mode skips a bead that already carries a ref: label" "" \
    "$(printf '%s' "$OPEN_JSON" | decide open 1 "incident:x")"

UNLABELED='[{"id":"sp-b","external_ref":"incident:y","status":"open","labels":[]}]'
is "fallback mode still finds an unlabeled bead" "open sp-b" \
    "$(printf '%s' "$UNLABELED" | decide open 1 "incident:y")"

is "malformed JSON on stdin yields no match, not a crash" "" \
    "$(printf 'not json' | decide open 0 "incident:x")"

# ======================================================================================
echo
echo "2. label-keyed dedup issues 0 bd show calls regardless of open-incident queue depth (UC-04, sp-80br6):"
# ======================================================================================
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export STUB_BD_STATE="$TMP/state.json" STUB_BD_LOG="$TMP/bd.log"
mkdir -p "$TMP/home" "$TMP/run"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/home/mail.sh"; chmod +x "$TMP/home/mail.sh"

inc() {
    env -i HOME="$HOME" PATH="$PATH" \
        SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_DB="fakedb" \
        SPIRA_RUN="$TMP/run" SPIRA_HOME="$TMP/home" \
        SPIRA_INCIDENT_LOCK="$TMP/run/incident.lock" \
        SPIRA_INCIDENT_REPO= \
        "$@" bash "$INC" file "decisions test" -
}
seed_bead() {  # seed_bead <id> <ref> — a noise bead with no ref: label, as older code left
    printf '{"id":"%s","external_ref":"%s","status":"open","labels":["spira","partition:incident"]}' \
        "$1" "$2" | env STUB_BD_STATE="$STUB_BD_STATE" python3 "$STUB_BD" seed
}

# N_BENCH open beads with distinct refs and no ref: label — the shape the O(N) dedup used
# to scan with one `bd show` per candidate.
N_BENCH=5
for _i in $(seq 1 "$N_BENCH"); do
    seed_bead "sp-noise-$_i" "incident:noise-ref-$_i"
done

: > "$STUB_BD_LOG"
printf 'seed\n' | inc SPIRA_INCIDENT_REF="incident:bench-target" >/dev/null
printf 'recur\n' | inc SPIRA_INCIDENT_REF="incident:bench-target" >/dev/null
show_calls="$(grep -c '^show ' "$STUB_BD_LOG" 2>/dev/null || true)"
is "label-keyed dedup issues 0 bd show calls with $N_BENCH open candidates" "0" "${show_calls:-0}"

# ======================================================================================
echo
echo "3. the repo/mail decision (UC-05):"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
: > "$TMP/mail.log"
cat > "$TMP/home/mail.sh" <<'M'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
printf '%s\n' "$*" >> "$MAIL_LOG"
cat >> "$MAIL_LOG"
M
chmod +x "$TMP/home/mail.sh"

inc_env() {   # inc_env VAR=val [VAR=val ...] -- assignments only; ref comes from one of them
    env -i HOME="$HOME" PATH="$PATH" \
        SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
        MAIL_LOG="$TMP/mail.log" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_DB="fakedb" \
        SPIRA_RUN="$TMP/run" SPIRA_HOME="$TMP/home" \
        SPIRA_INCIDENT_LOCK="$TMP/run/incident.lock" \
        "$@" bash "$INC" file "decisions repo test" - >/dev/null 2>&1
}

# POSITIVE CONTROL: a declared repo is stamped.
printf 'p1' | inc_env SPIRA_INCIDENT_REPO=brain SPIRA_INCIDENT_REF="incident:repo-declared"
bid_repo="$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
for b in d["beads"].values():
    if b.get("external_ref") == "incident:repo-declared": print(b["id"]); break
')"
[ -n "$bid_repo" ] && ok "repo test: bead was created" || bad "repo test: bead was created" "none found"
is "SPIRA_INCIDENT_REPO=brain is written via set-state" "brain" \
    "$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(d["beads"].get("'"$bid_repo"'", {}).get("repo", ""))
')"

# An undeclared repo gets needs-repo-triage and exactly one mail.
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"; : > "$TMP/mail.log"
printf 'p2' | inc_env SPIRA_INCIDENT_REPO= SPIRA_INCIDENT_REF="incident:repo-undeclared"
bid_norepo="$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
for b in d["beads"].values():
    if b.get("external_ref") == "incident:repo-undeclared": print(b["id"]); break
')"
labels_norepo="$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(" ".join(d["beads"].get("'"$bid_norepo"'", {}).get("labels", [])))
')"
want "an undeclared repo is marked needs-repo-triage" "needs-repo-triage" "$labels_norepo"
case "$labels_norepo" in
    *"repo:"*) bad "no-repo: must carry no repo: label" "got: $labels_norepo" ;;
    *)         ok "no-repo: no repo: label present when undeclared" ;;
esac
n_mails="$(grep -c '^send ' "$TMP/mail.log" 2>/dev/null || true)"
is "exactly one mail sent for the undeclared repo" "1" "${n_mails:-0}"
want "the mail subject leads with provenance, not the ref slug" " on " "$(cat "$TMP/mail.log")"

# ======================================================================================
echo
echo "4. the note-size decision (_recur_note_body) — pure function of (prev hash, payload) (UC-06):"
# ======================================================================================
# Sourcing incident.sh runs its top-level mkdir; pin SPIRA_RUN to scratch first so nothing
# real is touched (law-gates-run-in-a-clean-environment). SPIRA_INCIDENT_CAUSE is set only
# to keep incident.sh's own "cause not set" warning, unrelated to what this section tests,
# out of the suite's output.
export SPIRA_RUN="$TMP/source-run" SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_INCIDENT_CAUSE=probe
# shellcheck disable=SC1090
. "$INC"
PF="$TMP/payload"
printf 'x%.0s' $(seq 1 500) > "$PF"
PH="payload-hash:$(sha256sum "$PF" | cut -c1-16)"

is "unchanged payload, not reopened: no growth (empty body)" "" \
    "$(_recur_note_body "$PH" "$PF" 0)"
is "unchanged payload, reopened: full payload despite the match" "yes" \
    "$([ -n "$(_recur_note_body "$PH" "$PF" 1)" ] && echo yes || echo no)"

printf 'y%.0s' $(seq 1 500) > "$PF"
is "changed payload: full body, containing the new content" "yes" \
    "$(case "$(_recur_note_body "$PH" "$PF" 0)" in *yyy*) echo yes;; *) echo no;; esac)"
is "no previous hash label (first recurrence): full body" "yes" \
    "$([ -n "$(_recur_note_body "" "$PF" 0)" ] && echo yes || echo no)"

BIG="$TMP/big-payload"
python3 -c "print('z' * 5000, end='')" > "$BIG"
is "the body is capped at 2000 bytes of payload" "yes" \
    "$([ "$(_recur_note_body "" "$BIG" 0 | wc -c)" -le 2001 ] && echo yes || echo no)"

# ======================================================================================
echo
echo "5. outside-lookback dedup — a bead closed before SPIRA_INCIDENT_DEDUP_LOOKBACK yields a new bead, not a reopen (gap G3):"
# ======================================================================================
# THE GAP. Every existing close-then-refile test only closes a bead moments before refiling
# it — well within the default 7-day window. Nothing checks the other side: a bead closed
# long enough ago that reopening it would be wrong. incident-stub-bd.py's seed command
# plants a bead with an arbitrary closed_at directly, which embedded bd (this suite's
# alternative) cannot do at all — bd sql is refused in embedded mode, and this property does
# not need a real fixture to be true (it is bd's --closed-after filter plus incident.sh's own
# date arithmetic, both exercised end-to-end here through the stub, which implements
# --closed-after with the same semantics: closed_at compared lexicographically against the
# boundary date).
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
_old_closed="$(date -u -d '-30 days' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-30d +%Y-%m-%dT%H:%M:%SZ)"
printf '{"id":"sp-old","external_ref":"incident:outside-lookback","status":"closed","closed_at":"%s","labels":["spira","partition:incident"]}' \
    "$_old_closed" | env STUB_BD_STATE="$STUB_BD_STATE" python3 "$STUB_BD" seed

printf 'fresh occurrence' | inc SPIRA_INCIDENT_REF="incident:outside-lookback" >/dev/null

_ob_count="$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(sum(1 for b in d["beads"].values() if b.get("external_ref") == "incident:outside-lookback"))
')"
is "a closed_at outside the lookback yields a second, distinct bead" "2" "$_ob_count"

is "the old bead is left exactly as it was (closed, untouched)" "closed" \
    "$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(d["beads"].get("sp-old", {}).get("status", "?"))
')"

echo
tl_summary
