#!/usr/bin/env bash
#
# test-cockpit-unsent.sh — the unsent backlog counts across repositories, not just the first,
# and harness-owned namespaces (queue/*, suite-state/*) are never counted as orphans.
#
# THE FAILURE THIS SUITE EXISTS FOR. SP_BRANCH_DONE was echoed inside the per-repo loop and
# write_snapshot's first-wins dedup pinned it to the first repository's count, which was always
# zero because brain has no closed-bead branches. And SP_UNSENT counted every ref under
# refs/heads/spira/* regardless of whether the suffix resolved to a bead, so a stray ref was
# a permanent +1 on a figure whose whole purpose is to trend to zero.
#
# BOTH DEFECTS ARE INVISIBLE TO A SINGLE-REPO TEST. The per-repo overwrite only fires when a
# second repository contributes differently from the first, and a non-bead branch only matters
# when the probe distinguishes it from real work. This suite drives a TWO-repo fixture and
# plants both shapes.
#
# ALSO ABSORBED (coverage row 12 / cluster 4, formerly test-cockpit-unadopted-queue.sh): an
# aeon following sop-unadopted-refs-stale-db once deleted spira/queue/<stamp> while its CI was
# running. The probe counted the branch as SP_UNADOPTED (no bead resolves for a queue suffix),
# the watchtower filed an incident, and the SOP said to delete it. The local deletion did no
# damage only because origin kept the ref. (sp-ctag9) The reapable set is defined positively:
# only spira/sp-* refs (bead-id shaped) are candidates for the unadopted check. Harness-owned
# namespaces skip the check entirely and are counted in SP_PROTECTED.
#
# SPIRA_BDJSON_FIXTURE, NOT A REAL STORE. unsent_keys' only bd reads are `show <id>` per
# branch and the closed-beads `list` folded into the same function; the query shape itself is
# covered once in test-cockpit-bd-contract.sh, against real bd. Git stays real — it is cheap,
# and the branch-walking half of this probe is exactly what a mocked bd cannot exercise.
#
# defect: sp-884p sp-ctag9
# covers: spira/cockpit.sh
# scar: SP_BRANCH_DONE was overwritten per-repo so only the first repository's zero survived; SP_UNSENT counted every ref under refs/heads/spira/* regardless of whether the suffix resolved to a bead.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# Git identity for fixture commits — required in the container (no ~/.gitconfig).
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"

# Two git repos. `alpha` is the home repo (listed first by spira_repos), `beta` is the second.
ALPHA="$TMP/alpha"; BETA="$TMP/beta"
for r in "$ALPHA" "$BETA"; do
    git init -q "$r"                                   # hermetic-ok: throwaway fixture repos in $TMP
    git -C "$r" commit --allow-empty -m "init" -q      # hermetic-ok: seed commit for the fixture
done

MAP="$TMP/repo-map"
cat > "$MAP" <<MAP
# name | path | land | base | format | gate
alpha | $ALPHA | push | | |
beta  | $BETA  | push | | |
MAP
RUN="$TMP/run"; mkdir -p "$RUN"

# A closed bead and an open bead in BETA, with branches in beta. Nothing bead-backed in alpha.
cat > "$TMP/beads.json" <<'JSON'
[
  {"id":"sp-aaa","title":"work in beta","status":"closed","issue_type":"task","labels":["spira","plan","repo:beta"]},
  {"id":"sp-bbb","title":"open work in beta","status":"in_progress","issue_type":"task","labels":["spira","plan","repo:beta"]}
]
JSON

git -C "$BETA" checkout -q -b spira/sp-aaa
git -C "$BETA" commit --allow-empty -m "sp-aaa work" -q
git -C "$BETA" checkout -q -b spira/sp-bbb
git -C "$BETA" commit --allow-empty -m "sp-bbb work" -q
git -C "$BETA" checkout -q main 2>/dev/null || git -C "$BETA" checkout -q master

# A non-bead branch in alpha whose tip is NOT on the base branch — it carries unlanded work
# and must not be treated as a disposable stray. This is the class that triggered the
# unadopted-refs incident on every 30-minute watchtower pass: the SOP said "git branch -D"
# but the branch held commits absent from the repo's base. (sp-doh5)
git -C "$ALPHA" checkout -q -b spira/tmp-stray
git -C "$ALPHA" commit --allow-empty -m "stray" -q
git -C "$ALPHA" checkout -q main 2>/dev/null || git -C "$ALPHA" checkout -q master

# A non-bead branch in alpha whose tip IS already on the base branch — a true unadopted stray.
# No extra commits beyond main: merge-base --is-ancestor succeeds, so this is safe to delete.
# Positive control for SP_UNADOPTED: the detector must count this one.
git -C "$ALPHA" branch spira/sp-true-stray   # same commit as main; no new work

# spira/queue/<stamp> — a live batch PR branch. Its tip is already on main (same commit),
# so a name-blind probe would count it as SP_UNADOPTED. It must be SP_PROTECTED instead.
QSTAMP="20260917T173304Z"
git -C "$ALPHA" branch "spira/queue/$QSTAMP"

# spira-suite-state/<name> — written by the quarantine path. Same shape: tip on main.
SUITE_BR="test-reclaim-slay-branch-guard-20260917T164605Z"
git -C "$ALPHA" branch "spira-suite-state/$SUITE_BR"

# Open batch record naming the queue branch — the belt-and-suspenders path protects it
# regardless of name, even though the namespace scan above already would.
mkdir -p "$RUN/queue/alpha"
{
    printf 'pr=56\n'
    printf 'head=abc\n'
    printf 'base=def\n'
    printf 'members=sp-foo:abc\n'
    printf 'opened=1726598000\n'
    printf 'branch=spira/queue/%s\n' "$QSTAMP"
} > "$RUN/queue/alpha/open"

# Run cockpit.sh unsent — the probe's own subcommand, not a full `once` — with bd reads
# answered from a canned-JSON fixture rather than a live store.
unsent() {    # unsent <fixture-file>
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
        SPIRA_REPO="$ALPHA" SPIRA_HOME_REPO=alpha \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        SPIRA_QUEUE_DIR="$RUN/queue" \
        SPIRA_BDJSON_FIXTURE="$1" \
        bash "$HERE/cockpit.sh" unsent 2>/dev/null
}

out="$(unsent "$TMP/beads.json")"
val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//; s/^'//; s/'\$//"; }

echo "unsent backlog — two-repo fixture:"

# ======================================================================================
# DEFECT 1: SP_BRANCH_DONE must be the sum across ALL repos, not just the first.
# alpha has 0 closed-bead branches, beta has 1 (sp-aaa is closed). The sum is 1.
is "SP_BRANCH_DONE counts across repos" "1" "$(val SP_BRANCH_DONE)"

# ======================================================================================
# DEFECT 2: a non-bead branch is not counted in SP_UNSENT.
# beta has 2 bead branches (sp-aaa, sp-bbb). alpha has 0 bead branches (sp/tmp-stray is not
# a bead). Total bead-backed unsent: 2.
is "SP_UNSENT counts only bead-backed branches" "2" "$(val SP_UNSENT)"

# ======================================================================================
# DEFECT 3 (sp-doh5): a no-bead branch with UNLANDED commits is NOT a reapable stray.
# spira/tmp-stray has 1 commit not on the base — that is orphan work, not a disposable ref.
# spira/sp-true-stray has no commits beyond main — that IS a true stray, safe to delete.
is "SP_ORPHAN_WORK counts no-bead branches with unlanded commits" "1" "$(val SP_ORPHAN_WORK)"
is "SP_UNADOPTED counts no-bead branches whose tip is already on base" "1" "$(val SP_UNADOPTED)"
want "SP_UNADOPTED_NAMES names the stray branch" "sp-true-stray" "$(val SP_UNADOPTED_NAMES)"
nowant "SP_UNADOPTED_NAMES excludes orphan work" "tmp-stray" "$(val SP_UNADOPTED_NAMES)"

# ======================================================================================
# ABSORBED FROM test-cockpit-unadopted-queue.sh: harness-owned namespaces are never
# counted as unadopted, even though their tip is already on base.
nowant "SP_UNADOPTED_NAMES excludes queue branch"       "queue/"              "$(val SP_UNADOPTED_NAMES)"
nowant "SP_UNADOPTED_NAMES excludes suite-state branch" "spira-suite-state/"  "$(val SP_UNADOPTED_NAMES)"
want "SP_PROTECTED_NAMES names the queue branch"        "queue/$QSTAMP"                "$(val SP_PROTECTED_NAMES)"
want "SP_PROTECTED_NAMES names the suite-state branch"  "spira-suite-state/$SUITE_BR"  "$(val SP_PROTECTED_NAMES)"
is   "SP_PROTECTED counts exactly the two namespaced branches" "2" "$(val SP_PROTECTED)"

# ======================================================================================
# THE POSITIVE CONTROL: the probe found SOMETHING. An empty output would pass all the
# negative assertions above, which is the shape law-absence-needs-a-positive-control warns
# about.
want "output contains SP_AT" "SP_AT=" "$out"
want "output contains SP_UNSENT" "SP_UNSENT=" "$out"

# ======================================================================================
echo
echo "BATCHED-stranded detection:"
# ======================================================================================
# BATCHED branch absent from any open batch → SP_BATCHED_STRANDED=1.
# This is the sp-kogm shape: sp-bbb in beta has in_progress status; we mark its landstate
# BATCHED and provide no open batch file naming it. The probe must count it as stranded.
#
# POSITIVE CONTROL FIRST: a run with BATCHED state and no open batch must report 1.
# Only after that do we verify the BATCHED-in-batch case reports 0 — an all-zero result
# could pass both tests if the probe is not running at all.
mkdir -p "$RUN/landstate"
printf 'BATCHED %s %s' "$(git -C "$BETA" rev-parse spira/sp-bbb 2>/dev/null)" "$(date +%s)" \
    > "$RUN/landstate/sp-bbb"
out2="$(unsent "$TMP/beads.json")"
val2() { printf '%s' "$out2" | grep "^$1=" | head -1 | sed "s/^$1=//"; }
is "BATCHED with no open batch → SP_BATCHED_STRANDED=1" "1" "$(val2 SP_BATCHED_STRANDED)"
want "SP_BATCHED_STRANDED_NAMES names the branch" "sp-bbb" "$(val2 SP_BATCHED_STRANDED_NAMES)"

# BATCHED branch present in the open batch → SP_BATCHED_STRANDED=0.
# Create an open batch file for beta whose members= line includes sp-bbb:<tip>.
_bbb_tip="$(git -C "$BETA" rev-parse spira/sp-bbb 2>/dev/null)"
mkdir -p "$RUN/queue/beta"
printf 'pr=1\nopened=%s\nmembers=sp-bbb:%s\nbranch=spira/queue/test\n' \
    "$(date +%s)" "$_bbb_tip" > "$RUN/queue/beta/open"
out3="$(unsent "$TMP/beads.json")"
val3() { printf '%s' "$out3" | grep "^$1=" | head -1 | sed "s/^$1=//"; }
is "BATCHED present in open batch → SP_BATCHED_STRANDED=0" "0" "$(val3 SP_BATCHED_STRANDED)"
rm -f "$RUN/queue/beta/open" "$RUN/landstate/sp-bbb"

# BATCHED-too-long detection: epoch old enough → SP_BATCHED_TOO_LONG=1.
# POSITIVE CONTROL: an old epoch (default wait=1800s; plant 1801s in the past) fires the
# counter. Only after that do we verify a fresh timestamp returns 0.
echo
echo "BATCHED-too-long detection (sp-7kcj2):"
mkdir -p "$RUN/landstate"
_old_epoch=$(( $(date +%s) - 1801 ))
_bbb_tip="$(git -C "$BETA" rev-parse spira/sp-bbb 2>/dev/null)"
printf 'BATCHED %s %s' "$_bbb_tip" "$_old_epoch" > "$RUN/landstate/sp-bbb"
out_btl="$(unsent "$TMP/beads.json")"
val_btl() { printf '%s' "$out_btl" | grep "^$1=" | head -1 | sed "s/^$1=//"; }
is "BATCHED older than wait → SP_BATCHED_TOO_LONG=1" "1" "$(val_btl SP_BATCHED_TOO_LONG)"
want "SP_BATCHED_TOO_LONG_NAMES names the branch" "sp-bbb" "$(val_btl SP_BATCHED_TOO_LONG_NAMES)"

# Fresh BATCHED epoch → SP_BATCHED_TOO_LONG=0 (positive control that the 0 is real).
printf 'BATCHED %s %s' "$_bbb_tip" "$(date +%s)" > "$RUN/landstate/sp-bbb"
out_btl2="$(unsent "$TMP/beads.json")"
val_btl2() { printf '%s' "$out_btl2" | grep "^$1=" | head -1 | sed "s/^$1=//"; }
is "fresh BATCHED epoch → SP_BATCHED_TOO_LONG=0" "0" "$(val_btl2 SP_BATCHED_TOO_LONG)"
rm -f "$RUN/landstate/sp-bbb"

# ======================================================================================
echo
echo "no shell function is invoked through timeout(1):"
# ======================================================================================
# `timeout` is an external binary. It execs its argument, so it cannot run a shell function
# — `timeout 2 bdjson show X` died with rc=127 and "timeout: failed to execute process" on
# every branch in every repository, and 2>/dev/null swallowed it. The empty result took the
# unadopted path, so SP_UNSENT and SP_BRANCH_DONE both read 0 (the reassuring answer, which
# is the whole reason this suite exists) while SP_UNADOPTED read the total.
#
# The seam is BD_TIMEOUT: bdq already wraps bd in `timeout "${BD_TIMEOUT:-180}"`, so a caller
# that wants a short bound sets that rather than wrapping the function. This is a tree-wide
# grep because the failure mode is silent wherever it appears, not only here.
_tf_funcs='bdjson|bdq|ghq|json_only|log|die|say|ilog|bead_reopen|bump_recur'
# This suite is excluded from its own scan: it carries the offending shape twice on purpose,
# once as the planted control below and once quoted in the message that reports a match.
_tf_self="$(basename "$0")"
_tf_hits="$(grep -rnE "timeout +[\"'\$]*[0-9\$][^ ]* +($_tf_funcs)\b" \
            --include='*.sh' "$HERE" 2>/dev/null \
          | grep -vE '^[^:]+:[0-9]+: *#' \
          | grep -v "/$_tf_self:" || true)"
if [ -z "$_tf_hits" ]; then
    ok "no timeout(1) call wraps a shell function"
else
    bad "no timeout(1) call wraps a shell function" \
        "$(printf '%s' "$_tf_hits" | head -4 | tr '\n' ' ')"
fi

# POSITIVE CONTROL. A grep that matches nothing and a grep pointed at the wrong place give
# the same silence, and the silence is the reassuring reading
# (law-absence-needs-a-positive-control). Plant the exact shape that shipped and require the
# matcher to name it.
_tf_plant="$TMP/planted-timeout.sh"
printf '%s\n' '_st="$(timeout 2 bdjson show "${_b#spira/}" 2>/dev/null)"' > "$_tf_plant"
if grep -qnE "timeout +[\"'\$]*[0-9\$][^ ]* +($_tf_funcs)\b" "$_tf_plant" 2>/dev/null; then
    ok "control — the matcher names the shape that shipped"
else
    bad "control — the matcher names the shape that shipped" \
        "the planted 'timeout 2 bdjson' was not matched; the silence above proves nothing"
fi

echo
echo "probe-fault — a failed bead lookup lands in SP_PROBE_FAIL, not SP_UNADOPTED:"
# ======================================================================================
# DEFECT: a failed bdjson call returned empty, which the probe read as "no bead exists"
# and routed to SP_UNADOPTED. Closed beads appeared as unadopted strays. (sp-kc9v4)
#
# An unreadable fixture makes bdsim.py exit 1 with no stdout — the same shape bdjson sees
# from a real bd that cannot reach its store, whether bd exits non-zero or exits 0 with
# nothing printed (json_only strips anything that is not a JSON line either way).
#
# POSITIVE CONTROL FIRST: the fixture already proves SP_UNADOPTED=1 (sp-true-stray)
# when bd works (asserted above). Now prove that with an unreadable store the same branch
# does NOT feed SP_UNADOPTED.
out_fail="$(unsent "$TMP/does-not-exist.json")"
valf() { printf '%s' "$out_fail" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

want "bd-fail output has SP_AT (probe ran)" "SP_AT=" "$out_fail"
_pf="$(valf SP_PROBE_FAIL)"
if [ "${_pf:-0}" -gt 0 ] 2>/dev/null; then
    ok "bd-fail: SP_PROBE_FAIL > 0 — failed lookups counted as probe faults"
else
    bad "bd-fail: SP_PROBE_FAIL not > 0" "got '${_pf:-<absent>}'"
fi
is "bd-fail: SP_UNADOPTED=0 — failed lookup never feeds unadopted count" "0" "$(valf SP_UNADOPTED)"

echo
printf 'test-cockpit-unsent: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
