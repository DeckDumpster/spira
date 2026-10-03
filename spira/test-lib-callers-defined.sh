#!/usr/bin/env bash
#
# test-lib-callers-defined.sh — a bash body or seam allowlist that names a lib.sh
# function must name one lib.sh still defines (law-a-rename-repoints-no-reader).
#
# covers: spira/lib.sh aeon/src/seam.rs aeon/src/verdict.rs census/src/real.rs spira/test-lib-callers-defined.sh
# tier: T2
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

defined() { grep -qE "^[[:space:]]*(function[[:space:]]+)?$1[[:space:]]*\(\)" "${@:2}"; }

# allowlist_names <seam.rs> — the names in the seam's `case "$__aeon_fn" in` arm.
allowlist_names() {
    awk '/case "\$__aeon_fn" in/{on=1;next} on&&/\) ;;/{print;exit} on{print}' "$1" \
        | tr '|\\' '\n\n' | sed 's/).*//; s/[[:space:]]//g' | grep -E '^[A-Za-z_][A-Za-z0-9_]*$'
}

# census_bodies <real.rs> — the first word of every bash body census hands to lib.sh.
census_bodies() {
    grep -oE 'self\.seam\("[A-Za-z_][A-Za-z0-9_-]*|lib\.sh\\" >/dev/null 2>&1; [A-Za-z_][A-Za-z0-9_-]*' "$1" \
        | sed -E 's/.*[("; ]//' | sort -u
}

# missing <file> <names...> — prints each name neither lib.sh nor <file> defines.
missing() {
    local file="$1"; shift
    local n
    for n in "$@"; do
        case "$n" in *-*) continue ;; esac
        defined "$n" "$LIB" "$file" || printf '%s\n' "$n"
    done
}
LIB="$HERE/lib.sh"

echo "test-lib-callers-defined.sh"

# positive controls: planted offenders must be seen before silence is believed
cat > "$TMP/lib.sh" <<'L'
alive() { :; }
L
cat > "$TMP/seam.rs" <<'S'
case "$__aeon_fn" in
    alive|\
    gone_fn) ;;
    *) exit 97 ;;
esac
S
cat > "$TMP/real.rs" <<'R'
let s = self.seam("alive 2>/dev/null", &[]);
let t = format!(". \"{}/lib.sh\" >/dev/null 2>&1; vanished \"$1\"", h);
R
planted_a="$(LIB="$TMP/lib.sh" missing "$TMP/seam.rs" $(allowlist_names "$TMP/seam.rs"))"
is "planted allowlist offender is found" "gone_fn" "$planted_a"
planted_c="$(LIB="$TMP/lib.sh" missing "$TMP/real.rs" $(census_bodies "$TMP/real.rs"))"
is "planted census body offender is found" "vanished" "$planted_c"

real_a="$(allowlist_names "$ROOT/aeon/src/seam.rs")"
[ -n "$real_a" ] && ok "aeon seam allowlist was read" || bad "aeon seam allowlist was read" "empty"
is "every aeon seam allowlist name is defined in lib.sh" "" "$(missing "$ROOT/aeon/src/seam.rs" $real_a)"

real_c="$(census_bodies "$ROOT/census/src/real.rs")"
[ -n "$real_c" ] && ok "census bash bodies were read" || bad "census bash bodies were read" "empty"
is "every census bash body names a function lib.sh defines" "" "$(missing "$ROOT/census/src/real.rs" $real_c)"

tl_summary
