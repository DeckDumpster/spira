#!/usr/bin/env bash
#
# test-batcher-cut.sh — ONE end-to-end suite for the batcher crate's IO seam (sp-jzfog): a
# whole round through a real fixture repo, a stub round-vm.sh and a fake forge, invoking the
# built binary directly the way the queue binary's _batch_cut does (sp-vsob2: unconditionally, batch.sh's
# own cut retired). This checks WIRING, not behaviour — the pure core's replay tests
# (test-batcher.sh) already cover triggers, membership, set-asides and classification as
# fixtures with no IO at all. round-vm's OWN contract (a real VM, a real testenv run) is
# test-round-vm-e2e.sh's job, not this suite's — the stub here only proves batcher-cut calls
# round-vm run correctly and reads its results back.
#
# FOUR CASES:
#   A. happy path    — an express-certified member merges, the stub corpus is green, a PR
#                       opens, and the queue's open-batch record reads exactly what
#                       verdict.sh's own key=value format expects.
#   B. attribution    — a member whose tree breaks test-b.sh is found by the batcher's own
#                       concurrent attribution (sp-hvtgs): ejected with its suite, the
#                       survivors re-run only that suite, and the PR opens without it.
#   C. stale member    — an express-certified member whose branch conflicts with the base
#                       itself (not just batch accumulation) is reopened for rebase at once
#                       (section F), landstate RED, bump_requeue stamped.
#   D. stacking        — with case A's batch PR still open, certified members (one express)
#                       are built and proven into a prepared round on that PR's head without
#                       touching the open PR; once it lands the prepared round opens with no
#                       second corpus run (law-batcher-earns-the-round-by-parity).
#   G. land mode        — find_repo (sp-o1jm6) accepts queue.local, not only queue, and treats
#                       queue.forge byte-identically to a bare queue row.
#   I. suite corpus     — the round's suite corpus comes from the round branch's own tree,
#                       not the production checkout's working directory (law-batcher-earns-
#                       the-round-by-parity).
#   J. batcher parity   — a member whose tip is already an ancestor of the round head (reset
#                       to an old base, or no commits of its own) merges as a git no-op; it
#                       must be set aside as EMPTY, never merged, never marked BATCHED (sp-5xki9).
#   K. batcher parity (sp-myi6w) — the corpus runs through round-vm.sh run (sp-o3o6z),
#                       carrying maxpar and the toolchain pin batcher-parity requires; a
#                       --with-bins build failure (exit 4) is a local round red filed as an
#                       Ops incident, never attributed suite by suite and never a harness fault
#                       that drops the round unreported; a hung round-vm.sh is killed at the
#                       configured wall bound.
#   N. batcher parity (sp-7qk8u) — a CERTIFIED landstate record whose bead is open and not
#                       spira-submitted (the shape an ejected-then-recertified bead is in) is
#                       excluded from the round pool, not batched.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs queue/src/* spira/conf.sh spira/lib.sh spira/bead.sh spira/chamber/batcher.fayth spira/chamber/batcher.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"
# batcher, tsd-write and the queue binary case L lands through (queue/DESIGN.md §7.4) are the
# tree under test's own build, invoked by name on the suite's PATH (sp-gypjk).

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batcher-cut
TMP="$(mktemp -d)"; trap 'testdb_drop; chmod -R u+w "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchercut || { echo "test-batcher-cut: could not build fixture database"; exit 1; }

# ── the batcher binary (law-absence-needs-a-positive-control: no binary, no suite) ──
for _t in batcher tsd-write queue; do
    command -v "$_t" >/dev/null 2>&1 || { echo "test-batcher-cut: $_t is not on PATH"; exit 1; }
done

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"; QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
mkdir -p "$REPO/spira"
: > "$REPO/spira/test-a.sh"
: > "$REPO/spira/test-b.sh"
: > "$REPO/spira/test-old.sh"
git -C "$REPO" add -A && git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

# A REAL COPY OF lib.sh (and its own conf.sh), same as every other batch.sh suite: the IO
# seam shells out to lib.sh's own land_mark/bead_reopen/bump_requeue/repo_root/repo_land/
# spira_landref rather than re-deriving their side effects.
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/mail"; chmod +x "$SH/mail"

# A REAL COPY OF THE CHAMBER, so bead.sh's own --for batcher can read batcher.fayth the same
# way it would in production — file_judgement (io.rs) files through bead.sh's contract, never
# bd create directly, so the fayth's own FAYTH_LABELS is what a fake chamber has to supply.
mkdir -p "$SH/chamber"
cp "$HERE/chamber/batcher.fayth" "$SH/chamber/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

# ── stub round-vm: stands in for `round-vm run <tree-dir> --suites CSV --maxpar N --toolchain V
# --results-dir DIR --attr-spool SPOOL` (round-vm DESIGN.md §2.2a), writing testenv's result
# protocol (<status> <epoch> <secs> <fp> <mode> <producer> <rc>) into --results-dir, then
# `corpus.done`, then serving the spool's rerun requests until `close`. A suite is red when it
# is named in STUB_RED_SUITES (red on every tree, the base included), when the tree under test
# holds a file `breaks-<suite>` (red exactly on the trees that carry the member that added it),
# or while STUB_FLAKE_SUITE has been run fewer than STUB_FLAKE_RED_TIMES times (counted in
# STUB_FLAKE_COUNTER_FILE). STUB_ARGV_LOG records toolchain and maxpar; STUB_SLEEP_SECS hangs
# first (the wall-bound case); STUB_EXIT4 is testenv's workspace-build failure (exit 4).
cat > "$SH/round-vm-stub.sh" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = run ] || { printf 'round-vm-stub: unexpected verb: %s\n' "${1:-}" >&2; exit 2; }
shift
suites_csv="" maxpar="" toolchain="" results="" wt="" spool=""
while [ $# -gt 0 ]; do
    case "$1" in
        --suites) suites_csv="${2:-}"; shift 2 ;;
        --maxpar) maxpar="${2:-}"; shift 2 ;;
        --toolchain) toolchain="${2:-}"; shift 2 ;;
        --results-dir) results="${2:-}"; shift 2 ;;
        --attr-spool) spool="${2:-}"; shift 2 ;;
        *) wt="$1"; shift ;;  # the round worktree positional
    esac
done
# STUB_INSTALL_BINS: stand in for round-vm installing the VM's release build into the round
# worktree's target/release — where batcher-cut's bins_present and queue land-local look.
if [ -n "${STUB_INSTALL_BINS:-}" ] && [ -n "$wt" ]; then
    mkdir -p "$wt/target/release"
    # path-ok: the fixture round worktree's own build output, which the stub round-vm fills
    printf 'fakebin\n' > "$wt/target/release/fakebin"
    chmod +x "$wt/target/release/fakebin"   # path-ok: same fixture build output as the line above
fi
if [ -n "${STUB_ARGV_LOG:-}" ]; then
    {
        printf 'argv: run --suites %s\n' "$suites_csv"
        printf 'RUSTUP_TOOLCHAIN=%s\n' "$toolchain"
        printf 'SPIRA_BATCH_MAXPAR=%s\n' "$maxpar"
    } >> "$STUB_ARGV_LOG"
fi
: "${spool:?round-vm-stub: --attr-spool not given}"
done_file() { printf 'rc=%s\n' "$2" > "$1.tmp" && mv "$1.tmp" "$1"; }
[ -n "${STUB_SLEEP_SECS:-}" ] && sleep "$STUB_SLEEP_SECS"
if [ -n "${STUB_EXIT4:-}" ]; then done_file "$spool/corpus.done" 4; exit 4; fi
: "${results:?round-vm-stub: --results-dir not given}"
mkdir -p "$results"
IFS=',' read -r -a redset <<< "${STUB_RED_SUITES:-}"
is_red() {  # is_red <suite> <rev>
    local s="$1" r
    for r in "${redset[@]:-}"; do [ -n "$r" ] && [ "$r" = "$s" ] && return 0; done
    git -C "$wt" cat-file -e "$2:breaks-$s" 2>/dev/null && return 0
    [ -n "${STUB_FLAKE_SUITE:-}" ] && [ "$s" = "$STUB_FLAKE_SUITE" ] || return 1
    local cf="${STUB_FLAKE_COUNTER_FILE:?STUB_FLAKE_COUNTER_FILE unset}" n=0
    [ -r "$cf" ] && n="$(cat "$cf")"
    n=$((n + 1)); printf '%s' "$n" > "$cf"
    [ "$n" -le "${STUB_FLAKE_RED_TIMES:-1}" ]
}
run_suites() {  # run_suites <csv> <rev> <dir> — 1 if any red
    local out="$3" s red=0
    mkdir -p "$out"
    IFS=',' read -r -a list <<< "$1"
    for s in "${list[@]:-}"; do
        [ -n "$s" ] || continue
        if is_red "$s" "$2"; then
            printf '  FAIL — synthetic failure planted in %s\n' "$s" > "$out/$s.out"
            printf 'red %s 1 fp serial explicit 1\n' "$(date +%s)" > "$out/$s.result"
            red=1
        else
            : > "$out/$s.out"
            printf 'ok %s 1 - serial explicit 0\n' "$(date +%s)" > "$out/$s.result"
        fi
    done
    return "$red"
}
run_suites "$suites_csv" HEAD "$results"; rc=$?
done_file "$spool/corpus.done" "$rc"
declare -A taken
while :; do
    for r in "$spool"/req/*.req; do
        [ -e "$r" ] || continue
        j="$(basename "$r" .req)"
        [ -n "${taken[$j]:-}" ] && continue
        taken[$j]=1
        br="$(sed -n 's/^branch=//p' "$r")"; s="$(sed -n 's/^suites=//p' "$r")"
        run_suites "$s" "$br" "$spool/res/$j"; jrc=$?
        if [ "$(sed -n 's/^build=//p' "$r")" = round ]; then
            printf 'tree=%s\n' "$(git -C "$wt" rev-parse "$br^{tree}")" > "$spool/res/$j/batch.meta"
        fi
        done_file "$spool/res/$j.done" "$jrc"
    done
    [ -e "$spool/close" ] && break
    sleep 0.1
done
exit "$rc"
STUB
chmod +x "$SH/round-vm-stub.sh"

# ── stub incident.sh: logs the Ops filing (title, dedupe ref, cause) and returns an
# incrementing bead id, the same log-and-return-id shape as the spira-lc stub below.
INCIDENT_LOG="$TMP/incident-log"; : > "$INCIDENT_LOG"
cat > "$SH/incident.sh" <<INC
#!/usr/bin/env bash
cmd="\${1:-}"; shift
case "\$cmd" in
    file)
        title="\${1:-}"; src="\${2:--}"
        body="\$(cat "\$src" 2>/dev/null)"
        n=\$(( \$(wc -l < "$INCIDENT_LOG" 2>/dev/null || echo 0) + 1 ))
        {
            printf 'FILE title=%s ref=%s repo=%s cause=%s\n' \
                "\$title" "\${SPIRA_INCIDENT_REF:-}" "\${SPIRA_INCIDENT_REPO:-}" "\${SPIRA_INCIDENT_CAUSE:-}"
            printf '%s\n' "\$body"
        } >> "$INCIDENT_LOG"
        printf 'sp-inc%d\n' "\$n"
        ;;
    *) printf 'incident-stub: unexpected call: %s\n' "\$cmd" >&2; exit 1 ;;
esac
INC
chmod +x "$SH/incident.sh"

# ── fake forge: pr-create logs (head, base, title) and returns an incrementing PR number.
FORGE_LOG="$TMP/forge-log"; : > "$FORGE_LOG"
cat > "$SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    pr-create)
        head="\${1:-}"; base="\${2:-}"; title="\${3:-}"
        body="\$(cat)"
        n=\$(( \$(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf 'pr-create\t%s\t%s\t%s\t%s\n' "\$n" "\$head" "\$base" "\$title" >> "$FORGE_LOG"
        printf '%s\n' "\$body" >> "$TMP/pr-body-\$n"
        printf '%s\n' "\$n"
        ;;
    *) printf 'forge-fixture: unexpected call: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

# ── stub spira-lc: logs every invocation's argv (one line, space-joined) to
# SPIRA_LC_STUB_LOG, exits 0 for create-bead always (matching lcq's own "never a shortcut
# to CERTIFIED, but never blocks on it either" contract) and SPIRA_LC_STUB_RC (default 0)
# for cut/stack — so a test can plant a refusal (rc=3) as a POSITIVE CONTROL for the
# "additive, never blocks the round" contract without a real spira_lifecycle database.
cat > "$SH/spira-lc-stub.sh" <<'LCSTUB'
#!/usr/bin/env bash
log="${SPIRA_LC_STUB_LOG:?}"
certified_rows() {
    local f id st tip ep sep=''
    printf '['
    for f in "${SPIRA_RUN:-/nonexistent}"/landstate/*; do
        [ -f "$f" ] || continue
        read -r st tip ep < "$f"
        [ "$st" = CERTIFIED ] || continue
        printf '%s{"bead_id":"%s","tip":"%s","updated_at":%s}' "$sep" "$(basename "$f")" "$tip" "${ep:-0}"; sep=','
    done
    printf ']\n'
}
printf '%s\n' "$*" >> "$log"
case "${1:-}" in
    create-bead) exit 0 ;;
    cut|stack) exit "${SPIRA_LC_STUB_RC:-0}" ;;
    list) if [ "${3:-}" = CERTIFIED ]; then certified_rows; else printf '[]\n'; fi; exit 0 ;;   # lc_probe: an (empty) array; the pool: landstate's CERTIFIED records
    show) printf '{"bead":{}}\n'; exit 0 ;;   # read_stack: a bead the machine holds, unstacked
    *) exit 0 ;;
esac
LCSTUB
chmod +x "$SH/spira-lc-stub.sh"
# The stub, by name: a directory whose spira-lc is it, put first on PATH for a case (sp-gypjk).
mkdir -p "$SH/lc-stub-bin" && ln -sf ../spira-lc-stub.sh "$SH/lc-stub-bin/spira-lc"

REQUEUE_SPY="$TMP/requeue-spy"; : > "$REQUEUE_SPY"
cat >> "$SH/lib.sh" <<LIBSPY
bump_requeue() {
    printf '%s %s\n' "\${1:-}" "\${2:-}" >> "$REQUEUE_SPY"
    _bump_write_event "\${1:-}" requeued "\${2:-unrecorded}"
}
LIBSPY

cut_repo() {
    PATH="${LC_STUB_DIR:-$SH/lc-stub-bin}:$SH:$PATH" SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    STUB_RED_SUITES="${STUB_RED_SUITES:-}" \
    STUB_FLAKE_SUITE="${STUB_FLAKE_SUITE:-}" \
    STUB_FLAKE_COUNTER_FILE="${STUB_FLAKE_COUNTER_FILE:-}" \
    STUB_FLAKE_RED_TIMES="${STUB_FLAKE_RED_TIMES:-1}" \
    SPIRA_BATCHER_POLL_SECS=1 \
    STUB_ARGV_LOG="${STUB_ARGV_LOG:-}" \
    STUB_SLEEP_SECS="${STUB_SLEEP_SECS:-}" \
    STUB_EXIT4="${STUB_EXIT4:-}" \
    SPIRA_BATCH_MAXPAR="${SPIRA_BATCH_MAXPAR:-}" \
    SPIRA_BATCHER_WALL_SECS="${SPIRA_BATCHER_WALL_SECS:-}" \
    SPIRA_RELEASE_RUST_TOOLCHAIN="${SPIRA_RELEASE_RUST_TOOLCHAIN:-}" \
    SPIRA_LC_STUB_LOG="${SPIRA_LC_STUB_LOG:-$TMP/lc-default.log}" \
    SPIRA_LC_STUB_RC="${SPIRA_LC_STUB_RC:-0}" \
        batcher cut "$REPONAME" --round-vm "$SH/round-vm-stub.sh" 2>&1
}

B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status",""))' 2>/dev/null; }
open_batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
open_field() { grep "^${1}=" "$(open_batch_file)" 2>/dev/null | head -1 | cut -d= -f2-; }
notes_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("notes","") or "")' 2>/dev/null
}

labels_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))' 2>/dev/null; }

# A batch member is a bead WAITING FOR A ROUND: open and carrying the submitted label
# (sp-1346p) — a CERTIFIED record of a closed bead is stale and never batched.
plant() {   # plant <id> [express]
    local id="$1" express_label="" lbls="\"spira\",\"plan\",\"repo:$REPONAME\",\"spira-submitted\""
    [ "${2:-}" = express ] && lbls="$lbls,\"express\""
    printf '{"id":"%s","title":"%s bead","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-25T00:00:00Z"}\n' \
        "$id" "$id" "$lbls" | testdb_seed
}

# plant_open <id> [express] — status=open, no spira-submitted: the shape a bead re-marked
# CERTIFIED right after an eject (sp-pedat) is actually in, still waiting on its aeon.
plant_open() {
    local id="$1" lbls="\"spira\",\"plan\",\"repo:$REPONAME\""
    [ "${2:-}" = express ] && lbls="$lbls,\"express\""
    printf '{"id":"%s","title":"%s bead","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-25T00:00:00Z"}\n' \
        "$id" "$id" "$lbls" | testdb_seed
}

certify() {   # certify <id> <tip-sha> [epoch]
    printf 'CERTIFIED %s %s\n' "$2" "${3:-$(date +%s)}" > "$LANDSTATE/$1"
}

echo "test-batcher-cut.sh"
testdb_reset

# =============================================================================
# CASE A — happy path: an express member merges, the stub corpus is green, a PR
# opens, and the open-batch record reads exactly what verdict.sh's own format expects.
# =============================================================================
echo
echo "A. happy path: express member cuts a round and opens a PR:"
plant sp-caaa1 express
git -C "$REPO" worktree add -q -b spira/sp-caaa1 "$RUN/worktree/sp-caaa1" main
printf 'a\n' > "$RUN/worktree/sp-caaa1/a.txt"
git -C "$RUN/worktree/sp-caaa1" add -A
git -C "$RUN/worktree/sp-caaa1" commit -q -m "sp-caaa1: work"
tip_a="$(git -C "$REPO" rev-parse spira/sp-caaa1)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-caaa1"
certify sp-caaa1 "$tip_a"

out_a="$(STUB_RED_SUITES="" cut_repo)"
want "A: names the express trigger" "express sp-caaa1" "$out_a"
want "A: reports the PR opening"    "PR 1 opened"       "$out_a"
is   "A: open-batch file exists" "1" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"
is   "A: open-batch pr=1"        "1" "$(open_field pr)"
is   "A: open-batch members carries sp-caaa1:tip" "sp-caaa1:$tip_a" "$(open_field members)"
is   "A: open-batch branch is spira/queue/*" "1" "$(case "$(open_field branch)" in spira/queue/*) echo 1;; *) echo 0;; esac)"
is   "A: open-batch owner=batcher (sp-lomk3: verdict's own CI-red routing reads this)" \
    "batcher" "$(open_field owner)"
is   "A: open-batch batch_id is the batch branch spira-lc cut" "$(open_field branch)" "$(open_field batch_id)"
is   "A: open-batch version is 1 (one member)"                 "1"                    "$(open_field version)"
want "A: commit message names spira: land sp-caaa1, with the bead's own title" \
    "spira: land sp-caaa1 — sp-caaa1 bead" \
    "$(git -C "$REPO" log --format=%s "$(open_field branch)" -n 5 2>/dev/null)"
is   "A: forge pr-create called once" "1" "$(grep -c '^pr-create' "$FORGE_LOG")"
if command -v tsd-write >/dev/null 2>&1; then
    want "A: TSD batch-round row records verdict=green" '"verdict":"green"' \
        "$(tail -1 "$RUN/tsd/batch-round.jsonl" 2>/dev/null)"
fi

# Captured for case D, which reuses this batch as its "already open" starting point — case
# B and C need it gone first, to exercise the fresh-cut paths on their own.
pr_case_a="$(open_field pr)"
head_case_a="$(open_field head)"
base_case_a="$(open_field base)"
members_case_a="$(open_field members)"
branch_case_a="$(open_field branch)"

# =============================================================================
# CASE B — concurrent attribution (sp-hvtgs, law-a-round-takes-certified-tips as amended
# 2026-09-27): sp-cbbb2's tree carries breaks-test-b.sh, so test-b.sh is red on every tree
# that holds it. The batcher's own reruns (plain, base, without each member) name sp-cbbb2
# its owner; it is ejected — bead reopened, landstate EJECTED, note naming every suite it
# turned red — and only test-b.sh re-runs on the survivors: green, so the PR opens WITHOUT
# the ejected member but WITH its innocent bystander (the positive control for "only the
# named member is dropped, not the whole round").
# =============================================================================
echo
echo "B. local attribution ejects the culprit; the PR opens without it:"
plant sp-cbbb2 express
git -C "$REPO" worktree add -q -b spira/sp-cbbb2 "$RUN/worktree/sp-cbbb2" main
printf 'b\n' > "$RUN/worktree/sp-cbbb2/breaks-test-b.sh"
git -C "$RUN/worktree/sp-cbbb2" add -A
git -C "$RUN/worktree/sp-cbbb2" commit -q -m "sp-cbbb2: work"
tip_b="$(git -C "$REPO" rev-parse spira/sp-cbbb2)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cbbb2"
certify sp-cbbb2 "$tip_b"

plant sp-cbbb3
git -C "$REPO" worktree add -q -b spira/sp-cbbb3 "$RUN/worktree/sp-cbbb3" main
printf 'b3\n' > "$RUN/worktree/sp-cbbb3/b3.txt"
git -C "$RUN/worktree/sp-cbbb3" add -A
git -C "$RUN/worktree/sp-cbbb3" commit -q -m "sp-cbbb3: work"
tip_b3="$(git -C "$REPO" rev-parse spira/sp-cbbb3)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cbbb3"
certify sp-cbbb3 "$tip_b3"
rm -f "$(open_batch_file)"

prcreate_before_b="$(grep -c '^pr-create' "$FORGE_LOG")"
out_b="$(cut_repo)"
want   "B: reports the ejection, naming the member and the suite" "sp-cbbb2 ejected: red on test-b.sh" "$out_b"
want   "B: still reports the PR opening"                          "PR "                                "$out_b"
nowant "B: no judgement/double-red language — this was resolved mechanically" "judgement" "$out_b"
want   "B: attribution names the owner"  "red test-b.sh → owner sp-cbbb2" "$out_b"
want   "B: only the owner's suite re-runs on the survivors" "suites=test-b.sh" \
       "$(cat "$RUN"/batch-results/"$REPONAME"-*/spool/req/v1.req 2>/dev/null)"
is     "B: sp-cbbb2 reopened for rework (open)" "open" "$(status_of sp-cbbb2)"
nowant "B: sp-cbbb2 no longer submitted — a reopen is rework (sp-1346p)" "spira-submitted" "$(labels_of sp-cbbb2)"
want   "B: ejection note names every suite it turned red" "test-b.sh" "$(notes_of sp-cbbb2)"
is     "B: open-batch carries only the bystander" "sp-cbbb3:$tip_b3" "$(open_field members)"
nowant "B: open-batch does not carry the ejected member" "sp-cbbb2" "$(open_field members)"
is     "B: forge pr-create called (once more than before this case)" "$((prcreate_before_b + 1))" "$(grep -c '^pr-create' "$FORGE_LOG")"

# =============================================================================
# CASE G — a flake is charged to nobody: test-b.sh is red on the corpus run and green on the
# plain rerun with every member present. The batcher marks it flaky, ejects nobody, and the
# round proceeds to its PR (sp-hvtgs). SEEN RED under the pre-sp-hvtgs code: a locally-red
# round with no attribute.sh verdict never opened a PR.
# =============================================================================
echo
echo "G. a flaky red is charged to nobody and the round proceeds:"
rm -f "$(open_batch_file)"
plant sp-cgflk express
git -C "$REPO" worktree add -q -b spira/sp-cgflk "$RUN/worktree/sp-cgflk" main
printf 'g-flake\n' > "$RUN/worktree/sp-cgflk/g-flake.txt"
git -C "$RUN/worktree/sp-cgflk" add -A
git -C "$RUN/worktree/sp-cgflk" commit -q -m "sp-cgflk: work"
tip_g="$(git -C "$REPO" rev-parse spira/sp-cgflk)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cgflk"
certify sp-cgflk "$tip_g"

FLAKE_COUNTER="$TMP/flake-counter-g"; rm -f "$FLAKE_COUNTER"
prcreate_before_g="$(grep -c '^pr-create' "$FORGE_LOG")"
out_g="$(STUB_FLAKE_SUITE=test-b.sh STUB_FLAKE_COUNTER_FILE="$FLAKE_COUNTER" STUB_FLAKE_RED_TIMES=1 cut_repo)"
want   "G: the red is marked flaky"          "red test-b.sh → flaky" "$out_g"
nowant "G: nobody is ejected"                "ejected"               "$out_g"
want   "G: the round proceeds to its PR"     "PR "                   "$out_g"
is     "G: forge pr-create called once more" "$((prcreate_before_g + 1))" "$(grep -c '^pr-create' "$FORGE_LOG")"
if command -v tsd-write >/dev/null 2>&1; then
    want "G: the flake is recorded" '"outcome":"flaky"' "$(tail -1 "$RUN/tsd/round-attribution.jsonl" 2>/dev/null)"
fi
rm -f "$(open_batch_file)"

# =============================================================================
# CASE H — base red: a suite red with every member removed is the base's. It is filed as an
# Ops incident, charged to nobody, and does not hold the round (sp-hvtgs).
# =============================================================================
echo
echo "H. base itself is red: filed for Ops, nobody ejected, the round proceeds:"
rm -f "$(open_batch_file)"
plant sp-chbas express
git -C "$REPO" worktree add -q -b spira/sp-chbas "$RUN/worktree/sp-chbas" main
printf 'h\n' > "$RUN/worktree/sp-chbas/h.txt"
git -C "$RUN/worktree/sp-chbas" add -A
git -C "$RUN/worktree/sp-chbas" commit -q -m "sp-chbas: work"
tip_h2="$(git -C "$REPO" rev-parse spira/sp-chbas)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-chbas"
certify sp-chbas "$tip_h2"

prcreate_before_h="$(grep -c '^pr-create' "$FORGE_LOG")"
out_h="$(STUB_RED_SUITES="test-a.sh" cut_repo)"
want   "H: reports the base red, naming the suite" "red test-a.sh → base" "$out_h"
want   "H: reports filing an Ops incident"          "filed sp-inc"  "$out_h"
nowant "H: nobody is ejected"                       "ejected"       "$out_h"
is     "H: the round proceeds to its PR"  "$((prcreate_before_h + 1))" "$(grep -c '^pr-create' "$FORGE_LOG")"
want   "H: the incident names the base-red reason" "local-round-red" "$(cat "$INCIDENT_LOG")"
rm -f "$(open_batch_file)"

# =============================================================================
# CASE C — a stale member: an express-certified branch that conflicts with the
# base itself is reopened for rebase immediately (section F), not just skipped.
# POSITIVE CONTROL: the requeue spy is empty before this case runs.
# =============================================================================
echo
echo "C. stale member: base conflict reopens the bead at once:"
is "C: positive control — requeue spy silent before this case" "0" "$(grep -c '^sp-cccc3 ' "$REQUEUE_SPY" 2>/dev/null)"
# Case C is about a DIFFERENT member's base conflict in isolation: retire every earlier
# case's member record first, so nothing left CERTIFIED merges into case C's round and
# opens a PR it is not testing for.
rm -f "$LANDSTATE/sp-cbbb2" "$LANDSTATE/sp-cgflk" "$LANDSTATE/sp-chbas"
plant sp-cccc3 express
git -C "$REPO" worktree add -q -b spira/sp-cccc3 "$RUN/worktree/sp-cccc3" main
printf 'branch-version\n' > "$RUN/worktree/sp-cccc3/conflict.txt"
git -C "$RUN/worktree/sp-cccc3" add -A
git -C "$RUN/worktree/sp-cccc3" commit -q -m "sp-cccc3: work"
tip_c="$(git -C "$REPO" rev-parse spira/sp-cccc3)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cccc3"
certify sp-cccc3 "$tip_c" "$(( $(date +%s) - 3600 ))"

printf 'main-version\n' > "$REPO/conflict.txt"
git -C "$REPO" add conflict.txt && git -C "$REPO" commit -q -m "main: advance conflict.txt"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
rm -f "$QUEUEDIR/$REPONAME/base-moved"

out_c="$(STUB_RED_SUITES="" cut_repo)"
is   "C: sp-cccc3 is reopened"   "open" "$(status_of sp-cccc3)"
nowant "C: sp-cccc3 no longer submitted — reopened for rebase (sp-1346p)" "spira-submitted" "$(labels_of sp-cccc3)"
is   "C: bump_requeue stamped merge-conflict" "1" "$(grep -c '^sp-cccc3 merge-conflict$' "$REQUEUE_SPY")"
nowant "C: no PR opened for the conflicting-only round" "PR " "$out_c"

# =============================================================================
# CASE D — stacking: with case A's batch PR still open, the next round is built on its
# head and proven, but the open PR's branch and record are untouched and nothing opens;
# once the PR has landed, the prepared round opens without re-running the corpus.
# =============================================================================
echo
echo "D. stacking: the next round is prepared on the open batch's head, opened after it lands:"
{
    printf 'pr=%s\n'      "$pr_case_a"
    printf 'head=%s\n'    "$head_case_a"
    printf 'base=%s\n'    "$base_case_a"
    printf 'members=%s\n' "$members_case_a"
    printf 'opened=%s\n'  "$(date +%s)"
    printf 'branch=%s\n'  "$branch_case_a"
} > "$(open_batch_file)"

pr_before="$pr_case_a"
open_before="$(cat "$(open_batch_file)")"
branch_ref_before="$(git -C "$REPO" rev-parse "$branch_case_a")"
remote_ref_before="$(git -C "$REMOTE" rev-parse "$branch_case_a")"
prcreate_before="$(grep -c '^pr-create' "$FORGE_LOG")"

for spec in "sp-cddd4 express" "sp-cdde5" "sp-cddf6"; do
    set -- $spec
    plant "$@"
    git -C "$REPO" worktree add -q -b "spira/$1" "$RUN/worktree/$1" main
    printf '%s\n' "$1" > "$RUN/worktree/$1/$1.txt"
    git -C "$RUN/worktree/$1" add -A
    git -C "$RUN/worktree/$1" commit -q -m "$1: work"
    certify "$1" "$(git -C "$REPO" rev-parse "spira/$1")"
    git -C "$REPO" worktree remove -f "$RUN/worktree/$1"
done
tip_d="$(git -C "$REPO" rev-parse spira/sp-cddd4)"

D_ARGV="$TMP/d-argv"; : > "$D_ARGV"
out_d="$(STUB_ARGV_LOG="$D_ARGV" cut_repo)"
want   "D: reports a round prepared on the open head" "next round prepared on PR $pr_before's head" "$out_d"
nowant "D: no stacking message" "stacked" "$out_d"
is     "D: the corpus ran once for the prepared round" "1" "$(grep -c '^argv:' "$D_ARGV")"
is     "D: open-batch record untouched" "$open_before" "$(cat "$(open_batch_file)")"
is     "D: open PR's local branch untouched" "$branch_ref_before" "$(git -C "$REPO" rev-parse "$branch_case_a")"
is     "D: open PR's remote branch untouched (never force-pushed)" "$remote_ref_before" "$(git -C "$REMOTE" rev-parse "$branch_case_a")"
is     "D: forge pr-create not called" "$prcreate_before" "$(grep -c '^pr-create' "$FORGE_LOG")"
is     "D: express member not BATCHED while the PR is open" "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cddd4")"

: > "$D_ARGV"
out_d2="$(STUB_ARGV_LOG="$D_ARGV" cut_repo)"
want   "D: an unchanged pool is not rebuilt" "already prepared" "$out_d2"
is     "D: no second corpus run for an unchanged pool" "0" "$(grep -c '^argv:' "$D_ARGV")"

# The open PR lands: the remote base moves to its head.
git -C "$REMOTE" update-ref refs/heads/main "$head_case_a"
git -C "$REPO" fetch -q origin
rm -f "$(open_batch_file)"
: > "$D_ARGV"
out_d3="$(STUB_ARGV_LOG="$D_ARGV" cut_repo)"
want   "D: after landing, the prepared round opens a PR" "opened" "$out_d3"
is     "D: the prepared round opened without re-running the corpus" "0" "$(grep -c '^argv:' "$D_ARGV")"
is     "D: exactly one new PR" "$((prcreate_before + 1))" "$(grep -c '^pr-create' "$FORGE_LOG")"
want   "D: open-batch members carries the express member" "sp-cddd4:$tip_d" "$(open_field members)"
is     "D: prepared record consumed" "0" "$([ -f "$QUEUEDIR/$REPONAME/prepared" ] && echo 1 || echo 0)"

# =============================================================================
# CASE E — judgement-ci (sp-lomk3): verdict.sh's own CI-red producer, on a red CI
# check for a PR the batcher owns, calls this subcommand instead of running its
# own attribution. Exercised directly here, the way verdict.sh's own suite
# stubs the batcher and asserts the wiring from its own side.
# =============================================================================
echo
echo "E. judgement-ci: files a judgement bead for a CI-only red:"
judge_ci() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
        batcher judgement-ci "$REPONAME" "$@" 2>&1
}

out_e="$(judge_ci --suites test-owned.sh --members sp-caaa1 --evidence 'PR 1 — https://example.invalid/actions/runs/1')"
want "E: prints the filed bead id" "id=sp-" "$out_e"
judge_id="$(printf '%s\n' "$out_e" | sed -n 's/^id=//p')"
want "E: judgement bead title names CI"    "CI"             "$(B show "$judge_id" --json 2>/dev/null)"
want "E: judgement bead title names the suite" "test-owned.sh" "$(B show "$judge_id" --json 2>/dev/null)"
want "E: judgement bead body names the source" "CI (batcher-owned batch PR)" "$(B show "$judge_id" --long --json 2>/dev/null)"
want "E: judgement bead body names the member" "sp-caaa1"      "$(B show "$judge_id" --long --json 2>/dev/null)"
want "E: judgement bead body carries the evidence" "https://example.invalid/actions/runs/1" \
    "$(B show "$judge_id" --long --json 2>/dev/null)"

out_e2="$(judge_ci --suites '' --members sp-caaa1 --evidence x)"
is "E: no suites given is refused, not filed" "1" "$([ -z "$(printf '%s\n' "$out_e2" | sed -n 's/^id=//p')" ] && echo 1 || echo 0)"

# =============================================================================
# CASE F — spira-lc wiring (sp-o7nbr.4): BATCHED at a fresh cut and a stack both call
# spira-lc, and the open-batch record carries batch_id/version exactly when spira-lc
# applied. The machine is the subject, so these cuts run with SPIRA_LIFECYCLE_ENFORCE=1;
# OFF runs no spira-lc at all (batcher-cut/src/io.rs unit tests). POSITIVE CONTROL last: a planted refusal (rc=3) proves the call is additive —
# the PR still opens and batch_id/version stay unset, never blocking the round.
# =============================================================================
echo
echo "F. spira-lc wiring: cut calls spira-lc and records batch_id/version; a prepared round does not:"
rm -f "$(open_batch_file)"
LC_LOG="$TMP/lc-log"; : > "$LC_LOG"

plant sp-cfff6 express
git -C "$REPO" worktree add -q -b spira/sp-cfff6 "$RUN/worktree/sp-cfff6" main
printf 'f\n' > "$RUN/worktree/sp-cfff6/f.txt"
git -C "$RUN/worktree/sp-cfff6" add -A
git -C "$RUN/worktree/sp-cfff6" commit -q -m "sp-cfff6: work"
tip_f="$(git -C "$REPO" rev-parse spira/sp-cfff6)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cfff6"
certify sp-cfff6 "$tip_f"

SPIRA_LIFECYCLE_ENFORCE=1 PATH="$SH/lc-stub-bin:$PATH" SPIRA_LC_STUB_LOG="$LC_LOG" cut_repo >/dev/null
branch_f="$(open_field branch)"; head_f="$(open_field head)"; base_f="$(open_field base)"
want "F: cut calls spira-lc create-bead for the member" "create-bead sp-cfff6" "$(cat "$LC_LOG")"
want "F: cut calls spira-lc cut naming the same batch-id, head and base as the open-batch record" \
    "cut $branch_f --repo $REPONAME --head $head_f --base $base_f --members sp-cfff6:$tip_f --actor batcher" \
    "$(cat "$LC_LOG")"
is   "F: open-batch batch_id is the branch spira-lc cut" "$branch_f" "$(open_field batch_id)"
is   "F: open-batch version is 1 (one MemberAdded)"      "1"         "$(open_field version)"

: > "$LC_LOG"
plant sp-cggg7 express
git -C "$REPO" worktree add -q -b spira/sp-cggg7 "$RUN/worktree/sp-cggg7" main
printf 'g\n' > "$RUN/worktree/sp-cggg7/g.txt"
git -C "$RUN/worktree/sp-cggg7" add -A
git -C "$RUN/worktree/sp-cggg7" commit -q -m "sp-cggg7: work"
tip_g="$(git -C "$REPO" rev-parse spira/sp-cggg7)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cggg7"
certify sp-cggg7 "$tip_g"

SPIRA_LIFECYCLE_ENFORCE=1 PATH="$SH/lc-stub-bin:$PATH" SPIRA_LC_STUB_LOG="$LC_LOG" cut_repo >/dev/null
nowant "F: preparing the next round never calls spira-lc stack" "stack " "$(cat "$LC_LOG")"
nowant "F: preparing the next round never calls spira-lc cut" "cut " "$(cat "$LC_LOG")"
is   "F: open-batch batch_id unchanged by a prepared round" "$branch_f" "$(open_field batch_id)"
is   "F: open-batch version unchanged by a prepared round"  "1"         "$(open_field version)"
rm -f "$LANDSTATE/sp-cggg7" "$QUEUEDIR/$REPONAME/prepared"

# POSITIVE CONTROL: spira-lc refusing (rc=3, e.g. a member not CERTIFIED there) does not
# block the round — the PR still opens, batch_id/version are simply left unset, same as
# a legacy pre-cutover record.
rm -f "$(open_batch_file)"
: > "$LC_LOG"
plant sp-chhh8 express
git -C "$REPO" worktree add -q -b spira/sp-chhh8 "$RUN/worktree/sp-chhh8" main
printf 'h\n' > "$RUN/worktree/sp-chhh8/h.txt"
git -C "$RUN/worktree/sp-chhh8" add -A
git -C "$RUN/worktree/sp-chhh8" commit -q -m "sp-chhh8: work"
tip_h="$(git -C "$REPO" rev-parse spira/sp-chhh8)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-chhh8"
certify sp-chhh8 "$tip_h"

out_f_refused="$(SPIRA_LIFECYCLE_ENFORCE=1 PATH="$SH/lc-stub-bin:$PATH" SPIRA_LC_STUB_LOG="$LC_LOG" SPIRA_LC_STUB_RC=3 cut_repo)"
want "F: PLANTED REFUSAL — cut still reports the PR opening" "PR " "$out_f_refused"
want "F: PLANTED REFUSAL — the refusal is logged" "spira-lc cut refused for" "$out_f_refused"
is   "F: PLANTED REFUSAL — open-batch batch_id stays unset" "" "$(open_field batch_id)"
is   "F: PLANTED REFUSAL — open-batch version stays unset"  "" "$(open_field version)"

# =============================================================================
# CASE G — land mode (sp-o1jm6): find_repo accepts queue.local, never just queue, and
# queue.forge behaves byte-identically to a bare queue row (the alias lib.sh's own repo_land
# already normalizes). SEEN RED on today's code: find_repo refuses any mode but "queue", so
# localmode's cut reports "mode is \"queue.local\", not queue" instead of ever reaching
# should_cut.
# =============================================================================
echo
echo "G. land mode: queue.forge alias regression, queue.local accepted by find_repo:"

FREPO="$TMP/forge-alias-repo"
git init -q -b main "$FREPO"
git -C "$FREPO" commit -q --allow-empty -m base

LREPO="$TMP/local-mode-repo"
git init -q -b trunk "$LREPO"
git -C "$LREPO" commit -q --allow-empty -m base
git -C "$LREPO" branch local/main trunk

cat > "$SH/repo-map" <<RMAP
$REPONAME  | $REPO  | queue       | origin/main | | |
forgealias | $FREPO | queue.forge | main        | | |
localmode  | $LREPO | queue.local | local/main  | | |
RMAP

cut_other() {
    PATH="${LC_STUB_DIR:-$SH/lc-stub-bin}:$PATH" SPIRA_LC_STUB_LOG="$TMP/lc-default.log" SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        batcher cut "$1" --round-vm "$SH/round-vm-stub.sh" 2>&1
}

out_g_forge="$(cut_other forgealias)"
nowant "G: queue.forge alias is accepted, not refused (regression)" "not queue" "$out_g_forge"
want   "G: queue.forge alias reaches no-trigger cleanly, same as a bare queue row" "no round cut: no trigger" "$out_g_forge"

out_g_local="$(cut_other localmode)"
nowant "G: queue.local is accepted by find_repo (SEEN RED on today's code: refuses it)" "not queue" "$out_g_local"
want   "G: queue.local reaches no-trigger cleanly, never a push" "no round cut: no trigger" "$out_g_local"

# =============================================================================
# CASE I — batcher parity: the round's suite corpus comes from the round branch's
# own tree, not the production checkout's working directory (law-batcher-earns-
# the-round-by-parity). A suite the round adds must run, one it deletes must not
# be requested, and an untracked stray sitting in the checkout must not either.
# =============================================================================
echo
echo "I. suite corpus tracks the round branch, not the checkout's working tree:"
rm -f "$(open_batch_file)"

# POSITIVE CONTROL for the stray case: never committed, never on any branch.
: > "$REPO/spira/test-stray.sh"

plant sp-ciii9 express
git -C "$REPO" worktree add -q -b spira/sp-ciii9 "$RUN/worktree/sp-ciii9" main
git -C "$RUN/worktree/sp-ciii9" rm -q spira/test-old.sh
: > "$RUN/worktree/sp-ciii9/spira/test-new.sh"
git -C "$RUN/worktree/sp-ciii9" add -A
git -C "$RUN/worktree/sp-ciii9" commit -q -m "sp-ciii9: drop test-old.sh, add test-new.sh"
tip_i="$(git -C "$REPO" rev-parse spira/sp-ciii9)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-ciii9"
certify sp-ciii9 "$tip_i"

out_i="$(STUB_RED_SUITES="" cut_repo)"
want "I: reports the PR opening" "PR " "$out_i"
results_i="$(ls -td "$RUN"/batch-results/"$REPONAME"-* 2>/dev/null | head -1)/corpus"
is "I: the round's new suite is requested"        "1" "$([ -f "$results_i/test-new.sh.result" ] && echo 1 || echo 0)"
is "I: the round's deleted suite is not requested" "0" "$([ -f "$results_i/test-old.sh.result" ] && echo 1 || echo 0)"
is "I: an untracked stray in the checkout is not requested" "0" "$([ -f "$results_i/test-stray.sh.result" ] && echo 1 || echo 0)"
rm -f "$REPO/spira/test-stray.sh"

# =============================================================================
# CASE J — batcher parity (sp-5xki9): a member whose tip is already an ancestor
# of the round head — reset to an old base, or with no commits of its own —
# merges as a git no-op ("Already up to date"). It must be set aside as EMPTY,
# never merged into the batch worktree, and never marked BATCHED.
# =============================================================================
echo
echo "J. batcher parity: a member whose tip is already in the round is set aside, never merged or marked:"
rm -f "$(open_batch_file)"
plant sp-cjjjj express
base_now="$(git -C "$REPO" rev-parse origin/main)"
# THE BRANCH MUST EXIST for certified_pool to admit it (repo_branch_ids scopes the
# landstate directory to this repo's own refs/heads/spira/* — see io.rs). Its tip is the
# CURRENT base itself: exactly the "reset to an old main" / "no commits of its own" shape
# the bead names, an ancestor of the round head before any merge is attempted.
git -C "$REPO" branch -f spira/sp-cjjjj "$base_now"
certify sp-cjjjj "$base_now"
prcreate_before_j="$(grep -c '^pr-create' "$FORGE_LOG")"

out_j="$(STUB_RED_SUITES="" cut_repo)"
want "J: reports the member set aside as EMPTY, not a member" "EMPTY sp-cjjjj: tip is already in the round — not a member" "$out_j"
nowant "J: no PR opens crediting a no-op tip" "PR " "$out_j"
is   "J: sp-cjjjj stays CERTIFIED, never marked BATCHED" "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cjjjj")"
is   "J: forge pr-create not called for the empty round" "$prcreate_before_j" "$(grep -c '^pr-create' "$FORGE_LOG")"

# =============================================================================
# CASE K — batcher parity (sp-myi6w, sp-o3o6z): the corpus runs through round-vm.sh run, and
# a --with-bins build failure is a round-level red filed for Ops, never a harness
# fault that silently drops the round.
# =============================================================================
echo
echo "K. batcher parity: round-vm.sh run, maxpar, toolchain, wall bound, exit 4:"

# K1 — default argv: SPIRA_BATCH_MAXPAR and SPIRA_RELEASE_RUST_TOOLCHAIN both unset, so the
# batcher's own defaults (16, 1.82.0) must appear on round-vm.sh's own --maxpar/--toolchain
# regardless of whether the ambient environment happens to carry the Concierge's own values.
rm -f "$(open_batch_file)"
plant sp-cgaa1 express
git -C "$REPO" worktree add -q -b spira/sp-cgaa1 "$RUN/worktree/sp-cgaa1" main
printf 'g1\n' > "$RUN/worktree/sp-cgaa1/g1.txt"
git -C "$RUN/worktree/sp-cgaa1" add -A
git -C "$RUN/worktree/sp-cgaa1" commit -q -m "sp-cgaa1: work"
tip_g1="$(git -C "$REPO" rev-parse spira/sp-cgaa1)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cgaa1"
certify sp-cgaa1 "$tip_g1"

ARGV_LOG="$TMP/argv-log-default"; : > "$ARGV_LOG"
STUB_ARGV_LOG="$ARGV_LOG" cut_repo >/dev/null
want "K1: corpus runs through round-vm.sh run" "argv: run --suites" "$(cat "$ARGV_LOG")"
want "K1: default toolchain pin is 1.82.0 (release.yml's own pin, no independent default)" \
    "RUSTUP_TOOLCHAIN=1.82.0" "$(cat "$ARGV_LOG")"
want "K1: default maxpar is 16 (the Concierge's own proven parallelism)" \
    "SPIRA_BATCH_MAXPAR=16" "$(cat "$ARGV_LOG")"

# K2 — configured overrides are honored, pinned to NON-DEFAULT values so this cannot pass
# against code that hard-codes K1's own defaults.
rm -f "$(open_batch_file)"
plant sp-cgbb2 express
git -C "$REPO" worktree add -q -b spira/sp-cgbb2 "$RUN/worktree/sp-cgbb2" main
printf 'g2\n' > "$RUN/worktree/sp-cgbb2/g2.txt"
git -C "$RUN/worktree/sp-cgbb2" add -A
git -C "$RUN/worktree/sp-cgbb2" commit -q -m "sp-cgbb2: work"
tip_g2="$(git -C "$REPO" rev-parse spira/sp-cgbb2)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cgbb2"
certify sp-cgbb2 "$tip_g2"

ARGV_LOG2="$TMP/argv-log-override"; : > "$ARGV_LOG2"
SPIRA_BATCH_MAXPAR=7 SPIRA_RELEASE_RUST_TOOLCHAIN=1.77.3 STUB_ARGV_LOG="$ARGV_LOG2" cut_repo >/dev/null
want "K2: SPIRA_BATCH_MAXPAR overrides the default" "SPIRA_BATCH_MAXPAR=7" "$(cat "$ARGV_LOG2")"
want "K2: SPIRA_RELEASE_RUST_TOOLCHAIN overrides the default toolchain pin" \
    "RUSTUP_TOOLCHAIN=1.77.3" "$(cat "$ARGV_LOG2")"

# K3 — a --with-bins build failure (exit 4) is a round-level red: filed as an Ops incident
# naming the round's own members (the same file_local_red_incident path case H's base-fail
# uses), not a harness fault that aborts the round with nobody blamed (the defect this bead
# names: "unexpected exit 4"), and never attributed suite by suite — there is no member-subset
# suite rerun that bisects "does the merged tree compile".
rm -f "$(open_batch_file)"
plant sp-cgcc3 express
git -C "$REPO" worktree add -q -b spira/sp-cgcc3 "$RUN/worktree/sp-cgcc3" main
printf 'g3\n' > "$RUN/worktree/sp-cgcc3/g3.txt"
git -C "$RUN/worktree/sp-cgcc3" add -A
git -C "$RUN/worktree/sp-cgcc3" commit -q -m "sp-cgcc3: work"
tip_g3="$(git -C "$REPO" rev-parse spira/sp-cgcc3)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cgcc3"
certify sp-cgcc3 "$tip_g3"

prcreate_before_g3="$(grep -c '^pr-create' "$FORGE_LOG")"
out_g3="$(STUB_EXIT4=1 cut_repo)"
want   "K3: a workspace build failure is reported as a local round red" "workspace failed to build" "$out_g3"
nowant "K3: never reported as an unexpected exit"                       "unexpected exit" "$out_g3"
nowant "K3: never reported as a harness fault"                          "harness fault"   "$out_g3"
is     "K3: no PR opened for a round whose workspace failed to build" \
    "$prcreate_before_g3" "$(grep -c '^pr-create' "$FORGE_LOG")"
is     "K3: sp-cgcc3 stays CERTIFIED — attributed, not silently dropped" \
    "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cgcc3")"
want   "K3: reports filing an Ops incident" "filed sp-inc" "$out_g3"
want   "K3: the incident names the workspace-build reason" \
    "workspace-build" "$(cat "$INCIDENT_LOG")"

# K4 — a hung round-vm.sh is killed at the configured wall bound, not left to hold the
# round lock forever (main.rs's cut() holds it for the whole call).
rm -f "$(open_batch_file)"
plant sp-cgdd4 express
git -C "$REPO" worktree add -q -b spira/sp-cgdd4 "$RUN/worktree/sp-cgdd4" main
printf 'g4\n' > "$RUN/worktree/sp-cgdd4/g4.txt"
git -C "$RUN/worktree/sp-cgdd4" add -A
git -C "$RUN/worktree/sp-cgdd4" commit -q -m "sp-cgdd4: work"
tip_g4="$(git -C "$REPO" rev-parse spira/sp-cgdd4)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cgdd4"
certify sp-cgdd4 "$tip_g4"

g4_start="$(date +%s)"
out_g4="$(SPIRA_BATCHER_WALL_SECS=1 STUB_SLEEP_SECS=30 cut_repo)"
g4_elapsed=$(( $(date +%s) - g4_start ))
# Generous margin above the 1s bound plus timeout's own 10s kill-after: this proves the round
# is bounded at all, not that it is bounded tightly.
if [ "$g4_elapsed" -lt 20 ]; then
    ok "K4: a hung corpus run is killed near the configured wall bound (${g4_elapsed}s)"
else
    bad "K4: a hung corpus run is killed near the configured wall bound" "still running after ${g4_elapsed}s"
fi
want "K4: reports the wall bound as the cause" "wall bound" "$out_g4"

# =============================================================================
# CASE L — queue.local's terminal step (sp-828tp, epic sp-hq9x8): a round lands locally via
# queue land-local — no push, no forge call, no open-batch record — and terminal_ready
# (core) refuses before land-local is even invoked when the round's own --with-bins corpus is
# missing. SEEN RED on today's code: Land::Local reaches push_branch, whose own assert fires
# ("unreachable under queue.local"), so the batcher process panics instead of landing.
# =============================================================================
echo
echo "L. queue.local: a round lands locally via queue land-local — no push, no PR:"

LREPO="$TMP/local-land-repo"
git init -q -b trunk "$LREPO"
# One suite in the tree, as every real repository has: the round's corpus step (the stub
# round-vm, which is what installs the round's target/release under STUB_INSTALL_BINS) only
# runs when the round's own tree names a suite — an empty corpus never builds bins at all.
mkdir -p "$LREPO/spira"
: > "$LREPO/spira/test-local.sh"
git -C "$LREPO" add -A
git -C "$LREPO" commit -q -m base
git -C "$LREPO" branch local/main trunk
LRELEASES="$TMP/local-releases"; mkdir -p "$LRELEASES"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO  | queue       | origin/main | | |
locland   | $LREPO | queue.local | local/main  | | |
RMAP

cut_local() {
    PATH="${LC_STUB_DIR:-$SH/lc-stub-bin}:$SH:$PATH" SPIRA_LC_STUB_LOG="$TMP/lc-default.log" SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_WAIT="${SPIRA_QUEUE_BATCH_WAIT_OVERRIDE:-999999}" \
    SPIRA_RELEASES="$LRELEASES" \
    SPIRA_LC_STACKS_DIR="${SPIRA_LC_STACKS_DIR:-}" \
    STUB_INSTALL_BINS="${STUB_INSTALL_BINS:-}" \
        batcher cut locland --round-vm "$SH/round-vm-stub.sh" 2>&1
}

# L1 — missing --with-bins corpus: terminal_ready refuses before land-local is ever called,
# nothing changes.
localmain_pre="$(git -C "$LREPO" rev-parse local/main)"
plant sp-claa1 express
git -C "$LREPO" worktree add -q -b spira/sp-claa1 "$RUN/worktree/sp-claa1" trunk
printf 'l1\n' > "$RUN/worktree/sp-claa1/l1.txt"
git -C "$RUN/worktree/sp-claa1" add -A
git -C "$RUN/worktree/sp-claa1" commit -q -m "sp-claa1: work"
tip_l1="$(git -C "$LREPO" rev-parse spira/sp-claa1)"
git -C "$LREPO" worktree remove -f "$RUN/worktree/sp-claa1"
certify sp-claa1 "$tip_l1"

out_l1="$(cut_local)"
want   "L1: refuses — no --with-bins corpus for this head" "no --with-bins" "$out_l1"
nowant "L1: never reports landing locally"                  "landed locally" "$out_l1"
is     "L1: local/main is untouched"          "$localmain_pre" "$(git -C "$LREPO" rev-parse local/main)"
is     "L1: sp-claa1 stays CERTIFIED — refused, not ejected" \
       "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-claa1")"

# L2 — the --with-bins corpus is present: lands locally, no push, no PR, no open-batch file.
rm -f "$LANDSTATE/sp-claa1"
plant sp-clbb2 express
git -C "$LREPO" worktree add -q -b spira/sp-clbb2 "$RUN/worktree/sp-clbb2" trunk
printf 'l2\n' > "$RUN/worktree/sp-clbb2/l2.txt"
git -C "$RUN/worktree/sp-clbb2" add -A
git -C "$RUN/worktree/sp-clbb2" commit -q -m "sp-clbb2: work"
tip_l2="$(git -C "$LREPO" rev-parse spira/sp-clbb2)"
git -C "$LREPO" worktree remove -f "$RUN/worktree/sp-clbb2"
certify sp-clbb2 "$tip_l2"

prcreate_before_l2="$(grep -c '^pr-create' "$FORGE_LOG")"
out_l2="$(STUB_INSTALL_BINS=1 cut_local)"
want   "L2: reports landing locally"      "landed locally" "$out_l2"
nowant "L2: never reports a PR opening"   "PR "            "$out_l2"
is     "L2: forge pr-create never called" "$prcreate_before_l2" "$(grep -c '^pr-create' "$FORGE_LOG")"
# The round head is a --no-ff merge commit, not the member's own tip — its parents are
# [local/main's pre-round head, tip_l2], so compare local/main against what the batcher
# itself reported landing at, and separately prove that head really does carry tip_l2.
head_l2="$(printf '%s\n' "$out_l2" | sed -n 's/.*landed locally at \([0-9a-f]\{7,\}\).*/\1/p' | head -1)"
is     "L2: local/main fast-forwards to the round's own merge head" "$head_l2" "$(git -C "$LREPO" rev-parse local/main)"
is     "L2: the round head descends from the member's own tip" "0" "$(git -C "$LREPO" merge-base --is-ancestor "$tip_l2" "$head_l2"; echo $?)"
is     "L2: no open-batch file under queue.local" "0" "$([ -f "$QUEUEDIR/locland/open" ] && echo 1 || echo 0)"
if command -v tsd-write >/dev/null 2>&1; then
    want "L2: TSD batch-round row records verdict=landed_local" '"verdict":"landed_local"' \
        "$(tail -1 "$RUN/tsd/batch-round.jsonl" 2>/dev/null)"
fi

# =============================================================================
# CASE N — pacing parity (sp-ffezo, law-batcher-earns-the-round-by-parity): a pool of ONE
# non-express certified member, no batch open, cuts even though queue_batch_wait is a year —
# the Concierge's own hand rule, not an adaptive N that reduced to the live pool itself.
# SEEN RED on today's code: sp-cnaa1 alone never reached N=4, and the year-long wait keeps
# the idle path from firing either, so cut reports "no round cut: no trigger".
# =============================================================================
echo
echo "N. pacing parity: pool of one, no batch open, cuts despite a year-long queue_batch_wait:"

plant sp-cnaa1
git -C "$LREPO" worktree add -q -b spira/sp-cnaa1 "$RUN/worktree/sp-cnaa1" trunk
printf 'n\n' > "$RUN/worktree/sp-cnaa1/n.txt"
git -C "$RUN/worktree/sp-cnaa1" add -A
git -C "$RUN/worktree/sp-cnaa1" commit -q -m "sp-cnaa1: work"
tip_n="$(git -C "$LREPO" rev-parse spira/sp-cnaa1)"
git -C "$LREPO" worktree remove -f "$RUN/worktree/sp-cnaa1"
certify sp-cnaa1 "$tip_n"

out_n="$(SPIRA_QUEUE_BATCH_WAIT_OVERRIDE=31536000 STUB_INSTALL_BINS=1 cut_local)"
nowant "N: never reports no-trigger — a pool of one is enough" "no round cut: no trigger" "$out_n"
want   "N: reports landing locally"                             "landed locally"          "$out_n"

# =============================================================================
# CASE M — stacked dependents (sp-lno75, design stacked-dependents-2026-09-28): a round that
# takes a bead takes every prerequisite named in its stack, merged in topological order
# regardless of certification order — SEEN RED on today's code, which sorts the pool by
# (express, priority, certified_at) alone (order_key, batcher/src/core.rs:28) and has no
# notion of a stack: handing the pool certified in the order C, A, B (B stacked on A, C
# stacked on B) merges C first, so A and B's tips are already ancestors of the round head
# once their own turn comes, MergeResult::Empty sets each aside as "not a member" (sp-5xki9's
# own rule, applied here to a bug it didn't anticipate), and only C ever reaches LANDED.
# =============================================================================
echo
echo "M. stacked dependents: closure, topological order, every member reaches LANDED:"

LC_STACKS="$TMP/lc-stacks"; mkdir -p "$LC_STACKS"
cat > "$SH/spira-lc-stack-stub.sh" <<'LCSTACKSTUB'
#!/usr/bin/env bash
certified_rows() {
    local f id st tip ep sep=''
    printf '['
    for f in "${SPIRA_RUN:-/nonexistent}"/landstate/*; do
        [ -f "$f" ] || continue
        read -r st tip ep < "$f"
        [ "$st" = CERTIFIED ] || continue
        printf '%s{"bead_id":"%s","tip":"%s","updated_at":%s}' "$sep" "$(basename "$f")" "$tip" "${ep:-0}"; sep=','
    done
    printf ']\n'
}
case "${1:-}" in
    list) if [ "${3:-}" = CERTIFIED ]; then certified_rows; else printf '[]\n'; fi; exit 0 ;;
    show)
        f="${SPIRA_LC_STACKS_DIR:?}/${2:-}"
        stack="{}"
        [ -f "$f" ] && stack="$(cat "$f")"
        printf '{"bead":{"state":"CERTIFIED","version":"3","stack":%s}}\n' "$stack"
        exit 0
        ;;
    *) exit 0 ;;
esac
LCSTACKSTUB
chmod +x "$SH/spira-lc-stack-stub.sh"
mkdir -p "$SH/lc-stack-stub-bin" && ln -sf ../spira-lc-stack-stub.sh "$SH/lc-stack-stub-bin/spira-lc"

# A -> B -> C: a straight chain, each bead's branch built on the previous bead's own tip, the
# same shape a real stacked worktree gets once bead 3 (aeon base = landing ref + stack) lands.
git -C "$LREPO" worktree add -q -b spira/sp-cmaa1 "$RUN/worktree/sp-cmaa1" local/main
printf 'a\n' > "$RUN/worktree/sp-cmaa1/a.txt"
git -C "$RUN/worktree/sp-cmaa1" add -A
git -C "$RUN/worktree/sp-cmaa1" commit -q -m "sp-cmaa1: a"
tip_ma="$(git -C "$LREPO" rev-parse spira/sp-cmaa1)"
git -C "$LREPO" worktree remove -f "$RUN/worktree/sp-cmaa1"

git -C "$LREPO" worktree add -q -b spira/sp-cmbb2 "$RUN/worktree/sp-cmbb2" spira/sp-cmaa1
printf 'b\n' > "$RUN/worktree/sp-cmbb2/b.txt"
git -C "$RUN/worktree/sp-cmbb2" add -A
git -C "$RUN/worktree/sp-cmbb2" commit -q -m "sp-cmbb2: b"
tip_mb="$(git -C "$LREPO" rev-parse spira/sp-cmbb2)"
git -C "$LREPO" worktree remove -f "$RUN/worktree/sp-cmbb2"

git -C "$LREPO" worktree add -q -b spira/sp-cmcc3 "$RUN/worktree/sp-cmcc3" spira/sp-cmbb2
printf 'c\n' > "$RUN/worktree/sp-cmcc3/c.txt"
git -C "$RUN/worktree/sp-cmcc3" add -A
git -C "$RUN/worktree/sp-cmcc3" commit -q -m "sp-cmcc3: c"
tip_mc="$(git -C "$LREPO" rev-parse spira/sp-cmcc3)"
git -C "$LREPO" worktree remove -f "$RUN/worktree/sp-cmcc3"

printf '{"sp-cmaa1":"%s"}\n' "$tip_ma" > "$LC_STACKS/sp-cmbb2"
printf '{"sp-cmbb2":"%s"}\n' "$tip_mb" > "$LC_STACKS/sp-cmcc3"

# Certified in the order C, A, B — certified_at deliberately reversed against dependency
# order, so a sort on order_key alone (no stack awareness) would try to merge C first.
plant sp-cmcc3; plant sp-cmaa1; plant sp-cmbb2
certify sp-cmcc3 "$tip_mc" 100
certify sp-cmaa1 "$tip_ma" 200
certify sp-cmbb2 "$tip_mb" 300

# Stacking is a lifecycle-machine concept (read_stack runs nothing with the switch OFF), so
# this case runs ON.
out_m="$(SPIRA_LIFECYCLE_ENFORCE=1 STUB_INSTALL_BINS=1 LC_STUB_DIR="$SH/lc-stack-stub-bin" SPIRA_LC_STACKS_DIR="$LC_STACKS" cut_local)"
want   "M: reports landing locally"                             "landed locally"  "$out_m"
want   "M: all three members landed in one round"                "3 member(s)"    "$out_m"
head_m="$(git -C "$LREPO" rev-parse local/main)"
# Merged prerequisite-first: the round's own land-subject merge commits appear in the order
# A, B, C along local/main's first-parent history, not the certification order C, A, B.
is     "M: merged in topological order A, B, C" \
       "$(printf 'sp-cmaa1\nsp-cmbb2\nsp-cmcc3')" \
       "$(git -C "$LREPO" log --first-parent --format=%s "$head_m" | sed -n 's/^spira: land \(sp-cm[a-z0-9]*\).*/\1/p' | tac)"

# =============================================================================
# CASE N — batcher parity (sp-7qk8u): landstate CERTIFIED alone is not enough to admit a
# member. A bead re-marked CERTIFIED at the same tip right after an eject (sp-pedat) is open
# again, not spira-submitted — the same admission batch.sh's own _certified_list already
# refuses ("CERTIFIED landstate but bead status=open; refusing admission"). Upstream landed the filter (sp-1346p); this pins it.
# =============================================================================
echo
echo "N. batcher parity: CERTIFIED landstate but bead status=open (not spira-submitted) is excluded:"
# sp-cjjjj (J), sp-cgcc3 (K3) and sp-cgdd4 (K4) all stay CERTIFIED by design in their own
# cases and their branches persist in $REPO — retire them first so this round is only
# sp-ciiii, the way case C already retires case G/H's own leftovers (line 548 above).
rm -f "$(open_batch_file)" "$LANDSTATE/sp-cjjjj" "$LANDSTATE/sp-cgcc3" "$LANDSTATE/sp-cgdd4"
plant_open sp-ciiii express
git -C "$REPO" worktree add -q -b spira/sp-ciiii "$RUN/worktree/sp-ciiii" main
printf 'i\n' > "$RUN/worktree/sp-ciiii/i.txt"
git -C "$RUN/worktree/sp-ciiii" add -A
git -C "$RUN/worktree/sp-ciiii" commit -q -m "sp-ciiii: work"
tip_n="$(git -C "$REPO" rev-parse spira/sp-ciiii)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-ciiii"
certify sp-ciiii "$tip_n"

prcreate_before_n="$(grep -c '^pr-create' "$FORGE_LOG")"
out_n="$(STUB_RED_SUITES="" cut_repo)"
nowant "N: never reports a PR opening for the excluded-only round" "PR " "$out_n"
is     "N: forge pr-create not called" "$prcreate_before_n" "$(grep -c '^pr-create' "$FORGE_LOG")"
is     "N: no open-batch file" "0" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"
is     "N: sp-ciiii landstate stays CERTIFIED — untouched, not re-ejected" \
       "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-ciiii")"
is     "N: sp-ciiii bead status stays open" "open" "$(status_of sp-ciiii)"

tl_summary
