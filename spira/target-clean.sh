#!/usr/bin/env bash
# target-clean.sh — make a reused cargo target safe for the tree at <root>: the ONE selective
# clean (sp-ke0uk8), called by spira/unit-tests.sh and by a dwarf's build-directory lease
# (spira-config build_dir). The target is ${CARGO_TARGET_DIR:-<root>/target}.
#
#   target-clean.sh <root>            clean what differs from the recorded tree, then record <root>
#   target-clean.sh --release <root>  a lease's release: for what differs from the record or was
#                                     modified since it was written, keep each unit built after
#                                     the edit, clean the rest, then record <root>
#   target-clean.sh --stamp <root>    mark the target's units as built from the recorded tree:
#                                     hashes nothing, cleans nothing
#
# A target is shared by every tree built in it and cargo trusts mtimes across them, so a
# workspace crate whose bytes differ from the tree it was last built from is cleaned;
# dependencies stay warm. ONLY THOSE CRATES (sp-ipwlic, over-broad-scopes #8): the record is a
# manifest of per-file hashes over the workspace crates' own files, the workspace-wide build
# inputs, and every file outside a crate that a crate's dep-info names (include_str! targets),
# so an amend that edits a doc cleans nothing and one that edits a crate cleans that crate
# (cargo rebuilds its dependents from the changed fingerprint). A changed build input, a missing
# record, or a tree that cannot be hashed cleans the whole workspace. Cargo.lock is the exception
# outside a release: a tree whose lockfile alone differs cleans only its changed crates. A tree with no Cargo.toml
# is no cargo workspace: nothing to clean or record.
#
# Every run but a stamp writes .target-clean-report.json in the target at exit: what it removed.
#
# The record cannot name a build made without this script, and cargo can call such a unit fresh
# for a different tree. So every run but a release first drops each workspace unit built after
# the stamp. A release leaves them: they are the lease holder's, judged by edit time.
set -uo pipefail
release=""
stamp_only=""
case "${1:-}" in
  --release) release=1; shift ;;
  --stamp) stamp_only=1; shift ;;
esac
ROOT="${1:?usage: target-clean.sh [--release|--stamp] <root>}"
cd "$ROOT" || exit 2
ROOT="$(pwd -P)"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
REPORT="$TARGET/.target-clean-report.json"
# An earlier run's report must not be taken for this one's.
[ -n "$stamp_only" ] || rm -f "$REPORT"
acts=()
# The arguments as a JSON array of strings; they are package names and build-input paths, which need no escaping.
json_list() {
  local sep="" s
  printf '['
  for s in "$@"; do printf '%s"%s"' "$sep" "$s"; sep=,; done
  printf ']'
}
# add_act <branch> <cause> <inputs json> <crates json> <bytes|null> <files|null>
add_act() {
  acts+=("{\"branch\":\"$1\",\"cause\":\"$2\",\"inputs\":$3,\"crates\":$4,\"bytes\":$5,\"files\":$6}")
}
write_report() {
  local rc=$? IFS=,
  [ -z "$stamp_only" ] && [ -d "$TARGET" ] || return 0
  printf '{"at":"%s","rc":%d,"acts":[%s]}\n' "$(date -u '+%Y-%m-%dT%H:%M:%S.%NZ')" "$rc" "${acts[*]}" > "$REPORT"
}
trap write_report EXIT
[ -f Cargo.toml ] || exit 0
BUILT="$TARGET/.unit-tests-built-manifest"
STAMP="$TARGET/.unit-tests-built-stamp"
if [ -n "$stamp_only" ]; then
  mkdir -p "$TARGET" && touch "$STAMP" || exit 2
  exit 0
fi
unit_bytes=0
unit_files=0
# Remove unit directory $1, counting it into unit_bytes and unit_files only once it is gone.
remove_unit() {
  local b f
  b="$(du -sb -- "$1" | cut -f1)"
  f="$(find "$1" -type f | wc -l)"
  rm -rf "$1" || return
  unit_bytes=$((unit_bytes + ${b:-0}))
  unit_files=$((unit_files + f))
}
# add_units_act <branch> [package]...: the units removed, when any package lost one.
add_units_act() {
  local branch="$1"
  shift
  [ "$#" -eq 0 ] || add_act "$branch" "" '[]' "$(json_list "$@")" "$unit_bytes" "$unit_files"
}
# cargo_clean <branch> <cause> <inputs json> <crates json> <cargo clean args>...: print what cargo
# said and record the act with cargo's own count of what it removed; null where it gave none.
cargo_clean() {
  local branch="$1" cause="$2" inputs="$3" crates="$4" out rc bytes=null files=null unit=1
  shift 4
  out="$(CARGO_TERM_COLOR=never cargo clean "$@" 2>&1)"
  rc=$?
  [ -z "$out" ] || printf '%s\n' "$out" >&2
  local summary='Removed ([0-9]+) (files?|director[a-z]+)(, ([0-9.]+)(B|KiB|MiB|GiB|TiB) total)?'
  if [[ "$out" =~ $summary ]]; then
    files="${BASH_REMATCH[1]}"
    case "${BASH_REMATCH[2]}" in director*) files=0 ;; esac
    case "${BASH_REMATCH[5]}" in
      KiB) unit=1024 ;;
      MiB) unit=1048576 ;;
      GiB) unit=1073741824 ;;
      TiB) unit=1099511627776 ;;
    esac
    if [ -n "${BASH_REMATCH[4]}" ]; then
      bytes="$(awk -v n="${BASH_REMATCH[4]}" -v u="$unit" 'BEGIN { printf "%.0f", n * u }')"
    elif [ "${BASH_REMATCH[1]}" = 0 ]; then
      bytes=0
    fi
  fi
  add_act "$branch" "$cause" "$inputs" "$crates" "$bytes" "$files"
  return "$rc"
}
DEPS_DIR="$TARGET/debug/deps"
rm -f "$TARGET/.unit-tests-built-tree"
roots=(Cargo.toml Cargo.lock rust-toolchain.toml rust-toolchain .cargo/config.toml .cargo/config)
members=()
while IFS= read -r m; do [ -n "$m" ] && [ -f "$m/Cargo.toml" ] && members+=("$m"); done < <(
  awk '/^members[[:space:]]*=/{on=1} on{line=$0; sub(/#.*/,"",line); while (match(line, /"[^"]*"/)) { print substr(line, RSTART+1, RLENGTH-2); line=substr(line, RSTART+RLENGTH) } if ($0 ~ /\]/) exit}' Cargo.toml)
package_name() {
  awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^name[[:space:]]*=/{gsub(/.*=[[:space:]]*"|".*/,""); print; exit}' "$1/Cargo.toml"
}
# The fingerprint directories of package $1's units in every profile of the folder
# (<profile>/.fingerprint and <triple>/<profile>/.fingerprint), one per line.
units_of() {
  local d
  for d in "$TARGET"/*/.fingerprint/"$1"-* "$TARGET"/*/*/.fingerprint/"$1"-*; do
    [ -d "$d" ] || continue
    case "${d##*/$1-}" in *[!0-9a-f]*|"") continue ;; esac
    printf '%s\n' "$d"
  done
}
ns() { local t; t="$(stat -c "$1" -- "$2" 2>/dev/null)" || return 1; printf '%s' "${t/./}"; }
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
if [ -n "$release" ] && [ -z "$manifest" ]; then
  rm -f "$BUILT" || exit 2
  exit 0
fi
# clean_all <cause> [changed build input]...
clean_all() {
  local cause="$1"
  shift
  rm -f "$BUILT" || exit 2
  cargo_clean whole-workspace "$cause" "$(json_list "$@")" '[]' --workspace ||
    { echo "unit-tests: cargo clean --workspace failed" >&2; exit 2; }
}
if [ -z "$release" ] && [ -f "$BUILT" ]; then
  since="$STAMP"
  [ -f "$since" ] || since="$BUILT"
  since="$(ns '%.9Y' "$since")" || exit 2
  dropped=""
  dropped_names=()
  for m in "${members[@]}"; do
    name="$(package_name "$m")"
    [ -n "$name" ] || continue
    hit=""
    while IFS= read -r d; do
      if built="$(ns '%.9Y' "$d/invoked.timestamp")" && [ "$built" -gt "$since" ]; then
        if ! remove_unit "$d"; then
          [ -z "$hit" ] || dropped_names+=("$name")
          add_units_act units-built-after-stamp "${dropped_names[@]}"
          exit 2
        fi
        hit=1
      fi
    done < <(units_of "$name")
    [ -z "$hit" ] || { dropped+=" $name"; dropped_names+=("$name"); }
  done
  add_units_act units-built-after-stamp "${dropped_names[@]}"
  [ -z "$dropped" ] || echo "unit-tests: units built since the last recorded run, by a build this record does not name — rebuilding:$dropped"
fi
if [ -z "$manifest" ] || [ ! -f "$BUILT" ]; then
  echo "unit-tests: no record of the tree this target was built from — cleaning the workspace crates"
  if [ -z "$manifest" ]; then clean_all unhashable-tree; else clean_all missing-record; fi
else
  changed="$(diff <(cat "$BUILT") <(printf '%s\n' "$manifest") | sed -n 's/^[<>] [0-9a-f]\{64\}  //p' | LC_ALL=C sort -u)"
  if [ -n "$release" ]; then
    # A listing that fails names Cargo.toml: the whole workspace is cleaned.
    # A file touched since the record may have been built at a state the record does not name.
    touched="$({ for r in "${roots[@]}"; do [ "$r" -nt "$BUILT" ] && printf '%s\n' "$r"; done
                 find "${members[@]}" -path '*/target' -prune -o -type f -newer "$BUILT" -print || echo Cargo.toml
                 extras | tr '\0' '\n' | while IFS= read -r f; do [ "$f" -nt "$BUILT" ] && printf '%s\n' "$f"; done; })"
    changed="$(printf '%s\n%s\n' "$changed" "$touched" | sed '/^$/d' | LC_ALL=C sort -u)"
  fi
fi
# The newest mtime or ctime among the files, in ns; fails when one is gone.
newest_edit() {
  local f t best=0
  for f in "$@"; do
    for t in '%.9Y' '%.9Z'; do
      v="$(ns "$t" "$f")" || return 1
      [ "$v" -gt "$best" ] && best="$v"
    done
  done
  printf '%s' "$best"
}
# Drop the units of package $1 stamped before the newest edit in $2; succeeds when one is kept.
keep_built_units() {
  local pkg="$1" edit="$2" d kept=1 stamp
  while IFS= read -r d; do
    if stamp="$(ns '%.9Y' "$d/invoked.timestamp")" && [ "$stamp" -gt "$edit" ]; then
      kept=0
    else
      if remove_unit "$d"; then
        case " ${stale_names[*]-} " in *" $pkg "*) ;; *) stale_names+=("$pkg") ;; esac
      fi
    fi
  done < <(units_of "$pkg")
  return $kept
}
if [ -n "$manifest" ] && [ -f "$BUILT" ] && [ -n "${changed:-}" ]; then
  # A lockfile that differs between two trees is no reason to clean on its own: cargo keys each
  # unit by the dependencies it resolved, so a unit built against other versions is a different
  # unit. Only source files need this clean, because cargo trusts their mtimes across trees. A
  # release still counts it: the lock changed under units this lease built.
  root_changed=()
  for r in "${roots[@]}"; do
    [ -z "$release" ] && [ "$r" = Cargo.lock ] && continue
    grep -qxF "$r" <<< "$changed" && root_changed+=("$r")
  done
  if [ "${#root_changed[@]}" -gt 0 ] && [ -z "$release" ]; then
    echo "unit-tests: ${root_changed[*]} changed — cleaning the workspace crates"
    clean_all build-input-changed "${root_changed[@]}"
  else
    to_clean=()
    cleaned=()
    stale_names=()
    any_kept=""
    for m in "${members[@]}"; do
      name="$(package_name "$m")"
      [ -n "$name" ] || continue
      files=()
      while IFS= read -r f; do [ -n "$f" ] && files+=("$f"); done < <(grep "^$m/" <<< "$changed")
      if [ -d "$DEPS_DIR" ]; then
        under="${name//-/_}"
        while IFS= read -r f; do
          case "$f" in "$m"/*) continue ;; esac
          if cat "$DEPS_DIR/$under"-*.d 2>/dev/null | tr ' ' '\n' | grep -qxF "$ROOT/$f:" ||
             cat "$DEPS_DIR/$under"-*.d 2>/dev/null | tr ' ' '\n' | grep -qxF "$ROOT/$f"; then files+=("$f"); fi
        done <<< "$changed"
      fi
      [ "${#root_changed[@]}" -gt 0 ] && files+=("${root_changed[@]}")
      [ "${#files[@]}" -gt 0 ] || continue
      if [ -n "$release" ]; then
        if edit="$(newest_edit "${files[@]}")" && keep_built_units "$name" "$edit"; then any_kept=1; continue; fi
      fi
      to_clean+=(-p "$name")
      cleaned+=("$name")
    done
    add_units_act release-units-dropped "${stale_names[@]}"
    if [ "${#root_changed[@]}" -gt 0 ] && [ -z "$any_kept" ]; then
      echo "unit-tests: ${root_changed[*]} changed and no built unit is newer — cleaning the workspace crates"
      clean_all build-input-changed "${root_changed[@]}"
    elif [ "${#to_clean[@]}" -gt 0 ]; then
      echo "unit-tests: cleaning the workspace crates whose bytes changed:${to_clean[*]//-p/}"
      rm -f "$BUILT" || exit 2
      # Dropping a changed crate's fingerprints makes cargo rebuild it and its dependents; its
      # outputs stay for rustc to overwrite. Deleting them (cargo clean -p) cost gigabytes a lease.
      unit_bytes=0
      unit_files=0
      for name in "${cleaned[@]}"; do
        while IFS= read -r u; do
          [ -n "$u" ] || continue
          remove_unit "$u" || { echo "unit-tests: removing $name's fingerprint $u failed" >&2; exit 2; }
        done < <(units_of "$name")
      done
      add_act per-crate bytes-changed '[]' "$(json_list "${cleaned[@]}")" "$unit_bytes" "$unit_files"
    else
      if [ -z "$release" ] && grep -qxF Cargo.lock <<< "$changed"; then
        echo "unit-tests: only Cargo.lock changed — cargo keys each unit by its resolved dependencies; nothing cleaned"
      else
        echo "unit-tests: no workspace crate's bytes changed — nothing cleaned"
      fi
    fi
  fi
fi
[ -z "$manifest" ] || printf '%s\n' "$manifest" > "$BUILT" || exit 2
touch "$STAMP" || exit 2
