#!/usr/bin/env bash
#
# test-sentinel-check5-subsumed.sh — CHECK 5 (closed-not-landed) must not file an Ops
#   incident against a bead the Concierge closed as SUBSUMED by an epic: that close reason
#   means the work was folded into the epic, not that it landed on its own, and there is no
#   branch of its own ever coming — the same shape as spira-dropped or superseded, which
#   CHECK 5 already knows to skip. Modelled on sp-9geby's real close_reason: "Swept
#   2026-09-26 by the Concierge: SUBSUMED by epic sp-pswer...".
#
#   ./test-sentinel-check5-subsumed.sh
#
# THIS IS A REGRESSION FIXTURE, NOT YET A PASSING TEST (law-a-regression-test-must-be-seen-
# to-fail, sp-9mcl2). Split off sp-m8c34: the patch (sp-a9s2r) adds a `subsumed` skip-guard
# to CHECK 5's python block via re.search(r'SUBSUMED|DUPLICATE|tracked in epic',
# close_reason, re.I). Against UNPATCHED sentinel.sh this case is EXPECTED TO FAIL — CHECK 5
# has no such guard yet and files an incident for the subsumed bead exactly as it would for
# any other closed-with-no-landstate bead. Once sp-a9s2r lands, this must go green with no
# further change to this file.
#
# Boilerplate (repo, chamber, mock incident.sh, `sentinel()` wrapper) copied from
# test-check5-invariant.sh (sp-qsona), which is the canonical CHECK 5 fixture harness — same
# repo-map shape, same mock-incident capture, so this case slots into that suite's pattern
# rather than inventing a new one.
#
# defect: sp-9mcl2 sp-a9s2r
# tier: T3
# covers: sentinel/src/*
# timeout: 120
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sentinel-check5-subsumed
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check5sub || { echo "test-sentinel-check5-subsumed: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$RUN/landstate" "$SH/chamber"

cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SH/"
# THE RUST SENTINEL (sentinel.sh is gone): the binaries are invoked by name from the tree's build on PATH
# resolves them for THIS tree, and passed explicitly, because the fixture's own SPIRA_REPO is
# not the tree that built them.
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand        'exit 0'
stub sending       'exit 0'
stub reflect.sh    'exit 0'
stub ask.sh        'true'

printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/t.fayth"

printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"; chmod +x "$TMP/launch"
printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"; chmod +x "$TMP/systemctl"

# THE MOCK CAPTURES EVERY CALL — a real incident.sh would write its own bead, which is
# incident.sh's own suite's job to verify; what this suite verifies is whether CHECK 5
# calls it, and with what, for the subsumed bead.
INC_LOG="$TMP/incident.log"
: > "$INC_LOG"
cat > "$TMP/mock-incident.sh" <<MOCK
#!/usr/bin/env bash
printf 'REF=%s CAUSE=%s file %s\n' "\${SPIRA_INCIDENT_REF:-}" "\${SPIRA_INCIDENT_CAUSE:-}" "\$*" >> "$INC_LOG"
MOCK
chmod +x "$TMP/mock-incident.sh"

HOME_REPO="$(basename "$REPO")"
printf '%s | %s | pr | main | |\n' "$HOME_REPO" "$REPO" > "$TMP/repo-map"

run_sentinel() {
    SPIRA_HOME="$SH" PATH="$SH:$PATH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS=t SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_INCIDENT_SH="$TMP/mock-incident.sh" \
    SPIRA_SKIP_RECLAIM=1 \
        sentinel --audit 2>&1
}

status_of() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

echo "test-sentinel-check5-subsumed.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — an ordinary closed-with-no-landstate bead IS reported (proves the"
echo "harness can detect a filing at all — law-absence-needs-a-positive-control):"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-bare","title":"bare closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-bare.log"
rm -f "$RUN/landstate/sp-bare"
: > "$INC_LOG"
run_sentinel >/dev/null
inc_out="$(cat "$INC_LOG")"
want "positive control: an ordinary closed bead IS reported" "REF=closed-not-landed:sp-bare" "$inc_out"

# ======================================================================================
echo
echo "SUBSUMED — a bead closed by the Concierge sweep as 'SUBSUMED by epic ...' must NOT be"
echo "reported: the work was folded into the epic, no branch of its own is ever coming"
echo "(modelled on sp-9geby). EXPECTED TO FAIL on unpatched sentinel.sh (sp-9mcl2/sp-a9s2r):"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-subs","title":"subsumed bead","status":"closed","issue_type":"task","close_reason":"Swept 2026-09-26 by the Concierge: SUBSUMED by epic sp-pswer (bead lifecycle rewrite).","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-subs","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-subs.log"
rm -f "$RUN/landstate/sp-subs"
: > "$INC_LOG"

run_sentinel >/dev/null
is "sp-subs stays closed either way" closed "$(status_of sp-subs)"
inc_out="$(cat "$INC_LOG")"
nowant "no incident is filed for the subsumed bead" "sp-subs" "$inc_out"

tl_summary
