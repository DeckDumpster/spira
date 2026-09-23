#!/usr/bin/env bash
# covers: .github/workflows/gate.yml .github/workflows/release.yml
#
# Discover every Rust binary crate in the repo, build them all with the
# release profile, and copy the produced binaries to an output directory.
#
# USAGE
#   build-bins.sh <repo-root> <output-dir>
#
# A binary crate is any directory with both a Cargo.toml and a src/main.rs.
# The binary name is taken from the `name` field in [package]. This means
# every crate that produces a binary is built without a hand-maintained list:
# a new crate is discovered and built automatically, and a deleted one stops
# being built.
#
# EXIT
#   0   all discovered crates built successfully; at least one binary produced
#   1   at least one build failed, or no binary crates found
#   2   usage error
set -uo pipefail

repo="${1:?}"; shift
outdir="${1:?}"; shift 2>/dev/null || true

repo="$(cd "$repo" && pwd -P)"
mkdir -p "$outdir"
outdir="$(cd "$outdir" && pwd -P)"

built=0
failed=0

while IFS= read -r toml; do
    crate_dir="$(dirname "$toml")"
    [ -f "$crate_dir/src/main.rs" ] || continue

    bin_name="$(awk '
        /^\[package\]/ { in_pkg=1 }
        in_pkg && /^name[[:space:]]*=/ {
            gsub(/.*=[[:space:]]*"/, "")
            gsub(/".*/, "")
            print; exit
        }
    ' "$toml")"
    [ -n "$bin_name" ] || continue

    printf 'build-bins: building %s (bin=%s)\n' "${crate_dir#"$repo/"}" "$bin_name"
    if cargo build --release --manifest-path "$toml"; then
        bin_path="$crate_dir/target/release/$bin_name"
        if [ -f "$bin_path" ]; then
            cp "$bin_path" "$outdir/$bin_name"
            chmod +x "$outdir/$bin_name"
            printf 'build-bins: %s -> %s\n' "$bin_name" "$outdir"
            built=$((built+1))
        else
            printf 'build-bins: ERROR: binary not found after build: %s\n' "$bin_path" >&2
            failed=$((failed+1))
        fi
    else
        printf 'build-bins: ERROR: cargo build failed for %s\n' "${crate_dir#"$repo/"}" >&2
        failed=$((failed+1))
    fi
done < <(find "$repo" -name 'Cargo.toml' -not -path '*/target/*' | sort)

if [ "$built" -eq 0 ] && [ "$failed" -eq 0 ]; then
    printf 'build-bins: no binary crates found in %s\n' "$repo" >&2
    exit 1
fi
[ "$failed" -eq 0 ] || { printf 'build-bins: %d build(s) failed\n' "$failed" >&2; exit 1; }
printf 'build-bins: %d binary crate(s) built\n' "$built"
