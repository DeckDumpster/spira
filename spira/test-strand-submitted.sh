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
#   0. POSITIVE CONTROL — the partition's only bead carries no submitted label → the
#      mock ready query returns it, and strand.sh DOES report 'starved'. Proves the rest
#      of the harness (SPIRA_LABELS routing, the mock, live=0) can produce the finding
#      before trusting its silence in case 1.
#
#   1. THE BUG — the partition's only bead carries SPIRA_SUBMITTED_LABEL. The mock `bd
#      ready` call inspects the --exclude-label argument it was actually passed (as the
#      real `bd ready` would honour it) and returns nothing when the submitted label is
#      in that list, so this case fails on today's strand.sh: it does not pass the label
#      through and gets the bead back as "ready", reporting 'starved'.
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

# Mock bd: `list` always returns the fixture bead (used to build the holders set — this
# bead is "open", never "in_progress", so it never becomes a holder). `ready` behaves like
# the real predicate: it reads the --exclude-label argument it was actually passed and
# withholds the bead when that argument names the submitted label — exactly what a real
# `bd ready --exclude-label ...,$SUBMITTED,...` would do for a bead carrying it.
cat > "$TMP/mock-bd" <<MOCKBD
#!/usr/bin/env bash
case "\${1:-}" in -C) shift 2 ;; esac
cmd="\${1:-}"; shift || true
excl=""
while [ \$# -gt 0 ]; do
    case "\$1" in
        --exclude-label) excl="\$2"; shift 2 ;;
        *) shift ;;
    esac
done
labels='["spira","test-groom","$SUBMITTED"]'
bead='{"id":"sp-subm1","title":"groomer graph edit","status":"open","labels":'"\$labels"'}'
case "\$cmd" in
    list)  printf '[%s]\n' "\$bead" ;;
    ready)
        case ",\$excl," in
            *",$SUBMITTED,"*) printf '[]\n' ;;
            *)                printf '[%s]\n' "\$bead" ;;
        esac
        ;;
    *) exit 0 ;;
esac
MOCKBD
chmod +x "$TMP/mock-bd"

run_report() {
    SPIRA_HOME="$TMP/home" PATH="$TMP/home:$PATH" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_BD="$TMP/mock-bd" \
    SPIRA_DB="$TMP/no-db" \
    SPIRA_SUMMON=stub \
    SPIRA_SUBMITTED_LABEL="$SUBMITTED" \
    SPIRA_LABELS="spira,test-groom" \
        strand report 2>/dev/null
}

echo "test-strand-submitted.sh"

# ======================================================================================
echo
echo "case 0 — positive control: mock ready returns the bead → starved IS reported:"
# ======================================================================================
# Same run, but --exclude-label from classify_one never carries a label matching the one
# baked into the mock's check (nothing here names the submitted label back to the mock),
# so the mock hands the bead back and starvation is real.
out0="$(SPIRA_HOME="$TMP/home" PATH="$TMP/home:$PATH" SPIRA_RUN="$TMP/run" SPIRA_BD="$TMP/mock-bd" \
        SPIRA_DB="$TMP/no-db" SPIRA_SUMMON=stub \
        SPIRA_SUBMITTED_LABEL=some-other-label-entirely \
        SPIRA_LABELS="spira,test-groom" \
            strand report 2>/dev/null)"
want "positive control: starved IS reported" "starved" "$out0"

# ======================================================================================
echo
echo "case 1 — the bug: only ready bead carries the submitted label → NOT starved:"
# ======================================================================================
# strand.sh's classify_one must pass the submitted label through --exclude-label, same as
# fayth_exclude does for CHECK7's summon predicate. Fails on today's strand.sh: 'starved'
# was reported here (sp-wnsks), because classify_one never told the ready query about
# SPIRA_SUBMITTED_LABEL and the mock (acting as a real `bd ready` would) withheld nothing.
out1="$(run_report)"
nowant "the bug: starved is NOT reported for a submitted-only partition" "starved" "$out1"

tl_summary
