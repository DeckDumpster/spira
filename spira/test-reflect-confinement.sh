#!/usr/bin/env bash
#
# test-reflect-confinement.sh — the judgement tier (reflect.sh) is confined by deny rules.
#
# Deny rules hold under bypass-permissions; --allowedTools does not subtract from it. The
# session must therefore carry --settings naming a deny list that refuses file edits and the
# state-changing harness verbs, and must not carry the --allowedTools that implied otherwise.
#
# covers: spira/reflect.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
BIN="$TMP/bin"; mkdir -p "$BIN"
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat > /dev/null
printf '%s\n' "$@" > "$TMP/argv"
echo "no action"
SHIM
chmod +x "$BIN/claude"
export TMP
tl_config SPIRA_AGENT="$BIN/claude"

SPIRA_AGENT="$BIN/claude" bash "$HERE/reflect.sh" sp-fixture >/dev/null 2>&1
argv="$(cat "$TMP/argv" 2>/dev/null || true)"
ran=0; [ -n "$argv" ] && ran=1
is "reflect launched the agent shim" 1 "$ran"
nowant "no misleading --allowedTools" "--allowedTools" "$argv"
settings="$(awk '/^--settings$/{getline; print; exit}' "$TMP/argv")"
deny="$(printf '%s' "$settings" | python3 -c 'import json,sys; print("\n".join(json.load(sys.stdin)["permissions"]["deny"]))' 2>/dev/null || true)"
for rule in Write Edit NotebookEdit 'Bash(bd:*)' 'Bash(work:*)' 'Bash(lc:*)' 'Bash(mail:*)' \
            'Bash(spira-lc:*)' 'Bash(git commit:*)' 'Bash(git push:*)'; do
    want "denies $rule" "$rule" "$deny"
done
nowant "read-only shell is not denied wholesale" $'\nBash\n' $'\n'"$deny"$'\n'
tl_summary
