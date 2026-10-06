#!/usr/bin/env bash
#
# test-strand-submitted.sh — a partition whose only ready-looking bead carries the
#   submitted label is not starved: strand.sh's ready count must use the same predicate
#   the sentinel uses to summon (fayth_exclude/CHECK7), or the two disagree about what
#   "ready" means.
#
#   ./test-strand-submitted.sh
#
# WHY THIS EXISTS. sp-wnsks: strand.sh reported [spira,groom] starved and escalated it,
# while the sentinel's CHECK7 — in the very same pass — correctly said "nothing ready in
# its partition". The bead both were looking at carried SPIRA_SUBMITTED_LABEL: the
# groomer had finished it and it was waiting on the landing pass, not on an aeon.
# fayth_exclude (CHECK7's predicate) has excluded the submitted label for a long time;
# classify_one (strand.sh's predicate) never did, so a submitted bead read as "ready and
# unserved" and starvation was escalated for a partition that was, correctly, empty.
#
# TWO CASES (law-absence-needs-a-positive-control):
#
#   0. POSITIVE CONTROL — strand's exclusion does not name the bead's label (the configured
#      submitted label is a different one) → the ready set holds the bead, and strand DOES
#      report 'starved'. Proves the rest of the harness (SPIRA_LABELS routing, the
#      lifecycle stand-in, live=0) can produce the finding before trusting its silence in
#      case 1.
#
#   1. THE BUG — the partition's only bead carries SPIRA_SUBMITTED_LABEL. strand's ready set
#      is spira-claim's `ready-count <labels> <exclude> --json` over the lifecycle machine's
#      READY rows (sp-7g5q6; sp-v62vn: the only mode), and spira-claim applies the exclude
#      list strand hands it — so the bead drops out only if strand puts the submitted label
#      in that list. A strand that does not reports 'starved'.
#
# SPIRA_SUBMITTED_LABEL is pinned to a non-default value so a matcher that hardcodes the
# shipped default ("spira-submitted") cannot pass by accident.
#
# tier: T1
# defect: sp-wnsks
# covers: strand/src/* spira/lib.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run" "$TMP/home"
# THE STRAND IS A BINARY (strand.sh is gone), invoked by name from the tree's build on PATH. Its roster
# probe sources lib.sh from SPIRA_HOME, so the stub home carries the real lib.sh.
for _s in lib.sh conf.sh suite-covers.sh; do ln -s "$HERE/$_s" "$TMP/home/$_s"; done

SUBMITTED=mysubmitted-nondefault

# Mock bd: every `list` (strand's store read, spira-claim's `list --id` content read, the
# lifecycle stand-in's own read) returns the one fixture bead — open, so the stand-in below
# makes it a READY row. Nothing here asks bd for "ready": the machine's row is the ready set,
# and spira-claim's label predicate does the excluding.
cat > "$TMP/mock-bd" <<MOCKBD
#!/usr/bin/env bash
case "\${1:-}" in -C) shift 2 ;; esac
bead='{"id":"sp-subm1","title":"groomer graph edit","status":"open","issue_type":"task","labels":["spira","test-groom","$SUBMITTED"]}'
case "\${1:-}" in
    list) printf '[%s]\n' "\$bead" ;;
    *)    exit 0 ;;
esac
MOCKBD
chmod +x "$TMP/mock-bd"
# The lifecycle machine, answered from the mock bd's store (testlib lc_mirror_bd): spira-claim
# finds spira-lc by name on PATH, so its directory goes first.
lc_mirror_bd "$TMP/lc"

run_report() {
    tl_config SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/no-db" SPIRA_SUBMITTED_LABEL="$SUBMITTED"
    SPIRA_HOME="$TMP/home" PATH="$TMP/home:$TMP/lc:$PATH" \
    SPIRA_BD="$TMP/mock-bd" \
    SPIRA_SUMMON=stub \
    SPIRA_LABELS="spira,test-groom" \
        strand report 2>/dev/null
}

echo "test-strand-submitted.sh"

# ======================================================================================
echo
echo "case 0 — positive control: the ready set holds the bead → starved IS reported:"
# ======================================================================================
# Same run, but the configured submitted label is a different one, so strand's exclude list
# never names the bead's label, spira-claim hands the bead back, and starvation is real.
tl_config SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/no-db" SPIRA_SUBMITTED_LABEL=some-other-label-entirely
out0="$(SPIRA_HOME="$TMP/home" PATH="$TMP/home:$TMP/lc:$PATH" SPIRA_BD="$TMP/mock-bd" \
        SPIRA_SUMMON=stub \
        SPIRA_LABELS="spira,test-groom" \
            strand report 2>/dev/null)"
want "positive control: starved IS reported" "starved" "$out0"

# ======================================================================================
echo
echo "case 1 — the bug: only ready bead carries the submitted label → NOT starved:"
# ======================================================================================
# strand must put the submitted label in the exclude list it hands spira-claim, same as
# fayth_exclude does for CHECK7's summon predicate. 'starved' was reported here (sp-wnsks)
# when strand's ready query never named SPIRA_SUBMITTED_LABEL.
out1="$(run_report)"
nowant "the bug: starved is NOT reported for a submitted-only partition" "starved" "$out1"

tl_summary
