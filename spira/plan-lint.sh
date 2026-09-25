#!/usr/bin/env bash
# plan-lint.sh — every suite declares its tier and UC coverage; every UC id
# it names exists in the typed catalogue; every T0-T3 UC has a covering
# suite; a suite deletion that orphans a UC's last cover is refused.
#
#   plan-lint.sh                    check every suite; exit 1 naming each violation
#   plan-lint.sh --check <file>     check one suite
#   plan-lint.sh --gaps             list T0-T3 UC ids with no covering suite
#   plan-lint.sh --orphans <ref>    fail on a UC whose last cover was deleted since <ref>
#   plan-lint.sh --help             this text
#
# HARD FAILURES (exit 1)
#   - a suite (spira/test-*.sh) with no # tier: or no # covers: line
#   - a UC id token (UC-<area>-NN) on a suite's # covers: line that names no
#     use case in any docs/test-plan/*.toml catalogue
#   - a catalogue file itself: unknown field, bad tier, or an id declared
#     twice across any two catalogues (test-plan validate's own refusal)
#   - --orphans only: a UC whose last covering suite (at <ref>) is gone now,
#     with no [use_case.uncovered] marker and no new cover in this commit
#
# REPORTED, NOT FAILED (--gaps; exits 0 regardless of what it finds)
#   - a UC id declared at tier T0-T3 with no suite naming it on a # covers:
#     line and no [use_case.uncovered] marker
# This becomes a hard failure once the area beads land their pages — see
# docs/test-plan/README.md for the schema and the tier table.
#
# THE CATALOGUE IS TYPED (docs/test-plan/*.toml, test-plan/src/lib.rs), so id
# existence and catalogue well-formedness are answered by the test-plan
# binary (built on demand via cargo — see resolve_test_plan_bin below), not
# by grepping markdown. Per-suite header presence stays a bash string check:
# it is about a suite file, not the catalogue.
#
# covers: spira/suite-covers.sh spira/plan-lint.sh spira/suite-coverage-json.sh spira/plan-bin.sh docs/test-plan/*.toml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"
[ -r "$HERE/suite-covers.sh" ] || { printf 'plan-lint: suite-covers.sh is missing\n' >&2; exit 1; }
. "$HERE/suite-covers.sh"
. "$HERE/plan-bin.sh"
DOCS_DIR="$ROOT/docs/test-plan"

# catalogue_ucs -> "<uc-id> <tier>" one pair per line, from every docs/test-plan/*.toml
# catalogue via `test-plan catalogue-ids`. Returns 1 (having printed the real error) on a
# missing cargo/build failure or a malformed catalogue — never silently empty, which would
# read every suite's UC id as unknown instead of naming the actual defect.
catalogue_ucs() {
    local bin
    bin="$(resolve_test_plan_bin)" || return 1
    "$bin" catalogue-ids --catalogue-dir "$DOCS_DIR"
}

# lint_one <file> <relpath> <catalogue-file> -> 0 clean, 1 violation (prints each to stdout)
lint_one() {
    local f="$1" rel="$2" cat="$3" tier cov uc bad=0
    tier="$(suite_tier_of "$f")"
    cov="$(suite_covers_of "$f")"
    if [ -z "$tier" ]; then
        printf '%s: missing # tier:\n' "$rel"
        bad=1
    fi
    if [ -z "$cov" ]; then
        printf '%s: missing # covers:\n' "$rel"
        bad=1
    fi
    for uc in $(suite_uc_of "$f"); do
        if ! grep -qxF "$uc" <(cut -d' ' -f1 "$cat" 2>/dev/null); then
            printf '%s: unknown UC id on # covers: %s\n' "$rel" "$uc"
            bad=1
        fi
    done
    return "$bad"
}

usage() {
    sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
}

case "${1:-}" in
--help|-h)
    usage
    exit 0
    ;;
--check)
    [ -n "${2:-}" ] || { printf 'plan-lint: --check requires a file\n' >&2; exit 2; }
    _cat="$(mktemp)"; trap 'rm -f "$_cat"' EXIT
    catalogue_ucs > "$_cat" || exit 1
    lint_one "$2" "${2##*/}" "$_cat"
    exit $?
    ;;
--orphans)
    baseref="${2:?usage: plan-lint.sh --orphans <base-ref>}"
    bin="$(resolve_test_plan_bin)" || exit 1
    _cur="$(mktemp)"; _prev="$(mktemp)"; trap 'rm -f "$_cur" "$_prev"' EXIT
    bash "$HERE/suite-coverage-json.sh" > "$_cur"
    bash "$HERE/suite-coverage-json.sh" --ref "$baseref" > "$_prev"
    "$bin" validate --catalogue-dir "$DOCS_DIR" --suites "$_cur" --prev-suites "$_prev"
    exit $?
    ;;
--gaps)
    _cat="$(mktemp)"; trap 'rm -f "$_cat"' EXIT
    catalogue_ucs > "$_cat" || exit 1
    shopt -s nullglob
    suites=("$HERE"/test-*.sh)
    _covered_ucs=""
    for f in "${suites[@]}"; do
        _covered_ucs="$_covered_ucs $(suite_uc_of "$f")"
    done
    _gaps=0
    while read -r _uc _tier; do
        [ -n "$_uc" ] || continue
        case "$_tier" in T0|T1|T2|T3) ;; *) continue ;; esac
        case " $_covered_ucs " in
            *" $_uc "*) ;;
            *) printf 'gap: %s [%s] has no covering suite\n' "$_uc" "$_tier"; _gaps=$((_gaps+1)) ;;
        esac
    done < "$_cat"
    printf 'plan-lint: %d T0-T3 use case(s) with no covering suite\n' "$_gaps"
    exit 0
    ;;
""|--lint)
    _cat="$(mktemp)"; trap 'rm -f "$_cat"' EXIT
    catalogue_ucs > "$_cat" || exit 1
    shopt -s nullglob
    suites=("$HERE"/test-*.sh)
    if [ "${#suites[@]}" -eq 0 ]; then
        printf 'plan-lint: no suites matched %s/test-*.sh — refusing to report clean\n' "$HERE" >&2
        exit 3
    fi
    bad=0
    for f in "${suites[@]}"; do
        rel="${f#"$ROOT"/}"
        lint_one "$f" "$rel" "$_cat" || bad=1
    done
    if [ "$bad" = 0 ]; then
        printf 'plan-lint: clean — all %d suite(s) declare # tier: and # covers:, every UC id known\n' \
            "${#suites[@]}"
        exit 0
    fi
    exit 1
    ;;
*)
    usage >&2
    exit 2
    ;;
esac
