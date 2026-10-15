#!/usr/bin/env bash
# test-landing-parity.sh — every landing-path mechanism has a cell for every land mode in
# docs/landing-parity.md, and each cell is a real test, a reasoned n/a, or a gap with its bead.
#
# tier: T0
# covers: docs/landing-parity.md spira/test-landing-parity.sh queue/src/** batcher-cut/src/** landing-pass/src/** round-vm/src/** spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"

echo "test-landing-parity.sh"

trim() { local v="$1"; v="${v#"${v%%[![:space:]]*}"}"; printf '%s' "${v%"${v##*[![:space:]]}"}"; }

# check_table <table.md> <root> -> prints one line per defective cell; rc 1 when any
check_table() {
    local table="$1" root="$2" line n=0 bad=0 header=1 cell kind rest f name mech
    local -a cols cells
    while IFS= read -r line; do
        case "$line" in '|'*) ;; *) continue ;; esac
        case "$line" in '|---'*) continue ;; esac
        IFS='|' read -ra cells <<<"${line#|}"
        if [ "$header" = 1 ]; then cols=("${cells[@]}"); header=0; continue; fi
        mech="$(trim "${cells[0]}")"
        for ((i = 1; i < ${#cols[@]}; i++)); do
            cell="$(trim "${cells[$i]:-}")"
            col="$(trim "${cols[$i]}")"
            kind="${cell%%:*}"; rest="${cell#*:}"
            case "$cell" in
                '') echo "EMPTY: $mech × $col"; bad=1 ;;
                test:*)
                    f="${rest%%::*}"; name="${rest#*::}"
                    if [ ! -f "$root/$f" ] || ! grep -q -- "fn $name\b" "$root/$f"; then
                        echo "NO TEST: $mech × $col -> $rest"; bad=1
                    fi ;;
                n/a:*) [ -n "$(trim "$rest")" ] || { echo "NO REASON: $mech × $col"; bad=1; } ;;
                gap:*) case "$rest" in sp-*) [ -z "${PARITY_STRICT:-}" ] || { echo "GAP: $mech × $col -> $rest"; bad=1; } ;;
                           *) echo "GAP WITHOUT BEAD: $mech × $col"; bad=1 ;; esac ;;
                gap) echo "GAP WITHOUT BEAD: $mech × $col"; bad=1 ;;
                *) echo "UNKNOWN CELL: $mech × $col -> $cell"; bad=1 ;;
            esac
            n=$((n + 1))
        done
    done <"$table"
    [ "$n" -gt 0 ] || { echo "NO CELLS"; bad=1; }
    return $bad
}

TABLE="$ROOT/docs/landing-parity.md"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo
echo "positive controls — the checker can go red:"
cat >"$TMP/bad.md" <<'T'
| mechanism | queue.local | queue.forge |
|---|---|---|
| m1 | test:docs/landing-parity.md::nothing | |
| m2 | n/a: | gap |
| m3 | gap:sp-x | whatever |
T
out="$(check_table "$TMP/bad.md" "$ROOT")"; rc=$?
wantrc "a table with defective cells is refused" 1 "$rc"
want "an empty cell is named" "EMPTY: m1 × queue.forge" "$out"
want "a test that names nothing real is named" "NO TEST: m1 × queue.local" "$out"
want "an n/a with no reason is named" "NO REASON: m2 × queue.local" "$out"
want "a bare gap is named" "GAP WITHOUT BEAD: m2 × queue.forge" "$out"
want "an unknown cell is named" "UNKNOWN CELL: m3 × queue.forge" "$out"
nowant "a gap with a bead passes the default check" "m3 × queue.local" "$out"
strict="$(PARITY_STRICT=1 check_table "$TMP/bad.md" "$ROOT")"
want "strict mode names a gap that has a bead" "GAP: m3 × queue.local -> sp-x" "$strict"

echo
echo "the checked-in parity table:"
out="$(check_table "$TABLE" "$ROOT")"; rc=$?
wantrc "every cell is a real test, a reasoned n/a, or a gap with its bead" 0 "$rc"
[ -z "$out" ] || echo "$out"
for m in "sift screening" "known-red tip refusal" "provisional-member gate-red eject" \
         "eject names its evidence" "mender summons on a red suite" "pass telemetry" \
         "stale-red / back-pressure" "freeze / maintenance-window" "landing verification"; do
    want "row present: $m" "$m" "$(cat "$TABLE")"
done
for c in queue.local queue.forge pr push; do
    want "column present: $c" "| $c" "$(sed -n '/^| mechanism/p' "$TABLE")"
done
echo "gaps open: $(grep -o 'gap:sp-[A-Za-z0-9.]*' "$TABLE" | sort -u | tr '\n' ' ')"

tl_summary
