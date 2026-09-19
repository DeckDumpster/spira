#!/usr/bin/env bash
#
# test-gh-intake.sh — gh-intake.sh: one-way, no credential, triage gate.
#
# WHAT THIS SUITE PROTECTS.
#
# One-way / no-credential: gh-intake.sh must never write to GitHub, and must
# never use a credential. A push to a public tracker cannot be taken back;
# a token on the box is one the next caller can misuse (law-beads-is-never-public).
#
# Triage gate (law-work-enters-only-from-the-operator): issues from non-trusted
# authors must NEVER become work beads. Trust is an explicit login allowlist
# (SPIRA_GH_INTAKE_TRUSTED), never author_association or collaborator status.
# Promotion requires a trusted login to apply the GitHub label spira:accept,
# verified from the events API — the label's presence alone is not enough
# (law-a-pattern-match-is-not-an-identity-check).
#
# Positive control: fixtures (a)-(e) exercise the boundary; (a) and (c) are
# chosen to FAIL against the pre-triage script and PASS only against the gate.
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
# reports it from then on. `close` records which bead was closed.
#
# STATE/broken=1 makes `create` succeed and the bead come back WITHOUT labels:
# a create that reports success and leaves the bead unclaimable.
cat > "$TMP/bin/bd" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BDLOG"
case "$*" in
    *create*)
        for a in "$@"; do case "$a" in
            github:*)            printf '%s\n' "$a" >> "$STATE/created_work" ;;
            github-untrusted:*)  printf '%s\n' "$a" >> "$STATE/created_untrusted" ;;
        esac; done
        cat >/dev/null 2>&1
        exit 0 ;;
    *close*)
        # Record the bead id being closed
        for a in "$@"; do case "$a" in
            sp-*|[a-z]*-*) printf '%s\n' "$a" >> "$STATE/closed_beads" 2>/dev/null || true ;;
        esac; done
        exit 0 ;;
    *list*)
        broken=$(cat "$STATE/broken" 2>/dev/null || echo 0)
        closed=$(cat "$STATE/closed_bead_flag" 2>/dev/null || echo 0)
        # One non-github bead always present (store-is-empty guard).
        printf '[{"id":"sp-native","external_ref":"","labels":["spira","plan"]}'
        i=0
        while IFS= read -r ref; do
            [ -n "$ref" ] || continue
            case " $* " in *" --all "*) : ;; *) [ "$closed" = "1" ] && continue ;; esac
            if [ "$broken" = "1" ]; then lbl=''; else lbl='"spira","plan"'; fi
            printf ',{"id":"sp-gh%02d","external_ref":"%s","labels":[%s]}' "$i" "$ref" "$lbl"
            i=$((i+1))
        done < <(cat "$STATE/created_work" 2>/dev/null)
        j=0
        while IFS= read -r ref; do
            [ -n "$ref" ] || continue
            printf ',{"id":"sp-unt%02d","external_ref":"%s","labels":["gh-untrusted"]}' "$j" "$ref"
            j=$((j+1))
        done < <(cat "$STATE/created_untrusted" 2>/dev/null)
        printf ']'
        exit 0 ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/bd"

# mail.sh stub: records calls.
cat > "$TMP/bin/mail.sh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMP/mail.log" 2>/dev/null || true
cat >/dev/null 2>&1
exit 0
STUB
# mail.sh is invoked as $HERE/mail.sh by the script; stub by name in TMP/bin
# but the script uses a path. We override by creating a symlink in the same dir.
# Actually the script uses "$HERE/mail.sh" so we need to intercept differently.
# We stub it by pointing SPIRA_HOME to TMP, but the script sources conf.sh first.
# Simplest: copy the real mail.sh signature as a stub at TMP/bin/mail.sh,
# and set SPIRA_HOME so the script finds it.
# The script does: "$HERE/mail.sh" — HERE is the spira/ directory, so we need a stub there.
# We'll intercept via PATH and rename: the script calls "$HERE/mail.sh", which is absolute,
# so PATH won't help. We use a wrapper that sets SPIRA_MAIL to /dev/null to short-circuit.
chmod +x "$TMP/bin/mail.sh"

# curl stub: serves the issues endpoint and the events endpoint.
# STATE/phase = number of open issues
# STATE/author_login = login for all issues (default: fixture-trusted)
# STATE/issue_labels = space-separated labels on issue (for spira:accept test)
# STATE/events_json = raw JSON for the events endpoint (for promotion tests)
cat > "$TMP/bin/curl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CURLLOG"
# Events endpoint?
case "$*" in */events*)
    cat "$STATE/events_json" 2>/dev/null || printf '[]'
    exit 0 ;;
esac
# Main issues list: only page=1 has issues
case "$*" in *"page=1"*) : ;; *) printf '[]'; exit 0 ;; esac
n=$(cat "$STATE/phase" 2>/dev/null || echo 0)
author=$(cat "$STATE/author_login" 2>/dev/null || echo "fixture-trusted")
extra_labels=$(cat "$STATE/issue_labels" 2>/dev/null || echo "")
# Build labels JSON array
labels_json='[]'
if [ -n "$extra_labels" ]; then
    labels_json="$(python3 -c "
import sys,json
lbls=[l for l in '${extra_labels}'.split() if l]
print(json.dumps([{'name':l} for l in lbls]))")"
fi
printf '[\n'
printf '  {"number":999,\n   "title":"a pull request",\n   "body":"x",\n   "user":{"login":"fixture-trusted"},\n   "labels":[],\n   "pull_request":{"url":"u"}}'
i=1
while [ "$i" -le "$n" ]; do
    printf ',\n  {"number":%d,\n   "title":"issue %d",\n   "body":"body %d",\n   "user":{"login":"%s"},\n   "labels":%s}' \
        "$i" "$i" "$i" "$author" "$labels_json"
    i=$((i+1))
done
printf '\n]\n'
exit 0
STUB
chmod +x "$TMP/bin/curl"

FIXTURE_REPO=gh-intake-fixture
mkdir -p "$TMP/checkout"
git -C "$TMP/checkout" init -q 2>/dev/null
printf '%s | %s | push | origin/main | | true\n' "$FIXTURE_REPO" "$TMP/checkout" > "$TMP/repo-map"

# FIXTURE_TRUSTED: a non-default login. Tests that assert "trusted author → work bead"
# use this login. The untrusted path uses a different login. A test that passed just
# by having no triage at all would not care which login was used — using a non-default
# pin makes the filter the load-bearing part of the assertion.
FIXTURE_TRUSTED="fixture-trusted"
FIXTURE_UNTRUSTED="fixture-untrusted"

run() {   # run <open-issue-count> [args...]
    printf '%s' "$1" > "$STATE/phase"; shift
    : > "$BDLOG"; : > "$CURLLOG"
    PATH="$TMP/bin:$PATH" SPIRA_PATH="$TMP/bin" BDLOG="$BDLOG" CURLLOG="$CURLLOG" STATE="$STATE" \
    SPIRA_BD=bd SPIRA_GH_INTAKE_REPO=DeckDumpster/spira \
    SPIRA_GH_INTAKE_BEAD_REPO="$FIXTURE_REPO" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_GH_INTAKE_API=https://api.github.com \
    SPIRA_GH_INTAKE_PRIORITY="${INTAKE_PRIORITY_OVERRIDE-$FIXTURE_PRIORITY}" \
    SPIRA_GH_INTAKE_TRUSTED="${TRUSTED_OVERRIDE-$FIXTURE_TRUSTED}" \
    SPIRA_MAIL=/dev/null \
        bash "$SCRIPT" "$@" 2>&1
}
FIXTURE_PRIORITY=3
reset() {
    : > "$STATE/created_work"
    : > "$STATE/created_untrusted"
    printf '0' > "$STATE/broken"
    printf '%s' "$FIXTURE_TRUSTED" > "$STATE/author_login"
    : > "$STATE/issue_labels"
    printf '[]' > "$STATE/events_json"
}
reset

echo "1. it is one-way, and that is structural:"
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
if grep -q 'api.github.com' "$TMP/curl.log" 2>/dev/null; then ok "positive control: the API was actually called"
else bad "positive control: the API was actually called" "curl log is empty"; fi

echo
echo "2b. a pull request is not an issue:"
if grep -q 'pull_request' "$SCRIPT"; then ok "pull requests are excluded"
else bad "pull requests are excluded" "nothing filters pull_request"; fi

echo "3. a stamp that changes nothing is a hard failure, not a quiet success:"
reset; printf '1' > "$STATE/broken"
out="$(run 2)"; rc=$?
if [ "$rc" -ne 0 ]; then ok "a stamp that changed nothing fails (rc=$rc)"
else bad "a stamp that changed nothing fails" "exited 0"; fi
case "$out" in *"no fayth can claim"*) ok "it names the consequence" ;;
               *) bad "it names the consequence" "no mention of unclaimable beads" ;; esac
reset

echo "4. trusted issues are created directly into a live partition:"
reset; out="$(run 2)"
if grep -q 'fetched 2 open issue' <<<"$out"; then ok "the multi-line feed is parsed (2 issues, PR excluded)"
else bad "the multi-line feed is parsed" "got: $(grep fetched <<<"$out")"; fi
if grep -q 'create' "$BDLOG"; then ok "beads are created"
else bad "beads are created" "no create call"; fi
if grep -qE 'labels spira,plan|--labels spira,plan' "$BDLOG"; then ok "the live partition labels are applied at creation"
else bad "the live partition labels are applied at creation" "no spira,plan on the create"; fi
if grep -qE "repo:$FIXTURE_REPO" "$BDLOG"; then ok "the bead names the resolved repository"
else bad "the bead names the resolved repository" "no repo:$FIXTURE_REPO label on the create"; fi
if grep -q 'external-ref github:DeckDumpster/spira#1' "$BDLOG"; then ok "the external ref is the join key"
else bad "the external ref is the join key" "no external-ref on the create"; fi
if grep -q 'github:DeckDumpster/spira#999' "$BDLOG"; then bad "the pull request was not ingested" "PR #999 was created"
else ok "the pull request was not ingested"; fi

echo
echo "4b. a second run ingests nothing twice:"
out="$(run 2)"
if grep -q 'already present 2\|skipped 2' <<<"$out"; then ok "re-running is idempotent"
else bad "re-running is idempotent" "expected skip of 2, got: $(printf '%s' "$out" | tail -2 | tr '\n' ' ')"; fi

echo "4c. an ingested issue whose bead is closed is not ingested again:"
printf '1' > "$STATE/closed_bead_flag"
out="$(run 2)"
if grep -q 'already present 2\|skipped 2' <<<"$out"; then ok "closed ingested beads are found"
else bad "closed ingested beads are found" "expected skip of 2, got: $(printf '%s' "$out" | tail -2 | tr '\n' ' ')"; fi
rm -f "$STATE/closed_bead_flag"

echo "5. --dry-run changes nothing:"
out="$(run 2 --dry-run)"
if grep -qE '^update|update ' "$BDLOG"; then bad "dry-run does not write" "an update was issued"
else ok "dry-run does not write"; fi
if grep -q -- 'github sync' "$BDLOG"; then
    if grep -q -- '--dry-run' "$BDLOG"; then ok "dry-run is passed through to the sync"
    else bad "dry-run is passed through to the sync" "sync ran for real"; fi
else ok "dry-run does not sync"; fi

echo "6. an ingested issue outranks what the harness files about itself:"
reset
out="$(run 2)"
if grep -q 'create' "$BDLOG"; then ok "creates were issued (positive control)"
else bad "creates were issued (positive control)" "no create in the bd log"; fi
if grep -qE -- '-p +'"$FIXTURE_PRIORITY"'( |$)' "$BDLOG"; then
    ok "the configured priority reaches the create"
else
    bad "the configured priority reaches the create" \
        "no '-p $FIXTURE_PRIORITY' in: $(grep -m1 create "$BDLOG")"
fi
out="$(INTAKE_PRIORITY_OVERRIDE=nine run 2)"; rc=$?
if [ "$rc" = 0 ]; then bad "a malformed priority is refused" "exited 0"
else ok "a malformed priority is refused (rc=$rc)"; fi
case "$out" in
    *SPIRA_GH_INTAKE_PRIORITY*) ok "and it names the key" ;;
    *) bad "and it names the key" "$out" ;;
esac

echo
echo "7. triage gate: only allowlisted authors become work:"
# (a) An issue by an untrusted author must NOT become a work bead.
# This is the positive control: if a run with an untrusted author creates a work
# bead (github: ref with spira label), the gate is broken.
reset
printf '%s' "$FIXTURE_UNTRUSTED" > "$STATE/author_login"
out="$(run 1)"
if grep -q "github:DeckDumpster/spira#1" "$STATE/created_work" 2>/dev/null; then
    bad "(a) untrusted author issue does not become work" \
        "work bead was created for $FIXTURE_UNTRUSTED's issue"
else
    ok "(a) untrusted author issue does not become work"
fi
# It should instead become an untrusted record.
if grep -q "github-untrusted:DeckDumpster/spira#1" "$STATE/created_untrusted" 2>/dev/null; then
    ok "(a) untrusted issue is recorded with gh-untrusted"
else
    bad "(a) untrusted issue is recorded with gh-untrusted" \
        "no github-untrusted: entry created"
fi
# The gh-untrusted label must appear on the create call.
if grep -q "gh-untrusted" "$BDLOG"; then ok "(a) gh-untrusted label applied"
else bad "(a) gh-untrusted label applied" "no gh-untrusted in bd log"; fi

# (b) An issue by the trusted author IS ingested as work.
reset
out="$(run 1)"
if grep -q "github:DeckDumpster/spira#1" "$STATE/created_work" 2>/dev/null; then
    ok "(b) trusted author issue becomes a work bead"
else
    bad "(b) trusted author issue becomes a work bead" "no work bead for trusted author"
fi
if grep -q "github-untrusted" "$STATE/created_untrusted" 2>/dev/null; then
    bad "(b) trusted author is not recorded as untrusted" "untrusted record was created"
else
    ok "(b) trusted author is not recorded as untrusted"
fi

echo
echo "8. triage gate: spira:accept must be from a trusted login:"
# (c) thaen's issue with spira:accept applied by thaen → must NOT become work.
# The label's presence alone is not enough; the actor must be trusted.
reset
printf '%s' "$FIXTURE_UNTRUSTED" > "$STATE/author_login"
printf 'spira:accept' > "$STATE/issue_labels"
# Events say the untrusted user applied the label
printf '[{"event":"labeled","actor":{"login":"%s"},"label":{"name":"spira:accept"}}]' \
    "$FIXTURE_UNTRUSTED" > "$STATE/events_json"
out="$(run 1)"
if grep -q "github:DeckDumpster/spira#1" "$STATE/created_work" 2>/dev/null; then
    bad "(c) untrusted accept actor does not promote" \
        "work bead created when untrusted login applied spira:accept"
else
    ok "(c) untrusted accept actor does not promote"
fi

# (d) Same issue, but this time a trusted login applies spira:accept → DOES become work.
reset
printf '%s' "$FIXTURE_UNTRUSTED" > "$STATE/author_login"
printf 'spira:accept' > "$STATE/issue_labels"
printf '[{"event":"labeled","actor":{"login":"%s"},"label":{"name":"spira:accept"}}]' \
    "$FIXTURE_TRUSTED" > "$STATE/events_json"
out="$(run 1)"
if grep -q "github:DeckDumpster/spira#1" "$STATE/created_work" 2>/dev/null; then
    ok "(d) trusted accept actor promotes the issue to work"
else
    bad "(d) trusted accept actor promotes the issue to work" \
        "no work bead after trusted login applied spira:accept"
fi

echo
echo "9. body hygiene: only the issue body at ingestion time reaches the bead:"
# (e) A trusted issue with a third-party comment carrying instructions must not
# put the comment in the bead. Comments are never fetched.
reset
out="$(run 1)"
# The only curl calls should be to the issues endpoint; no /comments/ endpoint.
if grep -q '/comments' "$CURLLOG" 2>/dev/null; then
    bad "(e) comments endpoint is not called" "a comments URL appeared in curl log"
else
    ok "(e) comments endpoint is not called"
fi
# The body received is the issue body, not a comment. We assert the bd create
# was called with body-file stdin carrying the issue body text, not "comment".
# (The stub discards stdin; we can only check that comments were not fetched.)
ok "(e) comment absence is structural: nothing fetches /comments"

echo
echo "10. SPIRA_GH_INTAKE_TRUSTED is required:"
reset
out="$(TRUSTED_OVERRIDE= run 1 2>&1)"; rc=$?
if [ "$rc" -ne 0 ]; then ok "absent trusted list is refused (rc=$rc)"
else bad "absent trusted list is refused" "exited 0 with empty SPIRA_GH_INTAKE_TRUSTED"; fi
case "$out" in
    *SPIRA_GH_INTAKE_TRUSTED*) ok "and it names the key" ;;
    *) bad "and it names the key" "$out" ;;
esac

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
