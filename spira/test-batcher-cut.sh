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
#   N. batcher parity (sp-7qk8u) — a CERTIFIED lifecycle row whose bead is open and not
#                       spira-submitted (the shape an ejected-then-recertified bead is in) is
#                       excluded from the round pool, not batched.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs queue/src/* spira/conf.sh spira/lib.sh spira/bead.sh spira/runes/batcher.rune spira/runes/batcher.md spira/testlib/lc-fixture.sh UC-landing-merge-queue-48
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# CUT_PART names the slice of cases this run owns; test-batcher-cut-<part>.sh sets it and
# sources this file, so every slice shares one fixture build and none nears the wall bound.
CUT_PART="${CUT_PART:-main}"
case "$CUT_PART" in
    main)  CUT_SECTIONS="A D" ;;
    wiring) CUT_SECTIONS="F Gf" ;;
    land)  CUT_SECTIONS="Gl I J O Nr" ;;
    stack) CUT_SECTIONS="M" ;;
    *) echo "test-batcher-cut: unknown CUT_PART $CUT_PART" >&2; exit 1 ;;
esac
cut_runs() { case " $CUT_SECTIONS " in *" $1 "*) return 0 ;; esac; return 1; }
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180
# batcher and tsd-write are the
# tree under test's own build, invoked by name on the suite's PATH (sp-gypjk).

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
# testdb.sh sources conf.sh for its own bookkeeping, which (one source of config, per Ryan
# 2026-10-05) evals spira_config::resolve()'s FULL output into THIS shell — every registered
# key, including ones with no default, as a plain shell variable. Before that eval existed, a
# key the suite never declared stayed truly unset; now cut_repo's own
# `${SPIRA_BATCH_MAXPAR:-16}` sees the complete fixture's base value (8) instead, because
# sourcing conf.sh already set it. Unset it here, once, right after the only sourcing that
# pollutes it, so "unset" in cut_repo means what it always meant: no caller override, use the
# Concierge's own proven 16 (K1's own positive control for this).
unset SPIRA_BATCH_MAXPAR
testdb_require test-batcher-cut
TMP="$(mktemp -d)"; trap 'lcfix_down; testdb_drop; chmod -R u+w "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
testdb_up "batchercut$CUT_PART" || { echo "test-batcher-cut: could not build fixture database"; exit 1; }
lcfix_up || { echo "test-batcher-cut: could not build a lifecycle fixture"; exit 1; }

# ── the batcher binary (law-absence-needs-a-positive-control: no binary, no suite) ──
for _t in batcher tsd-write queue; do
    command -v "$_t" >/dev/null 2>&1 || { echo "test-batcher-cut: $_t is not on PATH"; exit 1; }
done

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
REPONAME=fixture-repo
# LCSTUB: the stub spira-lc's CERTIFIED pool, one file per bead.
LCSTUB="$RUN/lc-stub"; QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
mkdir -p "$REPO/spira"
: > "$REPO/spira/test-a.sh"
: > "$REPO/spira/test-b.sh"
: > "$REPO/spira/test-old.sh"
git -C "$REPO" add -A && git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
timeout 5 git -C "$REPO" push -q origin main
timeout 5 git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LCSTUB" "$QUEUEDIR/$REPONAME"

# A REAL COPY OF lib.sh (and its own conf.sh), same as every other batch.sh suite: the IO
# seam shells out to lib.sh's own land_mark/bead_reopen/bump_requeue/repo_root/repo_land/
# spira_landref rather than re-deriving their side effects.
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/mail"; chmod +x "$SH/mail"

# A REAL COPY OF THE RUNES, so bead.sh's own --for batcher can read batcher.rune the same
# way it would in production — file_judgement (io.rs) files through bead.sh's contract, never
# bd create directly, so the rune's own RUNE_LABELS is what a fake runes has to supply.
mkdir -p "$SH/runes"
cp "$HERE/runes/batcher.rune" "$SH/runes/"
# SPIRA_RUNES_DIR no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at this suite's own fixture runes explicitly.
tl_config SPIRA_RUNES_DIR="$SH/runes"

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
if [ "${1:-}" = refresh ]; then printf 'refresh %s\n' "${2:-}" >> "${STUB_REFRESH_LOG:-/dev/null}"; exit 0; fi
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
        --base) shift 2 ;;
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
    for f in "${SPIRA_RUN:-/nonexistent}"/lc-stub/*; do
        [ -f "$f" ] || continue
        read -r st tip ep < "$f"
        [ "$st" = CERTIFIED ] || continue
        grep -qx "$(basename "$f")" "${SPIRA_RUN:-/nonexistent}/lc-taken" 2>/dev/null && continue
        ex=0; [ -f "${SPIRA_RUN:-/nonexistent}/lc-express/$(basename "$f")" ] && ex=1
        sf="$tip"; [ -f "${SPIRA_RUN:-/nonexistent}/lc-unsifted/$(basename "$f")" ] && sf=""
        printf '%s{"bead_id":"%s","state":"CERTIFIED","tip":"%s","updated_at":%s,"express":"%s","sifted_tip":"%s"}' "$sep" "$(basename "$f")" "$tip" "${ep:-0}" "$ex" "$sf"; sep=','
    done
    printf ']\n'
}
printf '%s\n' "$*" >> "$log"
case "${1:-}" in
    create-bead) exit 0 ;;
    plan) [ "${2:-}" = items ] && exec "${SH_CONTENT_SHIM:?}/spira-lc" "$@"; exit 0 ;;
    cut)
        members=""; while [ $# -gt 0 ]; do [ "$1" = --members ] && members="$2"; shift; done
        for m in $(printf '%s' "$members" | tr ',' ' '); do printf '%s\n' "${m%%:*}" >> "${SPIRA_RUN:?}/lc-taken"; done
        exit "${SPIRA_LC_STUB_RC:-0}" ;;
    stack) exit "${SPIRA_LC_STUB_RC:-0}" ;;
    list) certified_rows; exit 0 ;;   # one unfiltered read per repo step: the stub's CERTIFIED rows, each carrying its state
    # read_stack: a bead the machine holds, unstacked; its state and a version for the
    # batcher's own withdrawal (sp-mve9i: Deliver then Returned takes a member to REWORK).
    show) st=; tip=; [ -f "${SPIRA_RUN:-/nonexistent}/lc-stub/${2:-}" ] && read -r st tip _ < "${SPIRA_RUN}/lc-stub/$2"
          printf '{"bead":{"bead_id":"%s","state":"%s","tip":"%s","version":1}}\n' "${2:-}" "$st" "$tip"; exit 0 ;;
    event) f="${SPIRA_RUN:-/nonexistent}/lc-stub/${3:-}"
           case "$*" in *Returned*|*GateRed*) [ -f "$f" ] && { read -r _ tip ep < "$f"; printf 'REWORK %s %s\n' "$tip" "$ep" > "$f"; } ;; esac
           exit 0 ;;
    # reopen (sp-swh8b8): the door moves the row, then reopens the store — open, unassigned,
    # no submitted label; here only the store half, the row being this fixture's file.
    reopen) b="${TESTDB_BD:-$(spira-config get spira.bd 2>/dev/null)}"; d="${SPIRA_DB:-$(spira-config get spira.db 2>/dev/null)}"
            "${b:-bd}" ${d:+-C "$d"} update "$2" --status open --assignee "" >/dev/null 2>&1
            "${b:-bd}" ${d:+-C "$d"} label remove "$2" "$(spira-config get spira.submitted_label 2>/dev/null || echo spira-submitted)" >/dev/null 2>&1
            f="${SPIRA_RUN:-/nonexistent}/lc-stub/${2:-}"; [ -f "$f" ] && { read -r _ tip ep < "$f"; printf 'REWORK %s %s\n' "$tip" "$ep" > "$f"; }
            exit 0 ;;
    *) exit 0 ;;
esac
LCSTUB
chmod +x "$SH/spira-lc-stub.sh"
lc_content_shim "$SH/lcshim"; export SH_CONTENT_SHIM="$SH/lcshim" PATH="$SH/lcshim:$PATH"
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
    # ONE SOURCE OF CONFIG (per Ryan 2026-10-05): the registered keys batcher reads via
    # spira.toml, not env — declared here, fresh per call, so a caller's own VAR=val prefix
    # (still read as a plain shell var below) still reaches the binary. The maxpar default
    # (16, "the Concierge's own proven parallelism") is declared explicitly because K1 below
    # asserts it as the no-override behaviour, not merely whatever the fixture happens to carry.
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$SH/repo-map" \
        SPIRA_QUEUE_DIR="$QUEUEDIR" SPIRA_QUEUE_BATCH_WAIT=999999 \
        SPIRA_FORGE="$SH/forge-fixture.sh" \
        SPIRA_BATCH_MAXPAR="${SPIRA_BATCH_MAXPAR:-16}" \
        SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}"
    [ -n "${SPIRA_BATCHER_WALL_SECS:-}" ] && tl_config SPIRA_BATCHER_WALL_SECS="$SPIRA_BATCHER_WALL_SECS"
    # SPIRA_RUN ALSO AS PLAIN ENV: spira-lc-stub.sh (below) is a fixture script exec'd by
    # batcher as a child process, and it reads "${SPIRA_RUN:-/nonexistent}" as a raw shell
    # variable, never through spira-config — tl_config's declaration never reaches it.
    PATH="$SH/lc-stub-bin:$SH:$PATH" SPIRA_HOME="$SH" SPIRA_RUN="$RUN" \
    SPIRA_RELEASE="$(PATH="$SH/lc-stub-bin:$SH:$PATH" tl_release)" SPIRA_LC_BIN="$SH/lc-stub-bin/spira-lc" \
    STUB_RED_SUITES="${STUB_RED_SUITES:-}" \
    STUB_FLAKE_SUITE="${STUB_FLAKE_SUITE:-}" \
    STUB_FLAKE_COUNTER_FILE="${STUB_FLAKE_COUNTER_FILE:-}" \
    STUB_FLAKE_RED_TIMES="${STUB_FLAKE_RED_TIMES:-1}" \
    SPIRA_BATCHER_POLL_SECS=1 \
    STUB_ARGV_LOG="${STUB_ARGV_LOG:-}" \
    STUB_SLEEP_SECS="${STUB_SLEEP_SECS:-}" \
    STUB_EXIT4="${STUB_EXIT4:-}" \
    SPIRA_LC_STUB_LOG="${SPIRA_LC_STUB_LOG:-$TMP/lc-default.log}" \
    SPIRA_LC_STUB_RC="${SPIRA_LC_STUB_RC:-0}" \
        batcher "${BATCHER_VERB:-cut}" "$REPONAME" --round-vm "$SH/round-vm-stub.sh" 2>&1
}

B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status",""))' 2>/dev/null; }
open_batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
open_field() { grep "^${1}=" "$(open_batch_file)" 2>/dev/null | head -1 | cut -d= -f2-; }
# A batch member is a bead WAITING FOR A ROUND: open and carrying the submitted label
# (sp-1346p) — a CERTIFIED record of a closed bead is stale and never batched.
plant() {   # plant <id> [express]
    local id="$1" lbls="\"spira\",\"plan\",\"repo:$REPONAME\",\"spira-submitted\""
    [ "${2:-}" = express ] && { mkdir -p "$RUN/lc-express"; : > "$RUN/lc-express/$id"; }
    printf '{"id":"%s","title":"%s bead","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-25T00:00:00Z"}\n' \
        "$id" "$id" "$lbls" | testdb_seed
}

# plant_open <id> [express] — status=open, no spira-submitted: the shape a bead re-marked
# CERTIFIED right after an eject (sp-pedat) is actually in, still waiting on its dwarf.
plant_open() {
    local id="$1" lbls="\"spira\",\"plan\",\"repo:$REPONAME\""
    [ "${2:-}" = express ] && { mkdir -p "$RUN/lc-express"; : > "$RUN/lc-express/$id"; }
    printf '{"id":"%s","title":"%s bead","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-25T00:00:00Z"}\n' \
        "$id" "$id" "$lbls" | testdb_seed
}

certify() {   # certify <id> <tip-sha> [epoch]
    printf 'CERTIFIED %s %s\n' "$2" "${3:-$(date +%s)}" > "$LCSTUB/$1"
    [ -f "$RUN/lc-taken" ] && { grep -vx "$1" "$RUN/lc-taken" > "$RUN/lc-taken.n" || true; mv "$RUN/lc-taken.n" "$RUN/lc-taken"; }
}

lc_certify() {   # lc_certify <id> <tip-sha> [epoch] — a CERTIFIED row on the real machine
    lcfix_seed "$1" CERTIFIED "$2"
    lcfix_sql -q "UPDATE bead SET updated_at=${3:-$(date +%s)}, sifted_tip='$2' WHERE bead_id='$1'" >/dev/null 2>&1
}

echo "test-batcher-cut.sh ($CUT_PART)"
testdb_reset


local_land_fixture() {
    [ -d "$TMP/local-land-repo" ] && return 0
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
        tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$SH/repo-map" \
            SPIRA_QUEUE_DIR="$QUEUEDIR" \
            SPIRA_QUEUE_BATCH_WAIT="${SPIRA_QUEUE_BATCH_WAIT_OVERRIDE:-999999}" \
            SPIRA_RELEASES="$LRELEASES" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}"
        PATH="$SH:$PATH" SPIRA_HOME="$SH" SPIRA_RELEASE="$(PATH="$SH:$PATH" tl_release)" \
        STUB_INSTALL_BINS="${STUB_INSTALL_BINS:-}" \
            batcher cut locland --round-vm "$SH/round-vm-stub.sh" 2>&1
    }
}

if cut_runs A; then

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

# Captured for case D, which reuses this batch as its "already open" starting point.
pr_case_a="$(open_field pr)"
head_case_a="$(open_field head)"
base_case_a="$(open_field base)"
members_case_a="$(open_field members)"
branch_case_a="$(open_field branch)"

fi

if cut_runs Gf; then

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

fi

if cut_runs D; then

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
is     "D: express member not BATCHED while the PR is open" "CERTIFIED" "$(cut -d' ' -f1 < "$LCSTUB/sp-cddd4")"

: > "$D_ARGV"
out_d2="$(STUB_ARGV_LOG="$D_ARGV" cut_repo)"
want   "D: an unchanged pool is not rebuilt" "already prepared" "$out_d2"
is     "D: no second corpus run for an unchanged pool" "0" "$(grep -c '^argv:' "$D_ARGV")"

# The open PR lands: the remote base moves to its head.
git -C "$REMOTE" update-ref refs/heads/main "$head_case_a"
timeout 5 git -C "$REPO" fetch -q origin
rm -f "$(open_batch_file)"
: > "$D_ARGV"
out_d3="$(STUB_ARGV_LOG="$D_ARGV" cut_repo)"
want   "D: after landing, the prepared round opens a PR" "opened" "$out_d3"
is     "D: the prepared round opened without re-running the corpus" "0" "$(grep -c '^argv:' "$D_ARGV")"
is     "D: exactly one new PR" "$((prcreate_before + 1))" "$(grep -c '^pr-create' "$FORGE_LOG")"
want   "D: open-batch members carries the express member" "sp-cddd4:$tip_d" "$(open_field members)"
is     "D: prepared record consumed" "0" "$([ -f "$QUEUEDIR/$REPONAME/prepared" ] && echo 1 || echo 0)"

fi

if cut_runs F; then

# =============================================================================
# CASE F — spira-lc wiring (sp-o7nbr.4): BATCHED at a fresh cut and a stack both call
# spira-lc, and the open-batch record carries batch_id/version exactly when spira-lc
# applied. POSITIVE CONTROL last: a planted refusal (rc=3) proves the call is additive —
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

PATH="$SH/lc-stub-bin:$PATH" SPIRA_LC_STUB_LOG="$LC_LOG" cut_repo >/dev/null
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

PATH="$SH/lc-stub-bin:$PATH" SPIRA_LC_STUB_LOG="$LC_LOG" cut_repo >/dev/null
nowant "F: preparing the next round never calls spira-lc stack" "stack " "$(cat "$LC_LOG")"
nowant "F: preparing the next round never calls spira-lc cut" "cut " "$(cat "$LC_LOG")"
is   "F: open-batch batch_id unchanged by a prepared round" "$branch_f" "$(open_field batch_id)"
is   "F: open-batch version unchanged by a prepared round"  "1"         "$(open_field version)"
rm -f "$LCSTUB/sp-cggg7" "$QUEUEDIR/$REPONAME/prepared"

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

out_f_refused="$(PATH="$SH/lc-stub-bin:$PATH" SPIRA_LC_STUB_LOG="$LC_LOG" SPIRA_LC_STUB_RC=3 cut_repo)"
want "F: PLANTED REFUSAL — cut still reports the PR opening" "PR " "$out_f_refused"
want "F: PLANTED REFUSAL — the refusal is logged" "spira-lc cut refused for" "$out_f_refused"
is   "F: PLANTED REFUSAL — open-batch batch_id stays unset" "" "$(open_field batch_id)"
is   "F: PLANTED REFUSAL — open-batch version stays unset"  "" "$(open_field version)"
fi


if cut_runs Gl; then

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
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$SH/repo-map" \
        SPIRA_QUEUE_DIR="$QUEUEDIR" SPIRA_QUEUE_BATCH_WAIT=999999 SPIRA_FORGE="$SH/forge-fixture.sh" \
        SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}"
    PATH="$SH/lc-stub-bin:$PATH" SPIRA_LC_STUB_LOG="$TMP/lc-default.log" SPIRA_HOME="$SH" SPIRA_RUN="$RUN" \
        SPIRA_RELEASE="$(PATH="$SH/lc-stub-bin:$PATH" tl_release)" SPIRA_LC_BIN="$SH/lc-stub-bin/spira-lc" \
        batcher cut "$1" --round-vm "$SH/round-vm-stub.sh" 2>&1
}

out_g_forge="$(cut_other forgealias)"
nowant "G: queue.forge alias is accepted, not refused (regression)" "not queue" "$out_g_forge"
want   "G: queue.forge alias reaches no-trigger cleanly, same as a bare queue row" "no round cut: no trigger" "$out_g_forge"

out_g_local="$(cut_other localmode)"
nowant "G: queue.local is accepted by find_repo (SEEN RED on today's code: refuses it)" "not queue" "$out_g_local"
want   "G: queue.local reaches no-trigger cleanly, never a push" "no round cut: no trigger" "$out_g_local"

fi

if cut_runs I; then

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

fi

if cut_runs J; then

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
# certified pool to this repo's own refs/heads/spira/* — see io.rs). Its tip is the
# CURRENT base itself: exactly the "reset to an old main" / "no commits of its own" shape
# the bead names, an ancestor of the round head before any merge is attempted.
git -C "$REPO" branch -f spira/sp-cjjjj "$base_now"
certify sp-cjjjj "$base_now"
prcreate_before_j="$(grep -c '^pr-create' "$FORGE_LOG")"

out_j="$(STUB_RED_SUITES="" cut_repo)"
want "J: reports the member set aside as EMPTY, not a member" "EMPTY sp-cjjjj: tip is already in the round — not a member" "$out_j"
nowant "J: no PR opens crediting a no-op tip" "PR " "$out_j"
is   "J: sp-cjjjj stays CERTIFIED, never marked BATCHED" "CERTIFIED" "$(cut -d' ' -f1 < "$LCSTUB/sp-cjjjj")"
is   "J: forge pr-create not called for the empty round" "$prcreate_before_j" "$(grep -c '^pr-create' "$FORGE_LOG")"

fi

if cut_runs M; then

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

local_land_fixture
# A -> B -> C: a straight chain, each bead's branch built on the previous bead's own tip, the
# same shape a real stacked worktree gets once bead 3 (dwarf base = landing ref + stack) lands.
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


# Certified in the order C, A, B — certified_at deliberately reversed against dependency
# order, so a sort on order_key alone (no stack awareness) would try to merge C first.
plant sp-cmcc3; plant sp-cmaa1; plant sp-cmbb2
lc_certify sp-cmcc3 "$tip_mc" 100
lc_certify sp-cmaa1 "$tip_ma" 200
lc_certify sp-cmbb2 "$tip_mb" 300
lcfix_sql -q "UPDATE bead SET stack='{\"sp-cmaa1\":\"$tip_ma\"}', stack_depth=1 WHERE bead_id='sp-cmbb2'" >/dev/null 2>&1
lcfix_sql -q "UPDATE bead SET stack='{\"sp-cmbb2\":\"$tip_mb\"}', stack_depth=1 WHERE bead_id='sp-cmcc3'" >/dev/null 2>&1

out_m="$(STUB_INSTALL_BINS=1 cut_local)"
want   "M: reports landing locally"                             "landed locally"  "$out_m"
want   "M: all three members landed in one round"                "3 member(s)"    "$out_m"
is     "M: sp-cmaa1 is LANDED (the closed-over prerequisite, never dropped as EMPTY)" \
       "LANDED" "$(lcfix_state sp-cmaa1)"
is     "M: sp-cmbb2 is LANDED"                                    "LANDED" "$(lcfix_state sp-cmbb2)"
is     "M: sp-cmcc3 is LANDED"                                    "LANDED" "$(lcfix_state sp-cmcc3)"
head_m="$(git -C "$LREPO" rev-parse local/main)"
# Merged prerequisite-first: the round's own land-subject merge commits appear in the order
# A, B, C along local/main's first-parent history, not the certification order C, A, B.
is     "M: merged in topological order A, B, C" \
       "$(printf 'sp-cmaa1\nsp-cmbb2\nsp-cmcc3')" \
       "$(git -C "$LREPO" log --first-parent --format=%s "$head_m" | sed -n 's/^spira: land \(sp-cm[a-z0-9]*\).*/\1/p' | tac)"

fi

if cut_runs Nr; then

# =============================================================================
# CASE N — batcher parity (sp-7qk8u): a bead back with its dwarf after an eject (sp-pedat) is
# not a member. Since sp-mve9i that is its lifecycle row — REWORK, not CERTIFIED — and never
# bd's status or the retired submitted label (design §3.4): the bead is open in bd here, with
# no submitted label, exactly as before, and it is the row alone that keeps it out.
# =============================================================================
echo
echo "N. batcher parity: a bead whose lifecycle row is REWORK (ejected, back with its dwarf) is excluded:"
# sp-cjjjj (J), sp-cgcc3 (K3) and sp-cgdd4 (K4) all stay CERTIFIED by design in their own
# cases and their branches persist in $REPO — retire them first so this round is only
# sp-ciiii, the way case C already retires case G/H's own leftovers (line 548 above).
rm -f "$(open_batch_file)" "$LCSTUB/sp-cjjjj" "$LCSTUB/sp-cgcc3" "$LCSTUB/sp-cgdd4"
plant_open sp-ciiii express
git -C "$REPO" worktree add -q -b spira/sp-ciiii "$RUN/worktree/sp-ciiii" main
printf 'i\n' > "$RUN/worktree/sp-ciiii/i.txt"
git -C "$RUN/worktree/sp-ciiii" add -A
git -C "$RUN/worktree/sp-ciiii" commit -q -m "sp-ciiii: work"
tip_n="$(git -C "$REPO" rev-parse spira/sp-ciiii)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-ciiii"
printf 'REWORK %s %s\n' "$tip_n" "$(date +%s)" > "$LCSTUB/sp-ciiii"

prcreate_before_n="$(grep -c '^pr-create' "$FORGE_LOG")"
out_n="$(STUB_RED_SUITES="" cut_repo)"
nowant "N: never reports a PR opening for the excluded-only round" "PR " "$out_n"
is     "N: forge pr-create not called" "$prcreate_before_n" "$(grep -c '^pr-create' "$FORGE_LOG")"
is     "N: no open-batch file" "0" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"
is     "N: sp-ciiii stub row stays REWORK — untouched, not re-ejected" \
       "REWORK" "$(cut -d' ' -f1 < "$LCSTUB/sp-ciiii")"
is     "N: sp-ciiii bead status stays open" "open" "$(status_of sp-ciiii)"

fi

if cut_runs O; then

# =============================================================================
# CASE O — sift screens ahead of rounds, never inside the cut (sp-9sbdvw): a cut takes a bead only
# when the lifecycle row carries a pass at its current tip, and performs no screen work itself.
# =============================================================================
echo
echo "O. a cut reads sift verdicts and screens nothing:"
rm -f "$(open_batch_file)" "$LCSTUB/sp-ciiii"
plant sp-coooo express
git -C "$REPO" worktree add -q -b spira/sp-coooo "$RUN/worktree/sp-coooo" main
printf 'o\n' > "$RUN/worktree/sp-coooo/o.txt"
git -C "$RUN/worktree/sp-coooo" add -A
git -C "$RUN/worktree/sp-coooo" commit -q -m "sp-coooo: work"
tip_o="$(git -C "$REPO" rev-parse spira/sp-coooo)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-coooo"
certify sp-coooo "$tip_o"
mkdir -p "$RUN/lc-unsifted"; : > "$RUN/lc-unsifted/sp-coooo"
rm -rf "$RUN/sift"

prcreate_before_o="$(grep -c '^pr-create' "$FORGE_LOG")"
out_o="$(STUB_RED_SUITES="" cut_repo)"
nowant "O: an unscreened bead is not cut" "PR " "$out_o"
is     "O: forge pr-create not called" "$prcreate_before_o" "$(grep -c '^pr-create' "$FORGE_LOG")"
is     "O: the cut did no screen work" "0" "$([ -e "$RUN/sift" ] && echo 1 || echo 0)"
rm -f "$RUN/lc-unsifted/sp-coooo"
out_o="$(STUB_RED_SUITES="" cut_repo)"
want   "O: the same bead, once it has a pass at its tip, is cut" "PR " "$out_o"

fi

tl_summary
