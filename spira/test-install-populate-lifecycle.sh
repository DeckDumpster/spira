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
#   - Loud: a bead that cannot be classified (labelled with a repository no config names)
#     fails the phase, exit 2, naming the bead.
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

# ── config: the run/queue dirs, a repository table layer, and a bd that logs its argv ──
mkdir -p "$TMP/run/landstate" "$TMP/run/queue"
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

# ── loud: an unclassifiable bead fails the phase, named ─────────────────────────────
testdb_seed <<JSONL
{"id":"sp-stray","title":"labelled with a repository nothing configures","type":"task","status":"open","labels":["repo:gone"]}
JSONL
wantrc "the stray bead seeds" 0 $?
OUT="$(install_populate)"; RC=$?
wantrc "an unclassifiable bead fails the phase (exit 2)" 2 "$RC"
want "the failure names the bead" "sp-stray" "$OUT"
want "the failure says why" "repo:gone" "$OUT"
is "the stray bead got no row" "" "$(lcfix_state sp-stray)"
is "the failed run changed no existing row" "$TABLE_AFTER_FIRST" "$(dump)"

tl_summary
