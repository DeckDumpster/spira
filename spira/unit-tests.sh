#!/usr/bin/env bash
# unit-tests.sh — the Rust unit tests of the crates a tip's diff affects, in the fixture
# container. Not a corpus suite: it is run only as `testenv --suites unit-tests <tip> <repo>`,
# which leaves the crate list in target/unit-tests.crates and, under --tests, the test-name
# filters in target/unit-tests.filter.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(dirname "$HERE")"
LIST="$ROOT/target/unit-tests.crates"
[ -f "$LIST" ] || { echo "unit-tests: $LIST missing — run through testenv --suites unit-tests" >&2; exit 2; }
words=()
while IFS= read -r c; do [ -n "$c" ] && words+=(-p "$c"); done < "$LIST"
[ "${#words[@]}" -gt 0 ] || { echo "unit-tests: the diff touches no crate — nothing to run"; exit 0; }
# sp-mgmjhy: a build and its tests share the host with other builders, so neither
# claims every core. nproc inside a container reports the whole host.
cores="$(nproc 2>/dev/null || echo 2)"
jobs="${CARGO_BUILD_JOBS:-6}"
[ "$jobs" -le "$cores" ] 2>/dev/null || jobs="$cores"
threads="${RUST_TEST_THREADS:-4}"
# Test temp files go to the container's tmpfs /tmp, never the checkout's disk.
export TMPDIR=/tmp
cd "$ROOT" || exit 2
# The target is shared by every tree at this path and cargo trusts mtimes across them, so a
# workspace crate whose bytes differ from the tree it was last built from is cleaned;
# dependencies stay warm. ONLY THOSE CRATES (sp-ipwlic, over-broad-scopes #8): the record is a
# manifest of per-file hashes over the workspace crates' own files, the workspace-wide build
# inputs, and every file outside a crate that a crate's dep-info names (include_str! targets),
# so an amend that edits a doc cleans nothing and one that edits a crate cleans that crate
# (cargo rebuilds its dependents from the changed fingerprint). A changed build input, a missing
# record, or a tree that cannot be hashed cleans the whole workspace, as before.
BUILT="${CARGO_TARGET_DIR:-$ROOT/target}/.unit-tests-built-manifest"
DEPS_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/debug/deps"
rm -f "${CARGO_TARGET_DIR:-$ROOT/target}/.unit-tests-built-tree"
roots=(Cargo.toml Cargo.lock rust-toolchain.toml rust-toolchain .cargo/config.toml .cargo/config)
members=()
while IFS= read -r m; do [ -n "$m" ] && [ -f "$m/Cargo.toml" ] && members+=("$m"); done < <(
  awk '/^members[[:space:]]*=/{on=1} on{line=$0; sub(/#.*/,"",line); while (match(line, /"[^"]*"/)) { print substr(line, RSTART+1, RLENGTH-2); line=substr(line, RSTART+RLENGTH) } if ($0 ~ /\]/) exit}' Cargo.toml)
# The files outside every member that some crate's last build read.
extras() {
  [ -d "$DEPS_DIR" ] || return 0
  find "$DEPS_DIR" -maxdepth 1 -name '*.d' -print0 2>/dev/null | xargs -0r cat 2>/dev/null \
    | tr ' ' '\n' | sed -n "s|^$ROOT/||p" | sed 's/:$//' | LC_ALL=C sort -u | while IFS= read -r f; do
      inside=0
      for m in "${members[@]}"; do case "$f" in "$m"/*) inside=1; break ;; esac; done
      [ "$inside" = 1 ] || [ ! -f "$f" ] || printf '%s\0' "$f"
    done
}
manifest="$({ for r in "${roots[@]}"; do [ -f "$r" ] && printf '%s\0' "$r"; done
              [ "${#members[@]}" -gt 0 ] && find "${members[@]}" -path '*/target' -prune -o -type f -print0
              extras; } | LC_ALL=C sort -zu | xargs -0r sha256sum)" || manifest=""
[ "${#members[@]}" -gt 0 ] || manifest=""
clean_all() {
  rm -f "$BUILT" || exit 2
  cargo clean --workspace -q || { echo "unit-tests: cargo clean --workspace failed" >&2; exit 2; }
}
if [ -z "$manifest" ] || [ ! -f "$BUILT" ]; then
  echo "unit-tests: no record of the tree this target was built from — cleaning the workspace crates"
  clean_all
elif [ "$(cat "$BUILT")" != "$manifest" ]; then
  changed="$(diff <(cat "$BUILT") <(printf '%s\n' "$manifest") | sed -n 's/^[<>] [0-9a-f]\{64\}  //p' | LC_ALL=C sort -u)"
  root_changed=""
  for r in "${roots[@]}"; do grep -qxF "$r" <<< "$changed" && root_changed="$r"; done
  if [ -n "$root_changed" ]; then
    echo "unit-tests: $root_changed changed — cleaning the workspace crates"
    clean_all
  else
    to_clean=()
    for m in "${members[@]}"; do
      name="$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^name[[:space:]]*=/{gsub(/.*=[[:space:]]*"|".*/,""); print; exit}' "$m/Cargo.toml")"
      [ -n "$name" ] || continue
      hit=""
      grep -q "^$m/" <<< "$changed" && hit=1
      if [ -z "$hit" ] && [ -d "$DEPS_DIR" ]; then
        under="${name//-/_}"
        while IFS= read -r f; do
          case "$f" in "$m"/*) continue ;; esac
          if cat "$DEPS_DIR/$under"-*.d 2>/dev/null | tr ' ' '\n' | grep -qxF "$ROOT/$f:" ||
             cat "$DEPS_DIR/$under"-*.d 2>/dev/null | tr ' ' '\n' | grep -qxF "$ROOT/$f"; then hit=1; break; fi
        done <<< "$changed"
      fi
      [ -n "$hit" ] && to_clean+=(-p "$name")
    done
    if [ "${#to_clean[@]}" -gt 0 ]; then
      echo "unit-tests: cleaning the workspace crates whose bytes changed:${to_clean[*]//-p/}"
      rm -f "$BUILT" || exit 2
      cargo clean -q "${to_clean[@]}" || { echo "unit-tests: cargo clean ${to_clean[*]} failed" >&2; exit 2; }
    else
      echo "unit-tests: no workspace crate's bytes changed — nothing cleaned"
    fi
  fi
fi
[ -z "$manifest" ] || printf '%s\n' "$manifest" > "$BUILT" || exit 2
BINS="$ROOT/target/unit-tests.bins"
[ -f "$BINS" ] || { echo "unit-tests: $BINS missing — run through testenv --suites unit-tests" >&2; exit 2; }
bins=()
while IFS= read -r c; do [ -n "$c" ] && bins+=(-p "$c"); done < "$BINS"
if [ "${#bins[@]}" -gt 0 ]; then
  cargo build "${bins[@]}" --bins -j "$jobs" || exit 1
fi
filters=()
if [ -f "$ROOT/target/unit-tests.filter" ]; then
  while IFS= read -r t; do [ -n "$t" ] && filters+=("$t"); done < "$ROOT/target/unit-tests.filter"
  echo "unit-tests: filtered to tests named ${filters[*]}"
fi
# The round runs this tier with no declared release, config or harness home and no release
# on PATH (round-vm's unit_cargo: env -i); a test that passes only because the fixture's
# launcher declared them must fail here too.
unit_path=""
IFS=: read -ra _dirs <<< "$PATH"
for d in "${_dirs[@]}"; do
  [ -x "$d/spira-config" ] || [ -f "$d/conf.sh" ] || unit_path="${unit_path:+$unit_path:}$d"
done
env -u SPIRA_RELEASE -u SPIRA_TOML -u SPIRA_HOME PATH="$unit_path" \
    cargo test "${words[@]}" -j "$jobs" -- --test-threads="$threads" "${filters[@]}"
