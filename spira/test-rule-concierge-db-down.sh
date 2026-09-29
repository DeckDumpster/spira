#!/usr/bin/env bash
#
# test-rule-concierge-db-down.sh — a failed or unreachable statute-book database must be
#   reported as such, by name, never mistaken for a genuinely empty statute book.
#
# THE DEFECT (sp-n93br)
# ----------------------
# rule.sh's `list` and `retire` piped `bd memories --json 2>/dev/null` straight into a
# python3 json.load with no check of bd's own exit status: a database bd could not reach
# produced empty stdin, and json.load raised a raw traceback instead of an error naming the
# store. concierge.sh made the same conflation one level up — an unreachable store and a
# genuinely empty one both rendered `render_memories` empty, and both were reported with the
# same "statute book rendered empty" line, sending the reader toward `rule.sh list`, which
# then produced the traceback above.
#
# WHAT PROVES THE FIX: `bd`'s own exit status, checked before anything is parsed or judged
# empty, distinguishes the two. A store that answers with nothing real is empty; a store
# that cannot be opened is unreachable — the message named must say which.
#
# PAIRS (law-absence-needs-a-positive-control): each unreachable-store case is paired with
# the same command run first against the same, still-reachable store, so a matcher that
# always fires (or always stays silent) cannot pass unnoticed.
#
# REAL DB, MADE UNREACHABLE FOR REAL (law-prefer-the-real-dependency): rather than stub bd's
# exit code, this suite fixes an embedded fixture's own Dolt directory to 000 permissions,
# so `bd` genuinely cannot open it and produces its own, real error text.
#
# testdb-mode: embedded — breaks a local directory's permissions to make it unreachable;
#   server mode's unreachability is a TCP port, not a directory, so this suite is exempted
#   from the batch-wide server fixture (testenv/fixture.rs's `embedded_only`, sp-gjx1b) —
#   without this, sp-34ru2's default made $EMBDIR never exist and this suite always skipped.
# tier: T2
# covers: rule.sh concierge.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$(cd "$HERE/.." && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require test-rule-concierge-db-down
testdb_up dbdown || bail "testdb_up failed"

RULE_SH="$HARNESS/rule.sh"
CONCIERGE_SH="$HARNESS/concierge.sh"
[ -f "$RULE_SH" ] || bail "rule.sh not found at $RULE_SH"
[ -x "$CONCIERGE_SH" ] || bail "concierge.sh not found at $CONCIERGE_SH"

DB="$SPIRA_DB"
EMBDIR="$DB/.beads/embeddeddolt"
# Embedded mode only: server mode's unreachability lives behind a TCP port this suite does
# not own, not a local directory it can lock. On a box without bd-embedded, testdb.sh itself
# already falls back to server mode, so absence of $EMBDIR here is that fallback, not a bug.
[ -d "$EMBDIR" ] || skip "fixture is not embedded-mode (no $EMBDIR) — this suite needs a local store it can make unreadable"

TMP="$(mktemp -d)"
break_store() { chmod 000 "$EMBDIR"; }
fix_store()   { chmod 755 "$EMBDIR" 2>/dev/null || true; }
trap 'fix_store; testdb_drop; rm -rf "$TMP"' EXIT INT TERM

# A resolved, absolute SPIRA_BD (as every real caller has, via `command -v bd`) so conf.sh's
# own schema-stamp cache behaves as it does in production: primed by the first call below
# against the working store, then trusted — without being re-verified — by the calls made
# after the store is broken. That is the actual shape of the scar: the box had already
# passed its schema check once, then the server under it died.
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_BD="$(command -v bd-embedded)"
[ -n "$SPIRA_BD" ] || bail "bd-embedded not on PATH"

bd -C "$DB" remember --key law-dbdown-seed "Seed statute so the reachable case is not itself empty." >/dev/null 2>&1 \
    || bail "could not seed the fixture"

run_rule() { SPIRA_DB="$DB" SPIRA_RUN="$SPIRA_RUN" SPIRA_BD="$SPIRA_BD" bash "$RULE_SH" "$@" 2>&1; }

echo "=== rule.sh: against the reachable store (also primes the schema-stamp cache) ==="

out_list_up=$(run_rule list); rc_list_up=$?
is   "list against a reachable store exits 0" "0" "$rc_list_up"
want "and finds the seeded statute"           "law-dbdown-seed" "$out_list_up"

out_show_up=$(run_rule show dbdown-seed); rc_show_up=$?
is   "show against a reachable store exits 0" "0" "$rc_show_up"
want "and returns its text"                   "Seed statute" "$out_show_up"

break_store

echo
echo "=== rule.sh: the same store, now unreachable ==="

out_list_down=$(run_rule list); rc_list_down=$?
if [ "$rc_list_down" -ne 0 ]; then ok "list against an unreachable store exits non-zero"
else bad "list against an unreachable store exits non-zero" "got rc=0"; fi
want   "list names the database path"                   "$DB"            "$out_list_down"
nowant "list does NOT claim the statute book is empty"  "rendered empty" "$out_list_down"
nowant "list does NOT crash with a python traceback"    "Traceback"      "$out_list_down"
nowant "list does NOT surface a bare JSONDecodeError"   "JSONDecodeError" "$out_list_down"

out_show_down=$(run_rule show dbdown-seed); rc_show_down=$?
if [ "$rc_show_down" -ne 0 ]; then ok "show against an unreachable store exits non-zero"
else bad "show against an unreachable store exits non-zero" "got rc=0"; fi
want   "show names the database path"                              "$DB"        "$out_show_down"
nowant "show does NOT report 'no statute' for an unreachable store" "no statute" "$out_show_down"

out_retire_down=$(run_rule retire dbdown-seed); rc_retire_down=$?
if [ "$rc_retire_down" -ne 0 ]; then ok "retire against an unreachable store exits non-zero"
else bad "retire against an unreachable store exits non-zero" "got rc=0"; fi
want   "retire names the database path"                "$DB"       "$out_retire_down"
nowant "retire does NOT crash with a python traceback" "Traceback" "$out_retire_down"

out_enact_down=$(run_rule enact dbdown-new "New statute while the store is down."); rc_enact_down=$?
if [ "$rc_enact_down" -ne 0 ]; then ok "enact against an unreachable store exits non-zero"
else bad "enact against an unreachable store exits non-zero" "got rc=0"; fi
want "enact names the database path" "$DB" "$out_enact_down"

fix_store

# The key survives: every command above refused to touch the book while it was unreachable.
out_list_restored=$(run_rule list)
want "list after recovery still finds the seeded statute (nothing above wrote through the outage)" \
    "law-dbdown-seed" "$out_list_restored"

echo
echo "=== concierge.sh: the statute book rendering empty vs. the store being unreachable ==="

FX="$TMP/fx"; mkdir -p "$FX/chamber"
cp "$HERE/chamber/concierge.md" "$FX/chamber/fx.md"
ln -sf "$HERE/mail.sh" "$FX/mail.sh"
ln -sf "$HERE/bead.sh" "$FX/bead.sh"
cat > "$FX/chamber/fx.fayth" <<'EOF'
FAYTH_NAME=fx
EOF

run_concierge_brief() {
    SPIRA_DB="$DB" SPIRA_RUN="$SPIRA_RUN" SPIRA_BD="$SPIRA_BD" \
        SPIRA_HOME="$FX" CONCIERGE_FAYTH=fx SPIRA_MEMORIES_CACHE="" \
        bash "$CONCIERGE_SH" brief 2>&1
}

# POSITIVE CONTROL: reachable store, statute still present → composes a brief.
out_brief_up=$(run_concierge_brief); rc_brief_up=$?
is "concierge brief against a reachable, non-empty store exits 0" "0" "$rc_brief_up"

# NEGATIVE CONTROL A: reachable store, genuinely no law- memories left.
bd -C "$DB" forget law-dbdown-seed >/dev/null 2>&1
out_brief_empty=$(run_concierge_brief); rc_brief_empty=$?
if [ "$rc_brief_empty" -ne 0 ]; then ok "concierge brief against a genuinely empty store exits non-zero"
else bad "concierge brief against a genuinely empty store exits non-zero" "got rc=0"; fi
want   "and says the statute book rendered empty"        "rendered empty" "$out_brief_empty"
nowant "and does NOT claim the database is unreachable"  "cannot reach"   "$out_brief_empty"

# NEGATIVE CONTROL B: same store, now unreachable — the case this bead is about.
bd -C "$DB" remember --key law-dbdown-seed "Seed statute so the reachable case is not itself empty." >/dev/null 2>&1
break_store
out_brief_down=$(run_concierge_brief); rc_brief_down=$?
if [ "$rc_brief_down" -ne 0 ]; then ok "concierge brief against an unreachable store exits non-zero"
else bad "concierge brief against an unreachable store exits non-zero" "got rc=0"; fi
want   "and names the database"                                  "$DB"            "$out_brief_down"
nowant "and does NOT claim the statute book rendered empty"      "rendered empty" "$out_brief_down"

fix_store

tl_summary
