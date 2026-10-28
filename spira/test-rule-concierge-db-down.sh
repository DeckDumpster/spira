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
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
SPIRA_BD="$(command -v bd-embedded)"
[ -n "$SPIRA_BD" ] || bail "bd-embedded not on PATH"
# SPIRA_DB is registered too: rule.sh/concierge.sh source conf.sh, whose own
# `resolve --sh-all` re-exports every registered key from SPIRA_TOML — overwriting the
# plain env SPIRA_DB below with the complete fixture's bogus default unless declared the
# same way (sfail round 3, pattern 3/7).
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_BD="$SPIRA_BD" SPIRA_DB="$DB"

# batch-job: fixture bd call against the suite's throwaway store
bd -C "$DB" remember --key law-dbdown-seed "Seed statute so the reachable case is not itself empty." >/dev/null 2>&1 \
    || bail "could not seed the fixture"

LAW_FIX="$(mktemp -d)"
git -C "$LAW_FIX" init -q && mkdir "$LAW_FIX/law"
printf '[[part]]\nnumber = 1\nname = "Fixture"\nslug = "fixture"\nscope = "Fixture Part."\nslugs = []\n' > "$LAW_FIX/law/toc.toml"
git -C "$LAW_FIX" add -A && git -C "$LAW_FIX" -c user.name=t -c user.email=t@t commit -q -m fixture
# enact files a bead for its law/ commit and queue-submits it; these stand in for the store and a round.
SHIM_BIN="$(mktemp -d)"
printf '#!/bin/sh\necho x >> "%s/filed"\necho "{\\"id\\":\\"sp-fix$(wc -l < "%s/filed" | tr -d " ")\\"}"\n' "$SHIM_BIN" "$SHIM_BIN" > "$SHIM_BIN/bead.sh"
printf '#!/bin/sh\n[ "$1" = submit ] || exit 2\ngit -C "%s" merge -q --ff-only "$2"\n' "$LAW_FIX" > "$SHIM_BIN/queue"
chmod +x "$SHIM_BIN/bead.sh" "$SHIM_BIN/queue"
PATH="$SHIM_BIN:$PATH"; export PATH
# enact commits law/ to the repo SPIRA_REPO names, in a worktree under SPIRA_RUN.
run_rule() { [ "$1" = enact ] && set -- "$@" --part 1; (cd "$LAW_FIX" && SPIRA_REPO="$LAW_FIX" SPIRA_DB="$DB" GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t bash "$RULE_SH" "$@" 2>&1); }

echo "=== rule.sh: against the reachable store (also primes the schema-stamp cache) ==="

out_list_up=$(run_rule list); rc_list_up=$?
is   "list against a reachable store exits 0" "0" "$rc_list_up"
want "and finds the seeded statute"           "law-dbdown-seed" "$out_list_up"

out_show_up=$(run_rule show dbdown-seed); rc_show_up=$?
is   "show against a reachable store exits 0" "0" "$rc_show_up"
want "and returns its text"                   "Seed statute" "$out_show_up"

break_store

echo
echo "=== rule.sh: the same store, now unreachable, with nothing mirrored yet ==="

# A mirror that already holds the book would serve these reads (tested further down); this
# directory has never been filled, so the reads must go to the store and report it.
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_BD="$SPIRA_BD" SPIRA_DB="$DB" SPIRA_STATUTE_DIR="$TMP/never-mirrored"

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

out_enact_down=$(run_rule enact dbdown-new --tool none --reason fixture "New statute while the store is down."); rc_enact_down=$?
if [ "$rc_enact_down" -ne 0 ]; then ok "enact against an unreachable store exits non-zero"
else bad "enact against an unreachable store exits non-zero" "got rc=0"; fi
want "enact names the database path" "$DB" "$out_enact_down"

fix_store
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_BD="$SPIRA_BD" SPIRA_DB="$DB" SPIRA_STATUTE_DIR="$SPIRA_RUN/statutes"

# The key survives: every command above refused to touch the book while it was unreachable.
out_list_restored=$(run_rule list)
want "list after recovery still finds the seeded statute (nothing above wrote through the outage)" \
    "law-dbdown-seed" "$out_list_restored"

echo
echo "=== concierge.sh: the statute book rendering empty vs. the store being unreachable ==="

FX="$TMP/fx"; mkdir -p "$FX/runes"
# SPIRA_HOME IS THE HOME NOW (locate_home no longer searches): every binary reads
# <home>/conf.d directly, so this fixture home needs one too (sfail round 2, pattern 1).
ln -s "$HERE/conf.d" "$FX/conf.d"
cp "$HERE/runes/concierge.md" "$FX/runes/fx.md"
ln -sf "$(command -v mail)" "$FX/mail"   # the real compiled binary, found on the suite's own PATH (mail is gone, sp-ooh1k)
ln -sf "$HERE/bead.sh" "$FX/bead.sh"
cat > "$FX/runes/fx.rune" <<'EOF'
RUNE_NAME=fx
EOF

run_concierge_brief() {
    # The complete fixture declares a non-empty SPIRA_RUNES_DIR; nothing derives it from
    # SPIRA_HOME any more (sfail round 2, pattern 6) — without this fx.rune is never found.
    # SPIRA_DB via tl_config too (same pattern 3/7 as rule.sh above): concierge.sh sources
    # conf.sh, whose resolve --sh-all would otherwise overwrite the plain env value below.
    # SPIRA_STATUTE_CORE / SPIRA_STATUTE_CORE_LOCAL (one source of config, per Ryan
    # 2026-10-05): the complete fixture declares real slugs for both (e.g.
    # law-absence-needs-a-positive-control) so every registered key is non-empty. This
    # fixture's bd store only ever holds law-dbdown-seed, so concierge.sh's brief found
    # every core slug missing — "demoted to the index" — and refused with rc=1 even against
    # the reachable, non-empty-store positive control. Nothing derives these empty any more,
    # so this suite must say so itself, matching the pre-migration behaviour of an unset env.
    tl_config SPIRA_MEMORIES_CACHE="" SPIRA_RUNES_DIR="$FX/runes" SPIRA_DB="$DB" \
        SPIRA_STATUTE_CORE="" SPIRA_STATUTE_CORE_LOCAL="" SPIRA_STATUTE_DIR="${BRIEF_STATUTE_DIR:-$(mktemp -d "$TMP/brief-statutes.XXXXXX")}"
    SPIRA_HOME="$FX" PATH="$FX:$PATH" CONCIERGE_RUNE=fx \
        bash "$CONCIERGE_SH" brief 2>&1
}

# POSITIVE CONTROL: reachable store, statute still present → composes a brief.
out_brief_up=$(run_concierge_brief); rc_brief_up=$?
is "concierge brief against a reachable, non-empty store exits 0" "0" "$rc_brief_up"

# NEGATIVE CONTROL A: reachable store, genuinely no law- memories left.
bd -C "$DB" forget law-dbdown-seed >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
out_brief_empty=$(run_concierge_brief); rc_brief_empty=$?
if [ "$rc_brief_empty" -ne 0 ]; then ok "concierge brief against a genuinely empty store exits non-zero"
else bad "concierge brief against a genuinely empty store exits non-zero" "got rc=0"; fi
want   "and says the statute book rendered empty"        "rendered empty" "$out_brief_empty"
nowant "and does NOT claim the database is unreachable"  "cannot reach"   "$out_brief_empty"

# NEGATIVE CONTROL B: same store, now unreachable — the case this bead is about.
bd -C "$DB" remember --key law-dbdown-seed "Seed statute so the reachable case is not itself empty." >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
break_store
BRIEF_STATUTE_DIR="$TMP/brief-statutes-unmirrored"
out_brief_down=$(run_concierge_brief); rc_brief_down=$?
if [ "$rc_brief_down" -ne 0 ]; then ok "concierge brief against an unreachable store exits non-zero"
else bad "concierge brief against an unreachable store exits non-zero" "got rc=0"; fi
want   "and names the database"                                  "$DB"            "$out_brief_down"
nowant "and does NOT claim the statute book rendered empty"      "rendered empty" "$out_brief_down"

fix_store

echo
echo "=== statutes are served from files: the store down, the text is the same ==="

MIRROR="$TMP/served-statutes"
HOOK="$TMP/ok-hook.sh"; printf '#!/bin/sh\nexit 0\n' > "$HOOK"; chmod +x "$HOOK"
served_cfg() {
    tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_BD="$SPIRA_BD" SPIRA_DB="$DB" SPIRA_STATUTE_DIR="$MIRROR" SPIRA_WIKI_HOOK="$HOOK" SPIRA_WIKI=""
}
served_cfg
out_enact=$(run_rule enact dbdown-served --tool none --reason fixture "Served from a file while the store is down."); rc_enact=$?
is   "enact against the reachable store exits 0" "0" "$rc_enact"
[ -f "$MIRROR/law-dbdown-served.txt" ] && ok "enact wrote the statute file" || bad "enact wrote the statute file" "no $MIRROR/law-dbdown-served.txt"

break_store
out_show_served=$(run_rule show dbdown-served); rc_show_served=$?
is   "show with the store stopped exits 0"       "0" "$rc_show_served"
want "and prints the statute"                    "Served from a file while the store is down." "$out_show_served"
out_list_served=$(run_rule list); rc_list_served=$?
is   "list with the store stopped exits 0"       "0" "$rc_list_served"
want "and names the statute"                     "law-dbdown-served" "$out_list_served"
BRIEF_STATUTE_DIR="$MIRROR"
out_brief_served=$(run_concierge_brief); rc_brief_served=$?
is   "concierge brief with the store stopped exits 0" "0" "$rc_brief_served"
fix_store
served_cfg

out_enact2=$(run_rule enact dbdown-served --tool none --reason fixture "Amended text, served after enact."); rc_enact2=$?
is   "re-enact exits 0" "0" "$rc_enact2"
want "show returns the new text" "Amended text, served after enact." "$(run_rule show dbdown-served)"
run_rule retire dbdown-served >/dev/null
is   "retire removes the file" "1" "$([ -f "$MIRROR/law-dbdown-served.txt" ]; echo $?)"

tl_summary
