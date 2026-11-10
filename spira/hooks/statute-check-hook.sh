#!/usr/bin/env bash
# statute-check-hook.sh — PreToolUse hook (Bash) for the Concierge and persona sessions: the first
# decision command of each verb class in a window is refused with the governing statutes in full;
# the retry passes. The logic is `statute-check hook`; this finds the binary and hands over.
# Override: STATUTE_CHECK_CONSIDERED="<slugs read, and why this complies>", logged.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd -P)"
bin="$(command -v statute-check 2>/dev/null || true)"
[ -n "$bin" ] || for d in "$here/../../bin" "$here/../.." ; do [ -x "$d/statute-check" ] && bin="$d/statute-check" && break; done
if [ -z "$bin" ]; then
    printf 'statute-check-hook: no statute-check binary in the release; decisions are not being checked\n' >&2
    exit 0
fi
exec "$bin" hook
