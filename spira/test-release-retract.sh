#!/usr/bin/env bash
#
# test-release-retract.sh — a cut that does not publish leaves no draft release behind.
# Runs release-retract.sh and stale-drafts.sh against a fake gh that keeps a release
# table in a file, and checks acceptance.yml retracts from a job that survives a skipped
# acceptance job.
#
# tier: T1
# covers: spira/release-retract.sh spira/stale-drafts.sh .github/workflows/acceptance.yml .github/workflows/acceptance-daily.yml
# host-reason: fake gh script in an isolated PATH dir; no container or database dependency
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REPO_ROOT="$(cd "$HERE/.." && pwd -P)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
. "$HERE/testlib.sh"

echo "test-release-retract.sh"

mkdir -p "$TMP/bin"
cat > "$TMP/bin/gh" <<'EOF'
#!/usr/bin/env bash
# state: lines "<tag> <isDraft> <createdAt>" in $FAKE_GH_DB; tag refs in $FAKE_GH_DB.refs
db="$FAKE_GH_DB"
case "$1 $2" in
    "release view")
        row="$(grep "^$3 " "$db")" || exit 1
        printf '%s\n' "$(printf '%s' "$row" | cut -d' ' -f2)" ;;
    "release delete") sed -i "/^$3 /d" "$db" ;;
    "release list") awk '$2=="true"{printf "%s\t%s\n",$1,$3}' "$db" ;;
    "api --method") sed -i "\#${4##*/}\$#d" "$db.refs" ;;
    *) exit 1 ;;
esac
EOF
chmod +x "$TMP/bin/gh"
export FAKE_GH_DB="$TMP/db" PATH="$TMP/bin:$PATH" GITHUB_REPOSITORY=o/r

old=2020-01-01T00:00:00Z
new="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf '%s\n' "spira-release-x-DRAFT true $old" "spira-release-x-PUB false $old" > "$FAKE_GH_DB"
printf '%s\n' "refs/tags/spira-release-x-DRAFT" "refs/tags/spira-release-x-PUB" > "$FAKE_GH_DB.refs"

echo "1. a failed cut's draft and tag are gone"
bash "$HERE/release-retract.sh" spira-release-x-DRAFT --repo o/r >/dev/null
out="$(bash "$HERE/stale-drafts.sh" --repo o/r 2>/dev/null)"; rc=$?
wantrc "no stale draft after retract" 0 "$rc"
nowant "draft release deleted" "spira-release-x-DRAFT" "$(cat "$FAKE_GH_DB")"
nowant "draft tag deleted" "spira-release-x-DRAFT" "$(cat "$FAKE_GH_DB.refs")"

echo "2. a published cut is unaffected"
bash "$HERE/release-retract.sh" spira-release-x-PUB --repo o/r >/dev/null
want "published release kept" "spira-release-x-PUB" "$(cat "$FAKE_GH_DB")"
want "published tag kept" "spira-release-x-PUB" "$(cat "$FAKE_GH_DB.refs")"

echo "3. stale-drafts: positive control, then age cutoff"
printf '%s\n' "spira-release-x-LEFT true $old" "spira-release-x-FRESH true $new" >> "$FAKE_GH_DB"
out="$(bash "$HERE/stale-drafts.sh" --repo o/r 2>/dev/null)"; rc=$?
wantrc "an old draft is reported" 1 "$rc"
want "old draft listed" "spira-release-x-LEFT" "$out"
nowant "fresh draft not listed" "spira-release-x-FRESH" "$out"
nowant "published release not listed" "spira-release-x-PUB" "$out"

echo "4. acceptance.yml retracts from a job that runs when acceptance is skipped"
_wf="$(cat "$REPO_ROOT/.github/workflows/acceptance.yml")"
_job="$(awk '/^  retract:$/{f=1;print;next} f&&/^  [a-z-]+:$/{exit} f' "$REPO_ROOT/.github/workflows/acceptance.yml")"
want   "retract job exists" "release-retract.sh" "$_job"
want   "retract runs on a skipped acceptance" "needs.acceptance.result != 'success'" "$_job"
want   "retract is gated on always()" "always()" "$_job"
nowant "no retract step left inside acceptance" "Retract release and tag on FAIL" "$_wf"
want   "daily workflow checks for stale drafts" "stale-drafts.sh" "$(cat "$REPO_ROOT/.github/workflows/acceptance-daily.yml")"

tl_summary
