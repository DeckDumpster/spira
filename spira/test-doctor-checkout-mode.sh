#!/usr/bin/env bash
#
# test-doctor-checkout-mode.sh — doctor.sh classifies the three checkout modes
# correctly, and the unit manifest offers promote only where it can work.
#
#   ./test-doctor-checkout-mode.sh
#
# WHAT THIS TESTS
# ---------------
# SPIRA_PROD is the directory systemd executes. It can stand in three relations
# to SPIRA_REPO, the checkout being developed, and they are not equally healthy:
#
#   split-checkout   SPIRA_PROD outside SPIRA_REPO. Two trees; promote.sh carries
#                    commits from one to the other. A mid-landing dev tree cannot
#                    reach the running copy.
#   single-checkout  SPIRA_PROD inside SPIRA_REPO. One tree, developed and
#                    executed. A SUPPORTED mode, not an error: a box that reaches
#                    production through a tagged release elsewhere has no second
#                    checkout to promote into, and conf.sh:806 keeps the default
#                    non-colon precisely so "I want no split" stays expressible.
#                    It carries one real cost and the operator is owed it in
#                    words: an in-progress edit is live immediately.
#   unset            No production directory at all. That IS an error — nothing
#                    can say what systemd should execute.
#
# doctor previously failed the harness outright on single-checkout, which made
# the mode unusable while spira-skew.service's own comment described it as
# supported. Three of that FAIL's stated reasons no longer hold: promote.sh has
# no caller in single-checkout mode, and skew's two-checkout question is filed
# separately. The surviving reason is a warning, not a refusal.
#
# THE MANIFEST HALF. promote.sh cannot work in single-checkout mode — its source
# and destination are the same directory. Leaving its units in the manifest makes
# every doctor run report an un-suspended fatal for a unit that is correctly
# absent, and a check that is always red is a check nobody reads. So the manifest
# offers promote only in split-checkout mode, and records it in OPTIONAL
# otherwise, which is the same idiom dolt-beads.service already uses.
#
# covers: spira/doctor.sh spira/conf.sh systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REPO="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-doctor-checkout-mode.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"; mkdir -p "$BIN" "$TMP/db/.beads" "$TMP/run"
touch "$TMP/watchers-empty"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

FAKE_HOME="$TMP/home"; mkdir -p "$FAKE_HOME/.config/systemd/user"

# Every unit reports enabled and active, so nothing from the OTHER doctor
# sections masks the lines this suite is reading.
cat > "$BIN/sc" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n' ;;
    *"is-enabled"*)          printf 'enabled\n'; exit 0 ;;
esac
exit 0
MOCK
chmod +x "$BIN/sc"

# run_doctor <spira-prod-value>
# SPIRA_PROD is ALWAYS passed, empty string included. conf.sh fills it from a
# derived default only when the key is UNSET (the no-colon form at conf.sh:806),
# so omitting it here would silently test the derived split-checkout path while
# claiming to test the empty one.
run_doctor() {
    local prod="$1"; shift
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/sc" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        SPIRA_PROD="$prod" \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

# The "promote" section only — so an unrelated FAIL elsewhere in doctor's output
# can never be mistaken for this section's verdict.
promote_section() { sed -n '/^promote$/,/^$/p'; }

# ===========================================================================
echo
echo "positive control — the section CAN emit a FAIL:"
# ===========================================================================
# Believing a quiet single-checkout run requires first seeing this section
# speak up (law-absence-needs-a-positive-control).
unset_out="$(run_doctor "" | promote_section)"
want "SPIRA_PROD unset is a FAIL"            "FAIL" "$unset_out"
want "the FAIL names the missing key"        "SPIRA_PROD" "$unset_out"

# ===========================================================================
echo
echo "single-checkout is supported — a WARN, never a FAIL:"
# ===========================================================================
single_out="$(run_doctor "$HERE" | promote_section)"
want   "names the mode"                      "single-checkout" "$single_out"
want   "warns"                               "warn" "$single_out"
nowant "does not fail the harness"           "FAIL" "$single_out"
want   "names the cost the operator accepts" "live" "$single_out"

# ===========================================================================
echo
echo "split-checkout still passes, and a missing prod tree still warns:"
# ===========================================================================
mkdir -p "$TMP/prod/spira"
split_out="$(run_doctor "$TMP/prod/spira" | promote_section)"
want   "split-checkout reported OK"          "split-checkout mode" "$split_out"
nowant "split-checkout is not a FAIL"        "FAIL" "$split_out"

absent_out="$(run_doctor "$TMP/prod-missing/spira" | promote_section)"
want   "a prod tree that does not exist yet warns" "warn" "$absent_out"
nowant "and is not fatal"                          "FAIL" "$absent_out"

# ===========================================================================
echo
echo "the manifest offers promote only where promote can work:"
# ===========================================================================
# units.sh is a library: source it and read the arrays it defines.
manifest() {
    local prod="$1"
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent SPIRA_PATH="$BIN" SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" SPIRA_INSTANCE=prod SPIRA_PROD="$prod" \
        SPIRA_WATCHERS="$TMP/watchers-empty" SPIRA_REPO_MAP=/nonexistent \
        bash -c '
            set +u
            . "'"$REPO"'/spira/conf.sh" >/dev/null 2>&1
            SPIRA_PROD="'"$prod"'"
            . "'"$REPO"'/systemd/units.sh" >/dev/null 2>&1
            printf "UNITS:%s\nENABLE:%s\nOPTIONAL:%s\n" \
                "${UNITS[*]}" "${ENABLE[*]}" "${OPTIONAL[*]}"
        ' 2>/dev/null
}

# EACH ARRAY IS READ ON ITS OWN LINE. Asserting against the whole blob would let the
# OPTIONAL line — which names promote on purpose — satisfy a search meant for UNITS, and
# the test would pass without the change it exists to check.
man_line() { printf '%s\n' "$2" | sed -n "s/^$1://p"; }
split_man="$(manifest "$TMP/prod/spira")"
want   "split-checkout installs promote" "spira-promote.timer" \
       "$(man_line UNITS "$split_man")"
want   "split-checkout enables promote"  "spira-promote-prod.timer" \
       "$(man_line ENABLE "$split_man")"

single_man="$(manifest "$HERE")"
nowant "single-checkout installs no promote unit" "spira-promote.service" \
       "$(man_line UNITS "$single_man")"
nowant "single-checkout enables no promote timer" "spira-promote-prod.timer" \
       "$(man_line ENABLE "$single_man")"
want   "and records it as deliberately declined"  "spira-promote.timer" \
       "$(man_line OPTIONAL "$single_man")"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
