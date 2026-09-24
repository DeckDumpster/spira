#!/usr/bin/env bash
#
# deps-lint.sh — refuse any external program invoked but not declared in deps.toml.
#
#   deps-lint.sh [<root>]    scan <root>/spira/*.sh and Rust sources under <root>
#                            default root: the repo containing this file
#
# EXIT
#   0   clean
#   1   undeclared programs found (names printed to stdout, one per line)
#   2   usage / config error
#   3   refusing to report clean (manifest did not load)
#
# WHAT IS SCANNED
#   spira/*.sh    command -v PROG patterns (not inside double-quoted strings)
#   **/*.rs       Command::new("LITERAL") literal strings
#
# SYSTEM_ALLOW covers coreutils and shell functions checked via `command -v`
# that are not harness dependencies. Add a name there, not to deps.toml, when
# the program is a standard system utility.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="${1:-$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null || echo "$HERE/..")}"
DEPS="$HERE/deps.toml"

[ -f "$DEPS" ] || { printf 'deps-lint: %s not found\n' "$DEPS" >&2; exit 2; }

python3 - "$DEPS" "$ROOT" <<'_PY'
import sys, re, tomllib, pathlib, os

deps_path, root = sys.argv[1], pathlib.Path(sys.argv[2])

with open(deps_path, "rb") as f:
    data = tomllib.load(f)

known = {d["name"] for d in data.get("dep", [])}
if not known:
    print("deps-lint: refusing to report clean — deps.toml loaded no entries", file=sys.stderr)
    sys.exit(3)

# System tools and shell functions checked via `command -v` that are not in deps.toml.
SYSTEM_ALLOW = {
    "bash", "sh", "dash", "env", "true", "false",
    "sort", "cut", "awk", "gawk", "sed", "grep", "find", "cat", "echo",
    "printf", "date", "kill", "sleep", "wait", "read", "test",
    "mkdir", "rmdir", "rm", "mv", "cp", "ln", "stat", "sha256sum",
    "wc", "tr", "head", "tail", "tee", "diff", "patch",
    "timeout", "curl", "gcc", "nc", "setsid", "pgrep", "fuser", "script",
    "systemctl", "systemd-run",
    "nodejs",                        # alternate name for node on some platforms
    "gate_meter", "yield_note", "fayth_names",  # shell functions, not programs
}

CV_RE = re.compile(r'command\s+-v\s+([a-z][a-z0-9_-]+)')
RUST_RE = re.compile(r'Command::new\("([a-z][a-z0-9_-]+)"\)')

offenders = []

# Shell scripts: command -v PROG, skipping matches inside double-quoted strings.
spira_dir = root / "spira"
for sh in sorted(spira_dir.glob("*.sh")):
    try:
        text = sh.read_text(errors="replace")
    except OSError:
        continue
    for lineno, line in enumerate(text.splitlines(), 1):
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        for m in CV_RE.finditer(line):
            prog = m.group(1)
            if prog in known or prog in SYSTEM_ALLOW:
                continue
            # Skip if the match is inside a quoted string.
            before = line[: m.start()]
            if before.count('"') % 2 == 1 or before.count("'") % 2 == 1:
                continue
            offenders.append(f"shell:{sh.name}:{lineno}: {prog}")

# Rust sources: Command::new("literal").
for rs in sorted(root.rglob("*.rs")):
    if "target" in rs.parts:
        continue
    try:
        text = rs.read_text(errors="replace")
    except OSError:
        continue
    for lineno, line in enumerate(text.splitlines(), 1):
        for m in RUST_RE.finditer(line):
            prog = m.group(1)
            if prog in known or prog in SYSTEM_ALLOW:
                continue
            offenders.append(f"rust:{rs.relative_to(root)}:{lineno}: {prog}")

if offenders:
    for o in offenders:
        print(o)
    sys.exit(1)
_PY
