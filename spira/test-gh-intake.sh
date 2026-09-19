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
# Triage gate (law-work-enters-only-from-the-operator): trust is GitHub's own
# access control — author_association OWNER/MEMBER/COLLABORATOR. Untrusted
# associations go to a digest, never to a partition. Promotion requires the
# actor who applied spira:accept to currently have admin/maintain/write access,
# verified from the collaborators API — the label's presence alone is not enough
# (law-a-pattern-match-is-not-an-identity-check).
#
# Positive control: fixtures (a)-(d) exercise the boundary; (a) and (c) are
# chosen to FAIL against the pre-amendment (allowlist) script and PASS only
# against the association-based gate.
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
        for a in "$@"; do case "$a" in
            sp-*|[a-z]*-*) printf '%s\n' "$a" >> "$STATE/closed_beads" 2>/dev/null || true ;;
        esac; done
        exit 0 ;;
    *list*)
        broken=$(cat "$STATE/broken" 2>/dev/null || echo 0)
        closed=$(cat "$STATE/closed_bead_flag" 2>/dev/null || echo 0)
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

# curl stub: serves issues, events, org-member, and collaborator-permission endpoints.
#
# STATE/phase = number of open issues (default 0)
# STATE/author_association = GitHub author_association for all issues (default MEMBER)
# STATE/issue_labels = space-separated GitHub labels on the issues (for spira:accept tests)
# STATE/events_json = raw JSON for the events endpoint (for promotion tests)
# STATE/org_member_code = HTTP status code for org members endpoint (204 or 404, default 404)
# STATE/actor_permission = permission level for collaborators endpoint (default "none")
cat > "$TMP/bin/curl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CURLLOG"
# Events endpoint
case "$*" in */events*)
    cat "$STATE/events_json" 2>/dev/null || printf '[]'
    exit 0 ;;
esac
# Org member check: -w '%{http_code}' means print the status code
case "$*" in *"/members/"*)
    code=$(cat "$STATE/org_member_code" 2>/dev/null || echo "404")
    printf '%s' "$code"
    exit 0 ;;
esac
# Collaborator permission
case "$*" in *"/collaborators/"*)
    perm=$(cat "$STATE/actor_permission" 2>/dev/null || echo "none")
    printf '{"permission":"%s","user":{"login":"x"}}' "$perm"
    exit 0 ;;
esac
# Main issues list: only page=1 has issues
case "$*" in *"page=1"*) : ;; *) printf '[]'; exit 0 ;; esac
n=$(cat "$STATE/phase" 2>/dev/null || echo 0)
author=$(cat "$STATE/author_login" 2>/dev/null || echo "fixture-member")
assoc=$(cat "$STATE/author_association" 2>/dev/null || echo "MEMBER")
extra_labels=$(cat "$STATE/issue_labels" 2>/dev/null || echo "")
labels_json='[]'
if [ -n "$extra_labels" ]; then
    labels_json="$(python3 -c "
import sys,json
lbls=[l for l in '${extra_labels}'.split() if l]
print(json.dumps([{'name':l} for l in lbls]))")"
fi
printf '[\n'
printf '  {"number":999,\n   "title":"a pull request",\n   "body":"x",\n   "user":{"login":"fixture-member"},\n   "author_association":"MEMBER",\n   "labels":[],\n   "pull_request":{"url":"u"}}'
i=1
while [ "$i" -le "$n" ]; do
    printf ',\n  {"number":%d,\n   "title":"issue %d",\n   "body":"body %d",\n   "user":{"login":"%s"},\n   "author_association":"%s",\n   "labels":%s}' \
        "$i" "$i" "$i" "$author" "$assoc" "$labels_json"
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

# FIXTURE_MEMBER: login for trusted (MEMBER association) issues.
# FIXTURE_CONTRIBUTOR: login for untrusted (CONTRIBUTOR association) issues.
# Using non-default associations as fixture values pins the test to the
# discriminating fact — a test that passed against no triage at all would not
# distinguish MEMBER from CONTRIBUTOR.
FIXTURE_MEMBER="fixture-member"
FIXTURE_CONTRIBUTOR="fixture-contributor"

run() {   # run <open-issue-count> [args...]
    printf '%s' "$1" > "$STATE/phase"; shift
    : > "$BDLOG"; : > "$CURLLOG"
    PATH="$TMP/bin:$PATH" SPIRA_PATH="$TMP/bin" BDLOG="$BDLOG" CURLLOG="$CURLLOG" STATE="$STATE" \
    SPIRA_BD=bd SPIRA_GH_INTAKE_REPO=DeckDumpster/spira \
    SPIRA_GH_INTAKE_BEAD_REPO="$FIXTURE_REPO" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_GH_INTAKE_API=https://api.github.com \
    SPIRA_GH_INTAKE_PRIORITY="${INTAKE_PRIORITY_OVERRIDE-$FIXTURE_PRIORITY}" \
    SPIRA_MAIL=/dev/null \
        bash "$SCRIPT" "$@" 2>&1
}
FIXTURE_PRIORITY=3
reset() {
    : > "$STATE/created_work"
    : > "$STATE/created_untrusted"
    printf '0' > "$STATE/broken"
    printf '%s' "$FIXTURE_MEMBER" > "$STATE/author_login"
    printf 'MEMBER' > "$STATE/author_association"
    : > "$STATE/issue_labels"
    printf '[]' > "$STATE/events_json"
    printf '404' > "$STATE/org_member_code"
    printf 'none' > "$STATE/actor_permission"
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
echo "7. triage gate: OWNER/MEMBER/COLLABORATOR become work; others do not:"
# (a) CONTRIBUTOR-authored issue must NOT become a work bead.
# This is the positive control: if a CONTRIBUTOR-authored issue creates a work
# bead, the association check is missing. Against the pre-amendment (allowlist)
# script this test would fail differently — the script would die on empty
# SPIRA_GH_INTAKE_TRUSTED or ingest nothing.
reset
printf '%s' "$FIXTURE_CONTRIBUTOR" > "$STATE/author_login"
printf 'CONTRIBUTOR' > "$STATE/author_association"
out="$(run 1)"
if grep -q "github:DeckDumpster/spira#1" "$STATE/created_work" 2>/dev/null; then
    bad "(a) CONTRIBUTOR issue does not become work" \
        "work bead created for CONTRIBUTOR author"
else
    ok "(a) CONTRIBUTOR issue does not become work"
fi
if grep -q "github-untrusted:DeckDumpster/spira#1" "$STATE/created_untrusted" 2>/dev/null; then
    ok "(a) CONTRIBUTOR issue is recorded as untrusted"
else
    bad "(a) CONTRIBUTOR issue is recorded as untrusted" "no untrusted record created"
fi
if grep -q "gh-untrusted" "$BDLOG"; then ok "(a) gh-untrusted label applied"
else bad "(a) gh-untrusted label applied" "no gh-untrusted in bd log"; fi

# (b) MEMBER-authored issue IS ingested as work.
reset
out="$(run 1)"
if grep -q "github:DeckDumpster/spira#1" "$STATE/created_work" 2>/dev/null; then
    ok "(b) MEMBER issue becomes a work bead"
else
    bad "(b) MEMBER issue becomes a work bead" "no work bead for MEMBER author"
fi
if grep -q "github-untrusted" "$STATE/created_untrusted" 2>/dev/null; then
    bad "(b) MEMBER author is not recorded as untrusted" "untrusted record was created"
else
    ok "(b) MEMBER author is not recorded as untrusted"
fi

echo
echo "8. triage gate: spira:accept must be from a login with current access:"
# (c) CONTRIBUTOR issue with spira:accept applied by another CONTRIBUTOR must
# NOT become work. The actor check must be seen: if the permission check were
# skipped entirely, any spira:accept would promote any issue.
reset
printf '%s' "$FIXTURE_CONTRIBUTOR" > "$STATE/author_login"
printf 'CONTRIBUTOR' > "$STATE/author_association"
printf 'spira:accept' > "$STATE/issue_labels"
printf '[{"event":"labeled","actor":{"login":"%s"},"label":{"name":"spira:accept"}}]' \
    "$FIXTURE_CONTRIBUTOR" > "$STATE/events_json"
printf '404' > "$STATE/org_member_code"
printf 'read' > "$STATE/actor_permission"
out="$(run 1)"
if grep -q "github:DeckDumpster/spira#1" "$STATE/created_work" 2>/dev/null; then
    bad "(c) non-collaborator accept does not promote" \
        "work bead created when actor lacks write access"
else
    ok "(c) non-collaborator accept does not promote"
fi
# Positive control: the permission endpoint was actually called.
if grep -q '/collaborators/' "$CURLLOG" 2>/dev/null || grep -q '/members/' "$CURLLOG" 2>/dev/null; then
    ok "(c) positive control: access endpoint was called"
else
    bad "(c) positive control: access endpoint was called" "no collaborators or members call in curl log"
fi

# (d) Same CONTRIBUTOR issue, but actor has write access → promoted to work.
reset
printf '%s' "$FIXTURE_CONTRIBUTOR" > "$STATE/author_login"
printf 'CONTRIBUTOR' > "$STATE/author_association"
printf 'spira:accept' > "$STATE/issue_labels"
printf '[{"event":"labeled","actor":{"login":"%s"},"label":{"name":"spira:accept"}}]' \
    "$FIXTURE_MEMBER" > "$STATE/events_json"
printf '404' > "$STATE/org_member_code"
printf 'write' > "$STATE/actor_permission"
out="$(run 1)"
if grep -q "github:DeckDumpster/spira#1" "$STATE/created_work" 2>/dev/null; then
    ok "(d) write-access actor promotes the issue to work"
else
    bad "(d) write-access actor promotes the issue to work" \
        "no work bead after write-access actor applied spira:accept"
fi

echo
echo "9. body hygiene: only the issue body at ingestion time reaches the bead:"
reset
out="$(run 1)"
if grep -q '/comments' "$CURLLOG" 2>/dev/null; then
    bad "(e) comments endpoint is not called" "a comments URL appeared in curl log"
else
    ok "(e) comments endpoint is not called"
fi
ok "(e) comment absence is structural: nothing fetches /comments"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
