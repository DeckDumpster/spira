#!/usr/bin/env bash
#
# lockfile-lint.sh — refuse a Cargo.lock version bump with no matching Cargo.toml change.
#
#   lockfile-lint.sh
#
# WHY THIS EXISTS. rust-toolchain.toml pins the toolchain, but a Cargo.lock already on
# disk can still have been bumped by an earlier `cargo build` run under a newer, unpinned
# cargo — resolving a registry package to a version the pinned toolchain cannot parse,
# with no Cargo.toml edit to review. This lint refuses that diff (sp-4kws1).
#
# WHAT IT CHECKS. For every registry package whose locked version increases between
# SPIRA_GATE_BASE and this tree's Cargo.lock, some Cargo.toml in the diff must mention
# that package's name — the trace a deliberate dependency change leaves. A bump with
# no such trace is refused.
#
# WITH NO SPIRA_GATE_BASE, this skips — the same convention build-fence.sh uses for "no
# diff to check": gate.sh always supplies it, so a real certification always has one.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
cd "$HERE/.." 2>/dev/null || { printf 'lockfile-lint: cannot reach the tree holding %s\n' "$0" >&2; exit 1; }

BASE="${SPIRA_GATE_BASE:-}"
if [ -z "$BASE" ]; then
    printf 'lockfile-lint: SKIP no SPIRA_GATE_BASE — no diff to check\n' >&2
    exit 0
fi

[ -f Cargo.lock ] || { printf 'lockfile-lint: no Cargo.lock in this tree — skipped\n' >&2; exit 0; }

base_lock="$(git show "${BASE}:Cargo.lock" 2>/dev/null)" || {
    printf 'lockfile-lint: no Cargo.lock at %s — skipped\n' "$BASE" >&2
    exit 0
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
printf '%s\n' "$base_lock" > "$TMP/base.lock"
git diff "$BASE" -- '*Cargo.toml' > "$TMP/toml.diff" 2>/dev/null || true

python3 - "$TMP/base.lock" Cargo.lock "$TMP/toml.diff" <<'_PY'
import re
import sys
import tomllib

base_path, head_path, diff_path = sys.argv[1], sys.argv[2], sys.argv[3]


def package_versions(path):
    with open(path, "rb") as f:
        data = tomllib.load(f)
    out = {}
    for pkg in data.get("package", []):
        name, version = pkg.get("name"), pkg.get("version")
        if name and version:
            out.setdefault(name, set()).add(version)
    return out


def version_tuple(v):
    try:
        return tuple(int(p) for p in v.split(".")[:3])
    except ValueError:
        return None


base = package_versions(base_path)
head = package_versions(head_path)

with open(diff_path, encoding="utf-8", errors="replace") as f:
    toml_diff = f.read()


def named_in_diff(name):
    return re.search(r"\b" + re.escape(name) + r"\b", toml_diff) is not None


offenders = []
for name, head_versions in head.items():
    base_versions = base.get(name)
    if not base_versions:
        continue  # a brand-new package is not a BUMP of an existing one
    for hv in sorted(head_versions - base_versions):
        hv_t = version_tuple(hv)
        if hv_t is None:
            continue
        base_t = [version_tuple(bv) for bv in base_versions]
        base_t = [t for t in base_t if t is not None]
        if base_t and all(t < hv_t for t in base_t):
            if not named_in_diff(name):
                offenders.append((name, sorted(base_versions), hv))

if offenders:
    for name, base_versions, hv in offenders:
        print(
            f"lockfile-lint: {name} {'/'.join(base_versions)} -> {hv} "
            "with no matching Cargo.toml change"
        )
    sys.exit(1)
sys.exit(0)
_PY
rc=$?
if [ "$rc" -ne 0 ]; then
    printf 'lockfile-lint: a Cargo.lock bump has no matching Cargo.toml change — either\n' >&2
    printf 'the dependency change belongs in a Cargo.toml, or the lockfile drifted under\n' >&2
    printf 'an unpinned local toolchain and should be regenerated under rust-toolchain.toml\n' >&2
fi
exit "$rc"
