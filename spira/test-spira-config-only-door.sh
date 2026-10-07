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
# `spira-lint/src/rules/lockfile_lint.rs` (sp-ufbkh) parses a tree's Cargo.lock to compare
# locked package versions against the base — a separate document (Cargo's own lockfile
# format), never spira.toml.
# `sim/src/actors.rs` and `sim/src/fit.rs` parse the simulator's own actors.toml and
# durations.toml, never spira.toml.
# spira/deps.toml is read only through `spira_config::deps`; no other crate parses it.
EXEMPT_TOML_FROM_STR="desired-state/src/store.rs reconciler-flow/src/io.rs spira-lint/src/rules/lockfile_lint.rs sim/src/actors.rs sim/src/fit.rs"

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

# A file handed over to tests WHOLESALE, via `#[cfg(test)] mod name;` naming it from a
# SIBLING file, is never production code at all — even though nothing inside IT reads
# `#[cfg(test)]`, because that gate lives in the file that imports it, not in this one.
# sentinel/src/tests.rs (declared `#[cfg(test)] mod tests;` from sentinel/src/main.rs) is
# exactly this shape: a contract-test module that pins SPIRA_TOML for its own save/restore
# fixture, not a second parser. `production_region`'s single-file heuristic missed this
# because the `#[cfg(test)]` line it looks for lives in the OTHER file; without this pass
# it treats the whole module as production and flags the fixture's env::var call.
# Populates the (caller-local) TEST_MODULE_SKIP associative array with every file reached
# this way, resolved per Rust's own module-path rule: a declaring file that is the crate
# root (`main.rs`/`lib.rs`) or a directory's `mod.rs` names siblings directly
# (`dir/name.rs` or `dir/name/mod.rs`); any other declaring file `foo.rs` names them under
# its own module directory (`dir/foo/name.rs` or `dir/foo/name/mod.rs`).
build_test_module_skip_set() {
    local root="$1" f dir base stem content prev line modname t1 t2
    while IFS= read -r -d '' f; do
        dir="$(dirname "$f")"
        base="$(basename "$f")"
        case "$base" in
            main.rs|lib.rs|mod.rs) stem="" ;;
            *) stem="${base%.rs}" ;;
        esac
        content="$(tr -d '\000' < "$f")"
        prev=""
        while IFS= read -r line; do
            if [ "$prev" = '#[cfg(test)]' ]; then
                modname=""
                case "$line" in
                    mod\ *\;) modname="${line#mod }"; modname="${modname%;}" ;;
                    pub\ mod\ *\;) modname="${line#pub mod }"; modname="${modname%;}" ;;
                esac
                if [ -n "$modname" ]; then
                    if [ -n "$stem" ]; then
                        t1="$dir/$stem/$modname.rs"; t2="$dir/$stem/$modname/mod.rs"
                    else
                        t1="$dir/$modname.rs"; t2="$dir/$modname/mod.rs"
                    fi
                    [ -f "$t1" ] && TEST_MODULE_SKIP["$t1"]=1
                    [ -f "$t2" ] && TEST_MODULE_SKIP["$t2"]=1
                fi
            fi
            prev="$line"
        done <<<"$content"
    done < <(find "$root" -type f -name '*.rs' -not -path '*/target/*' -print0)
}

find_offenders() {
    local root="$1" f rel exempt region
    declare -A TEST_MODULE_SKIP=()
    build_test_module_skip_set "$root"
    while IFS= read -r -d '' f; do
        rel="${f#"$root"/}"
        case "$rel" in
            spira-config/src/*) continue ;;
        esac
        [ -n "${TEST_MODULE_SKIP[$f]:-}" ] && continue
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
mkdir -p "$SCRATCH/sim/src"
cat > "$SCRATCH/sim/src/actors.rs" <<'RS'
fn f() {
    let _doc: Foo = toml::from_str(&text).unwrap();
}
RS
mkdir -p "$SCRATCH/gated-crate/src"
cat > "$SCRATCH/gated-crate/src/main.rs" <<'RS'
#[cfg(test)]
mod tests;

fn main() {}
RS
cat > "$SCRATCH/gated-crate/src/tests.rs" <<'RS'
#[test]
fn t() {
    let saved = std::env::var("SPIRA_TOML").ok();
    std::env::set_var("SPIRA_TOML", "/tmp/no-such-spira.toml");
    let _ = saved;
}
RS

offenders="$(find_offenders "$SCRATCH")"
want "planted toml::from_str is flagged" "offender-crate/src/main.rs: calls toml::from_str" "$offenders"
want "planted spira.toml path resolution is flagged" "offender-crate/src/main.rs: resolves spira.toml's path" "$offenders"
nowant "spira-config's own parser is not flagged" "spira-config/src/lib.rs" "$offenders"
nowant "desired-state's store.rs is exempt (its own document kind)" "store.rs" "$offenders"
nowant "the sim's actors.rs is exempt (its own document kind)" "sim/src/actors.rs" "$offenders"
nowant "a whole-file #[cfg(test)] mod is exempt (sentinel's own tests.rs shape)" "gated-crate/src/tests.rs" "$offenders"

echo
echo "2. the real tree names no second parser"

real_offenders="$(find_offenders "$ROOT")"
is "no crate outside spira-config resolves or parses spira.toml" "" "$real_offenders"

tl_summary
