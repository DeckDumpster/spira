#!/usr/bin/env bash
# target-clean.sh — make a reused cargo target safe for the tree at <root>: the ONE selective
# clean (sp-ke0uk8), called by spira/unit-tests.sh and by a dwarf's build-directory lease
# (spira-config build_dir). The target is ${CARGO_TARGET_DIR:-<root>/target}.
#
#   target-clean.sh <root>            clean what differs from the recorded tree, then record <root>
#   target-clean.sh --record <root>   record <root> without cleaning (a lease's release)
#
# A target is shared by every tree built in it and cargo trusts mtimes across them, so a
# workspace crate whose bytes differ from the tree it was last built from is cleaned;
# dependencies stay warm. ONLY THOSE CRATES (sp-ipwlic, over-broad-scopes #8): the record is a
# manifest of per-file hashes over the workspace crates' own files, the workspace-wide build
# inputs, and every file outside a crate that a crate's dep-info names (include_str! targets),
# so an amend that edits a doc cleans nothing and one that edits a crate cleans that crate
# (cargo rebuilds its dependents from the changed fingerprint). A changed build input, a missing
# record, or a tree that cannot be hashed cleans the whole workspace. A tree with no Cargo.toml
# is no cargo workspace: nothing to clean or record.
set -uo pipefail
record_only=""
if [ "${1:-}" = "--record" ]; then record_only=1; shift; fi
ROOT="${1:?usage: target-clean.sh [--record] <root>}"
cd "$ROOT" || exit 2
ROOT="$(pwd -P)"
[ -f Cargo.toml ] || exit 0
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
BUILT="$TARGET/.unit-tests-built-manifest"
DEPS_DIR="$TARGET/debug/deps"
rm -f "$TARGET/.unit-tests-built-tree"
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
mkdir -p "$TARGET" || exit 2
if [ -n "$record_only" ]; then
  # An unhashable tree leaves no record, so the next lease cleans the whole workspace.
  if [ -n "$manifest" ]; then printf '%s\n' "$manifest" > "$BUILT" || exit 2; else rm -f "$BUILT" || exit 2; fi
  exit 0
fi
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
