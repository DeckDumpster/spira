#!/usr/bin/env bash
#
# test-round-ami-ssm-commands.sh — the Round AMI SSM command document survives /bin/sh.
#
# tier: T1
# covers: round-vm/ssm-commands.jq .github/workflows/round-ami-build.yml
# host-reason: jq and sh only; no container or database dependency
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-round-ami-ssm-commands.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

F="$ROOT/round-vm/ssm-commands.jq"
want "filter wraps the setup in bash -c" '"bash -c " + @sh' "$(cat "$F")"
want "filter sets pipefail inside the bash string" "set -euo pipefail; mkdir" "$(cat "$F")"
want "toolchain passed to setup" "ami-setup.sh /root/tree" "$(cat "$F")"

if ! command -v jq >/dev/null 2>&1; then
    ok "jq absent; rendered-document checks skipped"
else
    doc="$(jq -nc --arg b "stage-bkt" --arg r "123" --arg t "1.90.0" -f "$F")"
    n="$(printf '%s' "$doc" | jq '.commands | length')"
    is "one command, so no sh-level line can fail on bash-isms" 1 "$n"
    printf '%s\n' "$(printf '%s' "$doc" | jq -r '.commands[0]')" > "$TMP/script.sh"
    is "command starts with bash -c" "bash -c " "$(head -c 8 "$TMP/script.sh")"
    mkdir -p "$TMP/bin"
    printf '#!/bin/sh\nprintf "%%s" "$2"\n' > "$TMP/bin/bash"; chmod +x "$TMP/bin/bash"
    if command -v dash >/dev/null 2>&1; then
        got="$(PATH="$TMP/bin:$PATH" dash "$TMP/script.sh")"
        want "dash hands the whole script to bash" "set -euo pipefail; mkdir" "$got"
    else
        ok "dash absent; skipped"
    fi
    bash -n "$TMP/script.sh" && ok "generated script parses under bash" || bad "generated script parses under bash" "rejected"
    want "bucket path interpolated" "s3://stage-bkt/round-ami/123.tgz" "$doc"
    want "toolchain passed to setup" "ami-setup.sh /root/tree 1.90.0" "$doc"
fi
grep -q 'ssm-commands.jq' "$ROOT/.github/workflows/round-ami-build.yml" \
    && ok "workflow uses the filter" || bad "workflow uses the filter" "not referenced"

tl_summary
