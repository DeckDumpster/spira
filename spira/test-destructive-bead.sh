#!/usr/bin/env bash
#
# test-destructive-bead.sh — bdq create refuses a bead whose title or description contains
#   vocabulary indicating the procedure halts the harness, unless needs-ryan is present.
#
#   ./test-destructive-bead.sh
#
# THE DEFECT THIS PREVENTS. sp-6ylz had "needs the world stopped" in its title and was
# dispatchable anyway. An aeon claimed it and ran world.sh stop from step 2, killing the
# sentinel, ops timer, both watchers, and three live aeons. The filer had written the danger
# into the title and still filed it dispatchable. The fence converts the symptom — the title
# — into the mechanism. (sp-6hdi, law-destructive-beads-are-never-dispatchable)
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control). The bad-vocabulary case is
# tested before any innocuous case, confirming the check fires on a known offender.
#
# defect: sp-6hdi
# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-destructive-bead.sh"

# Source conf.sh to read SPIRA_ASK_LABEL rather than hardcoding a value — the shipped default
# (needs-operator) differs from a configured installation, so a hardcoded label never matches
# the fence's check and the bypass test fails on a clean environment. Capture it before the
# env -i helpers drop it from the environment.
# shellcheck source=spira/conf.sh
. "$HERE/conf.sh" 2>/dev/null || true
ASK_LABEL="${SPIRA_ASK_LABEL:-needs-operator}"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A stub bd that records its call and exits 0, so passing cases reach it.
STUB_BD="$TMP/bd"
printf '#!/usr/bin/env bash\nprintf "bd-called\\n"; exit 0\n' > "$STUB_BD"
chmod +x "$STUB_BD"

# Run _bdq_check_destructive directly for fine-grained tests of each pattern.
check_title() {  # check_title <title> <labels> -> combined stdout+stderr
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/nomap" SPIRA_BD="$STUB_BD" \
        SPIRA_ASK_LABEL="$ASK_LABEL" \
        bash -c '. "$1/lib.sh"; _bdq_check_destructive create "$2" --labels "$3"' \
            -- "$HERE" "$1" "$2" 2>&1
}

# Same, but puts the text in the description rather than the title.
check_desc() {  # check_desc <desc> <labels> -> combined stdout+stderr
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/nomap" SPIRA_BD="$STUB_BD" \
        SPIRA_ASK_LABEL="$ASK_LABEL" \
        bash -c '. "$1/lib.sh"; _bdq_check_destructive create "clean title" -d "$2" --labels "$3"' \
            -- "$HERE" "$1" "$2" 2>&1
}

# Run bdq create end-to-end, with no repo: label so the repo-label check passes trivially.
run_bdq() {  # run_bdq <title> <labels> -> combined stdout+stderr
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/nomap" SPIRA_BD="$STUB_BD" BD_TIMEOUT=10 \
        SPIRA_ASK_LABEL="$ASK_LABEL" \
        bash -c '. "$1/lib.sh"; bdq create "$2" --labels "$3"' \
            -- "$HERE" "$1" "$2" 2>&1
}

# ==========================================================================
echo
echo "POSITIVE CONTROL: sp-6ylz exact title is refused:"
# ==========================================================================

SP6YLZ="needs the world stopped"
out="$(check_title "$SP6YLZ" "spira,plan" || true)"
want "sp-6ylz title: refused"               "needs-ryan"      "$out"
want "sp-6ylz title: names matched phrase"  "world stopped"   "$out"

# ==========================================================================
echo
echo "each destructive pattern is refused:"
# ==========================================================================

out="$(check_title "Run world.sh stop immediately" "spira,plan" || true)"
want "world.sh stop: refused"              "needs-ryan"  "$out"
want "world.sh stop: names the phrase"    "world.sh"    "$out"

out="$(check_title "spira-world down for maintenance" "spira,plan" || true)"
want "spira-world down: refused"           "needs-ryan"  "$out"
want "spira-world down: names the phrase"  "spira-world" "$out"

out="$(check_title "run systemd/install.sh to deploy" "spira,plan" || true)"
want "systemd/install.sh: refused"         "needs-ryan"        "$out"
want "systemd/install.sh: names the phrase" "systemd/install"  "$out"

out="$(check_title "systemctl daemon-reload after edit" "spira,plan" || true)"
want "daemon-reload: refused"              "needs-ryan"     "$out"
want "daemon-reload: names the phrase"     "daemon-reload"  "$out"

out="$(check_title "systemctl stop spira-sentinel.timer" "spira,plan" || true)"
want "systemctl stop spira-*: refused"    "needs-ryan"  "$out"
want "systemctl stop spira-*: names it"  "systemctl"   "$out"

out="$(check_title "systemctl restart spira-ops.service" "spira,plan" || true)"
want "systemctl restart spira-*: refused"  "needs-ryan"  "$out"
want "systemctl restart spira-*: names it" "systemctl"   "$out"

out="$(check_title "apply a schema migration to beads" "spira,plan" || true)"
want "schema migration: refused"           "needs-ryan"  "$out"
want "schema migration: names the phrase"  "schema"      "$out"

out="$(check_title "schema migrate the users table" "spira,plan" || true)"
want "schema migrate: refused"             "needs-ryan"  "$out"

# ==========================================================================
echo
echo "vocabulary in description is also refused:"
# ==========================================================================

out="$(check_desc "Step 1: run world.sh stop to halt the loop" "spira,plan" || true)"
want "destructive desc: refused"           "needs-ryan"  "$out"
want "destructive desc: names the phrase"  "world.sh"    "$out"

# ==========================================================================
echo
echo "ask label bypasses the fence:"
# ==========================================================================

out="$(check_title "$SP6YLZ" "spira,plan,$ASK_LABEL" 2>&1)" || true
nowant "needs-ryan: not refused"  "needs-ryan label" "$out"

out="$(check_title "Run world.sh stop" "$ASK_LABEL" 2>&1)" || true
nowant "needs-ryan only label: not refused"  "needs-ryan label" "$out"

# ==========================================================================
echo
echo "innocent text is not refused:"
# ==========================================================================

out="$(check_title "install the new persona and update config" "spira,plan" 2>&1)" || true
nowant "innocent install: not refused"  "needs-ryan"  "$out"

out="$(check_title "stop the failing queue probe" "spira,plan" 2>&1)" || true
nowant "stop without spira-: not refused"  "needs-ryan"  "$out"

out="$(check_title "systemctl status spira-sentinel" "spira,plan" 2>&1)" || true
nowant "systemctl status (not stop/restart): not refused"  "needs-ryan"  "$out"

out="$(check_title "investigate why schema is missing" "spira,plan" 2>&1)" || true
nowant "schema without migrate: not refused"  "needs-ryan"  "$out"

out="$(check_title "restart the web server" "spira,plan" 2>&1)" || true
nowant "restart without spira-*: not refused"  "needs-ryan"  "$out"

# ==========================================================================
echo
echo "bdq create end-to-end: destructive title refused, bd not called:"
# ==========================================================================

e2e_bad="$(run_bdq "Run world.sh stop to apply config" "spira,plan" || true)"
want   "bdq: refused"         "needs-ryan"  "$e2e_bad"
nowant "bdq: bd not called"   "bd-called"   "$e2e_bad"

e2e_ok="$(run_bdq "Run world.sh stop with approval" "spira,plan,$ASK_LABEL" 2>&1)" || true
want "bdq with needs-ryan: bd called"  "bd-called"  "$e2e_ok"

# ==========================================================================
echo
echo "DELETE FROM schema_migrations refused even with needs-ryan:"
# ==========================================================================
# _bdq_check_schema_delete runs AFTER the needs-ryan bypass in _bdq_check_destructive,
# so escalation beads that recommend this SQL cannot be filed regardless of needs-ryan.
# This is the sp-1khst fence: three escalation beads carrying needs-ryan were approved.

check_schema_title() {  # check_schema_title <title> <labels> -> combined stdout+stderr
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/nomap" SPIRA_BD="$STUB_BD" \
        bash -c '. "$1/lib.sh"; _bdq_check_schema_delete create "$2" --labels "$3"' \
            -- "$HERE" "$1" "$2" 2>&1
}
check_schema_desc() {  # check_schema_desc <desc> <labels> -> combined stdout+stderr
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/nomap" SPIRA_BD="$STUB_BD" \
        bash -c '. "$1/lib.sh"; _bdq_check_schema_delete create "clean title" -d "$2" --labels "$3"' \
            -- "$HERE" "$1" "$2" 2>&1
}

# Positive control: the exact SQL from the retired SOP is refused in a title.
out="$(check_schema_title "DELETE FROM schema_migrations WHERE version > 53" "spira,plan" || true)"
want "schema-delete title: refused"         "schema_migrations"         "$out"
want "schema-delete title: says measure"    "bd migrate schema"         "$out"

# Refused in description too.
out="$(check_schema_desc "Step 1: DELETE FROM schema_migrations WHERE version > 53" "spira,plan" || true)"
want "schema-delete desc: refused"          "schema_migrations"         "$out"

# Refused WITH needs-ryan — this is the gap the previous checks had.
out="$(check_schema_title "DELETE FROM schema_migrations WHERE version > 53" "spira,plan,needs-ryan" || true)"
want "schema-delete with needs-ryan: still refused"  "schema_migrations"  "$out"

# Case variations.
out="$(check_schema_title "delete from schema_migrations where version > 53" "spira,plan" || true)"
want "lowercase: refused"  "schema_migrations"  "$out"

# Innocent text is not refused.
out="$(check_schema_title "check schema migration count with bd migrate schema" "spira,plan" 2>&1)" || true
nowant "bd migrate schema: not refused"  "schema_migrations"  "$out"

out="$(check_schema_title "investigate schema mismatch between v61 and v53" "spira,plan" 2>&1)" || true
nowant "schema mismatch without DELETE: not refused"  "schema_migrations"  "$out"

# End-to-end: bdq refuses even with needs-ryan.
e2e_schema="$(run_bdq "DELETE FROM schema_migrations WHERE version > 53" "spira,plan,needs-ryan" || true)"
want   "bdq schema-delete with needs-ryan: refused"      "schema_migrations"  "$e2e_schema"
nowant "bdq schema-delete with needs-ryan: bd not called" "bd-called"         "$e2e_schema"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
