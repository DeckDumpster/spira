#!/usr/bin/env bash
#
# test-batcher-cut.sh — ONE end-to-end suite for the batcher crate's IO seam (sp-jzfog): a
# whole round through a real fixture repo, a stub testenv-batch and a fake forge, invoking the
# built binary directly the way queue.sh's _batch_cut does (sp-vsob2: unconditionally, batch.sh's
# own cut retired). This checks WIRING, not behaviour — the pure core's replay tests
# (test-batcher.sh) already cover triggers, membership, set-asides and classification as
# fixtures with no IO at all.
#
# FOUR CASES:
#   A. happy path    — an express-certified member merges, the stub corpus is green, a PR
#                       opens, and the queue's open-batch record reads exactly what
#                       verdict.sh's own key=value format expects.
#   B. double-red     — the stub corpus reports one suite red on both the first run and the
#                       re-run: no PR opens, the member stays CERTIFIED, and a bead is filed
#                       for the batcher persona (sp-47kq1) to resolve by judgement.
#   C. stale member    — an express-certified member whose branch conflicts with the base
#                       itself (not just batch accumulation) is reopened for rebase at once
#                       (section F), landstate RED, bump_requeue stamped.
#   D. stacking        — with case A's batch PR still open, a second express-certified member
#                       is pushed onto that PR's own head rather than waiting or opening a
#                       second PR (law-queue-back-pressure-is-an-open-pr).
#   G. land mode        — find_repo (sp-o1jm6) accepts queue.local, not only queue, and treats
#                       queue.forge byte-identically to a bare queue row.
#   I. suite corpus     — the round's suite corpus comes from the round branch's own tree,
#                       not the production checkout's working directory (law-batcher-earns-
#                       the-round-by-parity).
#   J. batcher parity   — a member whose tip is already an ancestor of the round head (reset
#                       to an old base, or no commits of its own) merges as a git no-op; it
#                       must be set aside as EMPTY, never merged, never marked BATCHED (sp-5xki9).
#   K. batcher parity (sp-myi6w) — the corpus invocation matches the Concierge's own
#                       (round.sh): --mode parallel, --with-bins, SPIRA_BATCH_MAXPAR,
#                       RUSTUP_TOOLCHAIN all present on argv/env; a --with-bins build
#                       failure (exit 4) is a local round red filed as an Ops incident,
#                       never handed to attribute.sh and never a harness fault that drops
#                       the round unreported; a hung testenv-batch.sh is killed at the
#                       configured wall bound.
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs spira/queue.sh spira/conf.sh spira/lib.sh spira/bead.sh spira/chamber/batcher.fayth spira/chamber/batcher.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batcher-cut
TMP="$(mktemp -d)"; trap 'testdb_drop; chmod -R u+w "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchercut || { echo "test-batcher-cut: could not build fixture database"; exit 1; }

# ── build the batcher binary (law-absence-needs-a-positive-control: no binary, no suite) ──
# RESOLVE THE TOOLCHAIN DIRECTORY, NOT JUST cargo's OWN PATH. cargo execs `rustc` BY NAME,
# and testdb.sh (sourced above) pulls in conf.sh, which overwrites PATH wholesale — so
# finding cargo's path is not enough; its own directory has to go back on PATH for the
# `rustc` it execs to resolve (same fix test-batcher.sh's own comment describes, needed
# here because this suite, unlike that one, sources testdb.sh).
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -z "$CARGO_BIN" ] && [ -x "/usr/local/cargo/bin/cargo" ] && CARGO_BIN="/usr/local/cargo/bin/cargo"
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-batcher-cut: cargo not found — the batcher binary cannot be built"
    exit 77
fi
PATH="$(dirname "$CARGO_BIN"):$PATH"; export PATH
# PIN CARGO_TARGET_DIR EXPLICITLY. A suite runs inside testenv-batch.sh's own podman exec,
# which sets its own CARGO_TARGET_DIR for the suites that build Rust under test — trusting
# $ROOT/target here would silently build into that redirected directory instead, and this
# suite's own binary lookup would find nothing there (SEEN RED without this: the build
# reported "Finished" while $ROOT/target/release/batcher stayed absent).
CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
BATCHER_BIN="$CARGO_TARGET_DIR_FOR_BUILD/release/batcher"
if [ ! -x "$BATCHER_BIN" ]; then
    printf '  (building batcher-cut into %s)\n' "$BATCHER_BIN"
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
        "$CARGO_BIN" build --release --manifest-path "$ROOT/Cargo.toml" -p batcher-cut 2>&1 | tail -10
fi
[ -x "$BATCHER_BIN" ] || { echo "test-batcher-cut: batcher binary did not build"; exit 1; }
TSD_BIN="$CARGO_TARGET_DIR_FOR_BUILD/release/tsd-write"
if [ ! -x "$TSD_BIN" ]; then
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
        "$CARGO_BIN" build --release --manifest-path "$ROOT/Cargo.toml" -p tsd 2>&1 | tail -10
fi

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
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/mail.sh"; chmod +x "$SH/mail.sh"

# A REAL COPY OF THE CHAMBER, so bead.sh's own --for batcher can read batcher.fayth the same
# way it would in production — file_judgement (io.rs) files through bead.sh's contract, never
# bd create directly, so the fayth's own FAYTH_LABELS is what a fake chamber has to supply.
mkdir -p "$SH/chamber"
cp "$HERE/chamber/batcher.fayth" "$SH/chamber/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

# ── stub testenv-batch: writes the exact result protocol testenv-batch.sh itself documents
# (<status> <epoch> <secs> <fp> <mode> <producer> <rc>), red for every suite named in
# STUB_RED_SUITES (comma-separated), ok otherwise. STUB_FLAKE_SUITE is red only for its first
# STUB_FLAKE_RED_TIMES invocations (counted in STUB_FLAKE_COUNTER_FILE) and green after — a
# suite that would flip green on an immediate rerun, the shape case G needs to show that a
# local red now goes through attribution regardless of whether an internal retry would have
# waved it through. STUB_ARGV_LOG, when set, records this invocation's full argv plus the env
# batcher-parity (sp-myi6w) requires — RUSTUP_TOOLCHAIN, SPIRA_BATCH_MAXPAR — one line each,
# so a case can assert on them without guessing at io::run_suites' own internals.
# STUB_SLEEP_SECS hangs before doing anything else, for the wall-bound case. STUB_EXIT4 exits
# 4 immediately, before any suite ever runs — mirroring testenv-batch.sh's own --with-bins
# build failure, which happens before suite selection.
cat > "$SH/testenv-batch-stub.sh" <<'STUB'
#!/usr/bin/env bash
if [ -n "${STUB_ARGV_LOG:-}" ]; then
    {
        printf 'argv:'; printf ' %s' "$@"; printf '\n'
        printf 'RUSTUP_TOOLCHAIN=%s\n' "${RUSTUP_TOOLCHAIN:-}"
        printf 'SPIRA_BATCH_MAXPAR=%s\n' "${SPIRA_BATCH_MAXPAR:-}"
    } >> "$STUB_ARGV_LOG"
fi
[ -n "${STUB_SLEEP_SECS:-}" ] && sleep "$STUB_SLEEP_SECS"
[ -n "${STUB_EXIT4:-}" ] && exit 4
suites_csv=""
while [ $# -gt 0 ]; do
    case "$1" in
        --suites) shift; suites_csv="${1:-}"; shift ;;
        *) shift ;;
    esac
done
results="${SPIRA_BATCH_RESULTS:?SPIRA_BATCH_RESULTS unset}"
mkdir -p "$results"
red=0
IFS=',' read -r -a suites <<< "$suites_csv"
IFS=',' read -r -a redset <<< "${STUB_RED_SUITES:-}"
is_red() {
    local s="$1" r
    for r in "${redset[@]:-}"; do [ -n "$r" ] && [ "$r" = "$s" ] && return 0; done
    return 1
}
is_flake_red() {
    local s="$1" n=0
    [ -n "${STUB_FLAKE_SUITE:-}" ] && [ "$s" = "$STUB_FLAKE_SUITE" ] || return 1
    local cf="${STUB_FLAKE_COUNTER_FILE:?STUB_FLAKE_COUNTER_FILE unset}"
    [ -r "$cf" ] && n="$(cat "$cf")"
    n=$((n + 1))
    printf '%s' "$n" > "$cf"
    [ "$n" -le "${STUB_FLAKE_RED_TIMES:-1}" ]
}
for s in "${suites[@]:-}"; do
    [ -n "$s" ] || continue
    if is_red "$s" || is_flake_red "$s"; then
        printf 'red %s 1 fp serial explicit 1\n' "$(date +%s)" > "$results/$s.result"
        printf '  FAIL — synthetic failure planted in %s\n' "$s" > "$results/$s.out"
        red=1
    else
        printf 'ok %s 1 - serial explicit 0\n' "$(date +%s)" > "$results/$s.result"
        : > "$results/$s.out"
    fi
done
[ "$red" = 1 ] && exit 1
exit 0
STUB
chmod +x "$SH/testenv-batch-stub.sh"

# ── stub attribute.sh: the round's own local red-suite attribution, stubbed so this suite
# checks WIRING (does a red go through attribution, does an EJECT line actually eject, does a
# BASE owner block the round) without paying for real bisection or containers — that is
# test-attribute.sh's own job (host-reason: podman). STUB_ATTR_MODE selects the shape:
#   eject     — every suite named in --suites is attributed to STUB_ATTR_OWNER
#   basefail  — every suite named in --suites is attributed to BASE
#   fail      — the attribution step itself is unavailable (default: fails closed rather
#               than silently reporting "nothing to blame" for a mode nobody set)
cat > "$SH/attribute-stub.sh" <<'ATTR'
#!/usr/bin/env bash
suites_csv=""
while [ $# -gt 0 ]; do
    case "$1" in
        --suites) shift; suites_csv="${1:-}"; shift ;;
        --round|--base|--members|--repo) shift 2 ;;
        *) shift ;;
    esac
done
case "${STUB_ATTR_MODE:-fail}" in
    eject)
        IFS=',' read -r -a _s <<< "$suites_csv"
        for s in "${_s[@]}"; do
            [ -n "$s" ] || continue
            printf 'ATTR %s owner=%s method=single wall=1s fail=\n' "$s" "${STUB_ATTR_OWNER:?STUB_ATTR_OWNER unset}"
        done
        printf 'EJECT %s %s\n' "${STUB_ATTR_OWNER:?}" "$suites_csv"
        exit 0
        ;;
    basefail)
        IFS=',' read -r -a _s <<< "$suites_csv"
        for s in "${_s[@]}"; do
            [ -n "$s" ] || continue
            printf 'ATTR %s owner=BASE method=base wall=1s fail=\n' "$s"
        done
        exit 0
        ;;
    *)
        printf 'attribute-stub: attribution step is stubbed out and refuses\n' >&2
        exit 1
        ;;
esac
ATTR
chmod +x "$SH/attribute-stub.sh"

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
printf '%s\n' "$*" >> "$log"
case "${1:-}" in
    create-bead) exit 0 ;;
    cut|stack) exit "${SPIRA_LC_STUB_RC:-0}" ;;
    *) exit 0 ;;
esac
LCSTUB
chmod +x "$SH/spira-lc-stub.sh"

REQUEUE_SPY="$TMP/requeue-spy"; : > "$REQUEUE_SPY"
cat >> "$SH/lib.sh" <<LIBSPY
bump_requeue() {
    printf '%s %s\n' "\${1:-}" "\${2:-}" >> "$REQUEUE_SPY"
    _bump_write_event "\${1:-}" requeued "\${2:-unrecorded}"
}
LIBSPY

cut_repo() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_TSD_BIN="$TSD_BIN" \
    STUB_RED_SUITES="${STUB_RED_SUITES:-}" \
    STUB_FLAKE_SUITE="${STUB_FLAKE_SUITE:-}" \
    STUB_FLAKE_COUNTER_FILE="${STUB_FLAKE_COUNTER_FILE:-}" \
    STUB_FLAKE_RED_TIMES="${STUB_FLAKE_RED_TIMES:-1}" \
    STUB_ATTR_MODE="${STUB_ATTR_MODE:-}" \
    STUB_ATTR_OWNER="${STUB_ATTR_OWNER:-}" \
    STUB_ARGV_LOG="${STUB_ARGV_LOG:-}" \
    STUB_SLEEP_SECS="${STUB_SLEEP_SECS:-}" \
    STUB_EXIT4="${STUB_EXIT4:-}" \
    SPIRA_BATCH_MAXPAR="${SPIRA_BATCH_MAXPAR:-}" \
    SPIRA_BATCHER_WALL_SECS="${SPIRA_BATCHER_WALL_SECS:-}" \
    SPIRA_RELEASE_RUST_TOOLCHAIN="${SPIRA_RELEASE_RUST_TOOLCHAIN:-}" \
    SPIRA_LC_BIN="${SPIRA_LC_BIN:-}" \
    SPIRA_LC_STUB_LOG="${SPIRA_LC_STUB_LOG:-}" \
    SPIRA_LC_STUB_RC="${SPIRA_LC_STUB_RC:-0}" \
        "$BATCHER_BIN" cut "$REPONAME" --testenv-batch "$SH/testenv-batch-stub.sh" \
            --attribute "$SH/attribute-stub.sh" 2>&1
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

plant() {   # plant <id> [express]
    local id="$1" express_label="" lbls="\"spira\",\"plan\",\"repo:$REPONAME\""
    [ "${2:-}" = express ] && lbls="$lbls,\"express\""
    printf '{"id":"%s","title":"%s bead","status":"closed","issue_type":"task","labels":[%s],"updated_at":"2026-09-25T00:00:00Z"}\n' \
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
is   "A: sp-caaa1 landstate BATCHED" "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-caaa1")"
is   "A: no batch_id without SPIRA_LC_BIN (legacy default, never blocks the PR)" "" "$(open_field batch_id)"
is   "A: no version without SPIRA_LC_BIN"                                        "" "$(open_field version)"
want "A: commit message names spira: land sp-caaa1, with the bead's own title" \
    "spira: land sp-caaa1 — sp-caaa1 bead" \
    "$(git -C "$REPO" log --format=%s "$(open_field branch)" -n 5 2>/dev/null)"
is   "A: forge pr-create called once" "1" "$(grep -c '^pr-create' "$FORGE_LOG")"
if [ -x "$TSD_BIN" ]; then
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
# CASE B — local attribution (sp-hqoap, law-a-round-takes-certified-tips as amended
# 2026-09-27): the stub corpus turns test-b.sh red; attribution names sp-cbbb2 its owner.
# sp-cbbb2 is ejected — bead reopened, landstate EJECTED, note naming every suite it turned
# red — and the round re-runs on the reduced membership: green, so the PR opens WITHOUT the
# ejected member but WITH its innocent bystander (the positive control for "only the named
# member is dropped, not the whole round").
# =============================================================================
echo
echo "B. local attribution ejects the culprit; the PR opens without it:"
plant sp-cbbb2 express
git -C "$REPO" worktree add -q -b spira/sp-cbbb2 "$RUN/worktree/sp-cbbb2" main
printf 'b\n' > "$RUN/worktree/sp-cbbb2/b.txt"
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

# The stub corpus cannot see which member's diff caused a suite to fail (it is a fixed
# red/green switch, not a real interpreter of the tree under test) — so test-b.sh is scripted
# red on the FIRST corpus run only (STUB_FLAKE_RED_TIMES=1), the same shape as sp-cbbb2's own
# commit actually being the cause: once it is ejected and the round re-runs, the suite is
# green, exactly as removing the real culprit's diff would make it.
FLAKE_COUNTER_B="$TMP/flake-counter-b"; rm -f "$FLAKE_COUNTER_B"
prcreate_before_b="$(grep -c '^pr-create' "$FORGE_LOG")"
out_b="$(STUB_FLAKE_SUITE=test-b.sh STUB_FLAKE_COUNTER_FILE="$FLAKE_COUNTER_B" STUB_FLAKE_RED_TIMES=1 \
         STUB_ATTR_MODE=eject STUB_ATTR_OWNER=sp-cbbb2 cut_repo)"
want   "B: reports the ejection, naming the member and the suite" "sp-cbbb2 ejected: red on test-b.sh" "$out_b"
want   "B: still reports the PR opening"                          "PR "                                "$out_b"
nowant "B: no judgement/double-red language — this was resolved mechanically" "judgement" "$out_b"
is     "B: sp-cbbb2 landstate EJECTED"    "EJECTED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cbbb2")"
is     "B: sp-cbbb2 reopened, not left closed" "open" "$(status_of sp-cbbb2)"
want   "B: ejection note names every suite it turned red" "test-b.sh" "$(notes_of sp-cbbb2)"
is     "B: sp-cbbb3 (the bystander) landstate BATCHED" "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cbbb3")"
is     "B: open-batch carries only the bystander" "sp-cbbb3:$tip_b3" "$(open_field members)"
nowant "B: open-batch does not carry the ejected member" "sp-cbbb2" "$(open_field members)"
is     "B: forge pr-create called (once more than before this case)" "$((prcreate_before_b + 1))" "$(grep -c '^pr-create' "$FORGE_LOG")"

# =============================================================================
# CASE G — attribution itself is unavailable: a locally-red round never opens a PR, even
# when the very next run of the same suite would have come back green. The fixture's own
# red is a FLAKE (red once, green after) precisely because a static, always-red suite would
# already have been blocked by the pre-sp-hqoap code's own double-red/judgement path for an
# unrelated reason — this shape is the one that exposes the actual gap: the old code's own
# internal retry flipped a transient red to green and opened the PR regardless of any
# attribution step, since it never called one. SEEN RED without the fix: reverting the
# batcher-cut/main.rs changes in this branch and rerunning this case opens a PR.
# =============================================================================
echo
echo "G. attribution stubbed out (fails): a locally-red round never opens a PR:"
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
want   "G: reports attribute.sh's own failure" "attribute.sh" "$out_g"
nowant "G: never reports a PR opening"         "PR "          "$out_g"
is     "G: forge pr-create not called"    "$prcreate_before_g" "$(grep -c '^pr-create' "$FORGE_LOG")"
is     "G: no open-batch file"            "0" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"
is     "G: sp-cgflk stays CERTIFIED — held, not ejected (attribution itself is what failed, not the member)" \
       "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cgflk")"

# =============================================================================
# CASE H — BASE_FAIL: a suite red against the base itself blocks the round and is filed as
# an Ops incident; no member is ejected for it (the deliverable's own wording).
# =============================================================================
echo
echo "H. base itself is red: round blocked, filed for Ops, nobody ejected:"
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
out_h="$(STUB_RED_SUITES="test-a.sh" STUB_ATTR_MODE=basefail cut_repo)"
want   "H: reports the base fail, naming the suite" "test-a.sh" "$out_h"
want   "H: reports filing an Ops incident"          "filed sp-inc"  "$out_h"
nowant "H: never reports a PR opening"              "PR "           "$out_h"
is     "H: forge pr-create not called"    "$prcreate_before_h" "$(grep -c '^pr-create' "$FORGE_LOG")"
is     "H: no open-batch file"            "0" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"
is     "H: sp-chbas stays CERTIFIED — not ejected, the base is at fault" \
       "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-chbas")"
want   "H: the incident names the base-red reason" "local-round-red" "$(cat "$INCIDENT_LOG")"

# =============================================================================
# CASE C — a stale member: an express-certified branch that conflicts with the
# base itself is reopened for rebase immediately (section F), not just skipped.
# POSITIVE CONTROL: the requeue spy is empty before this case runs.
# =============================================================================
echo
echo "C. stale member: base conflict reopens the bead at once:"
is "C: positive control — requeue spy silent before this case" "0" "$(grep -c '^sp-cccc3 ' "$REQUEUE_SPY" 2>/dev/null)"
# sp-cbbb2 (case B), sp-cgflk (case G) and sp-chbas (case H) are all still CERTIFIED by
# design — a double-red, a failed attribution, and a base fail all leave their member be.
# Case C is about a DIFFERENT member's base conflict in isolation, so retire them first the
# way a builder eventually would (fix and re-certify elsewhere, or abandon); otherwise they
# would merge cleanly into case C's round and open a PR neither case is testing for.
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
is   "C: sp-cccc3 landstate RED" "RED"  "$(cut -d' ' -f1 < "$LANDSTATE/sp-cccc3")"
is   "C: bump_requeue stamped merge-conflict" "1" "$(grep -c '^sp-cccc3 merge-conflict$' "$REQUEUE_SPY")"
nowant "C: no PR opened for the conflicting-only round" "PR " "$out_c"

# =============================================================================
# CASE D — stacking: with case A's batch PR still open, a second express member
# is pushed onto that PR's own head rather than a new PR being opened
# (law-queue-back-pressure-is-an-open-pr).
# =============================================================================
echo
echo "D. stacking: a second express member lands on the open batch's own head:"
# Case A's own batch, reconstructed: case B and C each needed it gone to exercise their own
# fresh-cut paths, and the branch ref spira/queue/<stamp-a> case A pushed locally is still
# there to stack onto.
{
    printf 'pr=%s\n'      "$pr_case_a"
    printf 'head=%s\n'    "$head_case_a"
    printf 'base=%s\n'    "$base_case_a"
    printf 'members=%s\n' "$members_case_a"
    printf 'opened=%s\n'  "$(date +%s)"
    printf 'branch=%s\n'  "$branch_case_a"
} > "$(open_batch_file)"

pr_before="$pr_case_a"
head_before="$head_case_a"
prcreate_before="$(grep -c '^pr-create' "$FORGE_LOG")"

plant sp-cddd4 express
git -C "$REPO" worktree add -q -b spira/sp-cddd4 "$RUN/worktree/sp-cddd4" main
printf 'd\n' > "$RUN/worktree/sp-cddd4/d.txt"
git -C "$RUN/worktree/sp-cddd4" add -A
git -C "$RUN/worktree/sp-cddd4" commit -q -m "sp-cddd4: work"
tip_d="$(git -C "$REPO" rev-parse spira/sp-cddd4)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cddd4"
certify sp-cddd4 "$tip_d"

out_d="$(STUB_RED_SUITES="" cut_repo)"
want "D: reports the PR being stacked" "PR $pr_before stacked" "$out_d"
is   "D: open-batch pr unchanged (no new PR)" "$pr_before" "$(open_field pr)"
is   "D: forge pr-create not called again" "$prcreate_before" "$(grep -c '^pr-create' "$FORGE_LOG")"
nowant "D: open-batch head unchanged (it did move)" "$head_before" "$(open_field head)"
want "D: open-batch members now carries sp-cddd4" "sp-cddd4:$tip_d" "$(open_field members)"
is   "D: sp-cddd4 landstate BATCHED" "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cddd4")"
is   "D: stacked open-batch still owner=batcher" "batcher" "$(open_field owner)"

# =============================================================================
# CASE E — judgement-ci (sp-lomk3): verdict.sh's own CI-red producer, on a red CI
# check for a PR the batcher owns, calls this subcommand instead of running its
# own attribution. Exercised directly here, the way verdict.sh's own suite
# stubs SPIRA_BATCHER_BIN and asserts the wiring from its own side.
# =============================================================================
echo
echo "E. judgement-ci: files a judgement bead for a CI-only red:"
judge_ci() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
        "$BATCHER_BIN" judgement-ci "$REPONAME" "$@" 2>&1
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
# applied. POSITIVE CONTROL last: a planted refusal (rc=3) proves the call is additive —
# the PR still opens and batch_id/version stay unset, never blocking the round.
# =============================================================================
echo
echo "F. spira-lc wiring: cut and stack call spira-lc and record batch_id/version:"
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

SPIRA_LC_BIN="$SH/spira-lc-stub.sh" SPIRA_LC_STUB_LOG="$LC_LOG" cut_repo >/dev/null
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

SPIRA_LC_BIN="$SH/spira-lc-stub.sh" SPIRA_LC_STUB_LOG="$LC_LOG" cut_repo >/dev/null
want "F: stacking calls spira-lc stack, not cut, on the same batch-id" \
    "stack $branch_f --members sp-cggg7:$tip_g --actor batcher" "$(cat "$LC_LOG")"
nowant "F: stacking never calls spira-lc cut again for the same batch-id" "cut $branch_f " "$(cat "$LC_LOG")"
is   "F: open-batch batch_id unchanged across the stack" "$branch_f" "$(open_field batch_id)"
is   "F: open-batch version advanced to 2"               "2"         "$(open_field version)"

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

out_f_refused="$(SPIRA_LC_BIN="$SH/spira-lc-stub.sh" SPIRA_LC_STUB_LOG="$LC_LOG" SPIRA_LC_STUB_RC=3 cut_repo)"
want "F: PLANTED REFUSAL — cut still reports the PR opening" "PR " "$out_f_refused"
want "F: PLANTED REFUSAL — the refusal is logged" "spira-lc cut refused for" "$out_f_refused"
is   "F: PLANTED REFUSAL — open-batch batch_id stays unset" "" "$(open_field batch_id)"
is   "F: PLANTED REFUSAL — open-batch version stays unset"  "" "$(open_field version)"
is   "F: PLANTED REFUSAL — member still lands BATCHED" "BATCHED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-chhh8")"

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
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        "$BATCHER_BIN" cut "$1" --testenv-batch "$SH/testenv-batch-stub.sh" 2>&1
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
results_i="$(ls -td "$RUN"/batch-results/"$REPONAME"-* 2>/dev/null | grep -v -- '-rerun$' | head -1)"
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
# CASE K — batcher parity (sp-myi6w): the corpus invocation matches the Concierge's own
# (round.sh test), and a --with-bins build failure is a round-level red attributed to
# members, never a harness fault that silently drops the round.
# =============================================================================
echo
echo "K. batcher parity: --mode parallel --with-bins, maxpar, toolchain, wall bound, exit 4:"

# K1 — default argv/env: SPIRA_BATCH_MAXPAR and SPIRA_RELEASE_RUST_TOOLCHAIN both unset, so
# the batcher's own defaults (16, 1.82.0) must appear on the child's argv/env regardless of
# whether the ambient environment happens to carry the Concierge's own values.
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
want "K1: corpus run in --mode parallel --with-bins" "argv: --mode parallel --with-bins" "$(cat "$ARGV_LOG")"
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
# names: "unexpected exit 4"), and never handed to attribute.sh — there is no member-subset
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

# K4 — a hung testenv-batch.sh is killed at the configured wall bound, not left to hold the
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
# queue.sh land-local — no push, no forge call, no open-batch record — and terminal_ready
# (core) refuses before land-local is even invoked when the round's own --with-bins corpus is
# missing. SEEN RED on today's code: Land::Local reaches push_branch, whose own assert fires
# ("unreachable under queue.local"), so the batcher process panics instead of landing.
# =============================================================================
echo
echo "L. queue.local: a round lands locally via queue.sh land-local — no push, no PR:"

LREPO="$TMP/local-land-repo"
git init -q -b trunk "$LREPO"
git -C "$LREPO" commit -q --allow-empty -m base
git -C "$LREPO" branch local/main trunk
LRELEASES="$TMP/local-releases"; mkdir -p "$LRELEASES"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO  | queue       | origin/main | | |
locland   | $LREPO | queue.local | local/main  | | |
RMAP

cut_local() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_RELEASES="$LRELEASES" \
    SPIRA_TSD_BIN="$TSD_BIN" \
    SPIRA_LC_BIN="${SPIRA_LC_BIN:-}" \
    SPIRA_LC_STACKS_DIR="${SPIRA_LC_STACKS_DIR:-}" \
        "$BATCHER_BIN" cut locland --testenv-batch "$SH/testenv-batch-stub.sh" \
            --attribute "$SH/attribute-stub.sh" 2>&1
}
mk_local_bins() {   # mk_local_bins <tip> — the --with-bins corpus for <tip>'s own tree
    local tip="$1" tree dir
    tree="$(git -C "$LREPO" rev-parse "${tip}^{tree}")"
    dir="$RUN/cargo-target-bins/$tree/release"
    mkdir -p "$dir"
    printf 'fakebin\n' > "$dir/fakebin"
    chmod +x "$dir/fakebin"
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
mk_local_bins "$tip_l2"

prcreate_before_l2="$(grep -c '^pr-create' "$FORGE_LOG")"
out_l2="$(cut_local)"
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
is     "L2: sp-clbb2 landstate LANDED" "LANDED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-clbb2")"
if [ -x "$TSD_BIN" ]; then
    want "L2: TSD batch-round row records verdict=landed_local" '"verdict":"landed_local"' \
        "$(tail -1 "$RUN/tsd/batch-round.jsonl" 2>/dev/null)"
fi

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
case "${1:-}" in
    show)
        f="${SPIRA_LC_STACKS_DIR:?}/${2:-}"
        stack="{}"
        [ -f "$f" ] && stack="$(cat "$f")"
        printf '{"bead":{"stack":%s}}\n' "$stack"
        exit 0
        ;;
    *) exit 0 ;;
esac
LCSTACKSTUB
chmod +x "$SH/spira-lc-stack-stub.sh"

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

mk_local_bins "$tip_mc"

# Certified in the order C, A, B — certified_at deliberately reversed against dependency
# order, so a sort on order_key alone (no stack awareness) would try to merge C first.
plant sp-cmcc3; plant sp-cmaa1; plant sp-cmbb2
certify sp-cmcc3 "$tip_mc" 100
certify sp-cmaa1 "$tip_ma" 200
certify sp-cmbb2 "$tip_mb" 300

out_m="$(SPIRA_LC_BIN="$SH/spira-lc-stack-stub.sh" SPIRA_LC_STACKS_DIR="$LC_STACKS" cut_local)"
want   "M: reports landing locally"                             "landed locally"  "$out_m"
want   "M: all three members landed in one round"                "3 member(s)"    "$out_m"
is     "M: sp-cmaa1 landstate LANDED (the closed-over prerequisite, never dropped as EMPTY)" \
       "LANDED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cmaa1")"
is     "M: sp-cmbb2 landstate LANDED"                             "LANDED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cmbb2")"
is     "M: sp-cmcc3 landstate LANDED"                             "LANDED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cmcc3")"
head_m="$(git -C "$LREPO" rev-parse local/main)"
# Merged prerequisite-first: the round's own land-subject merge commits appear in the order
# A, B, C along local/main's first-parent history, not the certification order C, A, B.
is     "M: merged in topological order A, B, C" \
       "$(printf 'sp-cmaa1\nsp-cmbb2\nsp-cmcc3')" \
       "$(git -C "$LREPO" log --first-parent --format=%s "$head_m" | sed -n 's/^spira: land \(sp-cm[a-z0-9]*\).*/\1/p' | tac)"

tl_summary
