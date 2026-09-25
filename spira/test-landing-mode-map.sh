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
#   3. NO HARNESS PROGRAM REBASES THE LANDING WORKTREE, and every fetch in the
#      landing path writes no FETCH_HEAD, and every rebase-failure arm reads the
#      classified kind first. These were static, testdb-free checks embedded in
#      test-landing-race.sh / test-landing-rebase.sh's integration suites; moved
#      here (plan section 3, row 26) since none of them touches a fixture.
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
    sed -n '/^land_repo() {/,/^}/p' "$1"
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
        grep -q '"$mode" = queue' <<<"$syn_body" || {
            printf '  synthetic: mode %s not handled\n' "$m"
            unhandled_in_syn=$(( unhandled_in_syn + 1 ))
        }
        ;;
    push)
        grep -qE '^[[:space:]]*\*\)' <<<"$syn_body" || {
            printf '  synthetic: mode %s (catch-all) not handled\n' "$m"
            unhandled_in_syn=$(( unhandled_in_syn + 1 ))
        }
        ;;
    *)
        grep -qE "^[[:space:]]+${m}\)" <<<"$syn_body" || {
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
if grep -qE '^[[:space:]]+pr\)' <<<"$scope_body"; then
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
            if grep -q '"$mode" = queue' <<<"$real_body"; then
                ok "mode 'queue' handled (early return in land_repo)"
            else
                bad "mode 'queue' handled" "no '\\\$mode = queue' in land_repo"
                unhandled=$(( unhandled + 1 ))
            fi
            ;;
        push)
            if grep -qE '^[[:space:]]+\*\)' <<<"$real_body"; then
                ok "mode 'push' handled (catch-all *) in land_repo)"
            else
                bad "mode 'push' handled" "no catch-all *) case in land_repo"
                unhandled=$(( unhandled + 1 ))
            fi
            ;;
        *)
            if grep -qE "^[[:space:]]+${m}\)" <<<"$real_body"; then
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

# ---------------------------------------------------------------------------
# CHECK 3: no harness program rebases the landing worktree. It is scratch,
# rebuilt from the base on every attempt, so it has no history worth replaying;
# rebasing it produces copies of the branch's commits under SHAs the branch ref
# does not point at. Comments are stripped rather than excluded, because this
# file and landing.sh both explain the rule at length.
# ---------------------------------------------------------------------------
offends() {               # offends <dir> -> "<file>: <hit>" lines, comments stripped
    local d="$1" f hit out=""
    for f in "$d"/*.sh; do
        [ -e "$f" ] || continue
        case "$(basename "$f")" in test-*) continue ;; esac
        # The `cd` arm reaches PAST the connector on purpose: `[^;&|]*` stops dead
        # at the `&&` in `cd "$land" && git rebase`, so a first draft written that
        # way matched nothing and read as a clean tree.
        hit="$(sed 's/#.*//' "$f" | grep -nE 'git +-C +"\$land"[^;&|]*rebase|cd +"\$land".*git +rebase' || true)"
        [ -n "$hit" ] && out="$out$(basename "$f"): $hit
"
    done
    printf '%s' "$out"
}

echo
echo "no harness program rebases the landing worktree:"
_offend_real="$(offends "$HERE")"
[ -z "$_offend_real" ] \
    && ok "no harness program rebases the landing worktree" \
    || bad "no harness program rebases the landing worktree" "$_offend_real"

echo "positive control — the fence's silence has to be earned:"
PLANT_REBASE="$TMP/plant-rebase"; mkdir -p "$PLANT_REBASE"
printf '#!/usr/bin/env bash\ngit -C "$land" rebase -q "$base"\n'    > "$PLANT_REBASE/one.sh"
printf '#!/usr/bin/env bash\ncd "$land" && git rebase -q "$base"\n' > "$PLANT_REBASE/two.sh"
_offend_planted="$(offends "$PLANT_REBASE")"
grep -q "one.sh" <<<"$_offend_planted" \
    && ok "the fence names a git -C \$land rebase" \
    || bad "the fence names a git -C \$land rebase" "$_offend_planted"
grep -q "two.sh" <<<"$_offend_planted" \
    && ok "and a cd \$land followed by git rebase" \
    || bad "and a cd \$land followed by git rebase" "$_offend_planted"

# ---------------------------------------------------------------------------
# CHECK 4: no fetch in the landing path writes FETCH_HEAD. Concurrent landing
# passes share one .git object store, so concurrent git-fetch calls race to
# write .git/FETCH_HEAD; under lock contention a fetch can fail silently and
# leave a stale origin/main, which can land with no new commit on the base.
# --no-write-fetch-head removes the write entirely.
# ---------------------------------------------------------------------------
bare_fetch_in() {         # bare_fetch_in <dir> -> "<file>: <hit>" lines, comments stripped
    local d="$1" f hit out=""
    for f in "$d/landing.sh" "$d/skew.sh"; do
        [ -e "$f" ] || continue
        hit="$(sed 's/#.*//' "$f" | grep -nE '\bgit\b.*\bfetch\b' | grep -vE -- '--no-write-fetch-head' || true)"
        [ -n "$hit" ] && out="$out$(basename "$f"): $hit
"
    done
    printf '%s' "$out"
}

echo
echo "every fetch in the landing path uses --no-write-fetch-head:"
_bare_real="$(bare_fetch_in "$HERE")"
[ -z "$_bare_real" ] \
    && ok "every fetch in the landing path uses --no-write-fetch-head" \
    || bad "every fetch in the landing path uses --no-write-fetch-head" "$_bare_real"

echo "positive control — the fence's silence has to be earned:"
PLANT_FETCH="$TMP/plant-fetch"; mkdir -p "$PLANT_FETCH"
printf '#!/usr/bin/env bash\ngit -C "$repo" fetch -q "$remote" 2>/dev/null\n' \
    > "$PLANT_FETCH/landing.sh"
printf '#!/usr/bin/env bash\ngit -C "$repo" fetch -q "$remote" 2>/dev/null\n' \
    > "$PLANT_FETCH/skew.sh"
_bare_planted="$(bare_fetch_in "$PLANT_FETCH")"
grep -q "landing.sh" <<<"$_bare_planted" \
    && ok "the fence catches a bare fetch in landing.sh" \
    || bad "the fence catches a bare fetch in landing.sh" "$_bare_planted"
grep -q "skew.sh" <<<"$_bare_planted" \
    && ok "and a bare fetch in skew.sh" \
    || bad "and a bare fetch in skew.sh" "$_bare_planted"

# ---------------------------------------------------------------------------
# CHECK 5: every route from a rebase failure to a reopen reads the classified
# kind first. A one-token edit — dropping the REBASE_FAILURE read — is trivial
# to reintroduce and fails silently for hours.
# ---------------------------------------------------------------------------
arms() {
    awk '
        { c = $0; sub(/#.*/, "", c) }
        c ~ /^[ \t]*$/ { next }
        n { if (c ~ /REBASE_FAILURE/) guarded = 1
            if (++k >= 6) { if (!guarded) print "line " n; n = 0 } }
        c ~ /![ \t]*rebase_branch/ { n = NR; k = 0; guarded = 0 }
        END { if (n && !guarded) print "line " n }' "$1"
}

echo
echo "every rebase failure arm in landing.sh reads the kind:"
_arms_real="$(arms "$HERE/landing.sh")"
[ -z "$_arms_real" ] \
    && ok "every rebase failure arm in landing.sh reads the kind" \
    || bad "every rebase failure arm in landing.sh reads the kind" "$_arms_real"

echo "positive control — an unguarded arm is detected:"
printf '%s\n' 'if ! rebase_branch "$br" "$base"; then' '    bead_reopen "$id" "conflicts"' 'fi' \
    > "$TMP/plant-arm.sh"
_arms_planted="$(arms "$TMP/plant-arm.sh")"
grep -q "line 1" <<<"$_arms_planted" \
    && ok "the fence can see an arm that does not read the kind" \
    || bad "the fence can see an arm that does not read the kind" "$_arms_planted"

echo
printf 'test-landing-mode-map.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
