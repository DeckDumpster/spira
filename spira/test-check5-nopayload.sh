#!/usr/bin/env bash
#
# test-check5-nopayload.sh — CHECK 5 accepts a close when a bead declares its deliverable
#   via delivers:TYPE labels and the evidence exists; bare closed beads are still reopened.
#
# WHY THIS EXISTS. A bead whose deliverable is not a repository commit — a diagnosis, a set
# of filed beads, a wiki note — cannot carry the commit that CHECK 5 normally looks for. The
# prior answer (no-payload) exempted unconditionally, so a sweep that fell over after its
# first command was indistinguishable from one that filed twenty beads. The typed-and-verified
# form (sp-4z3s) requires the aeon to declare what it produced and CHECK 5 to confirm it is
# present. no-payload is retired by this change: a bead carrying it is now treated as a bare
# closed bead and reopened.
#
# CASES, each asserted (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — a bare closed bead with no commit IS reopened. Without this, every
#      exemption is indistinguishable from a check that never fires.
#
#   2. delivers:beads WITH children — NOT reopened; at least one child bead names it as source.
#
#   3. delivers:beads WITHOUT children — IS reopened; the declared evidence is absent.
#
#   4. delivers:note:PATH with a file written in the bead's window — NOT reopened.
#
#   5. delivers:note:PATH with an old file (predates started_at) — IS reopened.
#
#   6. delivers:note:PATH with a missing file — IS reopened.
#
#   7. no-payload (retired) — IS reopened, same as the bare closed case.
#
#   8. SEAM CHECK — the bulk query carries delivers: labels so the exemption can read them.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency). The exemption
# reads labels and children off `bd list`; a stub would drift silently.
#
# defect: sp-4z3s
# covers: spira/sentinel.sh spira/lib.sh
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
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
testdb_require test-check5-nopayload
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check5delivers || { echo "test-check5-nopayload: could not build a fixture database"; exit 1; }

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

printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/t.fayth"

B() { bd -C "$SPIRA_DB" "$@"; }
cat > "$TMP/launch" <<'L'
#!/usr/bin/env bash
exit 0
L
cat > "$TMP/systemctl" <<'S'
#!/usr/bin/env bash
printf '%s\n' "inactive"
S
chmod +x "$TMP/launch" "$TMP/systemctl"

HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" pr main '' '' > "$TMP/repo-map"

sentinel() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no-such-conf" \
        bash "$SH/sentinel.sh" 2>&1
}

status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

# A past timestamp earlier than any file we create, and current time.
PAST="2026-09-01T00:00:00Z"

echo "test-check5-nopayload.sh"

# ======================================================================================
echo
echo "positive control — a bare closed bead with no commit IS reopened:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-bare","title":"bare closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-bare.log"
is "sp-bare starts closed" closed "$(status_of sp-bare)"
out="$(sentinel)"
is "sp-bare is reopened" open "$(status_of sp-bare)"
want "the pass says so" "reopened sp-bare" "$out"

# ======================================================================================
echo
echo "no-payload (retired) — treated as bare closed, IS reopened:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-nop","title":"no-payload closed","status":"closed","issue_type":"task","labels":["spira","plan","no-payload","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-nop","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-nop.log"
is "sp-nop starts closed" closed "$(status_of sp-nop)"
out="$(sentinel)"
is "sp-nop is reopened (no-payload is retired)" open "$(status_of sp-nop)"
want "the pass reopens it" "reopened sp-nop" "$out"

# ======================================================================================
echo
echo "delivers:beads WITH child beads — NOT reopened:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-del-b","title":"delivers:beads with child","status":"closed","issue_type":"task","labels":["spira","plan","delivers:beads","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-del-b","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-child","title":"a filed bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"$PAST","dependencies":[{"issue_id":"sp-child","depends_on_id":"sp-del-b","type":"parent-child"}]}
JSONL
touch "$RUN/sp-del-b.log"
is "sp-del-b starts closed" closed "$(status_of sp-del-b)"
out="$(sentinel)"
is "sp-del-b stays closed (child exists)" closed "$(status_of sp-del-b)"
nowant "the pass does not reopen it" "reopened sp-del-b" "$out"
want   "the pass records the verification" "delivers" "$out"

# ======================================================================================
echo
echo "delivers:beads WITHOUT child beads — IS reopened:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-del-b0","title":"delivers:beads no child","status":"closed","issue_type":"task","labels":["spira","plan","delivers:beads","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-del-b0","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-del-b0.log"
is "sp-del-b0 starts closed" closed "$(status_of sp-del-b0)"
out="$(sentinel)"
is "sp-del-b0 is reopened (no children)" open "$(status_of sp-del-b0)"
want "the pass says so" "reopened sp-del-b0" "$out"

# ======================================================================================
echo
echo "delivers:note:PATH with file written in bead's window — NOT reopened:"
# ======================================================================================
NOTE_FILE="$TMP/diagnosis.md"
# Write the note file now (current mtime > PAST epoch)
printf 'diagnosis content\n' > "$NOTE_FILE"
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-del-n","title":"delivers:note with file","status":"closed","issue_type":"task","labels":["spira","plan","delivers:note:$NOTE_FILE","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-del-n","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-del-n.log"
is "sp-del-n starts closed" closed "$(status_of sp-del-n)"
out="$(sentinel)"
is "sp-del-n stays closed (note file is new)" closed "$(status_of sp-del-n)"
nowant "the pass does not reopen it" "reopened sp-del-n" "$out"
want   "the pass records the verification" "delivers" "$out"

# ======================================================================================
echo
echo "delivers:note:PATH with file older than started_at — IS reopened:"
# ======================================================================================
OLD_FILE="$TMP/old-diagnosis.md"
printf 'old content\n' > "$OLD_FILE"
# Set mtime to a time far in the past, before PAST epoch
touch -t 202001010000 "$OLD_FILE"
# Use a started_at after 2020 so the file is definitely old
RECENT="2026-09-08T00:00:00Z"
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-del-nold","title":"delivers:note with old file","status":"closed","issue_type":"task","labels":["spira","plan","delivers:note:$OLD_FILE","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$RECENT","dependencies":[{"issue_id":"sp-del-nold","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-del-nold.log"
is "sp-del-nold starts closed" closed "$(status_of sp-del-nold)"
out="$(sentinel)"
is "sp-del-nold is reopened (file predates session)" open "$(status_of sp-del-nold)"
want "the pass says so" "reopened sp-del-nold" "$out"

# ======================================================================================
echo
echo "delivers:note:PATH with missing file — IS reopened:"
# ======================================================================================
MISSING_FILE="$TMP/no-such-file.md"
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-del-nmiss","title":"delivers:note missing","status":"closed","issue_type":"task","labels":["spira","plan","delivers:note:$MISSING_FILE","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-del-nmiss","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-del-nmiss.log"
is "sp-del-nmiss starts closed" closed "$(status_of sp-del-nmiss)"
out="$(sentinel)"
is "sp-del-nmiss is reopened (file missing)" open "$(status_of sp-del-nmiss)"
want "the pass says so" "reopened sp-del-nmiss" "$out"

# ======================================================================================
# SEAM CHECK — verify delivers: labels appear in the bulk query that CHECK 5 reads.
# The supersession exemption shipped broken because the label field was absent from the
# bulk listing; check the same seam for delivers: so it cannot silently fail to read.
# ======================================================================================
echo
echo "bulk listing carries delivers: labels:"
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-del-seam","title":"seam check","status":"closed","issue_type":"task","labels":["spira","plan","delivers:beads","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-del-seam","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
bulk="$(B list --status closed --limit 0 --label spira,plan --json 2>/dev/null)"
has_delivers="$(python3 -c '
import json, sys
d = json.loads(sys.argv[1])
d = d if isinstance(d, list) else [d]
for i in d:
    if i["id"] == "sp-del-seam" and "delivers:beads" in (i.get("labels") or []):
        print("yes"); break
else:
    print("no")
' "$bulk" 2>/dev/null)"
is "sp-del-seam's delivers:beads label is in the bulk listing" "yes" "$has_delivers"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
