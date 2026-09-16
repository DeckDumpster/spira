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
BDLOG="$TMP/bd.log"; CURLLOG="$TMP/curl.log"; STATE="$TMP/state"

# bd stub. It MODELS the store: `create` records the external ref, and `list`
# reports it from then on. Without that the post-check could never be exercised —
# the run would look identical whether the labels stuck or silently did not,
# which is the failure this suite exists to catch.
#
# STATE/broken=1 makes `create` succeed and the bead come back WITHOUT labels:
# a create that reports success and leaves the bead unclaimable, which is what a
# renamed label or a rejected flag would look like.
cat > "$TMP/bin/bd" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BDLOG"
case "$*" in
    *create*)
        for a in "$@"; do case "$a" in github:*) printf '%s\n' "$a" >> "$STATE/created" ;; esac; done
        cat >/dev/null 2>&1
        exit 0 ;;
    *list*)
        broken=$(cat "$STATE/broken" 2>/dev/null || echo 0)
        # One non-github bead always present, so the store-is-empty guard is not
        # what is being tested here.
        printf '[{"id":"sp-native","external_ref":"","labels":["spira","plan"]}'
        i=0
        while IFS= read -r ref; do
            [ -n "$ref" ] || continue
            if [ "$broken" = "1" ]; then lbl=''; else lbl='"spira","plan"'; fi
            printf ',{"id":"sp-gh%02d","external_ref":"%s","labels":[%s]}' "$i" "$ref" "$lbl"
            i=$((i+1))
        done < <(cat "$STATE/created" 2>/dev/null)
        printf ']'
        exit 0 ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/bd"

# curl stub: serves the issues endpoint. STATE/phase is how many open issues the
# tracker holds. One PULL REQUEST is always included, because the real endpoint
# returns them and nothing but the pull_request key distinguishes one.
cat > "$TMP/bin/curl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CURLLOG"
case "$*" in *"page=1"*) : ;; *) printf '[]'; exit 0 ;; esac
n=$(cat "$STATE/phase" 2>/dev/null || echo 0)
# PRETTY-PRINTED, AS THE REAL ENDPOINT IS. A page spans many lines. A reader that
# parses the feed a line at a time finds no complete document and reports an
# empty tracker, which is indistinguishable from a wrong repository name — it
# cost a run before this fixture had newlines in it.
printf '[\n'
printf '  {"number":999,\n   "title":"a pull request",\n   "body":"x",\n   "pull_request":{"url":"u"}}'
i=1
while [ "$i" -le "$n" ]; do
    printf ',\n  {"number":%d,\n   "title":"issue %d",\n   "body":"body %d"}' "$i" "$i" "$i"
    i=$((i+1))
done
printf '\n]\n'
exit 0
STUB
chmod +x "$TMP/bin/curl"

# SPIRA_PATH IS HOW THE STUBS SURVIVE. gh-intake.sh sources conf.sh, which
# rebuilds PATH from SPIRA_PATH — a stub directory merely prepended to PATH is
# discarded, and the suite would drive the real bd against the real store.
# THE BEAD REPOSITORY IS A FIXTURE, NOT THIS BOX. gh-intake.sh refuses to file
# beads against a repo: label that does not resolve to a checkout through the
# repo-map -- correctly, because every bead it filed would otherwise park. Taking
# the default meant BEAD_REPO became "spira", which resolves on a developer's box
# and nowhere else: this suite passed on the host and went red in the container
# and in CI, asserting against one machine's layout rather than against the code.
#
# So the suite brings its own map and its own checkout, and names the repository
# something the shipped default could never produce. A fixture pinned to a
# NON-DEFAULT value is what makes the repo: label below evidence: asserting
# "spira" would pass just as well against a literal written into the script.
FIXTURE_REPO=gh-intake-fixture
mkdir -p "$TMP/checkout"
git -C "$TMP/checkout" init -q 2>/dev/null
printf '%s | %s | push | origin/main | | true\n' "$FIXTURE_REPO" "$TMP/checkout" > "$TMP/repo-map"

run() {   # run <open-issue-count> [args...]
    printf '%s' "$1" > "$STATE/phase"; shift
    : > "$BDLOG"; : > "$CURLLOG"
    PATH="$TMP/bin:$PATH" SPIRA_PATH="$TMP/bin" BDLOG="$BDLOG" CURLLOG="$CURLLOG" STATE="$STATE" \
    SPIRA_BD=bd SPIRA_GH_INTAKE_REPO=DeckDumpster/spira \
    SPIRA_GH_INTAKE_BEAD_REPO="$FIXTURE_REPO" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_GH_INTAKE_API=https://api.github.com \
    SPIRA_GH_INTAKE_PRIORITY="${INTAKE_PRIORITY_OVERRIDE-$FIXTURE_PRIORITY}" \
        bash "$SCRIPT" "$@" 2>&1
}
# PINNED TO A NON-DEFAULT. Asserting against the shipped 2 passes just as well if the value
# were written into the code, which is the thing the key exists to stop.
FIXTURE_PRIORITY=1
reset() { : > "$STATE/created"; printf '0' > "$STATE/broken"; }
reset

echo "1. it is one-way, and that is structural:"
# The primary mechanism is that no code path writes to GitHub at all — not a flag
# that must be remembered. A push to a public tracker cannot be taken back.
if grep -qE 'curl[^|]*-X *(POST|PATCH|PUT|DELETE)|--data|-d ' "$SCRIPT"; then
    bad "no write to GitHub is constructed" "the script issues a mutating request"
else
    ok "no write to GitHub is constructed"
fi
out="$(run 2)"
if grep -qE '\-X *(POST|PATCH|PUT|DELETE)' "$CURLLOG" 2>/dev/null; then
    bad "no write was attempted" "curl log shows a mutating request"
else
    ok "no write was attempted"
fi

echo "2. it needs no credential at all:"
# THE SOURCE IS PUBLIC BY DEFINITION — it is an issue tracker anyone can read. A
# token would exist only to satisfy a client library, and a credential on the box
# is a credential the next caller can misuse. With none, "this bridge cannot
# write to GitHub" stops being a property to probe and becomes a fact about the
# machine (law-beads-is-never-public).
if grep -qE 'GITHUB_TOKEN|github\.token|Authorization:' "$SCRIPT"; then
    bad "no credential is read" "the script references a GitHub token"
else
    ok "no credential is read"
fi
out="$(run 2)"
if grep -qE 'Authorization|token' "$TMP/curl.log" 2>/dev/null; then
    bad "no credential is sent" "an Authorization header was sent"
else
    ok "no credential is sent"
fi
# Positive control: the fetch must actually have happened, or the assertion above
# is satisfied by a script that made no request at all.
if grep -q 'api.github.com' "$TMP/curl.log" 2>/dev/null; then ok "positive control: the API was actually called"
else bad "positive control: the API was actually called" "curl log is empty"; fi

echo
echo "2b. a pull request is not an issue:"
# The issues endpoint returns pull requests too. Ingesting them files a bead for
# every PR ever opened against the repository.
if grep -q 'pull_request' "$SCRIPT"; then ok "pull requests are excluded"
else bad "pull requests are excluded" "nothing filters pull_request"; fi

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

echo "4. beads are created directly into a live partition:"
# Created WITH the labels, not created and then labelled: a bead that exists for
# even one sentinel pass without them is one the loop has already declined.
reset; out="$(run 2)"
# The count is asserted explicitly: "created 0" is a pass for every assertion
# below that only looks for absence.
if grep -q 'fetched 2 open issue' <<<"$out"; then ok "the multi-line feed is parsed (2 issues, PR excluded)"
else bad "the multi-line feed is parsed" "got: $(grep fetched <<<"$out")"; fi
if grep -q 'create' "$BDLOG"; then ok "beads are created"
else bad "beads are created" "no create call"; fi
if grep -qE 'labels spira,plan|--labels spira,plan' "$BDLOG"; then ok "the live partition labels are applied at creation"
else bad "the live partition labels are applied at creation" "no spira,plan on the create"; fi
# A BEAD WITH NO repo: LABEL IS PARKED ON FIRST CLAIM. aeon.sh resolves repo:<name>
# through the repo-map, and a bead that names none is labelled needs-ryan and left
# for a human -- the livelock the harness itself defines. Ingesting without it
# files work that is guaranteed to stall at the moment an aeon picks it up.
# Matched against the FIXTURE's name, not "spira". The label must come from the
# resolved repository, and asserting the shipped default would pass equally well
# against a literal written into the script.
if grep -qE "repo:$FIXTURE_REPO" "$BDLOG"; then ok "the bead names the resolved repository"
else bad "the bead names the resolved repository" "no repo:$FIXTURE_REPO label on the create"; fi
if grep -q 'external-ref github:DeckDumpster/spira#1' "$BDLOG"; then ok "the external ref is the join key"
else bad "the external ref is the join key" "no external-ref on the create"; fi
# The pull request in the feed must not have become a bead.
if grep -q 'github:DeckDumpster/spira#999' "$BDLOG"; then bad "the pull request was not ingested" "PR #999 was created"
else ok "the pull request was not ingested"; fi

echo
echo "4b. a second run ingests nothing twice:"
out="$(run 2)"
if grep -q 'already present 2' <<<"$out"; then ok "re-running is idempotent"
else bad "re-running is idempotent" "expected 'already present 2', got: $(printf '%s' "$out" | tail -2 | tr '\n' ' ')"; fi

echo "5. --dry-run changes nothing:"
out="$(run 2 --dry-run)"
if grep -qE '^update|update ' "$BDLOG"; then bad "dry-run does not write" "an update was issued"
else ok "dry-run does not write"; fi
if grep -q -- 'github sync' "$BDLOG"; then
    if grep -q -- '--dry-run' "$BDLOG"; then ok "dry-run is passed through to the sync"
    else bad "dry-run is passed through to the sync" "sync ran for real"; fi
else ok "dry-run does not sync"; fi

echo "6. an ingested issue outranks what the harness files about itself:"
# A report from outside names something broken for somebody who is not this machine. Filed at
# the tracker's default it entered BELOW the band the harness files its own findings in, and
# the loop takes work in priority order — so with one aeon, 22 reported bugs sat behind every
# self-observed defect indefinitely.
reset
out="$(run 2)"
# THE POSITIVE CONTROL: creates were issued at all. Every assertion below is satisfied by a
# run that created nothing.
if grep -q 'create' "$BDLOG"; then ok "creates were issued (positive control)"
else bad "creates were issued (positive control)" "no create in the bd log"; fi
if grep -qE -- '-p +'"$FIXTURE_PRIORITY"'( |$)' "$BDLOG"; then
    ok "the configured priority reaches the create"
else
    bad "the configured priority reaches the create" \
        "no '-p $FIXTURE_PRIORITY' in: $(grep -m1 create "$BDLOG")"
fi
# A malformed value must be refused, not handed to `bd` — a failed create loses the finding
# and the run reports the issue as ingested.
out="$(INTAKE_PRIORITY_OVERRIDE=nine run 2)"; rc=$?
if [ "$rc" = 0 ]; then bad "a malformed priority is refused" "exited 0"
else ok "a malformed priority is refused (rc=$rc)"; fi
case "$out" in
    *SPIRA_GH_INTAKE_PRIORITY*) ok "and it names the key" ;;
    *) bad "and it names the key" "$out" ;;
esac

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
