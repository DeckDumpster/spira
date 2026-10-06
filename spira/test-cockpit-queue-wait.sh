#!/usr/bin/env bash
#
# test-cockpit-queue-wait.sh — a bead carrying SPIRA_QUEUE_WAIT_LABEL must not
# appear in SP_NEXT; cockpit-collect must pass the wait label to bd's --exclude-label.
#
# THE DEFECT. cockpit-collect's NEXT query excluded spira-poison, SPIRA_ASK_LABEL and
# SPIRA_CI_LABEL but not SPIRA_QUEUE_WAIT_LABEL. A bead sp-n9z (content-landed,
# spira-queue-waiting, landstate=LANDED, no branch) appeared in the NEXT row while
# every fayth excluded it — zero aeons could claim it, the operator read it as a
# stalled loop (sp-rvoun).
#
# THE FIX. cockpit-collect's NEXT query adds SPIRA_QUEUE_WAIT_LABEL to --exclude-label,
# matching what fayth_exclude passes to bd ready for each aeon.
#
# Since sp-7g5q6 the NEXT rows come from `spira-claim ready-count <labels> <excl> --json`:
# the lifecycle machine's READY rows (the only mode since sp-v62vn retired
# lifecycle_enforce), filtered to the partition's labels and away from the exclude set. So
# the fixture's beads carry the partition's labels (the scope label too, whatever it
# resolves to), and a spira-lc stand-in (testlib.sh lc_mirror_bd) tells the mock bd's
# story as lifecycle rows (sp-mve9i).
#
# WHAT THIS SUITE CHECKS (4 assertions).
#   1. POSITIVE CONTROL: a normal bead (no wait label) appears in SP_NEXT.
#   2. Queue-wait bead NOT in SP_NEXT: a bead carrying spira-queue-waiting is
#      absent from the panel rows.
#   3. SP_NEXT_N is 0 when the only bead has the wait label.
#   4. SP_READY is 0 when the only bead has the wait label.
#
# defect: sp-rvoun
# tier: T1
# covers: cockpit-collect/src/*
# hermetic-ok: mock bd and spira-lc binaries, no systemd or database
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"
: "${SPIRA_SCOPE_LABEL:=$(basename "$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null || printf '')")}"

# The partition's labels as a JSON list fragment: the scope label (when one resolved) and
# plan — what the builder fayth's FAYTH_LABELS asks every NEXT row to carry.
SCOPE_JSON="${SPIRA_SCOPE_LABEL:+\"$SPIRA_SCOPE_LABEL\",}\"plan\""
lc_mirror_bd "$TMP/lc"

# run_core <bd-binary> <wait-label> -> stdout of cockpit-collect probe core (SP_NEXT* and
# SP_READY keys); spira-claim reads the lifecycle rows from the lc_mirror_bd stand-in
# first on PATH.
run_core() {
    local bd_path="$1" wait_label="${2:-spira-queue-waiting}"
    tl_config SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$TMP/no-map" SPIRA_FAYTHS=builder \
        SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_QUEUE_WAIT_LABEL="$wait_label" \
        SPIRA_BD="$bd_path"
    # SPIRA_BD also as a raw env var: the spira-lc stand-in lc_mirror_bd installs reads
    # ${SPIRA_BD:-bd} straight from its own inherited env, never through config, and env -i
    # would otherwise wipe it entirely (sfail round 3, pattern 3/7).
    env -i SPIRA_TOML="$SPIRA_TOML" PATH="$TMP/lc:$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_LC_BIN="$SPIRA_LC_BIN" SPIRA_BD="$bd_path" \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_DB="$TMP/nodb" \
        cockpit-collect probe core 2>/dev/null
}

# make_bd_wait <path> <wait-label> — a mock bd that returns a bead carrying the
# wait label. Its ready respects --exclude-label: if the label appears in the exclude
# list the bead is filtered out, as the real bd would do. Its list (the lifecycle
# stand-in's source, and spira-claim's by-id read) returns the bead as stored.
make_bd_wait() {
    local path="$1" wlabel="$2"
    cat > "$path" <<EOF
#!/usr/bin/env bash
is_ready=0; excluded=0; prev_arg=""
for arg in "\$@"; do
    [ "\$arg" = "ready" ] && is_ready=1
    if [ "\$prev_arg" = "--exclude-label" ]; then
        case ",\$arg," in *,$wlabel,*) excluded=1 ;; esac
    fi
    prev_arg="\$arg"
done
case " \$* " in *" list "*) ;; *) [ "\$is_ready" = 0 ] && { printf '[]\n'; exit 0; } ;; esac
if [ "\$is_ready" = 1 ] && [ "\$excluded" = 1 ]; then printf '[]\n'; exit 0; fi
printf '[{"id":"sp-qw-test","title":"queue-wait bead","status":"open","issue_type":"task","priority":1,"labels":[$SCOPE_JSON,"$wlabel"]}]\n'
EOF
    chmod +x "$path"
}

# make_bd_normal <path> — mock bd returning a normal (no-wait-label) bead from ready and
# list alike.
make_bd_normal() {
    local path="$1"
    cat > "$path" <<EOF
#!/usr/bin/env bash
case " \$* " in
    *" ready "*|*" list "*) printf '[{"id":"sp-normal","title":"normal bead","status":"open","issue_type":"task","priority":1,"labels":[$SCOPE_JSON]}]\n' ;;
    *) printf '[]\n' ;;
esac
EOF
    chmod +x "$path"
}

echo "test-cockpit-queue-wait.sh"
echo
echo "1 — positive control: normal bead (no wait label) appears in SP_NEXT"
# Without this, an implementation that hides everything would pass the queue-wait test.
make_bd_normal "$TMP/bd-normal"
out="$(run_core "$TMP/bd-normal")"
want "normal bead in SP_NEXT" "sp-normal" "$out"

echo
echo "2 — queue-wait bead NOT in SP_NEXT"
make_bd_wait "$TMP/bd-wait" "spira-queue-waiting"
out="$(run_core "$TMP/bd-wait" "spira-queue-waiting")"
nowant "queue-wait bead absent from SP_NEXT" "sp-qw-test" "$out"

echo
echo "3 — SP_NEXT_N is 0 when only bead has wait label"
is "SP_NEXT_N=0 when all beads are queue-waiting" "SP_NEXT_N=0" \
    "$(printf '%s\n' "$out" | grep '^SP_NEXT_N=')"

echo
echo "4 — SP_READY is 0 when only bead has wait label"
is "SP_READY=0 when all beads are queue-waiting" "SP_READY=0" \
    "$(printf '%s\n' "$out" | grep '^SP_READY=')"
echo
tl_summary
