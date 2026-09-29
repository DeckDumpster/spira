#!/usr/bin/env bash
# tier: T0
# covers: reconciler-flow/src/*.rs queue-watch/src/*.rs spira-lc/src/*.rs desired-state/src/*.rs spira-config/src/*.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"

echo "test-spira-config-only-door.sh"

# spira-config is the only door onto spira.toml (sp-cx0mj, epic sp-zs04v): no other crate
# resolves its path, reads it, or parses it. Two `toml::from_str` calls are exempt because
# neither one's document is spira.toml at all: `desired-state/src/store.rs` owns its own
# composite/resource document format, and `reconciler-flow/src/io.rs` reads that same
# desired-state document (a Flow resource), never spira.toml — the bead's own carve-out for
# "its own separate documents are out of scope" applies to both readers of that format.
EXEMPT_TOML_FROM_STR="desired-state/src/store.rs reconciler-flow/src/io.rs"

# The lines of $1 before its first `#[cfg(test)]` module, with comment-only lines dropped —
# production code only, so a doc comment describing the file (or a test fixture that must
# literally write one named spira.toml to exercise discovery/loading) does not itself count
# as "naming the file".
production_region() {
    local file="$1" test_line
    test_line="$(grep -n '^#\[cfg(test)\]' "$file" | head -1 | cut -d: -f1)"
    if [ -n "$test_line" ]; then
        head -n "$((test_line - 1))" "$file"
    else
        cat "$file"
    fi | grep -v '^[[:space:]]*//'
}

find_offenders() {
    local root="$1" f rel exempt
    while IFS= read -r -d '' f; do
        rel="${f#"$root"/}"
        case "$rel" in
            spira-config/src/*) continue ;;
        esac
        exempt=0
        for e in $EXEMPT_TOML_FROM_STR; do
            [ "$rel" = "$e" ] && exempt=1
        done
        if [ "$exempt" -eq 0 ] && production_region "$f" | grep -q 'toml::from_str'; then
            echo "$f: calls toml::from_str outside spira-config"
        fi
        # Flags path-construction of the literal filename (or its env var) — not every
        # mention of the string "spira.toml", which also appears as a plain descriptive
        # label (spira-lc's RepoConfig.source) once discovery itself goes through
        # spira-config.
        if production_region "$f" | grep -qE '\.join\("spira\.toml"\)|Path::new\("spira\.toml"\)|read_to_string\("spira\.toml"\)|File::open\("spira\.toml"\)|env::var(_os)?\("SPIRA_TOML"\)'; then
            echo "$f: resolves spira.toml's path outside spira-config"
        fi
    done < <(find "$root" -type f -name '*.rs' -path '*/src/*' -not -path '*/target/*' -print0)
}

echo
echo "1. POSITIVE CONTROL — a planted second parser is caught"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM
mkdir -p "$SCRATCH/spira-config/src" "$SCRATCH/offender-crate/src" "$SCRATCH/desired-state/src"
cat > "$SCRATCH/spira-config/src/lib.rs" <<'RS'
pub fn validate(_t: &str) {}
RS
cat > "$SCRATCH/offender-crate/src/main.rs" <<'RS'
fn main() {
    let text = std::fs::read_to_string("spira.toml").unwrap();
    let _doc: toml::Value = toml::from_str(&text).unwrap();
}
RS
cat > "$SCRATCH/desired-state/src/store.rs" <<'RS'
fn f() {
    let _doc: Foo = toml::from_str(&text).unwrap();
}
RS

offenders="$(find_offenders "$SCRATCH")"
want "planted toml::from_str is flagged" "offender-crate/src/main.rs: calls toml::from_str" "$offenders"
want "planted spira.toml path resolution is flagged" "offender-crate/src/main.rs: resolves spira.toml's path" "$offenders"
nowant "spira-config's own parser is not flagged" "spira-config/src/lib.rs" "$offenders"
nowant "desired-state's store.rs is exempt (its own document kind)" "store.rs" "$offenders"

echo
echo "2. the real tree names no second parser"

real_offenders="$(find_offenders "$ROOT")"
is "no crate outside spira-config resolves or parses spira.toml" "" "$real_offenders"

tl_summary
