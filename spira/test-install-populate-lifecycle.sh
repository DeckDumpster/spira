#!/usr/bin/env bash
#
# test-install-populate-lifecycle.sh — spira-install's one-time lifecycle population
# (phase 4.6, sp-k62xz8): Spira installed on top of an existing beads database gives every
# bead in it a lifecycle row, in the state its bd status and landing evidence imply, by
# spira-lc's own classifier (`classify --every-bead`). Driven through the installer's own
# entry point, `spira-install --populate-lifecycle` — the same populate_phase every full
# install runs right after the lifecycle store is built.
#
# A REAL bd (testdb.sh) holding beads in several states, a REAL lifecycle store
# (testlib/lc-fixture.sh), and a real scratch git repository carrying a landing commit.
#
# WHAT THIS PROVES
#   - PLANTED CONTROL: with the store built and the beads present, nothing has rows; and with
#     population disabled (SPIRA_INSTALL_LC_POPULATE_CONSIDERED) the installer says so and
#     the rows are still absent — so the rows asserted afterwards are population's doing.
#   - Every bead gets a row in the state its record implies: open -> READY, in_progress with
#     no live holder -> READY, closed with a `spira: land <id>` commit on base -> LANDED,
#     closed with no landing evidence -> DROPPED; a bead with NO repo: label is populated too
#     (bd status alone).
#   - The installer reports the counts: beads, rows created, already present.
#   - Non-destructive: a lifecycle row that existed before install is left exactly as it was,
#     and bd is only ever read (every bd call the classifier makes is list/show).
#   - Idempotent: a second run creates nothing, and leaves the bead table and the event log
#     exactly as the first run left them.
#   - A beads DB from somewhere else: a bead labelled with a repository this config does not
#     have gets a row by its bd status alone (open -> READY, closed -> DROPPED — never LANDED,
#     even with a `spira: land` line for it in a configured repository), the run exits 0, and
#     install warns `unknown-repo: N (ids)`.
#   - Loud: a bead that cannot be classified (two repo: labels — genuinely ambiguous) fails
#     the phase, exit 2, naming the bead.
#
# defect: sp-k62xz8
# tier: T2
# covers: install/src/bin/install.rs install/src/populate.rs spira-lc/src/classify_cmd.rs spira-lc/src/bd_facts.rs spira-lc/src/repo_config.rs spira-lc/src/client.rs
# timeout: 300
# hermetic-ok: uses a fixture bd database and a private lifecycle dolt server; no systemd
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-install-populate-lifecycle.sh"

command -v git >/dev/null 2>&1 || skip "git not on PATH"
command -v spira-install >/dev/null 2>&1 || bail "spira-install is not on PATH (the tree's build provides it)"

. "$HERE/testdb.sh"
testdb_require test-install-populate-lifecycle
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up test-install-populate-lifecycle || bail "testdb_up failed"
[ -n "${SPIRA_DB:-}" ] || bail "testdb_up did not hand back a throwaway SPIRA_DB"

. "$HERE/testlib/lc-fixture.sh"
lcfix_up || bail "lc-fixture: the lifecycle store did not come up"
trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM

rows()   { lcfix_sql -q "SELECT COUNT(*) AS n FROM $1" -r csv 2>/dev/null | sed -n 2p; }
dump()   { lcfix_sql -q "SELECT bead_id, state, IFNULL(tip,''), holds, IFNULL(reason,''), version, updated_at FROM bead ORDER BY bead_id" -r csv 2>/dev/null; }
reason() { lcfix_reason "$1"; }

# ── the scratch repository: one bead landed on main by a landing line ───────────────
GITREPO="$TMP/gitrepo"
mkdir -p "$GITREPO"
git -C "$GITREPO" init -q -b main
git -C "$GITREPO" config user.email test@example.invalid
git -C "$GITREPO" config user.name test
git -C "$GITREPO" commit -q --allow-empty -m "initial"
printf 'the fix\n' > "$GITREPO/fix.txt"
git -C "$GITREPO" add fix.txt
git -C "$GITREPO" commit -q -m "a batch" -m "spira: land sp-landed"
# A landing line for a bead whose own label names ANOTHER (unconfigured) repository: it must
# not make that bead LANDED — an unknown repository has no evidence to read.
git -C "$GITREPO" commit -q --allow-empty -m "another batch" -m "spira: land sp-gone-landed"

# ── config: the run/queue dirs, a repository table layer, and a bd that logs its argv ──
mkdir -p "$TMP/run/queue"
REAL_BD="$(command -v "$SPIRA_BD" 2>/dev/null || printf '%s' "$SPIRA_BD")"
BD_LOG="$TMP/bd-calls.log"
cat > "$TMP/bd-logged" <<EOF
#!/usr/bin/env bash
# The classifier's bd, recorded: every subcommand it runs, then the real bd.
for a in "\$@"; do case "\$a" in -C) skip=1 ;; *) if [ -n "\${skip:-}" ]; then skip=; elif [ -z "\${sub:-}" ]; then sub="\$a"; fi ;; esac; done
printf '%s\n' "\$sub" >> "$BD_LOG"
exec "$REAL_BD" "\$@"
EOF
chmod +x "$TMP/bd-logged"
tl_config SPIRA_RUN="$TMP/run" SPIRA_QUEUE_DIR="$TMP/run/queue" SPIRA_BD="$TMP/bd-logged" SPIRA_ASK_LABEL=ask-x \
    || bail "cannot declare the suite's config"
printf '[repo.demo]\npath = "%s"\nmode = "queue"\nbase = "main"\n' "$GITREPO" > "$TMP/repos.toml"
export SPIRA_TOML="$SPIRA_TOML:$TMP/repos.toml"

# ── the beads database Spira is installed on top of ─────────────────────────────────
testdb_seed <<JSONL
{"id":"sp-open","title":"open bead","type":"task","status":"open","labels":["repo:demo"]}
{"id":"sp-wip","title":"in progress, holder long gone","type":"task","status":"in_progress","labels":["repo:demo"]}
{"id":"sp-landed","title":"closed, landed on main","type":"task","status":"closed","labels":["repo:demo"]}
{"id":"sp-unlanded","title":"closed, never landed","type":"task","status":"closed","labels":["repo:demo"]}
{"id":"sp-bare","title":"no repo label at all","type":"task","status":"open"}
{"id":"sp-kept","title":"already has a lifecycle row","type":"task","status":"open","labels":["repo:demo"]}
JSONL
wantrc "the beads database seeds cleanly" 0 $?

# A lifecycle row that predates install: population must leave it exactly as it is.
lcfix_seed sp-kept WORKING deadbeef || bail "could not seed sp-kept's row"
KEPT_BEFORE="$(lcfix_sql -q "SELECT state, tip, version, updated_at FROM bead WHERE bead_id='sp-kept'" -r csv 2>/dev/null | sed -n 2p)"

want "the pre-existing row reads back (so 'untouched' below is not empty == empty)" "WORKING" "$KEPT_BEFORE"

install_populate() { spira-install --populate-lifecycle 2>&1; }

# ── PLANTED CONTROL ─────────────────────────────────────────────────────────────────
is "control: before install, only the pre-existing row is in the bead table" "1" "$(rows bead)"
OUT="$(SPIRA_INSTALL_LC_POPULATE_CONSIDERED=1 install_populate)"; RC=$?
wantrc "control: population disabled still exits 0" 0 "$RC"
want "control: the installer says population was NOT run" "lifecycle population NOT run" "$OUT"
is "control: with population disabled, the beads still have no rows" "1" "$(rows bead)"
is "control: sp-open has no row" "" "$(lcfix_state sp-open)"

# ── the real population ─────────────────────────────────────────────────────────────
: > "$BD_LOG"
OUT="$(install_populate)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/  | /'
wantrc "population exits 0" 0 "$RC"
want "it reports the counts" "lifecycle population: 6 bead(s) in the database, 5 row(s) created, 1 already present" "$OUT"
is "every bead now has a row" "6" "$(rows bead)"
is "open -> READY" "READY" "$(lcfix_state sp-open)"
is "open is decided by its bd status" "open-status" "$(reason sp-open)"
is "in_progress with no live holder -> READY" "READY" "$(lcfix_state sp-wip)"
is "closed with a landing commit on base -> LANDED" "LANDED" "$(lcfix_state sp-landed)"
is "LANDED by the landing line, not by bd status" "terminal-landing-line" "$(reason sp-landed)"
is "closed with no landing evidence -> DROPPED" "DROPPED" "$(lcfix_state sp-unlanded)"
is "a bead with no repo: label is populated too" "READY" "$(lcfix_state sp-bare)"
is "the row that predates install is untouched" "$KEPT_BEFORE" \
    "$(lcfix_sql -q "SELECT state, tip, version, updated_at FROM bead WHERE bead_id='sp-kept'" -r csv 2>/dev/null | sed -n 2p)"
[ -s "$BD_LOG" ] && ok "the classifier ran through the configured bd" || bad "the classifier ran through the configured bd" "no bd call was logged"
WRITES="$(grep -vxE 'list|show' "$BD_LOG" | sort -u | tr '\n' ' ')"
is "bd is only read: every call is list or show" "" "$WRITES"

TABLE_AFTER_FIRST="$(dump)"
EVENTS_AFTER_FIRST="$(rows event)"
want "the bead table dump reads back (so 'byte-for-byte' below is not empty == empty)" "sp-landed,LANDED" "$TABLE_AFTER_FIRST"

# ── idempotent: a second run changes nothing ────────────────────────────────────────
OUT="$(install_populate)"; RC=$?
wantrc "the second run exits 0" 0 "$RC"
want "the second run creates nothing" "lifecycle population: 6 bead(s) in the database, 0 row(s) created, 6 already present" "$OUT"
is "the bead table is byte-for-byte what the first run left" "$TABLE_AFTER_FIRST" "$(dump)"
is "the event log gained nothing" "$EVENTS_AFTER_FIRST" "$(rows event)"

# ── a beads DB from somewhere else: repo: labels this config does not know ──────────
testdb_seed <<JSONL
{"id":"sp-stray","title":"open, labelled with a repository nothing configures","type":"task","status":"open","labels":["repo:gone"]}
{"id":"sp-gone-landed","title":"closed, unknown repository, landing line in demo","type":"task","status":"closed","labels":["repo:gone"]}
JSONL
wantrc "the unknown-repo beads seed" 0 $?
OUT="$(install_populate)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/  | /'
wantrc "unknown repo: labels do not stop the install (exit 0)" 0 "$RC"
want "the counts include them" "lifecycle population: 8 bead(s) in the database, 2 row(s) created, 6 already present" "$OUT"
want "install warns with the unknown-repo line" "WARNING: unknown-repo: 2 (" "$OUT"
is "the warning names sp-stray" "sp-stray" "$(printf '%s\n' "$OUT" | grep 'unknown-repo:' | grep -oE 'sp-stray([,)]|$)' | tr -d ',)')"
is "the warning names sp-gone-landed" "sp-gone-landed" "$(printf '%s\n' "$OUT" | grep 'unknown-repo:' | grep -oE 'sp-gone-landed([,)]|$)' | tr -d ',)')"
is "open with an unknown repo -> READY, by bd status" "READY" "$(lcfix_state sp-stray)"
is "closed with an unknown repo -> DROPPED, by bd status" "DROPPED" "$(lcfix_state sp-gone-landed)"
nowant "never LANDED by another repository's landing line" "terminal-landing-line" "$(reason sp-gone-landed)"
is "rows from the first run are untouched" "$TABLE_AFTER_FIRST" "$(dump | grep -vE '^(sp-stray|sp-gone-landed),')"
TABLE_AFTER_UNKNOWN="$(dump)"

# ── an epic is a container: OPEN, never READY; its child is READY ───────────────────
testdb_seed <<JSONL
{"id":"sp-epic","title":"a container","type":"epic","status":"open","labels":["repo:demo"]}
{"id":"sp-epic-kid","title":"its child","type":"task","status":"open","labels":["repo:demo"]}
JSONL
wantrc "the epic and its child seed" 0 $?
lcfix_seed sp-epic READY "" || bail "could not seed sp-epic's stale READY row"
OUT="$(install_populate)"; RC=$?
wantrc "population with an epic exits 0" 0 "$RC"
is "the child is READY" "READY" "$(lcfix_state sp-epic-kid)"
is "a pre-existing READY epic row stays until a reclassify" "READY" "$(lcfix_state sp-epic)"
spira-lc classify --every-bead --reclassify --bd-bin "$SPIRA_BD" --bd-db "$SPIRA_DB" \
    --landstate-dir "$TMP/run/landstate" --queue-dir "$TMP/run/queue" >/dev/null 2>&1
is "classify migrates the epic's row to OPEN" "OPEN" "$(lcfix_state sp-epic)"
is "the epic is decided by the container rule" "epic-container" "$(reason sp-epic)"
is "the child stays READY" "READY" "$(lcfix_state sp-epic-kid)"
TABLE_AFTER_UNKNOWN="$(dump)"

# ── loud: an unclassifiable bead fails the phase, named ─────────────────────────────
testdb_seed <<JSONL
{"id":"sp-twin","title":"two repo labels","type":"task","status":"open","labels":["repo:demo","repo:other"]}
JSONL
wantrc "the two-label bead seeds" 0 $?
OUT="$(install_populate)"; RC=$?
wantrc "an unclassifiable bead fails the phase (exit 2)" 2 "$RC"
want "the failure names the bead" "sp-twin" "$OUT"
want "the failure says why" "more than one repo: label" "$OUT"
is "the two-label bead got no row" "" "$(lcfix_state sp-twin)"
is "the failed run changed no existing row" "$TABLE_AFTER_UNKNOWN" "$(dump)"

tl_summary
