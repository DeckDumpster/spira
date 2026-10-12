#!/usr/bin/env bash
# test-conf-gen.sh — conf-gen.sh turns spira/conf.d/ into conf.sh's two generated fragments.
# Its dependency ordering is checked against a plain-bash reference on the real registry, and
# its refusals (a cycle, a heredoc left open, an empty registry) against fixture registries.
# tier: T1
# covers: spira/conf.d/* spira/conf-gen.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

key_file() {  # key_file <dir> <KEY> <default statement or comment>
    printf 'TYPE=string\nGROUP=t\nDOC=x\nDEFAULT<<'"'"'SPIRA_CONF_DEFAULT_EOF'"'"'\n%s\nSPIRA_CONF_DEFAULT_EOF\n' "$3" > "$1/conf.d/$2"
}

fixture() {  # fixture <name> — a spira/ holding conf-gen.sh and an empty registry
    mkdir -p "$TMP/$1/conf.d"
    cp "$HERE/conf-gen.sh" "$TMP/$1/conf-gen.sh"
}

gen() { bash "$TMP/$1/conf-gen.sh" >"$TMP/$1.out" 2>"$TMP/$1.err"; }

defaulted_order() {  # defaulted_order <generated defaults file> — the keys, in the order it sets them
    sed -n -E 's/^ *: "\$\{([A-Za-z0-9_]+):?=.*/\1/p' "$1"
}

# --- dependency order, on a fixture that can fail it ----------------------------------------
fixture dep
key_file "$TMP/dep" SPIRA_A '    : "${SPIRA_A:=1}"'
key_file "$TMP/dep" SPIRA_A_B '    : "${SPIRA_A_B:=$SPIRA_A/x}"'
key_file "$TMP/dep" SPIRA_C '    : "${SPIRA_C:=${SPIRA_A_B}/y}"'
key_file "$TMP/dep" SPIRA_D '    # NO DEFAULT.'
key_file "$TMP/dep" SPIRA_E '    : "${SPIRA_E=kept}"'
key_file "$TMP/dep" SPIRA_F $'    # names $SPIRA_E in a comment\n    : "${SPIRA_F:=z}"'
gen dep; wantrc "a registry with dependencies generates" 0 "$?"
is "a key's default follows every key its text names, ties alphabetical" "SPIRA_A SPIRA_E SPIRA_A_B SPIRA_F SPIRA_C" "$(defaulted_order "$TMP/dep/conf.d.defaults.generated.sh" | tr '\n' ' ' | sed 's/ $//')"
want "a key with no default statement is still on the allowlist" "SPIRA_D" "$(cat "$TMP/dep/conf.d.keys.generated.sh")"
nowant "and is not given a default" "SPIRA_D:=" "$(cat "$TMP/dep/conf.d.defaults.generated.sh")"
is "the allowlist is one key per line in byte order" "SPIRA_A SPIRA_A_B SPIRA_C SPIRA_D SPIRA_E SPIRA_F" "$(sed -n '/^SPIRA_CONF_KEYS="/,/^"/p' "$TMP/dep/conf.d.keys.generated.sh" | grep -v '"' | tr '\n' ' ' | sed 's/ $//')"

# --- refusals ---------------------------------------------------------------------------------
fixture cycle
key_file "$TMP/cycle" SPIRA_A '    : "${SPIRA_A:=$SPIRA_B}"'
key_file "$TMP/cycle" SPIRA_B '    : "${SPIRA_B:=$SPIRA_A}"'
key_file "$TMP/cycle" SPIRA_Z '    : "${SPIRA_Z:=1}"'
gen cycle; wantrc "a dependency cycle is refused" 1 "$?"
want "the refusal names the keys in the cycle" "SPIRA_A" "$(cat "$TMP/cycle.err")"
want "and the other one" "SPIRA_B" "$(cat "$TMP/cycle.err")"
nowant "and not the key outside it" "SPIRA_Z" "$(cat "$TMP/cycle.err")"
[ ! -e "$TMP/cycle/conf.d.defaults.generated.sh" ] && ok "a refused run leaves no generated fragment" || bad "a refused run leaves no generated fragment" "found one"

fixture open
key_file "$TMP/open" SPIRA_A '    : "${SPIRA_A:=1}"'
printf 'TYPE=string\nDEFAULT<<'"'"'SPIRA_CONF_DEFAULT_EOF'"'"'\n : "${SPIRA_B:=1}"\n' > "$TMP/open/conf.d/SPIRA_B"
gen open; wantrc "a DEFAULT heredoc that never closes is refused" 1 "$?"
want "the refusal names the file" "SPIRA_B" "$(cat "$TMP/open.err")"

fixture none
gen none; wantrc "an empty registry is refused" 1 "$?"

fixture odd
key_file "$TMP/odd" SPIRA_A '    : "${SPIRA_A:=1}"'
echo stray > "$TMP/odd/conf.d/lower_key"
printf 'TYPE=string\nWHAT=this\n' > "$TMP/odd/conf.d/SPIRA_B"
: > "$TMP/odd/conf.d/SPIRA_EMPTY"
gen odd; wantrc "stray and empty registry files do not stop generation" 0 "$?"
want "a lowercase file name is reported" "not an uppercase KEY" "$(cat "$TMP/odd.err")"
want "an unknown line is reported" "unrecognised line" "$(cat "$TMP/odd.err")"
want "an empty key file is on the allowlist" "SPIRA_EMPTY" "$(cat "$TMP/odd/conf.d.keys.generated.sh")"

# --- the real registry, against the bash it replaced ------------------------------------------
mkdir -p "$TMP/real/conf.d"
cp "$HERE"/conf.d/* "$TMP/real/conf.d/"
cp "$HERE/conf-gen.sh" "$TMP/real/conf-gen.sh"
gen real; wantrc "the real registry generates" 0 "$?"

reference_order() {  # the original algorithm: pairwise substring edges, Kahn rounds with a sorted tie-break
    local dir="$1" f k other body deps
    declare -A BODY INDEG DEPS
    local keys=() has=()
    for f in "$dir"/conf.d/*; do
        k="${f##*/}"; keys+=("$k"); body=""
        local in=0 line
        while IFS= read -r line || [ -n "$line" ]; do
            if [ "$in" = 1 ]; then [ "$line" = SPIRA_CONF_DEFAULT_EOF ] && { in=0; continue; }; body="$body$line"$'\n'; continue; fi
            [ "$line" = "DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'" ] && in=1
        done < "$f"
        BODY[$k]="$body"
    done
    for k in "${keys[@]}"; do
        case "${BODY[$k]}" in *': "${'"$k"':='*'}"'*|*': "${'"$k"'='*'}"'*) has+=("$k") ;; esac
    done
    for k in "${has[@]}"; do
        deps=""
        for other in "${has[@]}"; do
            [ "$other" = "$k" ] && continue
            case "${BODY[$k]}" in *"\$$other"*|*'${'"$other"*) deps="$deps $other" ;; esac
        done
        DEPS[$k]="$deps"; INDEG[$k]=$(echo $deps | wc -w)
    done
    local remaining=("${has[@]}") next avail
    while [ "${#remaining[@]}" -gt 0 ]; do
        next=(); avail=()
        for k in "${remaining[@]}"; do if [ "${INDEG[$k]}" -eq 0 ]; then avail+=("$k"); else next+=("$k"); fi; done
        [ "${#avail[@]}" -gt 0 ] || { echo "reference found a cycle" >&2; return 1; }
        while IFS= read -r k; do
            echo "$k"
            for other in "${remaining[@]}"; do case " ${DEPS[$other]} " in *" $k "*) INDEG[$other]=$(( INDEG[$other] - 1 )) ;; esac; done
        done < <(printf '%s\n' "${avail[@]}" | LC_ALL=C sort)
        remaining=("${next[@]}")
    done
}

want_order="$(reference_order "$TMP/real")"
got_order="$(defaulted_order "$TMP/real/conf.d.defaults.generated.sh")"
[ "$(printf '%s\n' "$want_order" | wc -l)" -gt 100 ] && ok "positive control: the reference found a registry with many defaults" || bad "positive control: the reference found a registry with many defaults" "$(printf '%s\n' "$want_order" | wc -l)"
is "the generated default order is the reference's, key for key" "$want_order" "$got_order"
is "every registry file is on the allowlist" "$(ls "$HERE/conf.d" | LC_ALL=C sort)" "$(sed -n '/^SPIRA_CONF_KEYS="/,/^"/p' "$TMP/real/conf.d.keys.generated.sh" | grep -v '"' | grep -v '^#')"

tl_summary
