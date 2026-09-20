#!/usr/bin/env bash
# test-brief-mail-options.sh — every --flag on a send operator invocation in a chamber brief
# is a known cmd_send option (law-a-matcher-reads-code-not-prose).
#
# covers: spira/mail.sh spira/chamber/*.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
MAIL="$HERE/mail.sh"
CHAMBER="$HERE/chamber"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s\n' "$1"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# Extract cmd_send's known --options from mail.sh by scanning its option loop.
# Returns one option per line, sorted.
cmd_send_opts() {
    python3 - "$MAIL" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
m = re.search(r'^cmd_send\(\).*?^}', text, re.MULTILINE | re.DOTALL)
if m:
    for o in sorted(set(re.findall(r'^\s+(--[a-z][a-z-]+)\)', m.group(0), re.MULTILINE))):
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

# Returns 0 if all --flags in $1 are known cmd_send options; 1 otherwise.
# Prints any unknown options to stdout.
check_brief() {
    local f="$1"
    local known; known="$(cmd_send_opts)"
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
    bad "no .md files found in $CHAMBER — cannot verify"
fi

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
