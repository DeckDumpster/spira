#!/usr/bin/env bash
# gate-touched.sh <base> <head> — suite selector for the landing gate.
#
# Delegates to select.sh (the ONE selector) for coverage-based selection, then
# appends SPIRA_GATE_EJECTED_SUITES so re-certification re-runs suites that
# proved red (law-a-retry-must-change-an-input).
#
# When SPIRA_GATE_FILES names a readable file the pre-computed diff list from
# gate.sh is used; otherwise the diff is computed from BASE and HEAD.
#
# SPIRA_GATE_EJECTED_SUITES  comma-separated suite names always included.
# SPIRA_GATE_SELECT_CAP      maximum suite count (0 = no cap). Ejected suites
#                             are always kept; excluded suites are logged to stderr.
# SPIRA_GATE_TIERS           comma-separated # tier: values to run here; default
#                             "T0,T1" — certification runs the cheap tiers, batch
#                             and main CI run T2/T3 (docs/test-plan/README.md). A
#                             suite with no # tier: declaration always runs, so
#                             this is a no-op until a suite carries the header.
#                             Ejected suites are exempt: a suite that proved red
#                             reruns regardless of tier (law-a-retry-must-change-an-input).
set -uo pipefail
BASE="${1:?usage: gate-touched.sh <base> <head>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# THE BUILD FENCE RUNS REGARDLESS OF SPIRA_GATE_SUITES. A compile is a static check of the
# tree, not a suite — sp-upkae certified green under fences-only certification
# (SPIRA_GATE_SUITES=off) and then broke the whole batch's build job, because that path never
# invokes cargo at all. build-fence.sh skips itself when the diff touches nothing that can
# move a build's result (sp-9uro3). This is the one place in the repo-map's fence chain that
# runs on every call regardless of mode, so wiring it here needs no repo-map change.
#
# SPIRA_GATE_BASE DEFAULTS TO THIS SCRIPT'S OWN $BASE ARGUMENT. gate.sh's real invocation
# already exports SPIRA_GATE_BASE (and SPIRA_GATE_FILES, which build-fence.sh prefers), so
# this default only matters for a direct call — like a test's own — that passes BASE and
# HEAD positionally without setting either env var.
SPIRA_GATE_BASE="${SPIRA_GATE_BASE:-$BASE}" bash "$HERE/build-fence.sh" || exit 1

# THE ALLOWLIST RATCHETS RUN HERE TOO (sp-5m133), same reason: a suite-selection mode that
# skips everything must not also skip the checks that keep a violator from getting
# grandfathered in worse than it already is. check-areas is static (no duckdb, no timing
# data) so it runs unconditionally; the wall-time budgets themselves are judged per-suite
# inside testenv-batch.sh, against the suites this call actually selects.
bash "$HERE/tier-budget.sh" lint-allowlist --base "$BASE" || exit 1
bash "$HERE/tier-budget.sh" lint-allowlist --base "$BASE" --area || exit 1
bash "$HERE/tier-budget.sh" check-areas --suite-dir "$HERE" || exit 1

# THE TEST PLAN'S OWN FENCE, for the same reason build-fence.sh sits here: a suite deletion
# that silently drops a use case's last coverage, or a coverage matrix that no longer matches
# its own inputs, is a defect in the tree, not something a suite run would catch. Its stdout
# is redirected to stderr: plan-lint.sh/plan-matrix.sh print status text on success (fine for
# standalone use) but gate-touched.sh's own stdout contract is the selected suite list only —
# build-fence.sh keeps to that by writing every message of its own to stderr; this fence's
# children do not, so the redirection is done here instead.
#
# GATED ON SPIRA_GATE_REPO ACTUALLY BEING THIS REPO. Unlike build-fence.sh (whose failure
# mode degrades harmlessly to "empty diff, skip" against any tree), this fence does real,
# ref-sensitive work — docs/test-plan/*.toml and the suite corpus it reads are THIS repo's
# own. A caller that points SPIRA_GATE_REPO at a different tree (test-gate-touched.sh's own
# scratch fixture, proving selection logic against a throwaway repo) is not asking this
# repo's test plan to be checked at all, and BASE there is a label meaningful only inside
# that scratch tree — resolving it against this repo's own ref namespace would check the
# wrong thing, not the right thing cautiously.
_repo="${SPIRA_GATE_REPO:-.}"
if [ "$_repo" = "." ] || [ "$(cd "$_repo" 2>/dev/null && pwd -P)" = "$(cd "$HERE/.." && pwd -P)" ]; then
    bash "$HERE/plan-matrix-fence.sh" "$BASE" >&2 || exit 1
fi

HEAD="${2:?usage: gate-touched.sh <base> <head>}"
repo="${SPIRA_GATE_REPO:-.}"
_tiers="${SPIRA_GATE_TIERS:-T0,T1}"

# SPIRA_GATE_SUITES=off: select NOTHING beyond SPIRA_CERTIFY_ALWAYS_COVERS, so the gate
# command's `[ -n "$_s" ] || exit 0` passes after its fences have run. landing.sh sets it for
# queue-mode certification when SPIRA_CERTIFY_SUITES=off; the batch's CI run is where the rest
# of the suites run.
#
# THE CARVE-OUT MATCHES covers: GLOBS AGAINST THE CRITICAL LIST DIRECTLY, not against the
# diff via select.sh. sp-dgaig's landed() rewrite in spira/lib.sh was certified with NO suite
# run at all (SPIRA_GATE_SUITES=off dropped everything) and broke test-landed-search.sh and
# test-landed-stays-landed.sh only when batch 323's CI caught it — both declare
# `# covers: spira/lib.sh`. Running select.sh's normal diff-mode selection here instead
# (even restricted to lib.sh as the only "changed" file) re-selected well over a hundred
# suites, because many carry function-level patterns like `spira/lib.sh#landed` and
# select.sh's function-diff narrowing needs a real base/head pair it cannot supply from a
# single named file — so every such suite ran unnarrowed. A shared library is the
# highest-fanout file in the tree for exactly this reason, which is why its covering suites
# must run here at all, but the fanout is also why a full re-selection defeats the off
# switch's purpose. Matching the declared glob against the critical list, file-part only,
# keeps the carve-out to suites that named this file — the same suites sp-dgaig should have
# broken certification against.
if [ "${SPIRA_GATE_SUITES:-on}" = off ]; then
    _crit_globs="${SPIRA_CERTIFY_ALWAYS_COVERS:-spira/lib.sh}"
    _crit_files=""
    set -f
    while IFS= read -r _crit_ln || [ -n "$_crit_ln" ]; do
        [ -n "$_crit_ln" ] || continue
        case "$_crit_ln" in *"	"*) _crit_f="${_crit_ln#*	}" ;; *) _crit_f="$_crit_ln" ;; esac
        for _crit_pat in $_crit_globs; do
            case "$_crit_f" in $_crit_pat) _crit_files="$_crit_files $_crit_f"; break ;; esac
        done
    done < <(
        if [ -f "${SPIRA_GATE_FILES:-}" ]; then
            cat "$SPIRA_GATE_FILES"
        else
            git -C "$repo" diff --name-only "${BASE}...${HEAD}" 2>/dev/null || true
        fi
    )
    set +f
    if [ -z "$_crit_files" ]; then
        printf 'gate-touched: SPIRA_GATE_SUITES=off — no suites here; the batch CI run is the suite gate\n' >&2
        exit 0
    fi
    printf 'gate-touched: SPIRA_GATE_SUITES=off but the diff touches%s — running its covering suite(s) anyway\n' \
        "$_crit_files" >&2
    . "$HERE/suite-covers.sh"
    _crit_dir="${SPIRA_BATCH_SUITE_DIR:-$HERE}"
    for _crit_s in "$_crit_dir"/test-*.sh; do
        [ -r "$_crit_s" ] || continue
        _crit_cov="$(suite_covers_of "$_crit_s")"
        [ -n "$_crit_cov" ] || continue
        set -f
        for _crit_tok in $_crit_cov; do
            _crit_tok="${_crit_tok%%\#*}"
            [ -n "$_crit_tok" ] || continue
            for _crit_f in $_crit_files; do
                case "$_crit_f" in
                    $_crit_tok) printf '%s\n' "$(basename "$_crit_s")"; set +f; continue 3 ;;
                esac
            done
        done
        set +f
    done | sort -u
    exit 0
fi

if [ -f "${SPIRA_GATE_FILES:-}" ]; then
    # Build corpus from BASE tree so suites added by the branch are not self-selected
    # through coverage. Falls back to SUITE_DIR when BASE is not a resolvable ref.
    _suite_dir="${SPIRA_BATCH_SUITE_DIR:-$HERE}"
    _tmp_corpus="$(mktemp -d)"
    while IFS= read -r _sp; do
        [ -n "$_sp" ] || continue
        _sn="$(basename "$_sp")"
        [ -f "$_suite_dir/$_sn" ] && ln -s "$_suite_dir/$_sn" "$_tmp_corpus/$_sn" 2>/dev/null || true
    done < <(git -C "$repo" ls-tree -r "$BASE" --name-only 2>/dev/null \
        | grep '^spira/test-[^/]*\.sh$' || true)
    if [ -n "$(ls -A "$_tmp_corpus" 2>/dev/null)" ]; then
        _corpus="$_tmp_corpus"
    else
        rm -rf "$_tmp_corpus"
        _corpus="$_suite_dir"
    fi
    _covered="$(bash "$HERE/select.sh" --files "$SPIRA_GATE_FILES" --suite-dir "$_corpus" \
        --repo "$repo" --no-all-fallback --tiers "$_tiers" 2>/dev/null || true)"
    [ "$_corpus" = "$_tmp_corpus" ] && rm -rf "$_tmp_corpus" 2>/dev/null || true
else
    _covered="$(bash "$HERE/select.sh" --base "$BASE" --head "$HEAD" --repo "$repo" \
        --no-all-fallback --tiers "$_tiers" 2>/dev/null || true)"
fi

_ejected=""
if [ -n "${SPIRA_GATE_EJECTED_SUITES:-}" ]; then
    _ejected="$(printf '%s\n' "$SPIRA_GATE_EJECTED_SUITES" | tr ',' '\n' \
        | while IFS= read -r s; do
            [ -n "$s" ] && [ -f "spira/$s" ] && printf '%s\n' "$s"
          done)"
fi

_all="$(
    { printf '%s\n' "$_covered"; printf '%s\n' "$_ejected"; } \
    | grep -v '^$' | sort -u
)"

_cap="${SPIRA_GATE_SELECT_CAP:-0}"
case "$_cap" in ''|*[!0-9]*) _cap=0 ;; esac
if [ "$_cap" -gt 0 ] && [ -n "$_all" ]; then
    _total="$(printf '%s\n' "$_all" | grep -c . 2>/dev/null || printf '0')"
    if [ "$_total" -gt "$_cap" ]; then
        _ej_norm="$(printf '%s\n' "$_ejected" | grep -v '^$' | sort -u)"
        _ej_n=0; [ -n "$_ej_norm" ] && \
            _ej_n="$(printf '%s\n' "$_ej_norm" | grep -c . 2>/dev/null || printf '0')"
        _slots=$(( _cap - _ej_n ))
        [ "$_slots" -lt 0 ] && _slots=0
        _cv_not_ej="$(printf '%s\n' "$_covered" | grep -v '^$' | sort -u \
            | while IFS= read -r _s; do
                printf '%s\n' "$_ej_norm" | grep -qxF "$_s" || printf '%s\n' "$_s"
              done)"
        if [ "$_slots" -gt 0 ]; then
            _cv_kept="$(printf '%s\n' "$_cv_not_ej" | head -"$_slots")"
            _cv_excl="$(printf '%s\n' "$_cv_not_ej" | tail -n +"$((_slots + 1))")"
        else
            _cv_kept=""
            _cv_excl="$_cv_not_ej"
        fi
        [ -n "$_cv_excl" ] && printf 'gate-touched: cap=%d; excluded: %s\n' "$_cap" \
            "$(printf '%s\n' "$_cv_excl" | grep -v '^$' | tr '\n' ',' | sed 's/,$//')" >&2
        _all="$(
            { printf '%s\n' "$_ej_norm"; printf '%s\n' "$_cv_kept"; } \
            | grep -v '^$' | sort -u
        )"
    fi
fi

# THE BATCH PRE-FLIGHT'S FAST FILTER. Set only by batch.sh's local pre-flight, never by
# certification or CI: it keeps the suites cheap enough to answer inside the pre-flight's
# wall and names every one it drops (fast-suites.sh). Ejected suites are exempt, as with
# the cap: a suite that proved red reruns regardless (law-a-retry-must-change-an-input).
if [ -n "${SPIRA_GATE_FAST_MAX_SECS:-}" ]; then
    _ej_keep="$(printf '%s\n' "${SPIRA_GATE_EJECTED_SUITES:-}" | tr ',' '\n' | grep -v '^$' || true)"
    _all="$(
        { printf '%s\n' "$_all" | grep -v '^$' \
            | bash "$HERE/fast-suites.sh" --max-secs "$SPIRA_GATE_FAST_MAX_SECS"
          printf '%s\n' "$_ej_keep"; } | grep -v '^$' | sort -u
    )"
fi

printf '%s\n' "$_all" | grep -v '^$'
exit 0
