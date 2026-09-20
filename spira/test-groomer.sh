#!/usr/bin/env bash
#
# test-groomer.sh — groomer.sh: graph hygiene operations and the unwanted-close refusal.
#
#   ./test-groomer.sh
#
# WHAT THIS SUITE IS GUARDING
# ---------------------------
# groomer.sh provides four hygiene operations (supersede, close, correct-lane, and the
# composable split/merge that aeons do via bd create + supersede). The central property this
# suite enforces is the one the bead makes a hard requirement: groomer.sh REFUSES to close a
# bead as unwanted, and that refusal is in the code, not a sentence in a brief.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# -------------------------------------------------------
# The unwanted refusal is tested by CALLING it and requiring the exit code to be 2 and the
# output to contain the word "REFUSED". A check that only tests the success cases proves
# nothing about the refusal — a missing case handler that falls through to "usage" (exit 1)
# would pass every success case. The positive control for the refusal IS the refusal: plant
# the offender, require the matcher to say so, then believe it when it is silent.
#
# STUB BD (law-gates-run-in-a-clean-environment)
# ----------------------------------------------
# groomer.sh calls bd for its side effects. A real bd call would require a live Dolt server
# and a seeded database, and would test bd as much as groomer.sh. Instead, SPIRA_BD is set
# to a stub that records its argv to a file and exits 0. The stub proves groomer.sh passed
# the right arguments; bd's own correctness is tested in suites that use testdb.sh.
#
# covers: spira/groomer.sh spira/conf.sh
# defect: sp-gsmx.8
# scar: groomer.sh lacked a hard refusal of unwanted-close; the only barrier against closing a bead as unwanted was a sentence in a brief.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

GROOMSH="$HERE/groomer.sh"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"

# Build a stub bd that records its arguments and returns appropriate responses. The stub
# is queried by checking the recorded argv file; each invocation appends a newline-delimited
# record. For 'show' commands, return JSON with status field. For other commands, just record.
STUB_BD="$T/stub-bd"
BD_LOG="$T/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG_PATH"

# Handle 'show' command with JSON output for testing.
# The command may come in as: bd -C /path/to/db show <id> --json
# or: bd show <id> --json
# Skip -C and its argument if present
shift_cnt=0
if [ "$1" = "-C" ]; then
    shift 2
fi

if [ "$1" = "show" ] && [ "${3:-}" = "--json" ]; then
    # Return a mock JSON response with IN_PROGRESS status by default.
    # If the bead id starts with "closed-", return CLOSED status instead.
    bead_id="$2"
    if [[ "$bead_id" == closed-* ]]; then
        printf '{"id":"%s","status":"CLOSED"}\n' "$bead_id"
    else
        printf '{"id":"%s","status":"IN_PROGRESS"}\n' "$bead_id"
    fi
    exit 0
fi

# All other commands: just record and exit 0
exit 0
STUB
chmod +x "$STUB_BD"

# Run groomer.sh in a clean environment. SPIRA_CONF points to a nonexistent file so no
# real config is read; defaults from conf.sh still apply. SPIRA_BD is the stub so no real
# bd is called. SPIRA_DB is a temp path (bd never runs, so the value does not need to exist).
run_groomer() {
    env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_DB="$T/fixture.db" \
        bash "$GROOMSH" "$@" 2>&1
}

# ==========================================================================================
echo
echo "POSITIVE CONTROL: groomer.sh unwanted is REFUSED (exits 2, prints REFUSED)"
# ==========================================================================================
# PLANT THE OFFENDER. groomer.sh unwanted must exit 2 and say "REFUSED". Without this
# positive control, a script that simply does not have an 'unwanted' case and falls through
# to "usage" (exit 1) would make every OTHER test pass while violating the acceptance
# criterion.
: > "$BD_LOG"
out="$(run_groomer unwanted sp-test)"; rc=$?
is   "unwanted exits 2"               2        "$rc"
want "unwanted output contains REFUSED" "REFUSED" "$out"
# Verify bd was NOT called (the refusal must happen before any bd operation).
is   "bd not called for unwanted"     ""       "$(cat "$BD_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "groomer.sh supersede <id> --with <successor>"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer supersede sp-aaa --with sp-bbb)"; rc=$?
is   "supersede exits 0"                    0                  "$rc"
want "bd called with supersede"             "supersede sp-aaa" "$(cat "$BD_LOG")"
want "bd called with --with successor"      "--with sp-bbb"    "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "groomer.sh close <id> --evidence <text>"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer close sp-ccc --evidence 'The referenced module was deleted in commit abc123')"; rc=$?
is   "close exits 0"            0         "$rc"
want "bd called with close"     "close"   "$(cat "$BD_LOG")"
want "bd called with sp-ccc"    "sp-ccc"  "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "groomer.sh close without --evidence is refused (exits 1)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer close sp-ddd)"; rc=$?
is   "close without evidence exits 1"  1           "$rc"
want "error mentions --evidence"       "--evidence" "$out"
is   "bd not called when evidence missing" ""       "$(cat "$BD_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "groomer.sh correct-lane <id> --lane <lane>"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer correct-lane sp-eee --lane ops)"; rc=$?
is   "correct-lane exits 0"         0              "$rc"
want "bd called with set-state"     "set-state"    "$(cat "$BD_LOG")"
want "bd called with lane=ops"      "lane=ops"     "$(cat "$BD_LOG")"
want "bd called with sp-eee"        "sp-eee"       "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "groomer.sh supersede without --with is refused (exits 1)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer supersede sp-fff)"; rc=$?
is   "supersede without --with exits 1" 1        "$rc"
is   "bd not called for missing --with" ""       "$(cat "$BD_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "groomer.sh depends-on-fix <bug-id> --fix <id> --evidence <text>"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer depends-on-fix sp-bug-1 --fix sp-fix-1 --evidence 'This fix addresses the root cause')"; rc=$?
is   "depends-on-fix exits 0"                 0                    "$rc"
want "bd called with dep add"                 "dep add"            "$(cat "$BD_LOG")"
want "bd called with bug-id"                  "sp-bug-1"           "$(cat "$BD_LOG")"
want "bd called with fix-id"                  "sp-fix-1"           "$(cat "$BD_LOG")"
want "bd called with note"                    "note"               "$(cat "$BD_LOG")"
want "note contains evidence"                 "This fix addresses" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "groomer.sh depends-on-fix without --fix is refused (exits 1)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer depends-on-fix sp-bug-2)"; rc=$?
is   "depends-on-fix without --fix exits 1"  1           "$rc"
want "error mentions --fix"                   "--fix"     "$out"
is   "bd not called when --fix missing"       ""          "$(cat "$BD_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "groomer.sh depends-on-fix without --evidence is refused (exits 1)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer depends-on-fix sp-bug-3 --fix sp-fix-3)"; rc=$?
is   "depends-on-fix without --evidence exits 1" 1           "$rc"
want "error mentions --evidence"                  "--evidence" "$out"
is   "bd not called when --evidence missing"      ""          "$(cat "$BD_LOG" 2>/dev/null)"

# ==========================================================================================
echo
echo "groomer.sh depends-on-fix refuses a closed fix bead (exits 1)"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_groomer depends-on-fix sp-bug-4 --fix closed-fix-1 --evidence 'This fix is closed')"; rc=$?
is   "depends-on-fix with closed fix exits 1"    1           "$rc"
want "error mentions closed"                      "closed"    "$out"
is   "dep add not called for closed fix"          ""          "$(grep '^dep add' "$BD_LOG" 2>/dev/null || true)"

# ==========================================================================================
echo
echo "groomer.sh with no arguments exits 1 (usage)"
# ==========================================================================================
out="$(run_groomer)"; rc=$?
is   "no arguments exits 1" 1 "$rc"

# ==========================================================================================
echo
echo "SPIRA_GROOMER_LABEL is in the conf key list and defaults to 'groom'"
# ==========================================================================================
keys="$(env -i HOME="$T" PATH="/usr/bin:/bin" SPIRA_CONF="$NONE" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_CONF_KEYS"' 2>/dev/null)"
want "SPIRA_GROOMER_LABEL is in the key list" "SPIRA_GROOMER_LABEL" "$keys"

val="$(env -i HOME="$T" PATH="/usr/bin:/bin" SPIRA_CONF="$NONE" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_GROOMER_LABEL"' 2>/dev/null)"
is   "SPIRA_GROOMER_LABEL defaults to groom" "groom" "$val"

# ==========================================================================================
echo
echo "groomer lane is in SPIRA_LANES default"
# ==========================================================================================
lanes="$(env -i HOME="$T" PATH="/usr/bin:/bin" SPIRA_CONF="$NONE" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_LANES"' 2>/dev/null)"
want "groomer lane is in SPIRA_LANES default" "groomer" "$lanes"

# ==========================================================================================
echo
echo "groomer.fayth declares FAYTH_LANE=groomer"
# ==========================================================================================
# Groomer is a party persona, not a task fayth. It must declare a lane so it draws from its
# own capacity rather than from SPIRA_MAX_AEONS.
if [ -f "$HERE/chamber/groomer.fayth" ]; then
    lane_val="$(env -i HOME="$T" PATH="/usr/bin:/bin" SPIRA_CONF="$NONE" SPIRA_DB="$T/db" \
        bash -c '. "'"$HERE"'/conf.sh" && . "'"$HERE"'/chamber/groomer.fayth" && printf "%s" "${FAYTH_LANE:-}"' 2>/dev/null)"
    is   "groomer.fayth FAYTH_LANE=groomer" "groomer" "$lane_val"
else
    bad  "groomer.fayth" "not found at $HERE/chamber/groomer.fayth"
fi

# ==========================================================================================
echo
echo "groomer.fayth declares FAYTH_GROOM_ESCALATION_CHECK=1"
# ==========================================================================================
# The escalation check in aeon.sh is activated by this key. Without it, a groom pass
# that claims ESCALATED without filing an ask bead is never caught.
if [ -f "$HERE/chamber/groomer.fayth" ]; then
    gesc_val="$(env -i HOME="$T" PATH="/usr/bin:/bin" SPIRA_CONF="$NONE" SPIRA_DB="$T/db" \
        bash -c '. "'"$HERE"'/conf.sh" && . "'"$HERE"'/chamber/groomer.fayth" && printf "%s" "${FAYTH_GROOM_ESCALATION_CHECK:-}"' 2>/dev/null)"
    is   "groomer.fayth FAYTH_GROOM_ESCALATION_CHECK=1" "1" "$gesc_val"
else
    bad  "groomer.fayth" "not found at $HERE/chamber/groomer.fayth"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
