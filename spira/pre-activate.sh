#!/usr/bin/env bash
#
# pre-activate.sh <release-dir> — five checks a release must pass before
# releases/current moves onto it. make install and skew.sh refresh (release
# mode) both funnel through `make install`, which runs this against the
# release it just built or is about to roll back to, before the symlink
# flips. Exit 0 only if every check passes; on any failure, print each failed
# check to stderr and exit 1 — the caller leaves current where it was.
#
# Checks:
#   deps       every runtime-tier program in the release's deps.toml is on
#              PATH at its version_min
#   config     spira.toml, if one is in force on this host, validates against
#              THIS release's spira-config schema — not the host's
#   store      the bead store is reachable and its schema matches the bd on
#              PATH (deps' version_min already pins which bd that is)
#   units      every systemd unit renders with no unresolved placeholder
#   self-test  the release's own spira/self-test.sh
set -uo pipefail

REL="${1:?usage: pre-activate.sh <release-dir>}"
[ -d "$REL" ] || { printf 'pre-activate: not a directory: %s\n' "$REL" >&2; exit 1; }

FAIL=0
fail() { printf 'pre-activate: FAIL %s: %s\n' "$1" "$2" >&2; FAIL=1; }
ok()   { printf 'pre-activate: ok   %s\n' "$1"; }

# --- deps: every runtime-tier program in deps.toml, present at version_min ---
check_deps() {
    local toml="$REL/spira/deps.toml"
    if [ ! -f "$toml" ]; then
        fail deps "$toml not found"
        return
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        fail deps "python3 not on PATH — cannot read deps.toml"
        return
    fi
    local out rc
    out="$(python3 - "$toml" <<'PY'
import re, subprocess, sys, tomllib

def ge(a, b):
    def parts(v):
        return [int(x) for x in re.findall(r"\d+", v)]
    return parts(a) >= parts(b)

with open(sys.argv[1], "rb") as f:
    doc = tomllib.load(f)

bad = []
for dep in doc.get("dep", []):
    if dep.get("tier") != "runtime":
        continue
    name = dep["name"]
    probe = dep.get("version_probe") or ""
    vmin = (dep.get("version_min") or "").strip()
    if not probe:
        bad.append(f"{name}: no version_probe in deps.toml")
        continue
    try:
        p = subprocess.run(["bash", "-c", probe], capture_output=True, text=True, timeout=10)
    except Exception as e:
        bad.append(f"{name}: probe error: {e}")
        continue
    if p.returncode != 0:
        bad.append(f"{name}: not usable (probe exit {p.returncode})")
        continue
    if vmin:
        m = re.search(r"\d+(?:\.\d+)*", p.stdout)
        got = m.group(0) if m else ""
        if not got or not ge(got, vmin):
            bad.append(f"{name}: version {got or 'unknown'} is below required {vmin}")

for b in bad:
    print(b)
sys.exit(1 if bad else 0)
PY
)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        while IFS= read -r line; do
            [ -n "$line" ] && fail deps "$line"
        done <<< "$out"
    else
        ok deps
    fi
}

# --- config: spira.toml (if any is in force) validates against the release's schema ---
check_config() {
    local toml="" c
    for c in "${SPIRA_TOML:-}" \
             "${SPIRA_REPO:-}/spira.toml" \
             "${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml" \
             /etc/spira/spira.toml; do
        [ -n "$c" ] && [ -f "$c" ] && { toml="$c"; break; }
    done
    if [ -z "$toml" ]; then
        ok "config (no spira.toml in force — nothing to validate)"
        return
    fi
    local bin="$REL/bin/spira-config"
    if [ ! -x "$bin" ]; then
        fail config "$bin not found in release"
        return
    fi
    local out rc
    out="$("$bin" validate "$toml" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        ok config
    else
        fail config "$toml: $out"
    fi
}

# --- store: the bead store is reachable and its schema matches the bd on PATH ---
check_store() {
    if ! command -v bd >/dev/null 2>&1; then
        fail store "bd not on PATH"
        return
    fi
    local out rc
    if [ -n "${SPIRA_DB:-}" ]; then
        out="$(bd -C "$SPIRA_DB" migrate status 2>&1)"
    else
        out="$(bd migrate status 2>&1)"
    fi
    rc=$?
    if [ "$rc" -eq 0 ]; then
        ok store
    else
        fail store "$out"
    fi
}

# --- units: every systemd unit renders with no unresolved placeholder ---
check_units() {
    local inst="$REL/systemd/install.sh"
    if [ ! -x "$inst" ]; then
        fail units "$inst not found"
        return
    fi
    local out rc
    out="$("$inst" --render 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        fail units "install.sh --render exited $rc: $(printf '%s' "$out" | tail -5)"
        return
    fi
    local left
    left="$(printf '%s\n' "$out" | grep -oE '@[A-Z_]+@' | sort -u | tr '\n' ' ')"
    if [ -n "$left" ]; then
        fail units "unresolved placeholders: $left"
    else
        ok units
    fi
}

# --- self-test: the release's own smoke test ---
check_self_test() {
    local st="$REL/spira/self-test.sh"
    if [ ! -x "$st" ]; then
        fail self-test "$st not found"
        return
    fi
    local out rc
    out="$("$st" "$REL" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        ok self-test
    else
        fail self-test "$out"
    fi
}

check_deps
check_config
check_store
check_units
check_self_test

exit "$FAIL"
