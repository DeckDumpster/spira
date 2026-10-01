#!/usr/bin/env bash
# test-brief-mail-options.sh — every --flag on a send operator invocation in a chamber brief
# is a known cmd_send option (law-a-matcher-reads-code-not-prose).
#
# tier: T0
# covers: mail/src/* spira/chamber/*.md UC-operator-channel-13
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# mail is a compiled Rust binary now (sp-ooh1k) — there is no `cmd_send()` bash function
# left to scan a case statement out of. `run_send` in main.rs is the same option loop,
# one `"--flag" => { ... }` match arm per option; read that file's source instead.
MAIL_MAIN_RS="$HERE/../mail/src/main.rs"
CHAMBER="$HERE/chamber"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# Extract run_send's known --options from mail/src/main.rs by scanning its match arms.
# Returns one option per line, sorted.
cmd_send_opts() {
    python3 - "$MAIL_MAIN_RS" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
m = re.search(r'^fn run_send\(.*?\n^}', text, re.MULTILINE | re.DOTALL)
if m:
    for o in sorted(set(re.findall(r'"(--[a-z][a-z-]+)"\s*=>', m.group(0)))):
        print(o)
PY
}

# Extract all --flags from send operator invocations in a .md file.
# Handles shell line-continuation (\) in code blocks.
brief_send_opts() {
    local f="$1"
    python3 - "$f" <<'PY'
import re, sys

lines = open(sys.argv[1]).read().splitlines()
i = 0
opts = set()
while i < len(lines):
    line = lines[i]
    if re.search(r'\bsend\s+operator\b', line):
        block = line.rstrip('\\')
        while line.rstrip().endswith('\\'):
            i += 1
            if i >= len(lines):
                break
            line = lines[i]
            block += ' ' + line.rstrip('\\')
        for m in re.finditer(r'--([a-z][a-z-]*)', block):
            opts.add('--' + m.group(1))
    i += 1
for o in sorted(opts):
    print(o)
PY
}

# Parsed once, not once per brief (coverage-map row 13): mail does not change between
# the briefs in one suite run, so re-parsing it per file bought nothing but forks.
KNOWN_OPTS="$(cmd_send_opts)"

# Returns 0 if all --flags in $1 are known cmd_send options; 1 otherwise.
# Prints any unknown options to stdout.
check_brief() {
    local f="$1"
    local known="$KNOWN_OPTS"
    local bad_opts="" opt
    while IFS= read -r opt; do
        [ -z "$opt" ] && continue
        if ! printf '%s\n' "$known" | grep -qxF -e "$opt"; then
            bad_opts="${bad_opts:+$bad_opts }$opt"
        fi
    done < <(brief_send_opts "$f")
    if [ -n "$bad_opts" ]; then
        printf '%s\n' "$bad_opts"
        return 1
    fi
    return 0
}

# ===========================================================================
echo
echo "positive control — checker catches an unknown option"
# ===========================================================================
# Plant a brief with a --priority option in a send call, which cmd_send does not accept.
cat > "$T/fake-brief.md" <<'MD'
## Escalate

    {{ASK}} send operator --from "Test <test@spira>" --subject "a question" \
        --kind question --default "yes" --priority 2
MD

out="$(check_brief "$T/fake-brief.md" 2>/dev/null || true)"
if printf '%s' "$out" | grep -qF -- '--priority'; then
    ok "checker finds --priority in a fake brief"
else
    bad "checker finds --priority in a fake brief — not detected (got: $out)"
fi

# ===========================================================================
echo
echo "known options pass the checker"
# ===========================================================================
cat > "$T/good-brief.md" <<'MD'
## Escalate

    {{ASK}} send operator --from "Test <test@spira>" \
        --subject "a question" --kind question --default "yes" --bead sp-abc
MD

out="$(check_brief "$T/good-brief.md" 2>/dev/null || true)"
if [ -z "$out" ]; then
    ok "a brief with only known options passes"
else
    bad "a brief with only known options passes — got: $out"
fi

# ===========================================================================
echo
echo "real chamber briefs"
# ===========================================================================
found_any=0
for f in "$CHAMBER"/*.md; do
    [ -f "$f" ] || continue
    found_any=1
    name="$(basename "$f")"
    out="$(check_brief "$f" 2>/dev/null || true)"
    if [ -z "$out" ]; then
        ok "$name: all send options are valid"
    else
        bad "$name: unknown options in send calls: $out"
    fi
done

if [ "$found_any" -eq 0 ]; then
    bad "no .md files found in $CHAMBER — cannot verify" ""
fi

tl_summary
