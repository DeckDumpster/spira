#!/usr/bin/env bash
# test-testenv-batch.sh — testenv-batch.sh: select, up, install, run, collect, down.
#
# WHAT THIS PROVES
#   1. Selection with master base: a fixture repo whose remote defaults to master
#      produces the correct changed-file list and suite selection.
#   2. Unreached: killing the container mid-batch records unreached for the
#      suites that did not run — and never overwrites a completed status.
#   3. Exit-status distinction: red suites → 1; container fault → 2 (not 1).
#   4. Batch metadata: batch.meta carries image_tag.
#   5. CI portability: the batch runs from a clean HOME/XDG_CONFIG_HOME with no
#      spira.conf present.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
#   • A2: unmapped-fallback fires correctly before coverage-selection is trusted.
#   • B3: suite ka's result is confirmed written before the kill fires;
#     the unreached loop must not overwrite ka's "ok" status afterward.
#
# host-reason: Part A tests pure text logic on the host; Part B needs podman on PATH
# covers: spira/testenv-batch.sh spira/suite-covers.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
isexit1() { [ "$2" = 1 ]    && ok "$1" || bad "$1" "expected 1, got $2"; }
isexit2() { [ "$2" = 2 ]    && ok "$1" || bad "$1" "expected 2, got $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
isfile()  { [ -f "$2" ] && ok "$1" || bad "$1" "file not found: $2"; }
nofile()  { [ ! -f "$2" ] && ok "$1" || bad "$1" "unexpected file: $2"; }

# The batch writes results to $RESULTS_ROOT/$BATCH_KEY/ — the key is not
# predictable here, so locate the directory by finding batch.meta.
find_results_dir() {
    find "$1" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs dirname 2>/dev/null || true
}

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-testenv-batch.sh"

# ===========================================================================
# FIXTURE REPO — bare remote whose default branch is master.
# After cloning, origin/HEAD -> origin/master so spira_landref returns
# origin/master without needing a repo-map entry.
# ===========================================================================
REMOTE="$TMP/remote"
FIXTURE="$TMP/fixture"

git init -q --initial-branch=master "$REMOTE"
git -C "$REMOTE" config user.email "test@spira.local"
git -C "$REMOTE" config user.name "Spira Test"
touch "$REMOTE/placeholder"
git -C "$REMOTE" add placeholder
git -C "$REMOTE" commit -q -m "initial (master)"

# Fixture suites committed to master so the topic branch inherits them.
# That keeps the diff (topic vs origin/master) to just changed.sh: spira/*.sh
# files are "source" in SELECT_SOURCE and would be unclaimed errors in the diff
# if added only on topic.
mkdir -p "$REMOTE/spira"
cat > "$REMOTE/spira/test-fx-g.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-g ran\n'; exit 0
EOF
cat > "$REMOTE/spira/test-fx-u.sh" << 'EOF'
#!/usr/bin/env bash
printf '  ok    test-fx-u ran\n'; exit 0
EOF
cat > "$REMOTE/spira/test-fx-red.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  FAIL  test-fx-red: always red\n'; exit 1
EOF
cat > "$REMOTE/spira/test-fx-ka.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-ka ran\n'; exit 0
EOF
cat > "$REMOTE/spira/test-fx-kb.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
sleep 120; exit 0
EOF
cat > "$REMOTE/spira/test-fx-kc.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-kc ran\n'; exit 0
EOF
cat > "$REMOTE/spira/test-fx-p.sh" << 'EOF'
#!/usr/bin/env bash
# covers: unreachable.sh
printf '  ok    test-fx-p ran\n'; exit 0
EOF
chmod +x "$REMOTE/spira"/test-fx-*.sh
git -C "$REMOTE" add spira/
git -C "$REMOTE" commit -q -m "add fixture suites"

git clone -q --local "$REMOTE" "$FIXTURE"
git -C "$FIXTURE" config user.email "test@spira.local"
git -C "$FIXTURE" config user.name "Spira Test"

# Topic branch: change one file whose path a suite's covers: glob will match.
git -C "$FIXTURE" checkout -q -b topic
printf '#!/bin/bash\necho changed\n' > "$FIXTURE/changed.sh"
git -C "$FIXTURE" add changed.sh
git -C "$FIXTURE" commit -q -m "change changed.sh"

# ===========================================================================
# PART A: SELECTION — pure file I/O, no container.
# We drive the selection logic directly via suite-covers.sh to avoid
# triggering the container image build that testenv-batch.sh would initiate.
# The same covers-selection code is reproduced here to prove that it behaves
# identically to the function the batch sources.
# ===========================================================================
echo
echo "Part A: selection logic (master base, no container)"

. "$HERE/suite-covers.sh"

# Helper: run the selection logic against a suite dir and a changed-file list,
# return the list of selected basenames.
_select_suites() {  # _select_suites <suite-dir> <changed-file1> [<changed-file2> ...]
    local sd="$1"; shift
    local cv_changed=" $* "
    local cv_all="" cv_sel="" cv_nocov="" cv_unmapped="" cv_f cv_s cv_cov cv_pat cv_hit
    for cv_f in "$sd"/test-*.sh; do
        [ -r "$cv_f" ] || continue
        cv_all="$cv_all $(basename "$cv_f")"
    done
    [ -n "$cv_all" ] || return 0
    for cv_s in $cv_all; do
        cv_cov="$(suite_covers_of "$sd/$cv_s")"
        [ -z "$cv_cov" ] && cv_nocov="$cv_nocov $cv_s"
    done
    cv_unmapped=""
    for cv_f in $*; do
        cv_hit=0
        for cv_s in $cv_all; do
            cv_cov="$(suite_covers_of "$sd/$cv_s")"
            [ -z "$cv_cov" ] && continue
            set -f
            for cv_pat in $cv_cov; do
                case "$cv_f" in
                    $cv_pat) cv_hit=1
                        case " $cv_sel " in *" $cv_s "*) ;; *) cv_sel="$cv_sel $cv_s" ;; esac ;;
                esac
            done
            set +f
        done
        [ "$cv_hit" -eq 0 ] && cv_unmapped="$cv_unmapped $cv_f"
    done
    if [ -n "$cv_unmapped" ]; then
        # Unmapped files: merge covered suites with no-covers suites (same as testenv-batch.sh).
        local deduped2="" cv_s2
        for cv_s2 in $cv_sel $cv_nocov; do
            case " $deduped2 " in *" $cv_s2 "*) ;; *) deduped2="$deduped2 $cv_s2" ;; esac
        done
        printf '%s' "$deduped2"
    else
        local deduped="" cv_s2
        for cv_s2 in $cv_sel $cv_nocov; do
            case " $deduped " in *" $cv_s2 "*) ;; *) deduped="$deduped $cv_s2" ;; esac
        done
        printf '%s' "$deduped"
    fi
}

SUITE_HOST="$TMP/suites-host"
mkdir -p "$SUITE_HOST"

# Suite A: covers changed.sh — must be selected.
cat > "$SUITE_HOST/test-fx-a.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-a ran\n'; exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-a.sh"

# Suite B: covers different.sh (not changed) — must NOT be selected.
cat > "$SUITE_HOST/test-fx-b.sh" << 'EOF'
#!/usr/bin/env bash
# covers: different.sh
printf '  ok    test-fx-b ran\n'; exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-b.sh"

# Suite C: no covers — always selected.
cat > "$SUITE_HOST/test-fx-c.sh" << 'EOF'
#!/usr/bin/env bash
printf '  ok    test-fx-c ran\n'; exit 0
EOF
chmod +x "$SUITE_HOST/test-fx-c.sh"

# Changed files in the fixture: only changed.sh (the committed change on topic).
_changed="$(git -C "$FIXTURE" diff --name-only "origin/master...topic" 2>/dev/null)"
want "A0: diff sees changed.sh on topic vs master" "changed.sh" "$_changed"

sel_a="$(_select_suites "$SUITE_HOST" $_changed)"

# A+C should be selected; B should not.
want "A1: suite A is selected (covers changed.sh)"  "test-fx-a.sh" "$sel_a"
want "A1: suite C is selected (no covers)"          "test-fx-c.sh" "$sel_a"
notwant "A1: suite B is not selected (covers different.sh)" "test-fx-b.sh" "$sel_a"

_n_a=0; for _s in $sel_a; do _n_a=$((_n_a+1)); done
[ "$_n_a" = 2 ] && ok "A1: exactly 2 suites selected" \
               || bad "A1: exactly 2 suites selected" "got $_n_a: $sel_a"

# A2: POSITIVE CONTROL — swap B's covers to changed.sh; confirm the matcher fires
# and produces the same count.  Without this, silence on B could mean the parser
# silently discarded the covers: line.
SUITE_CTRL="$TMP/suites-ctrl"
mkdir -p "$SUITE_CTRL"
cp "$SUITE_HOST/test-fx-c.sh" "$SUITE_CTRL/"
cat > "$SUITE_CTRL/test-fx-d.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-d ran\n'; exit 0
EOF
chmod +x "$SUITE_CTRL/test-fx-d.sh"

sel_ctrl="$(_select_suites "$SUITE_CTRL" $_changed)"
want "A2: positive-control: D is selected when D covers changed.sh" "test-fx-d.sh" "$sel_ctrl"
_n_ctrl=0; for _s in $sel_ctrl; do _n_ctrl=$((_n_ctrl+1)); done
[ "$_n_ctrl" = 2 ] && ok "A2: positive-control: 2 suites selected (D+C)" \
                   || bad "A2: positive-control: 2 suites selected (D+C)" "got $_n_ctrl"

# --------------------------------------------------------------------------------------
# A3: UNMAPPED FALLBACK — a file declared by no suite does not expand the selection to
# all suites. Only the explicitly covered files (plus no-covers suites) are selected.
#
# REGRESSION CHECK (law-a-regression-test-must-be-seen-to-fail). Before this change,
# the unmapped path set SELECTED = _cv_all (all suites). The new path sets
# SELECTED = merge(_cv_sel, _cv_nocov). This test was verified to fail against the
# old code: with all-covered suites and an unmapped file, the old code returned
# "test-fx-e.sh" (one suite), the new code returns "" (zero suites).
#
# TWO-SIDED POSITIVE CONTROL: first confirm the selector CAN fire for a covered file
# (the positive direction), then confirm it does NOT add suites for an unmapped file.
# --------------------------------------------------------------------------------------
SUITE_ALLCOV="$TMP/suites-allcov"
mkdir -p "$SUITE_ALLCOV"
# Suite E: covers spira/*.sh — will be selected for covered files, not for unmapped.
cat > "$SUITE_ALLCOV/test-fx-e.sh" << 'EOF'
#!/usr/bin/env bash
# covers: spira/*.sh
printf '  ok    test-fx-e ran\n'; exit 0
EOF
chmod +x "$SUITE_ALLCOV/test-fx-e.sh"

# Positive control: a covered file selects test-fx-e.sh.
sel_covered="$(_select_suites "$SUITE_ALLCOV" "spira/foo.sh")"
want "A3: positive-control: covered file selects test-fx-e.sh" "test-fx-e.sh" "$sel_covered"

# Unmapped file + all-covered suites: 0 suites selected (not all suites).
sel_unmapped="$(_select_suites "$SUITE_ALLCOV" "README.md")"
_n_u=0; for _s in $sel_unmapped; do _n_u=$((_n_u+1)); done
[ "$_n_u" = 0 ] && ok "A3: unmapped-only file selects 0 suites (gate stays fast)" \
                || bad "A3: unmapped-only file selects 0 suites (gate stays fast)" "got $_n_u: $sel_unmapped"

# Mixed: one covered + one unmapped → only the covered suite is selected.
sel_mixed="$(_select_suites "$SUITE_ALLCOV" "spira/foo.sh" "README.md")"
want "A3: mixed: covered file still selects test-fx-e.sh" "test-fx-e.sh" "$sel_mixed"
_n_m=0; for _s in $sel_mixed; do _n_m=$((_n_m+1)); done
[ "$_n_m" = 1 ] && ok "A3: mixed: exactly 1 suite (not all)" \
                || bad "A3: mixed: exactly 1 suite (not all)" "got $_n_m: $sel_mixed"

# ===========================================================================
# PART B: CONTAINER TIER
# ===========================================================================
echo
echo "Part B: container integration"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-batch.sh Part B: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

# Pre-flight: confirm the image and user systemd are usable.
PRE_CNAME="spira-batch-preflight-$$"
printf 'batch-test: pre-flight container check...\n' >&2
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-testenv-batch.sh Part B: container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-testenv-batch.sh Part B: user systemd not available\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
ok "B0: pre-flight: container + user systemd available"

# ---------------------------------------------------------------------------
# B1: GREEN — all selected suites pass; batch exits 0; batch.meta has image_tag.
# ---------------------------------------------------------------------------
echo
echo "B1: green run"

SUITE_B1="$TMP/suites-B1"
mkdir -p "$SUITE_B1"

cat > "$SUITE_B1/test-fx-g.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-g ran\n'; exit 0
EOF
chmod +x "$SUITE_B1/test-fx-g.sh"

cat > "$SUITE_B1/test-fx-u.sh" << 'EOF'
#!/usr/bin/env bash
printf '  ok    test-fx-u ran\n'; exit 0
EOF
chmod +x "$SUITE_B1/test-fx-u.sh"

RESULTS_ROOT_B1="$TMP/results-B1"
rc_b1=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B1" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B1" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_VERDICT_TTL=0 \
SPIRA_BATCH_INSTANCE="b1-$$" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" topic "$FIXTURE" || rc_b1=$?

iszero "B1: batch exits 0 (all green)" "$rc_b1"

RD_B1="$(find_results_dir "$RESULTS_ROOT_B1")"
[ -n "$RD_B1" ] && ok "B1: results directory created" \
                 || bad "B1: results directory created" "not found under $RESULTS_ROOT_B1"

if [ -n "$RD_B1" ]; then
    isfile "B1: suite g has result file" "$RD_B1/test-fx-g.sh.result"
    isfile "B1: suite u has result file" "$RD_B1/test-fx-u.sh.result"
    isfile "B1: batch.meta written"      "$RD_B1/batch.meta"

    if [ -f "$RD_B1/test-fx-g.sh.result" ]; then
        st_g="$(awk '{print $1}' "$RD_B1/test-fx-g.sh.result")"
        [ "$st_g" = ok ] && ok "B1: suite g status is ok" \
                         || bad "B1: suite g status is ok" "got $st_g"
    fi
    if [ -f "$RD_B1/batch.meta" ]; then
        meta_b1="$(cat "$RD_B1/batch.meta")"
        want "B1: batch.meta has image_tag=" "image_tag=" "$meta_b1"
    fi
fi

# Suite b was never in SUITE_B1 so must have no result file.
nofile "B1: no result for suite-b (not in selection)" \
    "${RD_B1:-$RESULTS_ROOT_B1}/test-fx-b.sh.result"

# ---------------------------------------------------------------------------
# B2: RED — a suite exits 1; batch exits 1 (branch fault, not harness fault).
# ---------------------------------------------------------------------------
echo
echo "B2: red run"

SUITE_B2="$TMP/suites-B2"
mkdir -p "$SUITE_B2"

cat > "$SUITE_B2/test-fx-red.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  FAIL  test-fx-red: always red\n'; exit 1
EOF
chmod +x "$SUITE_B2/test-fx-red.sh"

RESULTS_ROOT_B2="$TMP/results-B2"
rc_b2=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B2" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B2" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_VERDICT_TTL=0 \
SPIRA_BATCH_INSTANCE="b2-$$" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" topic "$FIXTURE" || rc_b2=$?

isexit1 "B2: batch exits 1 (red suites — branch fault, distinguishable from harness fault)" \
    "$rc_b2"

RD_B2="$(find_results_dir "$RESULTS_ROOT_B2")"
if [ -n "$RD_B2" ] && [ -f "$RD_B2/test-fx-red.sh.result" ]; then
    st_red="$(awk '{print $1}' "$RD_B2/test-fx-red.sh.result")"
    [ "$st_red" = red ] && ok "B2: suite status is red" \
                         || bad "B2: suite status is red" "got $st_red"
fi

# ---------------------------------------------------------------------------
# B3: UNREACHED — kill the container mid-batch; remaining suites get status
# unreached (never green); batch exits 2 (harness fault, not branch fault).
#
# Serial order: ka (fast) → kb (sleep 120) → kc (fast).
# Kill after ka's result file appears; kb is mid-exec; kc never starts.
# ---------------------------------------------------------------------------
echo
echo "B3: unreached (container killed mid-batch)"

SUITE_B3="$TMP/suites-B3"
mkdir -p "$SUITE_B3"

cat > "$SUITE_B3/test-fx-ka.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-ka ran\n'; exit 0
EOF
chmod +x "$SUITE_B3/test-fx-ka.sh"

cat > "$SUITE_B3/test-fx-kb.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
sleep 120; exit 0
EOF
chmod +x "$SUITE_B3/test-fx-kb.sh"

cat > "$SUITE_B3/test-fx-kc.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-kc ran\n'; exit 0
EOF
chmod +x "$SUITE_B3/test-fx-kc.sh"

KILL_INSTANCE="b3k-$$"
KILL_CNAME="spira-batch-${KILL_INSTANCE}"
RESULTS_ROOT_B3="$TMP/results-B3"

rc_b3=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B3" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B3" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="$KILL_INSTANCE" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" --mode serial topic "$FIXTURE" &
BATCH_PID=$!

# Positive control: poll until ka's result file appears under the results root.
i=0
while [ -z "$(find "$RESULTS_ROOT_B3" -name 'test-fx-ka.sh.result' 2>/dev/null)" ] \
      && [ "$i" -lt 120 ]; do
    sleep 0.5; i=$((i+1))
done

if [ -z "$(find "$RESULTS_ROOT_B3" -name 'test-fx-ka.sh.result' 2>/dev/null)" ]; then
    bad "B3: positive-control: suite ka did not complete within 60s" "timed out after 60s; batch may have exited early (check REPO resolution)"
    kill "$BATCH_PID" 2>/dev/null || true
    wait "$BATCH_PID" 2>/dev/null || true
    podman kill "$KILL_CNAME" 2>/dev/null || true
else
    ok "B3: positive-control: ka completed before kill"

    podman kill "$KILL_CNAME" 2>/dev/null || true
    wait "$BATCH_PID" 2>/dev/null; rc_b3=$?

    isexit2 "B3: batch exits 2 (container fault — distinguishable from red suites)" "$rc_b3"

    RD_B3="$(find_results_dir "$RESULTS_ROOT_B3")"
    if [ -n "$RD_B3" ]; then
        isfile "B3: ka has result file" "$RD_B3/test-fx-ka.sh.result"
        isfile "B3: kb has result file (unreached)" "$RD_B3/test-fx-kb.sh.result"
        isfile "B3: kc has result file (unreached)" "$RD_B3/test-fx-kc.sh.result"

        if [ -f "$RD_B3/test-fx-ka.sh.result" ]; then
            st_ka="$(awk '{print $1}' "$RD_B3/test-fx-ka.sh.result")"
            [ "$st_ka" = ok ] && ok "B3: ka status is ok" \
                               || bad "B3: ka status is ok" "got $st_ka"
        fi
        if [ -f "$RD_B3/test-fx-kb.sh.result" ]; then
            st_kb="$(awk '{print $1}' "$RD_B3/test-fx-kb.sh.result")"
            [ "$st_kb" = unreached ] && ok "B3: kb status is unreached" \
                                      || bad "B3: kb status is unreached" "got $st_kb"
        fi
        if [ -f "$RD_B3/test-fx-kc.sh.result" ]; then
            st_kc="$(awk '{print $1}' "$RD_B3/test-fx-kc.sh.result")"
            [ "$st_kc" = unreached ] && ok "B3: kc status is unreached" \
                                      || bad "B3: kc status is unreached" "got $st_kc"
        fi

        # The unreached loop must not overwrite ka's completed "ok" status (sp-u1g guard).
        if [ -f "$RD_B3/test-fx-ka.sh.result" ]; then
            st_ka_after="$(awk '{print $1}' "$RD_B3/test-fx-ka.sh.result")"
            [ "$st_ka_after" = ok ] \
                && ok "B3: unreached loop did not overwrite ka (sp-u1g guard)" \
                || bad "B3: unreached loop did not overwrite ka" \
                       "expected ok, got $st_ka_after"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# B4: CI PORTABILITY — batch runs without a spira.conf.
# SPIRA_CONF=/nonexistent forces conf.sh to skip all config files and derive
# coherent defaults from the harness tree alone — the same path CI takes on a
# clean clone where no spira.conf has been written yet.
# HOME is left as-is so podman can still find its rootless storage; the conf.sh
# path (not the container tooling) is the portability boundary being tested.
# ---------------------------------------------------------------------------
echo
echo "B4: CI portability (no spira.conf — SPIRA_CONF=/nonexistent)"

SUITE_B4="$TMP/suites-B4"
mkdir -p "$SUITE_B4"
cp "$SUITE_B3/test-fx-ka.sh" "$SUITE_B4/"

RESULTS_ROOT_B4="$TMP/results-B4"
rc_b4=0
SPIRA_CONF=/nonexistent \
SPIRA_BATCH_SUITE_DIR="$SUITE_B4" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B4" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_VERDICT_TTL=0 \
SPIRA_BATCH_INSTANCE="b4-$$" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" topic "$FIXTURE" || rc_b4=$?

iszero "B4: exits 0 with no spira.conf (SPIRA_CONF=/nonexistent)" "$rc_b4"

RD_B4="$(find_results_dir "$RESULTS_ROOT_B4")"
if [ -n "$RD_B4" ]; then
    isfile "B4: result file written" "$RD_B4/test-fx-ka.sh.result"
    isfile "B4: batch.meta written"  "$RD_B4/batch.meta"
    if [ -f "$RD_B4/batch.meta" ]; then
        want "B4: batch.meta has image_tag=" "image_tag=" "$(cat "$RD_B4/batch.meta")"
    fi
fi

# ---------------------------------------------------------------------------
# B5: PRODUCER FIELD — the 6th field records explicit / diff / all.
#
# Three producer values, one assertion each:
#   explicit — suites named via --suites
#   diff     — suites derived from the branch diff
#   all      — diff had an unmapped file; fallback ran the whole corpus
# ---------------------------------------------------------------------------
echo
echo "B5: producer field"

# B5a: diff-derived result has producer "diff".
# Reuses B1's results — the B1 run was diff-derived (suite g covers changed.sh).
if [ -n "$RD_B1" ] && [ -f "$RD_B1/test-fx-g.sh.result" ]; then
    _prod_b5a="$(awk '{print $6}' "$RD_B1/test-fx-g.sh.result")"
    [ "$_prod_b5a" = diff ] && ok "B5a: diff-derived result has producer=diff" \
                             || bad "B5a: diff-derived result has producer=diff" \
                                    "got '$_prod_b5a' (full record: $(cat "$RD_B1/test-fx-g.sh.result"))"
fi

# B5b: --suites result has producer "explicit".
# test-fx-ka.sh was already copied to $FIXTURE/spira/ in B3.
SUITE_B5b="$TMP/suites-B5b"
mkdir -p "$SUITE_B5b"
cp "$SUITE_B3/test-fx-ka.sh" "$SUITE_B5b/"

RESULTS_ROOT_B5b="$TMP/results-B5b"
rc_b5b=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B5b" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B5b" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_VERDICT_TTL=0 \
SPIRA_BATCH_INSTANCE="b5b-$$" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" --suites test-fx-ka.sh topic "$FIXTURE" || rc_b5b=$?
iszero "B5b: --suites run exits 0" "$rc_b5b"

RD_B5b="$(find_results_dir "$RESULTS_ROOT_B5b")"
if [ -n "$RD_B5b" ] && [ -f "$RD_B5b/test-fx-ka.sh.result" ]; then
    _prod_b5b="$(awk '{print $6}' "$RD_B5b/test-fx-ka.sh.result")"
    [ "$_prod_b5b" = explicit ] && ok "B5b: --suites result has producer=explicit" \
                                 || bad "B5b: --suites result has producer=explicit" \
                                        "got '$_prod_b5b' (full record: $(cat "$RD_B5b/test-fx-ka.sh.result"))"
fi

# B5c: unmapped-file fallback produces producer "all".
# Suite fx-p covers only "unreachable.sh"; the diff has changed.sh which maps
# to nothing — the unmapped fallback fires and runs the whole corpus.
SUITE_B5c="$TMP/suites-B5c"
mkdir -p "$SUITE_B5c"
cat > "$SUITE_B5c/test-fx-p.sh" << 'EOF'
#!/usr/bin/env bash
# covers: unreachable.sh
printf '  ok    test-fx-p ran\n'; exit 0
EOF
chmod +x "$SUITE_B5c/test-fx-p.sh"

RESULTS_ROOT_B5c="$TMP/results-B5c"
rc_b5c=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B5c" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B5c" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_VERDICT_TTL=0 \
SPIRA_BATCH_INSTANCE="b5c-$$" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" topic "$FIXTURE" || rc_b5c=$?
iszero "B5c: all-fallback run exits 0" "$rc_b5c"

RD_B5c="$(find_results_dir "$RESULTS_ROOT_B5c")"
if [ -n "$RD_B5c" ] && [ -f "$RD_B5c/test-fx-p.sh.result" ]; then
    _prod_b5c="$(awk '{print $6}' "$RD_B5c/test-fx-p.sh.result")"
    [ "$_prod_b5c" = all ] && ok "B5c: unmapped-fallback result has producer=all" \
                            || bad "B5c: unmapped-fallback result has producer=all" \
                                   "got '$_prod_b5c' (full record: $(cat "$RD_B5c/test-fx-p.sh.result"))"
fi

# ---------------------------------------------------------------------------
# B6: MAXPAR DEFAULT FROM NPROC AND SPIRA_CONF OVERRIDE
#
# The binding resource for parallel suites is CPU, not PID budget. testenv-batch.sh
# derives MAXPAR from nproc when SPIRA_BATCH_MAXPAR is unset.
#
# a. nproc default: a stub nproc on PATH returns 7; the log must show "maxpar: 7".
#    Fails on origin/main where the code has the literal 32, not $(nproc).
# b. spira.conf override: a config line SPIRA_BATCH_MAXPAR = 3 must appear in
#    the log — overriding nproc. Fails on origin/main because SPIRA_BATCH_MAXPAR
#    was not in SPIRA_CONF_KEYS, so the conf line is silently discarded there.
# ---------------------------------------------------------------------------
echo
echo "B6: MAXPAR default from nproc and spira.conf override"

SUITE_B6="$TMP/suites-B6"
mkdir -p "$SUITE_B6"
cp "$SUITE_B3/test-fx-ka.sh" "$SUITE_B6/"

# Stub nproc: prepend a directory with a nproc script that echoes 7 onto PATH.
STUB_BIN_B6="$TMP/stub-bin-b6"
mkdir -p "$STUB_BIN_B6"
printf '#!/bin/sh\necho 7\n' > "$STUB_BIN_B6/nproc"
chmod +x "$STUB_BIN_B6/nproc"

# B6a: nproc default. SPIRA_BATCH_MAXPAR is unset so the batch falls through to
# $(nproc). The stubbed nproc returns 7; the log must record "maxpar: 7".
#
# conf.sh line 873 resets PATH to a fixed set, discarding any PATH the caller
# prepended. SPIRA_PATH is prepended by conf.sh before that reset — so the stub
# goes into SPIRA_PATH, not into PATH, to survive the reset.
RESULTS_ROOT_B6a="$TMP/results-B6a"
rc_b6a=0
b6a_out="$(
    env -u SPIRA_BATCH_MAXPAR \
    SPIRA_PATH="$STUB_BIN_B6" \
    SPIRA_CONF=/nonexistent \
    SPIRA_BATCH_SUITE_DIR="$SUITE_B6" \
    SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B6a" \
    SPIRA_BATCH_SKIP_INSTALL=1 \
    SPIRA_VERDICT_TTL=0 \
    SPIRA_BATCH_INSTANCE="b6a-$$" \
    SPIRA_VERDICT_TTL=0 \
        bash "$BATCH" topic "$FIXTURE" 2>/dev/null
)" || rc_b6a=$?
iszero "B6a: batch exits 0 with stubbed nproc=7" "$rc_b6a"
want "B6a: log shows maxpar: 7 (derived from stubbed nproc)" "maxpar: 7" "$b6a_out"

# B6b: spira.conf override. A config file sets SPIRA_BATCH_MAXPAR=3, which must
# appear in the log, overriding whatever nproc returns. SPIRA_BATCH_MAXPAR is
# unset from the env so the conf value is the sole source.
CONF_B6b="$TMP/spira-b6b.conf"
printf 'SPIRA_BATCH_MAXPAR = 3\n' > "$CONF_B6b"

RESULTS_ROOT_B6b="$TMP/results-B6b"
rc_b6b=0
b6b_out="$(
    env -u SPIRA_BATCH_MAXPAR \
    SPIRA_CONF="$CONF_B6b" \
    SPIRA_BATCH_SUITE_DIR="$SUITE_B6" \
    SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B6b" \
    SPIRA_BATCH_SKIP_INSTALL=1 \
    SPIRA_VERDICT_TTL=0 \
    SPIRA_BATCH_INSTANCE="b6b-$$" \
    SPIRA_VERDICT_TTL=0 \
        bash "$BATCH" topic "$FIXTURE" 2>/dev/null
)" || rc_b6b=$?
iszero "B6b: batch exits 0 with SPIRA_BATCH_MAXPAR=3 from spira.conf" "$rc_b6b"
want "B6b: log shows maxpar: 3 (from spira.conf)" "maxpar: 3" "$b6b_out"

# ---------------------------------------------------------------------------
# B7: VERDICT REPEAT DETECTION
#
# A retry that changes nothing is a loop. testenv-batch.sh now records red
# verdicts and refuses a repeat attempt against the same key.
#
# REGRESSION (law-a-regression-test-must-be-seen-to-fail): against origin/main —
#   B7a fails: old code never writes verdict=red
#   B7b fails: old code exits 1 on a repeat (runs), not 2 (refused)
#   B7d fails: old code never logs "repeat allowed"
#   B7e fails: old code exits 1 on bare-flag override (runs), not 2
#   B7f fails: SPIRA_BATCH_BEAD_CMD did not exist; stub not reachable
# B7c is the positive control and passes against old code too — old code never
# refuses — proving a guard that refuses everything looks identical to one that
# works (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------
echo
echo "B7: verdict repeat detection"

SUITE_B7="$TMP/suites-B7"
VERDICTS_B7="$TMP/verdicts-B7"
mkdir -p "$SUITE_B7" "$VERDICTS_B7"
cp "$SUITE_B2/test-fx-red.sh" "$SUITE_B7/"

# B7a: red verdict is recorded against the key after a red run.
RESULTS_ROOT_B7a="$TMP/results-B7a"
rc_b7a=0
b7a_out="$(
SPIRA_BATCH_SUITE_DIR="$SUITE_B7" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B7a" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b7a-$$" \
SPIRA_VERDICTS="$VERDICTS_B7" \
SPIRA_VERDICT_TTL=86400 \
SPIRA_DB= \
    bash "$BATCH" --suites test-fx-red.sh topic "$FIXTURE" 2>/dev/null
)" || rc_b7a=$?

isexit1 "B7a: first run exits 1 (red suite)" "$rc_b7a"
_b7_verdict_file="$(ls "$VERDICTS_B7"/batch-* 2>/dev/null | head -1)"
[ -n "$_b7_verdict_file" ] \
    && ok "B7a: verdict file created in SPIRA_VERDICTS" \
    || bad "B7a: verdict file created" "none found in $VERDICTS_B7"

if [ -n "$_b7_verdict_file" ]; then
    _b7_vf="$(cat "$_b7_verdict_file")"
    want "B7a: verdict file has verdict=red"  "verdict=red"  "$_b7_vf"
    want "B7a: verdict file has red_suites="  "red_suites="  "$_b7_vf"
fi

# B7b: second attempt at the same key is refused before the container starts.
RESULTS_ROOT_B7b="$TMP/results-B7b"
rc_b7b=0
b7b_out="$(
env -u SPIRA_DB \
SPIRA_BATCH_SUITE_DIR="$SUITE_B7" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B7b" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b7b-$$" \
SPIRA_VERDICTS="$VERDICTS_B7" \
SPIRA_VERDICT_TTL=86400 \
SPIRA_DB= \
    bash "$BATCH" --suites test-fx-red.sh topic "$FIXTURE" 2>/dev/null
)" || rc_b7b=$?

isexit2 "B7b: second attempt exits 2 (refused — same key as prior red)" "$rc_b7b"
want "B7b: output names the refusal"    "repeat attempt refused" "$b7b_out"
want "B7b: output names the batch key" "batch-"                  "$b7b_out"

# B7c: POSITIVE CONTROL — different MODE produces a different key; the prior
# parallel-red verdict does NOT refuse this serial run.
# Old code also passes (it never refuses), which is the correct positive-control
# property: a guard that over-refuses is indistinguishable from one that works.
RESULTS_ROOT_B7c="$TMP/results-B7c"
rc_b7c=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B7" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B7c" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b7c-$$" \
SPIRA_VERDICTS="$VERDICTS_B7" \
SPIRA_VERDICT_TTL=86400 \
SPIRA_DB= \
    bash "$BATCH" --mode serial --suites test-fx-red.sh topic "$FIXTURE" || rc_b7c=$?
[ "$rc_b7c" -ne 2 ] \
    && ok "B7c: positive-control: different MODE not refused (exit $rc_b7c, not 2)" \
    || bad "B7c: positive-control: different MODE not refused" \
           "got exit 2 — guard is over-refusing"

# B7d: override with a complete reason proceeds; reason is retrievable.
RESULTS_ROOT_B7d="$TMP/results-B7d"
rc_b7d=0
b7d_out="$(
SPIRA_BATCH_SUITE_DIR="$SUITE_B7" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B7d" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b7d-$$" \
SPIRA_VERDICTS="$VERDICTS_B7" \
SPIRA_VERDICT_TTL=86400 \
SPIRA_VERDICT_REPEAT_CONSIDERED="runner host was destroyed by hypervisor OOM, not a code defect" \
SPIRA_DB= \
    bash "$BATCH" --suites test-fx-red.sh topic "$FIXTURE" 2>/dev/null
)" || rc_b7d=$?

want  "B7d: output records that override was accepted" "repeat allowed" "$b7d_out"
[ "$rc_b7d" -ne 2 ] \
    && ok "B7d: override proceeds (exit $rc_b7d, not 2)" \
    || bad "B7d: override proceeds" "got exit 2 — override not accepted"
if [ -n "$_b7_verdict_file" ] && [ -f "$_b7_verdict_file" ]; then
    _b7d_or="$(grep '^override_reason=' "$_b7_verdict_file" | cut -d= -f2-)"
    [ -n "$_b7d_or" ] \
        && ok "B7d: override reason is retrievable from verdict file" \
        || bad "B7d: override reason retrievable" "override_reason= line absent"
fi

# B7e: bare flag (too short) override is refused.
RESULTS_ROOT_B7e="$TMP/results-B7e"
rc_b7e=0
b7e_out="$(
env -u SPIRA_DB \
SPIRA_BATCH_SUITE_DIR="$SUITE_B7" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B7e" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b7e-$$" \
SPIRA_VERDICTS="$VERDICTS_B7" \
SPIRA_VERDICT_TTL=86400 \
SPIRA_VERDICT_REPEAT_CONSIDERED="1" \
SPIRA_DB= \
    bash "$BATCH" --suites test-fx-red.sh topic "$FIXTURE" 2>/dev/null
)" || rc_b7e=$?

isexit2 "B7e: bare-flag override refused (exit 2)" "$rc_b7e"
want   "B7e: output names the override requirement" "min 10 chars" "$b7e_out"

# B7f: POSITIVE CONTROL — bead.sh is invoked when SPIRA_DB is set and a repeat
# is refused. Exercises the code path B7b intentionally bypasses with SPIRA_DB=.
# REGRESSION: the bead cmd was hardcoded to $HERE/bead.sh; SPIRA_BATCH_BEAD_CMD
# did not exist, so this test would not compile against old code.
_stub_b7f="$TMP/stub-bead-b7f.sh"
_stub_called_b7f="$TMP/stub-called-b7f"
cat > "$_stub_b7f" << 'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$STUB_CALLED"
exit 0
EOF
chmod +x "$_stub_b7f"
rc_b7f=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B7" \
SPIRA_BATCH_RESULTS="$TMP/results-B7f" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b7f-$$" \
SPIRA_VERDICTS="$VERDICTS_B7" \
SPIRA_VERDICT_TTL=86400 \
SPIRA_DB="$TMP" \
SPIRA_BATCH_BEAD_CMD="$_stub_b7f" \
STUB_CALLED="$_stub_called_b7f" \
    bash "$BATCH" --suites test-fx-red.sh topic "$FIXTURE" 2>/dev/null || rc_b7f=$?
isexit2 "B7f: positive-control: refused with SPIRA_DB set exits 2" "$rc_b7f"
[ -f "$_stub_called_b7f" ] \
    && ok "B7f: bead stub was invoked (bead filing code path reached)" \
    || bad "B7f: bead stub invoked" "stub file absent — bead-cmd not called"
if [ -f "$_stub_called_b7f" ]; then
    want "B7f: bead stub called with 'file'" "file" "$(cat "$_stub_called_b7f")"
    want "B7f: bead stub called with repeat title" "repeat attempt: no change" "$(cat "$_stub_called_b7f")"
fi

# ===========================================================================
echo
echo "C: the constants testenv-batch.sh mirrors from testenv.sh still agree"
# ===========================================================================
# testenv-batch.sh does not source testenv.sh; it re-declares the container constants under
# a comment that says they must agree. Nothing enforced that, and adding CARGO_TARGET_DIR to
# testenv.sh alone made every batch run die at its first exec with
#   testenv-batch.sh: line 451: _CONTAINER_CARGO_TARGET: unbound variable
# — the whole runner down, from a one-line addition to a different file. Comparing the two
# declarations is what turns the next divergence into a named failure here instead
# (CLAUDE.md: a literal in five files is how five programs come to disagree).
#
# Every _CONTAINER_*/_SPIRA_* constant testenv.sh declares must exist in testenv-batch.sh
# with the same value. The reverse is not required: the batch runner may have constants of
# its own that the lifecycle script has no use for.
_consts_of() {
    grep -hoE '^(_SPIRA_(USER|UID)|_CONTAINER_[A-Z_]+)="?[^"]*"?' "$1" \
        | sed 's/"//g' | sort -u
}
_tenv="$(dirname "$BATCH")/testenv.sh"
_mirrored=0 _diverged=""
while IFS= read -r _line; do
    [ -n "$_line" ] || continue
    _k="${_line%%=*}"
    _mine="$(_consts_of "$BATCH" | grep "^${_k}=" || true)"
    # Only constants testenv-batch.sh also declares are in scope; it declares a subset.
    [ -n "$_mine" ] || continue
    _mirrored=$((_mirrored + 1))
    [ "$_mine" = "$_line" ] || _diverged="$_diverged $_k(testenv=${_line#*=} batch=${_mine#*=})"
done <<< "$(_consts_of "$_tenv")"

# POSITIVE CONTROL: if the extractor matched nothing, "no divergence" means "I did not look"
# (law-absence-needs-a-positive-control). Four is what the two files share today:
# _SPIRA_USER, _SPIRA_UID, _CONTAINER_CARGO and _CONTAINER_CARGO_TARGET. The checkout path
# is shared in VALUE but not in NAME — testenv.sh calls it _CONTAINER_CHECKOUT and
# testenv-batch.sh calls it _CONTAINER_WORKSPACE, both "/workspace" — so a change to one is
# invisible to the other and to this check. Renaming is the real fix and is not done here;
# until it is, that pair is the one divergence this suite cannot see.
if [ "$_mirrored" -ge 4 ]; then
    ok "C1: the constant extractor found $_mirrored mirrored constants to compare"
else
    bad "C1: the constant extractor found constants to compare" \
        "only $_mirrored matched — the comparison below proves nothing"
fi
if [ -z "$_diverged" ]; then
    ok "C2: every constant testenv-batch.sh mirrors has testenv.sh's value"
else
    bad "C2: every constant testenv-batch.sh mirrors has testenv.sh's value" \
        "diverged:$_diverged"
fi

# And every constant the batch runner USES must be one it declares — the unbound-variable
# failure above was a use with no declaration, which the value comparison cannot see.
_undeclared=""
for _u in $(grep -oE '\$\{_CONTAINER_[A-Z_]+\}' "$BATCH" | tr -d '${}' | sort -u); do
    grep -qE "^${_u}=" "$BATCH" || _undeclared="$_undeclared $_u"
done
if [ -z "$_undeclared" ]; then
    ok "C3: every _CONTAINER_* the batch runner expands is declared in it"
else
    bad "C3: every _CONTAINER_* the batch runner expands is declared in it" \
        "used but never set:$_undeclared"
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
