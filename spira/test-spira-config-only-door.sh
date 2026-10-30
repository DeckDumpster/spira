#!/usr/bin/env bash
# tier: T0
# covers: reconciler-flow/src/*.rs queue-watch/src/*.rs spira-lc/src/*.rs desired-state/src/*.rs spira-config/src/*.rs spira-lint/src/*.rs spira-lint/src/rules/config_door.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180
ROOT="$(cd "$HERE/.." && pwd -P)"

echo "test-spira-config-only-door.sh"

# The rule itself — offenders, exemptions, the #[cfg(test)] skip — is spira-lint's `config-door`
# (spira-lint/src/rules/config_door.rs), so `work checks` and this suite cannot disagree.
command -v spira-lint >/dev/null 2>&1 || bail "spira-lint is not on PATH"

find_offenders() { spira-lint --root "$1" --only config-door 2>&1; }


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

git init -q "$SCRATCH" && git -C "$SCRATCH" add -A
offenders="$(find_offenders "$SCRATCH")"
want "planted toml::from_str is flagged" "offender-crate/src/main.rs:3: calls toml::from_str" "$offenders"
want "the finding names the entry point" "spira_config::process::spec()" "$offenders"
want "planted spira.toml path resolution is flagged" "offender-crate/src/main.rs:2: resolves the config path" "$offenders"
nowant "spira-config's own parser is not flagged" "spira-config/src/lib.rs" "$offenders"
nowant "desired-state's store.rs is exempt (its own document kind)" "store.rs" "$offenders"
nowant "the sim's actors.rs is exempt (its own document kind)" "sim/src/actors.rs" "$offenders"
nowant "a whole-file #[cfg(test)] mod is exempt (sentinel's own tests.rs shape)" "gated-crate/src/tests.rs" "$offenders"

echo
echo "2. the real tree names no second parser"

# A fresh repository holding the tree's sources, not $ROOT: in testenv the checkout is a worktree
# whose gitdir the container cannot see, and spira-lint refuses a root it cannot list.
copy_sources() {
    (cd "$1" && find . -name target -prune -o -name '*.rs' -type f -print0 | tar --null -T - -cf -) | tar -xf - -C "$2"
}

PLANTED="$(mktemp -d)"; COPY="$(mktemp -d)"; REAL=
trap 'chmod -R u+rwx "$PLANTED" 2>/dev/null; rm -rf "$SCRATCH" "$PLANTED" "$COPY" "$REAL"' EXIT INT TERM
mkdir -p "$PLANTED/c/src" "$PLANTED/target/unittest/deps/locked"
echo 'fn main() {}' > "$PLANTED/c/src/main.rs"
echo 'fn x() {}' > "$PLANTED/target/unittest/deps/locked/x.rs"
chmod 000 "$PLANTED/target/unittest/deps/locked"
ls "$PLANTED/target/unittest/deps/locked" >/dev/null 2>&1 && bail "the planted directory is readable (running as root?); the control cannot bite"
copy_sources "$PLANTED" "$COPY"; rc=$?
is "an unreadable directory under target/ does not fail the copy" 0 "$rc"
[ -f "$COPY/c/src/main.rs" ] && [ ! -e "$COPY/target" ]; is "the copy holds the sources and no build output" 0 "$?"

REAL="$(mktemp -d)"
git init -q "$REAL"
copy_sources "$ROOT" "$REAL" || bail "could not copy the sources from $ROOT"
[ "$(find "$REAL" -name '*.rs' | wc -l)" -gt 100 ] || bail "copied too few sources from $ROOT"
git -C "$REAL" add -A
real_offenders="$(find_offenders "$REAL")"; rc=$?
is "no crate outside spira-config resolves or parses the config" 0 "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$real_offenders"

tl_summary
