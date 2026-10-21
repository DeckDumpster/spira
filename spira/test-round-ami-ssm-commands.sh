#!/usr/bin/env bash
#
# test-round-ami-ssm-commands.sh — the Round AMI SSM command document survives /bin/sh.
#
# tier: T1
# covers: round-vm/ami-setup.sh round-vm/wait-image.sh round-vm/ssm-commands.jq .github/workflows/round-ami-build.yml
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
A="$(cat "$ROOT/round-vm/ami-setup.sh")"
want "image carries sqlite3" " sqlite3 " "$A"
want "image links node and npm" "node npm npx" "$A"
want "image installs nextest at the foreign pin with a toolchain new enough to build it" "cargo install --locked cargo-nextest --version \"\$nextest\"" "$A"
want "setup leaves the dir template.sh deleted before cargo install" "template.sh\"
# template.sh removes its work dir, which may be ours: cargo install needs a cwd that exists.
cd /" "$A"
want "image checks each foreign repo" "runner-deps.sh\" --check" "$A"

if ! command -v jq >/dev/null 2>&1; then
    ok "jq absent; rendered-document checks skipped"
else
    doc="$(jq -nc --arg u "https://h/round-ami/123.tgz?X-Amz-Signature=a&X-Amz-Expires=1" --arg t "1.90.0" -f "$F")"
    n="$(printf '%s' "$doc" | jq '.commands | length')"
    is "one command, so no sh-level line can fail on bash-isms" 1 "$n"
    printf '%s\n' "$(printf '%s' "$doc" | jq -r '.commands[0]')" > "$TMP/script.sh"
    is "command starts with bash -c" "bash -c " "$(head -c 8 "$TMP/script.sh")"
    mkdir -p "$TMP/bin"
    printf '#!/bin/sh\nprintf "%%s" "$2"\n' > "$TMP/bin/bash"; chmod +x "$TMP/bin/bash"
    if command -v dash >/dev/null 2>&1; then
        got="$(PATH="$TMP/bin:$PATH" dash "$TMP/script.sh")"
        want "dash hands the whole script to bash" "L=/root/setup.log" "$got"
    else
        ok "dash absent; skipped"
    fi
    bash -n "$TMP/script.sh" && ok "generated script parses under bash" || bad "generated script parses under bash" "rejected"
    want "presigned url interpolated, quoted" "'https://h/round-ami/123.tgz?X-Amz-Signature=a&X-Amz-Expires=1'" "$(cat "$TMP/script.sh")"
    if grep -Eq '(^|[^[:alnum:]_-])aws[[:space:]]' "$TMP/script.sh"; then bad "no aws binary on the instance" "found"; else ok "no aws binary on the instance"; fi
    mkdir -p "$TMP/fx/round-vm" "$TMP/fx/root"
    printf '#!/bin/bash\nfor i in $(seq 1 400); do echo "chatty line $i"; done\necho "BOOM failing command" >&2\nexit 7\n' > "$TMP/fx/round-vm/ami-setup.sh"
    sed -e "s#/root/setup.log#$TMP/fx/setup.log#g" -e "s#/root/tree#$TMP/fx/root#g" -e "s#round-vm/ami-setup.sh#$TMP/fx/round-vm/ami-setup.sh#g" "$TMP/script.sh" > "$TMP/fx/run.sh"
    mkdir -p "$TMP/bin2"
    printf '#!/bin/sh\nwhile [ $# -gt 0 ]; do [ "$1" = -o ] && echo x > "$2"; shift; done\nprintf 200\n' > "$TMP/bin2/curl"
    printf '#!/bin/sh\nexit 0\n' > "$TMP/bin2/tar"; chmod +x "$TMP/bin2/curl" "$TMP/bin2/tar"
    out="$(PATH="$TMP/bin2:$PATH" bash "$TMP/fx/run.sh" 2>&1 >/dev/null)"; rc=$?
    is "failed setup propagates its exit code" 7 "$rc"
    want "failure output names the failing command" "BOOM failing command" "$out"
    n="$(printf '%s\n' "$out" | wc -l)"
    [ "$n" -le 65 ] && ok "failure output is a bounded tail" || bad "failure output is a bounded tail" "$n lines"
    want "non-200 fails loudly" "http_code" "$doc"
    want "empty body refused before tar" "-s /root/tree.tgz" "$doc"
    want "toolchain passed to setup" "ami-setup.sh /root/tree 1.90.0" "$doc"
fi
want "prefetch skips other targets' manifests (wasm-only deps need a newer cargo)" "cargo fetch --locked --target x86_64-unknown-linux-gnu" "$(cat "$ROOT/round-vm/src/template.sh")"
grep -q 'ssm-commands.jq' "$ROOT/.github/workflows/round-ami-build.yml" \
    && ok "workflow uses the filter" || bad "workflow uses the filter" "not referenced"

W="$ROOT/round-vm/wait-image.sh"
mkdir -p "$TMP/wbin"
cat > "$TMP/wbin/aws" <<'AWS'
#!/bin/sh
n="$(cat "$FX/n" 2>/dev/null || echo 0)"; echo $((n + 1)) > "$FX/n"
if [ "$n" -ge "$PENDING_POLLS" ]; then echo "$FINAL"; else echo pending; fi
AWS
printf '#!/bin/sh\nexit 0\n' > "$TMP/wbin/sleep"; chmod +x "$TMP/wbin/aws" "$TMP/wbin/sleep"
wrun() { rm -f "$TMP/n"; FX="$TMP" PENDING_POLLS="$1" FINAL="$2" PATH="$TMP/wbin:$PATH" ROUND_AMI_WAIT_INTERVAL=60 bash "$W" ami-1 >/dev/null 2>&1; }
wrun 15 available; is "image pending 15 minutes still succeeds" 0 "$?"
wrun 15 failed; is "image that goes failed fails" 1 "$?"
wrun 100000 available; is "image pending past the bound fails" 1 "$?"
grep -q 'wait-image.sh' "$ROOT/.github/workflows/round-ami-build.yml" \
    && ok "workflow uses the bounded wait" || bad "workflow uses the bounded wait" "not referenced"
if grep -q 'ec2 wait image-available' "$ROOT/.github/workflows/round-ami-build.yml"; then bad "stock 10-minute waiter gone" "found"; else ok "stock 10-minute waiter gone"; fi

tl_summary
