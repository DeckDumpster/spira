#!/usr/bin/env bash
#
# test-batcher-cut.sh — ONE end-to-end suite for the batcher crate's IO seam (sp-jzfog): a
# whole round through a real fixture repo, a stub testenv-batch and a fake forge, wired the
# same way queue.sh calls it (SPIRA_QUEUE_BATCHER=1). This checks WIRING, not behaviour —
# the pure core's replay tests (test-batcher.sh) already cover triggers, membership,
# set-asides and classification as fixtures with no IO at all.
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
#
# tier: T2
# covers: batcher-cut/src/*.rs batcher/src/*.rs spira/queue.sh spira/conf.sh spira/lib.sh spira/bead.sh spira/chamber/batcher.fayth spira/chamber/batcher.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batcher-cut
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
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
git -C "$REPO" add -A && git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

# A REAL COPY OF lib.sh (and its own conf.sh), same as every other batch.sh suite: the IO
# seam shells out to lib.sh's own land_mark/bead_reopen/bump_requeue/repo_root/repo_land/
# spira_landref rather than re-deriving their side effects.
cp "$HERE"/*.sh "$SH/"
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
# STUB_RED_SUITES (comma-separated), ok otherwise.
cat > "$SH/testenv-batch-stub.sh" <<'STUB'
#!/usr/bin/env bash
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
for s in "${suites[@]:-}"; do
    [ -n "$s" ] || continue
    if is_red "$s"; then
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
        "$BATCHER_BIN" cut "$REPONAME" --testenv-batch "$SH/testenv-batch-stub.sh" 2>&1
}

B() { "${TESTDB_BD:-bd}" -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status",""))' 2>/dev/null; }
open_batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
open_field() { grep "^${1}=" "$(open_batch_file)" 2>/dev/null | head -1 | cut -d= -f2-; }

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
want "A: commit message names spira: land sp-caaa1" "spira: land sp-caaa1" \
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
# CASE B — double-red: the stub corpus fails test-b.sh on both the first run and
# the re-run. No PR opens; the member is left CERTIFIED for judgement to resolve.
# =============================================================================
echo
echo "B. double-red: no PR opens, member stays CERTIFIED:"
plant sp-cbbb2 express
git -C "$REPO" worktree add -q -b spira/sp-cbbb2 "$RUN/worktree/sp-cbbb2" main
printf 'b\n' > "$RUN/worktree/sp-cbbb2/b.txt"
git -C "$RUN/worktree/sp-cbbb2" add -A
git -C "$RUN/worktree/sp-cbbb2" commit -q -m "sp-cbbb2: work"
tip_b="$(git -C "$REPO" rev-parse spira/sp-cbbb2)"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-cbbb2"
certify sp-cbbb2 "$tip_b"
rm -f "$(open_batch_file)"

out_b="$(STUB_RED_SUITES="test-b.sh" cut_repo)"
want "B: reports the double-red"        "double-red"  "$out_b"
nowant "B: does not report a PR opening" "PR "         "$out_b"
is   "B: no open-batch file"            "0" "$([ -f "$(open_batch_file)" ] && echo 1 || echo 0)"
is   "B: sp-cbbb2 still CERTIFIED"      "CERTIFIED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-cbbb2")"

# THE SUMMON (sp-47kq1): a double-red the crate cannot resolve mechanically files a bead for
# the batcher persona through bead.sh's own contract, never bd create directly, so its
# partition label comes from batcher.fayth rather than being guessed here a second time.
want "B: reports the judgement bead it filed" "filed sp-" "$out_b"
judgement_id="$(printf '%s\n' "$out_b" | sed -n 's/.*filed \(sp-[a-z0-9.]*\) for judgement.*/\1/p')"
is   "B: exactly one bead filed for judgement" "1" \
    "$(B list --all --label batch-judgement --label "repo:$REPONAME" --json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d if isinstance(d,list) else [d]))')"
want "B: judgement bead title names the repo and suite" "$REPONAME" "$(B show "$judgement_id" --json 2>/dev/null)"
want "B: judgement bead title names the failing suite" "test-b.sh" "$(B show "$judgement_id" --json 2>/dev/null)"
want "B: judgement bead body names the culprit member" "sp-cbbb2" "$(B show "$judgement_id" --long --json 2>/dev/null)"

land_mark_before="$(grep -c '^pr-create' "$FORGE_LOG")"
is   "B: forge pr-create not called again" "$land_mark_before" "$(grep -c '^pr-create' "$FORGE_LOG")"

# =============================================================================
# CASE C — a stale member: an express-certified branch that conflicts with the
# base itself is reopened for rebase immediately (section F), not just skipped.
# POSITIVE CONTROL: the requeue spy is empty before this case runs.
# =============================================================================
echo
echo "C. stale member: base conflict reopens the bead at once:"
is "C: positive control — requeue spy silent before this case" "0" "$(grep -c '^sp-cccc3 ' "$REQUEUE_SPY" 2>/dev/null)"
# sp-cbbb2 (case B) is still CERTIFIED by design — a double-red leaves it be. Case C is
# about a DIFFERENT member's base conflict in isolation, so retire sp-cbbb2 first the way
# a builder eventually would (fix and re-certify elsewhere, or abandon); otherwise it would
# merge cleanly into case C's round and open a PR neither case is testing for.
rm -f "$LANDSTATE/sp-cbbb2"
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

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
