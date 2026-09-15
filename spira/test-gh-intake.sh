#!/usr/bin/env bash
#
# test-gh-intake.sh — ingesting issues cannot publish the store, and cannot
# silently ingest nothing.
#
# WHY THIS SUITE IS NOT OPTIONAL. gh-intake.sh drives `bd github sync`, which is
# bidirectional by default. The beads store holds internal working notes and the
# overseer's own judgement; pushing it to a public issue tracker is irreversible
# and is the exact thing law-beads-is-never-public forbids. One-way is therefore a
# property to be tested, not a flag to be remembered.
#
# THE SECOND FAILURE IS QUIETER. A pulled issue arrives with no labels, so it
# matches none of the live partitions and no fayth can ever claim it. The ingest
# would report success having filed work nobody will do -- deferred and
# forgotten, which is neither of the two states a filed bead is allowed to be in
# (law-filed-bead-queued-xor-escalated; 130 beads once sat in exactly that state).
# So "pulled something, stamped nothing" must be a hard failure.
#
# FIXTURES. bd and curl are both stubbed on PATH and driven by state files. No
# network call is made and the real store is never opened.
#
# covers: spira/gh-intake.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SCRIPT="$HERE/gh-intake.sh"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-gh-intake.sh"
echo

if [ ! -x "$SCRIPT" ]; then
    bad "gh-intake.sh is executable" "not found at $SCRIPT"
    printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "gh-intake.sh is executable"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/bin" "$TMP/state"
BDLOG="$TMP/bd.log"; STATE="$TMP/state"

# bd stub. It MODELS the store rather than replaying a script: `update --labels`
# records the id, and `list --json` reports that id as carrying the labels from
# then on. Without that the post-check could never be exercised — the run would
# look identical whether stamping worked or silently did nothing, which is the
# failure this suite exists to catch.
#
# STATE/phase   how many ingested beads the store holds
# STATE/broken  when 1, `update` accepts and records nothing: a stamp that
#               reports success and changes nothing, i.e. the real defect.
cat > "$TMP/bin/bd" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BDLOG"
case "$*" in
    *"github sync"*) exit 0 ;;
    *update*)
        [ "$(cat "$STATE/broken" 2>/dev/null || echo 0)" = "1" ] && exit 0
        for a in "$@"; do case "$a" in sp-gh*) printf '%s\n' "$a" >> "$STATE/stamped" ;; esac; done
        exit 0 ;;
    *list*)
        n=$(cat "$STATE/phase" 2>/dev/null || echo 0)
        printf '['
        i=0; first=1
        while [ "$i" -lt "$n" ]; do
            id=$(printf 'sp-gh%02d' "$i")
            if grep -qx "$id" "$STATE/stamped" 2>/dev/null; then lbl='"spira","plan"'; else lbl=''; fi
            [ "$first" = 1 ] || printf ','
            printf '{"id":"%s","external_ref":"github:DeckDumpster/spira#%d","labels":[%s]}' "$id" "$i" "$lbl"
            first=0; i=$((i+1))
        done
        printf ']'
        exit 0 ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/bd"

# curl stub: serves the token write-probe only.
cat > "$TMP/bin/curl" <<'STUB'
#!/usr/bin/env bash
if [ "${TOKEN_WRITABLE:-0}" = "1" ]; then printf 'x-oauth-scopes: repo, workflow\n'
else printf 'x-oauth-scopes:\n'; fi
exit 0
STUB
chmod +x "$TMP/bin/curl"

# SPIRA_PATH IS HOW THE STUBS SURVIVE. gh-intake.sh sources conf.sh, which
# rebuilds PATH from SPIRA_PATH — so a stub directory merely prepended to PATH is
# discarded, and the suite would silently drive the real bd against the real
# store. Going in through the configured seam is the only way that holds.
run() {   # run <ingested-count> [args...]
    printf '%s' "$1" > "$STATE/phase"; shift
    : > "$BDLOG"
    PATH="$TMP/bin:$PATH" SPIRA_PATH="$TMP/bin" BDLOG="$BDLOG" STATE="$STATE" \
    SPIRA_BD=bd GITHUB_TOKEN=fake-token SPIRA_GH_INTAKE_REPO=DeckDumpster/spira \
    TOKEN_WRITABLE="${TOKEN_WRITABLE:-0}" \
        bash "$SCRIPT" "$@" 2>&1
}
reset() { : > "$STATE/stamped"; printf '0' > "$STATE/broken"; }
reset

echo "1. it is one-way, and that is structural:"
# THE PRIMARY MECHANISM is that no code path constructs a push. A flag that had
# to be remembered would be a habit, and the statute is explicit that this must
# be enforced rather than remembered.
if grep -qE 'github (sync|push)[^|]*--push-only|github push' "$SCRIPT"; then
    bad "no push is ever constructed" "the script contains a push invocation"
else
    ok "no push is ever constructed"
fi
if grep -q -- '--pull-only' "$SCRIPT"; then ok "the sync is pinned to --pull-only"
else bad "the sync is pinned to --pull-only" "no --pull-only in the script"; fi

out="$(run 2)"
if grep -q -- '--pull-only' "$BDLOG"; then ok "the run actually passed --pull-only"
else bad "the run actually passed --pull-only" "bd was called without it"; fi
if grep -qE 'push-only|github push' "$BDLOG"; then bad "no push was attempted" "bd log shows a push"
else ok "no push was attempted"; fi

echo
echo "2. a token that can write is refused:"
# Defence in depth. The script cannot push, but a writable token on the box is a
# loaded gun for the next caller, so intake refuses to be the thing that
# normalises having one.
out="$(TOKEN_WRITABLE=1 run 2)"; rc=$?
if [ "$rc" -ne 0 ]; then ok "a writable token is refused (rc=$rc)"
else bad "a writable token is refused" "exited 0"; fi
case "$out" in *[Ww]rit*) ok "it says why" ;; *) bad "it says why" "no mention of write access" ;; esac

echo
echo "3. a stamp that changes nothing is a hard failure, not a quiet success:"
# An ingested bead with no labels matches no fayth predicate, so no aeon can ever
# claim it. Reporting success there files work nobody will do. The stub accepts
# the update and records nothing, which is exactly what a renamed label or a
# changed external-ref format would look like.
reset; printf '1' > "$STATE/broken"
out="$(run 2)"; rc=$?
if [ "$rc" -ne 0 ]; then ok "a stamp that changed nothing fails (rc=$rc)"
else bad "a stamp that changed nothing fails" "exited 0"; fi
case "$out" in *"no fayth can claim"*) ok "it names the consequence" ;;
               *) bad "it names the consequence" "no mention of unclaimable beads" ;; esac
reset

echo "4. it stamps into a live partition:"
out="$(run 2)"
if grep -qE 'update' "$BDLOG"; then ok "beads are labelled after the pull"
else bad "beads are labelled after the pull" "no update call in the bd log"; fi
# The label pair must be one a fayth predicate actually selects; a bead labelled
# with anything else is filed and unreachable.
if grep -qE 'spira' "$BDLOG"; then ok "the scope label is applied"
else bad "the scope label is applied" "no scope label in the update"; fi

echo
echo "5. --dry-run changes nothing:"
out="$(run 2 --dry-run)"
if grep -qE '^update|update ' "$BDLOG"; then bad "dry-run does not write" "an update was issued"
else ok "dry-run does not write"; fi
if grep -q -- 'github sync' "$BDLOG"; then
    if grep -q -- '--dry-run' "$BDLOG"; then ok "dry-run is passed through to the sync"
    else bad "dry-run is passed through to the sync" "sync ran for real"; fi
else ok "dry-run does not sync"; fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
