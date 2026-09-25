#!/usr/bin/env bash
#
# test-landing-mode-map.sh — landing.sh header table matches land_repo's code,
# and land_mark/land_state/land_mark_at live only in lib.sh.
#
# TWO CHECKS:
#
#   1. SINGLE IMPLEMENTATION. No harness file other than lib.sh defines land_mark,
#      land_state, or land_mark_at. Positive control: a synthetic file with a local
#      land_mark definition is detected before the real scan clears it.
#
#   2. MODE COVERAGE. Every mode named on the "land-modes:" line of landing.sh's
#      header is handled in land_repo. Positive control: a synthetic landing.sh
#      that lists a mode not present in land_repo's case is detected as an offender.
#
# covers: spira/*.sh
# tier: T0
# timeout: 30
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }

echo "test-landing-mode-map.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# CHECK 1: land_mark, land_state, land_mark_at are defined only in lib.sh.
# Any other *.sh in spira/ that defines one is an offender.
# ---------------------------------------------------------------------------
echo
echo "positive control — synthetic file with local land_mark is detected:"

SYN_LM="$TMP/syn-land-mark.sh"
printf 'land_mark() { printf %%s "\\$1"; }\n' > "$SYN_LM"

hit="$(grep -E '^land_mark(_at)?\(\)|^land_state\(\)' "$SYN_LM" 2>/dev/null || true)"
[ -n "$hit" ] && ok "scanner detects: local land_mark definition in synthetic file" \
               || bad "scanner detects: local land_mark definition in synthetic file" "no match"

echo
echo "no harness file other than lib.sh defines land_mark, land_state, or land_mark_at:"

IMPL_RE='^(land_mark(_at)?|land_state)\(\)'
impl_offenders=0
while IFS= read -r f; do
    [ "$(basename "$f")" = lib.sh ] && continue
    hits="$(grep -En "$IMPL_RE" "$f" 2>/dev/null || true)"
    if [ -n "$hits" ]; then
        printf '  OFFENDER %s:\n' "$(basename "$f")"
        printf '%s\n' "$hits" | sed 's/^/    /'
        impl_offenders=$(( impl_offenders + 1 ))
    fi
done < <(find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-landing-mode-map.sh' | sort)
[ "$impl_offenders" -eq 0 ] \
    && ok "no file other than lib.sh defines land_mark / land_state / land_mark_at" \
    || bad "no file other than lib.sh defines land_mark / land_state / land_mark_at" \
           "$impl_offenders offender(s) found (see above)"

# ---------------------------------------------------------------------------
# CHECK 2: every mode on the "land-modes:" line is handled INSIDE land_repo.
#
# queue mode is handled by the `if [ "$mode" = queue ]` early return (not the
# case statement), so we check for its literal string separately.
# push is the `*)` default case. hold (and any other named mode) is an
# explicit case arm.
#
# THE CHECK IS SCOPED TO land_repo'S OWN BODY, not the whole file: a `*)` or
# `hold)` arm belonging to some unrelated case statement elsewhere in this
# 2,000+ line file must not satisfy "mode is handled". land_repo_body()
# extracts the function's text (from its definition to its closing brace at
# column 0) and every mode pattern is grepped against that extract only.
# ---------------------------------------------------------------------------
land_repo_body() {
    awk '/^land_repo\(\) \{/{f=1} f{print} f && /^}/{exit}' "$1"
}

echo
echo "positive control — synthetic landing.sh with unhandled mode is detected:"

SYN_LAND="$TMP/syn-landing.sh"
cat > "$SYN_LAND" <<'SH'
# land-modes: push pr hold phantom
land_repo() {
    if [ "$mode" = queue ]; then :; fi
    case "$mode" in
    pr)   ;;
    hold) ;;
    *)    ;;
    esac
}
SH

syn_body="$(land_repo_body "$SYN_LAND")"
modes_line="$(grep '^# land-modes:' "$SYN_LAND" 2>/dev/null | head -1)"
modes_from_table="${modes_line#\# land-modes:}"
unhandled_in_syn=0
for m in $modes_from_table; do
    case "$m" in
    queue)
        printf '%s' "$syn_body" | grep -q '"$mode" = queue' || {
            printf '  synthetic: mode %s not handled\n' "$m"
            unhandled_in_syn=$(( unhandled_in_syn + 1 ))
        }
        ;;
    push)
        printf '%s' "$syn_body" | grep -qE '^\s*\*\)' || {
            printf '  synthetic: mode %s (catch-all) not handled\n' "$m"
            unhandled_in_syn=$(( unhandled_in_syn + 1 ))
        }
        ;;
    *)
        printf '%s' "$syn_body" | grep -qE "^\s+${m}\)" || {
            printf '  synthetic: mode %s not handled\n' "$m"
            unhandled_in_syn=$(( unhandled_in_syn + 1 ))
        }
        ;;
    esac
done
[ "$unhandled_in_syn" -gt 0 ] \
    && ok "scanner detects: unhandled mode 'phantom' in synthetic landing.sh" \
    || bad "scanner detects: unhandled mode 'phantom' in synthetic landing.sh" \
           "no unhandled mode found (positive control failed)"

echo
echo "positive control — an arm OUTSIDE land_repo does not count as handled:"

# The scoping bug this check guards against: a `pr)` arm sitting in some
# unrelated function must not satisfy "pr is handled in land_repo". Give the
# synthetic file a land_repo with no pr arm, and a pr) arm in a sibling
# function only.
SYN_SCOPE="$TMP/syn-scope.sh"
cat > "$SYN_SCOPE" <<'SH'
# land-modes: push pr
land_repo() {
    case "$mode" in
    *)    ;;
    esac
}
other_func() {
    case "$mode" in
    pr)   ;;
    esac
}
SH

scope_body="$(land_repo_body "$SYN_SCOPE")"
if printf '%s' "$scope_body" | grep -qE '^\s+pr\)'; then
    bad "scoped check ignores an arm outside land_repo" \
        "pr) matched inside land_repo's own extracted body"
else
    ok "scoped check ignores an arm outside land_repo — pr) in other_func() does not count"
fi

echo
echo "every mode on land-modes: line is handled in land_repo:"

real_modes_line="$(grep '^# land-modes:' "$HERE/landing.sh" 2>/dev/null | head -1)"
if [ -z "${real_modes_line:-}" ]; then
    bad "land-modes: marker present in landing.sh" "line not found"
else
    ok "land-modes: marker present in landing.sh"
    real_modes="${real_modes_line#\# land-modes:}"
    real_body="$(land_repo_body "$HERE/landing.sh")"
    [ -n "$real_body" ] || bad "land_repo_body extracted non-empty text" "extraction returned nothing"
    unhandled=0
    for m in $real_modes; do
        case "$m" in
        queue)
            if printf '%s' "$real_body" | grep -q '"$mode" = queue'; then
                ok "mode 'queue' handled (early return in land_repo)"
            else
                bad "mode 'queue' handled" "no '\"$mode\" = queue' in land_repo"
                unhandled=$(( unhandled + 1 ))
            fi
            ;;
        push)
            if printf '%s' "$real_body" | grep -qE '^\s+\*\)'; then
                ok "mode 'push' handled (catch-all *) in land_repo)"
            else
                bad "mode 'push' handled" "no catch-all *) case in land_repo"
                unhandled=$(( unhandled + 1 ))
            fi
            ;;
        *)
            if printf '%s' "$real_body" | grep -qE "^\s+${m}\)"; then
                ok "mode '$m' handled (${m}) case in land_repo)"
            else
                bad "mode '$m' handled" "no ${m}) case in land_repo"
                unhandled=$(( unhandled + 1 ))
            fi
            ;;
        esac
    done
    [ "$unhandled" -eq 0 ] \
        && ok "all modes on land-modes: line are handled in land_repo" \
        || bad "all modes on land-modes: line are handled in land_repo" \
               "$unhandled unhandled mode(s)"
fi

echo
printf 'test-landing-mode-map.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
