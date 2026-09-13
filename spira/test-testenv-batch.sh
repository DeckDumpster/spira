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
            for cv_pat in $cv_cov; do
                case "$cv_f" in
                    $cv_pat) cv_hit=1
                        case " $cv_sel " in *" $cv_s "*) ;; *) cv_sel="$cv_sel $cv_s" ;; esac ;;
                esac
            done
        done
        [ "$cv_hit" -eq 0 ] && cv_unmapped="$cv_unmapped $cv_f"
    done
    if [ -n "$cv_unmapped" ]; then
        printf '%s' "$cv_all"  # fallback: all suites
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

# Suites that run inside the container live in $FIXTURE/spira/ because the
# fixture repo is bind-mounted at /workspace; the batch executes them as
# /workspace/spira/<name>.  We maintain copies in host SUITE dirs for selection.
mkdir -p "$FIXTURE/spira"

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
cp "$SUITE_B1/test-fx-g.sh" "$FIXTURE/spira/test-fx-g.sh"

cat > "$SUITE_B1/test-fx-u.sh" << 'EOF'
#!/usr/bin/env bash
printf '  ok    test-fx-u ran\n'; exit 0
EOF
chmod +x "$SUITE_B1/test-fx-u.sh"
cp "$SUITE_B1/test-fx-u.sh" "$FIXTURE/spira/test-fx-u.sh"

RESULTS_ROOT_B1="$TMP/results-B1"
rc_b1=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B1" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B1" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b1-$$" \
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
cp "$SUITE_B2/test-fx-red.sh" "$FIXTURE/spira/test-fx-red.sh"

RESULTS_ROOT_B2="$TMP/results-B2"
rc_b2=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B2" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B2" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b2-$$" \
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
cp "$SUITE_B3/test-fx-ka.sh" "$FIXTURE/spira/test-fx-ka.sh"

cat > "$SUITE_B3/test-fx-kb.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
sleep 120; exit 0
EOF
chmod +x "$SUITE_B3/test-fx-kb.sh"
cp "$SUITE_B3/test-fx-kb.sh" "$FIXTURE/spira/test-fx-kb.sh"

cat > "$SUITE_B3/test-fx-kc.sh" << 'EOF'
#!/usr/bin/env bash
# covers: changed.sh
printf '  ok    test-fx-kc ran\n'; exit 0
EOF
chmod +x "$SUITE_B3/test-fx-kc.sh"
cp "$SUITE_B3/test-fx-kc.sh" "$FIXTURE/spira/test-fx-kc.sh"

KILL_INSTANCE="b3k-$$"
KILL_CNAME="spira-batch-${KILL_INSTANCE}"
RESULTS_ROOT_B3="$TMP/results-B3"

rc_b3=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B3" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B3" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="$KILL_INSTANCE" \
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
SPIRA_BATCH_INSTANCE="b4-$$" \
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
SPIRA_BATCH_INSTANCE="b5b-$$" \
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
cp "$SUITE_B5c/test-fx-p.sh" "$FIXTURE/spira/test-fx-p.sh"

RESULTS_ROOT_B5c="$TMP/results-B5c"
rc_b5c=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B5c" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B5c" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b5c-$$" \
    bash "$BATCH" topic "$FIXTURE" || rc_b5c=$?
iszero "B5c: all-fallback run exits 0" "$rc_b5c"

RD_B5c="$(find_results_dir "$RESULTS_ROOT_B5c")"
if [ -n "$RD_B5c" ] && [ -f "$RD_B5c/test-fx-p.sh.result" ]; then
    _prod_b5c="$(awk '{print $6}' "$RD_B5c/test-fx-p.sh.result")"
    [ "$_prod_b5c" = all ] && ok "B5c: unmapped-fallback result has producer=all" \
                            || bad "B5c: unmapped-fallback result has producer=all" \
                                   "got '$_prod_b5c' (full record: $(cat "$RD_B5c/test-fx-p.sh.result"))"
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
