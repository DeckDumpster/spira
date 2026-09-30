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
# `spira-lint/src/rules/deps_lint.rs` (sp-l8gl3) parses spira/deps.toml, spira-lint's own
# manifest of declared programs — a separate document, never spira.toml.
# `spira-lint/src/rules/lockfile_lint.rs` (sp-ufbkh) parses a tree's Cargo.lock to compare
# locked package versions against the base — a separate document (Cargo's own lockfile
# format), never spira.toml, joining deps_lint.rs's carve-out for "its own document kind".
# `testenv/src/container.rs` (sp-ehj2t) parses the same spira/deps.toml manifest as deps_lint.rs,
# to hash only the tiers the test image installs or verifies into the image tag — never spira.toml.
EXEMPT_TOML_FROM_STR="desired-state/src/store.rs reconciler-flow/src/io.rs spira-lint/src/rules/deps_lint.rs spira-lint/src/rules/lockfile_lint.rs testenv/src/container.rs"

# The lines of $1 before its first `#[cfg(test)]` module, with comment-only lines dropped —
# production code only, so a doc comment describing the file (or a test fixture that must
# literally write one named spira.toml to exercise discovery/loading) does not itself count
# as "naming the file".
#
# `grep -a` throughout: a source file holding a NUL byte (landing-pass/src/tests.rs does)
# otherwise reads as binary, and grep prints "binary file matches" in place of its lines —
# the file's production code would silently go unscanned. The NULs themselves are dropped,
# since the region is held in a shell variable, which cannot carry one.
production_region() {
    local file="$1" test_line
    test_line="$(grep -an '^#\[cfg(test)\]' "$file" | head -1 | cut -d: -f1)"
    if [ -n "$test_line" ]; then
        head -n "$((test_line - 1))" "$file"
    else
        cat "$file"
    fi | grep -av '^[[:space:]]*//' | tr -d '\000'
}

find_offenders() {
    local root="$1" f rel exempt region
    while IFS= read -r -d '' f; do
        rel="${f#"$root"/}"
        case "$rel" in
            spira-config/src/*) continue ;;
        esac
        exempt=0
        for e in $EXEMPT_TOML_FROM_STR; do
            [ "$rel" = "$e" ] && exempt=1
        done
        # CAPTURED, NEVER PIPED INTO `grep -q`. Under pipefail, grep -q exits at the first
        # match, the writer upstream dies of SIGPIPE (141), and the pipeline reads as NO
        # match — so an offender was reported or not by scheduling luck, and this suite went
        # red and green on the same tree.
        region="$(production_region "$f")"
        if [ "$exempt" -eq 0 ] && grep -aq 'toml::from_str' <<<"$region"; then
            echo "$f: calls toml::from_str outside spira-config"
        fi
        # Flags path-construction of the literal filename (or its env var) — not every
        # mention of the string "spira.toml", which also appears as a plain descriptive
        # label (spira-lc's RepoConfig.source) once discovery itself goes through
        # spira-config.
        if grep -aqE '\.join\("spira\.toml"\)|Path::new\("spira\.toml"\)|read_to_string\("spira\.toml"\)|File::open\("spira\.toml"\)|env::var(_os)?\("SPIRA_TOML"\)' <<<"$region"; then
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
