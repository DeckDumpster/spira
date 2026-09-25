#!/usr/bin/env bash
#
# test-park-unmapped.sh — park_unmapped (lib.sh) labels a bead ask+overseer, notes why, and
#   releases the aeon's own claim, in that order, when aeon.sh's repo-map resolution fails
#   (dispatch test plan G13; the real replacement for the deleted test-unmapped-repo-park.sh,
#   which exercised only bd's own --exclude-label and never aeon.sh).
#
#   ./test-park-unmapped.sh
#
# THE ORDER MATTERS. Labels must land before the claim is released: the moment
# release_own_claim clears the assignee and reopens the bead, `bd ready` can offer it to the
# next summon — and if the ask/overseer labels are not on it yet, a second aeon claims the
# same unmapped-repo bead in the gap. A stubbed bd records argv in call order so this suite
# can assert on it directly, never on wall-clock timing.
#
# defect: sp-4l0d, sp-nlhy, sp-foi7
# covers: spira/lib.sh spira/aeon.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
has() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
log() { :; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-park-unmapped.sh"

mkdir -p "$TMP/bin"
CALLS="$TMP/calls"
cat > "$TMP/bin/bd" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS"
exit 0
STUB
chmod +x "$TMP/bin/bd"

# ======================================================================================
echo
echo "case 0 — positive control: park_unmapped actually calls bd (a no-op guard reads as correct otherwise):"
# ======================================================================================
: > "$CALLS"
export CALLS
# A non-default ask label and repo-map path, pinned here rather than inherited from this
# host's own conf.sh — a fixture that asserted against the shipped default would pass just
# as well if park_unmapped had the literal "needs-operator" written into it.
export PATH="$TMP/bin:$PATH" SPIRA_BD=bd SPIRA_DB=fixture BEADS_ACTOR=aeon-tester \
    SPIRA_ASK_LABEL=needs-fixture-operator SPIRA_REPO_MAP="$TMP/fixture-repo-map"
park_unmapped sp-typo1 typo-repo
n_calls="$(grep -c . "$CALLS" || true)"
[ "${n_calls:-0}" -gt 0 ] && ok "positive control: bd was called at all ($n_calls call(s))" \
    || bad "positive control: bd was called at all" "no bd invocation recorded"

echo
echo "case 1 — both labels are applied, in order, before the claim is released:"
# ======================================================================================
calls="$(cat "$CALLS")"
has "ask label ($SPIRA_ASK_LABEL) is applied" "label add sp-typo1 $SPIRA_ASK_LABEL" "$calls"
has "overseer label is applied"               "label add sp-typo1 overseer"        "$calls"
has "a note is left on the bead"              "note sp-typo1"                      "$calls"
has "the claim is released (status open, no assignee)" \
    "update sp-typo1 --status open --assignee" "$calls"

ask_line="$(grep -n "label add sp-typo1 $SPIRA_ASK_LABEL" "$CALLS" | head -1 | cut -d: -f1)"
overseer_line="$(grep -n "label add sp-typo1 overseer" "$CALLS" | head -1 | cut -d: -f1)"
note_line="$(grep -n "note sp-typo1" "$CALLS" | head -1 | cut -d: -f1)"
release_line="$(grep -n "update sp-typo1 --status open --assignee" "$CALLS" | head -1 | cut -d: -f1)"

[ "${ask_line:-0}" -lt "${release_line:-999}" ] && ok "ask label lands before the release" \
    || bad "ask label lands before the release" "ask at line $ask_line, release at $release_line"
[ "${overseer_line:-0}" -lt "${release_line:-999}" ] && ok "overseer label lands before the release" \
    || bad "overseer label lands before the release" "overseer at line $overseer_line, release at $release_line"
[ "${note_line:-0}" -lt "${release_line:-999}" ] && ok "note lands before the release" \
    || bad "note lands before the release" "note at line $note_line, release at $release_line"

echo
echo "case 2 — the note names the repo and the repo-map, not a generic message:"
# ======================================================================================
note_call="$(grep 'note sp-typo1' "$CALLS" | head -1)"
has "note names the offending repo" "repo:typo-repo" "$note_call"
has "note names the ask label"      "$SPIRA_ASK_LABEL" "$note_call"

echo
echo "case 3 — no BEADS_ACTOR (no live aeon identity): the bead is still labeled, but the release itself fails safely:"
# ======================================================================================
# release_own_claim refuses to act without an actor identity — the same fence every other
# release site shares. park_unmapped must still apply the labels: an aeon that somehow lost
# its own identity mid-session should not also lose the parking protection.
: > "$CALLS"
unset BEADS_ACTOR SPIRA_AEON
park_unmapped sp-typo2 other-repo
calls2="$(cat "$CALLS")"
has  "labels still applied without an actor identity" "label add sp-typo2 $SPIRA_ASK_LABEL" "$calls2"
if [[ "$calls2" == *"update sp-typo2 --status open --assignee"* ]]; then
    bad "release is refused without an actor identity" "an update/assign call was still issued"
else
    ok "release is refused without an actor identity"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
