#!/usr/bin/env bash
# test-conf-registry-merge.sh (sp-g3uwp) — the registry's whole design is one file per key,
# so that two branches which each add a DIFFERENT key add two different files, which git can
# only merge cleanly. test-conf-key-merge.sh already proved the general principle (grouped
# text conflicts; one-per-line text does not) against a synthetic conf-keys.txt fixture. This
# suite proves the SAME claim against the real shape sp-g3uwp ships: actual conf.d/<KEY>
# files, added with `git add`, generated through the real conf-gen.sh after the merge —
# satisfying the bead's acceptance criterion directly rather than by analogy.
#
# THE CONTROL COMES FIRST (law-absence-needs-a-positive-control): reconstruct the grouped
# SPIRA_CONF_KEYS shape conf.sh had before this bead, make the same two-branch edit this
# suite's registry version makes, and require git to see a CONFLICT there.
#
# tier: T1
# covers: spira/conf.d/* spira/conf-gen.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-conf-registry-merge.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# =============================================================================
echo
echo "control: the pre-registry grouped-text shape still conflicts on two independent adds:"
# =============================================================================
OLD="$TMP/old-repo"
rm -rf "$OLD"; mkdir -p "$OLD"
( cd "$OLD" && git init -q -b base \
    && printf 'SPIRA_HOME_REPO SPIRA_DB SPIRA_RUN\nSPIRA_PATH SPIRA_WORKSPACES\n' > conf-keys.txt \
    && git add conf-keys.txt && git commit -q -m base ) >/dev/null
( cd "$OLD" && git checkout -q -b alpha base \
    && sed -i 's/^SPIRA_PATH SPIRA_WORKSPACES$/SPIRA_PATH SPIRA_WORKSPACES SPIRA_ZZTEST_ALPHA/' conf-keys.txt \
    && git commit -q -am "add SPIRA_ZZTEST_ALPHA" ) >/dev/null
( cd "$OLD" && git checkout -q -b bravo base \
    && sed -i 's/^SPIRA_PATH SPIRA_WORKSPACES$/SPIRA_PATH SPIRA_WORKSPACES SPIRA_ZZTEST_BRAVO/' conf-keys.txt \
    && git commit -q -am "add SPIRA_ZZTEST_BRAVO" ) >/dev/null
( cd "$OLD" && git checkout -q alpha && git merge -q --no-edit bravo ) >"$TMP/old-merge.log" 2>&1
old_rc=$?
if [ "$old_rc" -ne 0 ] && grep -q '^<<<<<<<' "$OLD/conf-keys.txt" 2>/dev/null; then
    ok "the grouped shape still conflicts (this control must fail before the fix means anything)"
else
    bad "the grouped shape still conflicts" "rc=$old_rc: $(cat "$TMP/old-merge.log")"
fi
( cd "$OLD" && git merge --abort ) >/dev/null 2>&1

# =============================================================================
echo
echo "fix: a real conf.d/ registry — two branches each adding a different KEY FILE merge clean:"
# =============================================================================
NEW="$TMP/new-repo"
rm -rf "$NEW"; mkdir -p "$NEW/spira/conf.d"
cp "$HERE/conf-gen.sh" "$NEW/spira/conf-gen.sh"
# A tiny seed registry — just enough for conf-gen.sh to run without refusing on an empty dir.
cat > "$NEW/spira/conf.d/SPIRA_ZZTEST_SEED" <<'EOF'
TYPE=string
GROUP=zztest
DOC=seed key so the fixture registry is never empty
DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'
    : "${SPIRA_ZZTEST_SEED:=seed}"
SPIRA_CONF_DEFAULT_EOF
EOF
( cd "$NEW" && git init -q -b base \
    && git add spira && git commit -q -m base ) >/dev/null

new_key_file() {
    local name="$1" default="$2"
    cat <<EOF2
TYPE=string
GROUP=zztest
DOC=$name fixture key
DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'
    : "\${$name:=$default}"
SPIRA_CONF_DEFAULT_EOF
EOF2
}

( cd "$NEW" && git checkout -q -b alpha base \
    && new_key_file SPIRA_ZZTEST_ALPHA alpha-value > spira/conf.d/SPIRA_ZZTEST_ALPHA \
    && git add spira/conf.d/SPIRA_ZZTEST_ALPHA && git commit -q -m "add SPIRA_ZZTEST_ALPHA" ) >/dev/null
( cd "$NEW" && git checkout -q -b bravo base \
    && new_key_file SPIRA_ZZTEST_BRAVO bravo-value > spira/conf.d/SPIRA_ZZTEST_BRAVO \
    && git add spira/conf.d/SPIRA_ZZTEST_BRAVO && git commit -q -m "add SPIRA_ZZTEST_BRAVO" ) >/dev/null
( cd "$NEW" && git checkout -q alpha && git merge -q --no-edit bravo ) >"$TMP/new-merge.log" 2>&1
new_rc=$?
wantrc "the registry form merges cleanly" 0 "$new_rc"
[ "$new_rc" -ne 0 ] && cat "$TMP/new-merge.log" >&2

[ -f "$NEW/spira/conf.d/SPIRA_ZZTEST_ALPHA" ]
wantrc "SPIRA_ZZTEST_ALPHA's file survived the merge" 0 "$?"
[ -f "$NEW/spira/conf.d/SPIRA_ZZTEST_BRAVO" ]
wantrc "SPIRA_ZZTEST_BRAVO's file survived the merge" 0 "$?"

# =============================================================================
echo
echo "both keys resolve through the real conf-gen.sh once merged:"
# =============================================================================
( cd "$NEW" && bash spira/conf-gen.sh ) >"$TMP/gen.log" 2>&1
wantrc "conf-gen.sh runs clean against the merged registry" 0 "$?"
GENERATED="$(cat "$NEW/spira/conf.d.keys.generated.sh" 2>/dev/null)"
want "merged allowlist: SPIRA_ZZTEST_ALPHA present" "SPIRA_ZZTEST_ALPHA" "$GENERATED"
want "merged allowlist: SPIRA_ZZTEST_BRAVO present" "SPIRA_ZZTEST_BRAVO" "$GENERATED"
DEFAULTS="$(cat "$NEW/spira/conf.d.defaults.generated.sh" 2>/dev/null)"
want "merged defaults: SPIRA_ZZTEST_ALPHA's default statement present" \
    'SPIRA_ZZTEST_ALPHA:=alpha-value' "$DEFAULTS"
want "merged defaults: SPIRA_ZZTEST_BRAVO's default statement present" \
    'SPIRA_ZZTEST_BRAVO:=bravo-value' "$DEFAULTS"

tl_summary
