#!/usr/bin/env bash
#
# test-check5-sweep-delivers.sh — sweep beads (maechen-sweep, groom) must carry
#   delivers:note: labels so the sentinel accepts their close without a commit.
#
# WHY THIS EXISTS. maechen-trigger.sh and groom-trigger.sh filed sweep beads without
# a delivers: label. A Maechen or Groomer pass produces no commit by design (correct
# work), so CHECK 5's closed-but-not-landed check reopened every closed sweep bead on
# every pass. The bead was re-summoned, re-closed, and re-reopened — an unbounded loop
# entered by correct work, counted toward the poison threshold (sp-fl9).
#
# THE FIX: maechen-trigger.sh adds delivers:note:${SPIRA_RUN}/maechen.log and
# groom-trigger.sh adds delivers:note:${SPIRA_RUN}/groom.log to the filed bead.
# The persona briefs require the aeon to write those files during the session.
# If the file is absent or stale, the bead is reopened — the exemption verifies.
#
# CASES (law-absence-needs-a-positive-control — all asserted):
#
#   SENTINEL SECTION (using fixture database):
#
#   1. POSITIVE CONTROL — maechen-sweep bead with NO delivers: label IS reopened.
#      Proves CHECK 5 is actually running for sweep beads.
#
#   2. Sweep bead WITH delivers:note:<path>, file written after started_at
#      → NOT reopened. The exemption works.
#
#   3. Sweep bead WITH delivers:note:<path>, file mtime BEFORE started_at
#      → IS reopened. The exemption verifies, it does not merely assert.
#
#   TRIGGER SECTION (using stub bd):
#
#   4. maechen-trigger.sh adds delivers:note:${SPIRA_RUN}/maechen.log to the bead.
#      FAILS before the fix; passes after.
#
#   5. groom-trigger.sh adds delivers:note:${SPIRA_RUN}/groom.log to the bead.
#      FAILS before the fix; passes after.
#
# defect: sp-fl9 (sp-requeue-nondeliverable)
# covers: spira/maechen-trigger.sh spira/groom-trigger.sh spira/sentinel.sh
# hermetic-ok: uses a fixture database and local git repo; trigger section uses stub bd
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check5-sweep-delivers
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up swpdeliv || { echo "test-check5-sweep-delivers: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$SH/chamber"

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub reflect.sh    'exit 0'
stub ask.sh        'true'

# Two fayths — one for each sweep partition. FAYTH_LABELS is an AND query,
# so a single fayth with "spira,maechen-sweep,groom" would never match beads
# that carry only "spira,maechen-sweep" or only "spira,groom". Separate fayths
# give CHECK5 the right per-persona partition to query.
printf 'FAYTH_LABELS="spira,maechen-sweep"\nFAYTH_EXCLUDE_LABELS="spira-poison,%s,%s"\nFAYTH_MAX_CONCURRENT=0\n' \
    "${SPIRA_ASK_LABEL:-needs-ryan}" "${SPIRA_CI_LABEL:-awaiting-ci}" \
    > "$SH/chamber/mae.fayth"
printf 'FAYTH_LABELS="spira,groom"\nFAYTH_EXCLUDE_LABELS="spira-poison,%s,%s"\nFAYTH_MAX_CONCURRENT=0\n' \
    "${SPIRA_ASK_LABEL:-needs-ryan}" "${SPIRA_CI_LABEL:-awaiting-ci}" \
    > "$SH/chamber/gro.fayth"

B() { bd -C "$SPIRA_DB" "$@"; }

printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"; chmod +x "$TMP/launch"
printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"; chmod +x "$TMP/systemctl"
HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" pr main '' '' > "$TMP/repo-map"

sentinel() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="mae gro" SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_SKIP_RECLAIM=1 \
        bash "$SH/sentinel.sh" 2>&1
}
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

# A past timestamp that started_at is set to — any file written NOW has mtime > this.
PAST="2026-09-01T00:00:00Z"
# A future timestamp — a file written BEFORE started_at will have mtime < this.
FUTURE="$(date -u -d '+1 hour' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u +%Y-%m-%dT%H:%M:%SZ)"

echo "test-check5-sweep-delivers.sh"

# ======================================================================================
echo
echo "SENTINEL case 1 — positive control: maechen-sweep bead with NO delivers: IS reopened"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-nosweep","title":"maechen pass — no delivers","status":"closed","issue_type":"task","labels":["spira","maechen-sweep","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-nosweep","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-nosweep.log"
is "sp-nosweep starts closed" closed "$(status_of sp-nosweep)"
out="$(sentinel)"
is "sp-nosweep is reopened (no delivers: label, no commit)" open "$(status_of sp-nosweep)"
want "the pass says so" "sp-nosweep" "$out"

# ======================================================================================
echo
echo "SENTINEL case 2 — delivers:note:<path> with fresh file → NOT reopened"
# ======================================================================================
EVIDENCE="$RUN/maechen.log"
mkdir -p "$(dirname "$EVIDENCE")"
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-sweepok","title":"maechen pass — delivers:note valid","status":"closed","issue_type":"task","labels":["spira","maechen-sweep","delivers:note:$EVIDENCE","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-sweepok","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-sweepok.log"
# Write the evidence file AFTER started_at (now > 2026-09-01).
printf 'Maechen pass done: census=3 classes ranked, threshold_met=no, beads_cut=0.\n' > "$EVIDENCE"
is "sp-sweepok starts closed" closed "$(status_of sp-sweepok)"
out="$(sentinel)"
is "sp-sweepok stays closed (delivers:note verified)" closed "$(status_of sp-sweepok)"
nowant "the pass does not reopen it" "reopened sp-sweepok" "$out"
want "the pass records verification" "delivers" "$out"

# ======================================================================================
echo
echo "SENTINEL case 3 — delivers:note:<path>, file written BEFORE started_at → IS reopened"
# ======================================================================================
EVIDENCE_OLD="$RUN/maechen-old.log"
# Create the file first, then set started_at to the future so file is "stale".
printf 'old pass entry\n' > "$EVIDENCE_OLD"
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-sweepstale","title":"maechen pass — delivers:note stale","status":"closed","issue_type":"task","labels":["spira","maechen-sweep","delivers:note:$EVIDENCE_OLD","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$FUTURE","dependencies":[{"issue_id":"sp-sweepstale","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-sweepstale.log"
is "sp-sweepstale starts closed" closed "$(status_of sp-sweepstale)"
out="$(sentinel)"
is "sp-sweepstale is reopened (file predates started_at)" open "$(status_of sp-sweepstale)"
want "the pass says so" "sp-sweepstale" "$out"
want "the reason mentions the time window" "predates the window" "$out"

# ======================================================================================
# TRIGGER SECTION — uses stub bd (no fixture database needed)
# ======================================================================================
NONE="$TMP/none.conf"
STUB_BD="$TMP/stub-bd"
BD_LOG="$TMP/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG_PATH"
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    list) printf '%s\n' "${BD_LIST_OUTPUT:-[]}"; exit 0 ;;
    *)    exit 0 ;;
esac
STUB
chmod +x "$STUB_BD"

TRUN="$TMP/trigrun"
mkdir -p "$TRUN"

# ======================================================================================
echo
echo "TRIGGER case 4 — maechen-trigger.sh adds delivers:note:\${SPIRA_RUN}/maechen.log"
# ======================================================================================
# The watermark is epoch 0 so the time trigger fires immediately.
printf '0\n' > "$TRUN/maechen.watermark"
: > "$BD_LOG"
env -i HOME="$TMP" PATH="$HERE:/usr/bin:/bin" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$STUB_BD" \
    BD_LOG_PATH="$BD_LOG" \
    BD_LIST_OUTPUT="[]" \
    SPIRA_DB="$TMP/fixture.db" \
    SPIRA_RUN="$TRUN" \
    SPIRA_REPO="$REPO" \
    SPIRA_MAECHEN_LABEL="maechen-sweep" \
    SPIRA_SCOPE_LABEL="spira" \
    SPIRA_MAECHEN_MAX_GAP_SECONDS=0 \
    SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    bash "$HERE/maechen-trigger.sh" >/dev/null 2>&1
want "maechen-trigger adds delivers:note:" "delivers:note:" "$(cat "$BD_LOG")"
want "delivers: label names maechen.log" "maechen.log" "$(cat "$BD_LOG")"

# ======================================================================================
echo
echo "TRIGGER case 5 — groom-trigger.sh adds delivers:note:\${SPIRA_RUN}/groom.log"
# ======================================================================================
: > "$BD_LOG"
env -i HOME="$TMP" PATH="$HERE:/usr/bin:/bin" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$STUB_BD" \
    BD_LOG_PATH="$BD_LOG" \
    BD_LIST_OUTPUT="[]" \
    SPIRA_DB="$TMP/fixture.db" \
    SPIRA_RUN="$TRUN" \
    SPIRA_REPO="$REPO" \
    SPIRA_GROOMER_LABEL="groom" \
    SPIRA_SCOPE_LABEL="spira" \
    SPIRA_GROOM_THRESHOLD=0 \
    bash "$HERE/groom-trigger.sh" >/dev/null 2>&1
want "groom-trigger adds delivers:note:" "delivers:note:" "$(cat "$BD_LOG")"
want "delivers: label names groom.log" "groom.log" "$(cat "$BD_LOG")"

echo
printf 'test-check5-sweep-delivers.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
