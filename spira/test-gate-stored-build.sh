#!/usr/bin/env bash
# test-gate-stored-build.sh — gate.yml's build job fetches a tree's stored build over HTTP
# when the store answers, stores what it builds, and builds as before when it does not.
#
# The workflow's own step scripts are cut out of gate.yml and run against a real HTTP store
# (a directory served with GET and PUT), with a toolchain stub that names a fixed compiler.
#
# tier: T0
# covers: .github/workflows/gate.yml round-vm/artifact-store.sh round-vm/src/run.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"

echo "test-gate-stored-build.sh"
command -v python3 >/dev/null 2>&1 || skip "python3 serves the store"

step_script() {
    awk -v name="$1" '
        $0 ~ "- name: " name { on = 1; next }
        on && /^        run: \|/ { run = 1; next }
        run && /^          / { sub(/^          /, ""); print; next }
        run && !/^ *$/ { exit }
        run { print }
    ' "$ROOT/.github/workflows/gate.yml"
}
FETCH="$(step_script "Fetch the tree's stored build")"
STORE_STEP="$(step_script "Store the build for later passes")"
[ -n "$FETCH" ] && ok "the fetch step is found in gate.yml" || bad "the fetch step is found in gate.yml" "empty"
[ -n "$STORE_STEP" ] && ok "the store step is found in gate.yml" || bad "the store step is found in gate.yml" "empty"

W="$(mktemp -d)"
trap 'kill $SRV 2>/dev/null; rm -rf "$W"' EXIT
mkdir -p "$W/store" "$W/fake" "$W/repo/round-vm"
cat > "$W/server.py" <<'PY'
import http.server, os, sys
root = sys.argv[1]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def p(self): return os.path.join(root, self.path.lstrip("/"))
    def do_GET(self):
        f = self.p()
        if os.path.isfile(f):
            b = open(f, "rb").read()
            self.send_response(200); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
        else:
            self.send_response(404); self.send_header("Content-Length", "0"); self.end_headers()
    def do_PUT(self):
        f = self.p(); os.makedirs(os.path.dirname(f), exist_ok=True)
        n = int(self.headers.get("Content-Length", 0))
        open(f, "wb").write(self.rfile.read(n))
        self.send_response(201); self.send_header("Content-Length", "0"); self.end_headers()
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
PY
python3 "$W/server.py" "$W/store" > "$W/port" &
SRV=$!
for _ in $(seq 50); do [ -s "$W/port" ] && break; sleep 0.1; done
PORT="$(cat "$W/port")"
URL="http://127.0.0.1:$PORT"

printf '#!/bin/sh\nprintf "rustc 1.99.0\\nrelease: 1.99.0\\ncommit-hash: deadbeef\\n"\n' > "$W/fake/rustc"
chmod +x "$W/fake/rustc"
cp "$ROOT/round-vm/artifact-store.sh" "$W/repo/round-vm/"
git -C "$W/repo" init -q
echo a > "$W/repo/f"
git -C "$W/repo" add f round-vm
git -C "$W/repo" -c user.name=t -c user.email=t@t commit -qm a

run_step() {
    local script="$1" url="$2"
    : > "$W/out"
    (cd "$W/repo" && env -i HOME="$W" PATH="$W/fake:/usr/bin:/bin" GITHUB_OUTPUT="$W/out" ARTIFACT_STORE_URL="$url" bash -c "$script" 2>&1)
}
substitute() { printf '%s' "$1" | sed -e "s/\${{ steps.stored.outputs.id }}/$2/" -e "s/\${{ steps.stored.outputs.tree }}/$3/" -e "s/\${{ steps.stored.outputs.toolchain }}/$4/"; }

out="$(run_step "$FETCH" "")"
want "an unset store is skipped and says so" "ARTIFACT_STORE_URL is not set" "$out"
want "an unset store is not a hit" "hit=false" "$(cat "$W/out")"

DEAD="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
out="$(run_step "$FETCH" "http://127.0.0.1:$DEAD")"
want "a store that does not answer is skipped and says so" "does not answer from this runner" "$out"
nowant "an unreachable store is not marked reachable" "reachable=true" "$(cat "$W/out")"

REFUSE="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
python3 - "$REFUSE" <<'PY' &
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(401); self.send_header("Content-Length", "0"); self.end_headers()
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
SRV2=$!
trap 'kill $SRV $SRV2 2>/dev/null; rm -rf "$W"' EXIT
for _ in $(seq 50); do (exec 3<>"/dev/tcp/127.0.0.1/$REFUSE") 2>/dev/null && break; sleep 0.1; done
out="$(run_step "$FETCH" "http://127.0.0.1:$REFUSE")"
want "a store that refuses the runner is skipped with its status" "status 401" "$out"
nowant "a refusing store is not marked reachable" "reachable=true" "$(cat "$W/out")"

out="$(run_step "$FETCH" "$URL")"
want "an empty store is a miss" "artifacts: miss" "$out"
want "an empty store is reachable, so the build is stored" "reachable=true" "$(cat "$W/out")"
nowant "an empty store is not a hit" "hit=true" "$(cat "$W/out")"
ID="$(sed -n 's/^id=//p' "$W/out")"
TREE="$(sed -n 's/^tree=//p' "$W/out")"
TC="$(sed -n 's/^toolchain=//p' "$W/out")"

mkdir -p "$W/repo/bin"
printf '#!/bin/sh\necho built\n' > "$W/repo/bin/tool"
chmod +x "$W/repo/bin/tool"
store_run="$(substitute "$STORE_STEP" "$ID" "$TREE" "$TC")"
out="$(run_step "$store_run" "$URL")"
want "the build is stored under the tree's key" "artifacts: stored $ID" "$out"
[ -s "$W/store/artifacts/$ID/MANIFEST" ] && ok "the manifest is on the store" || bad "the manifest is on the store" "$(find "$W/store" | head)"

rm -rf "$W/repo/bin"
out="$(run_step "$FETCH" "$URL")"
want "the stored build is a hit" "artifacts: hit $ID" "$out"
want "a hit is reported to the later steps" "hit=true" "$(cat "$W/out")"
is "the hit installs the stored executable" "built" "$("$W/repo/bin/tool")"

echo b > "$W/repo/f"
git -C "$W/repo" -c user.name=t -c user.email=t@t commit -qam b
rm -rf "$W/repo/bin"
out="$(run_step "$FETCH" "$URL")"
want "another tree is a miss" "artifacts: miss" "$out"
nowant "another tree does not take the first tree's build" "hit=true" "$(cat "$W/out")"

GATE="$(cat "$ROOT/.github/workflows/gate.yml")"
want "the build runs only when no stored build was found" "if: steps.stored.outputs.hit != 'true'
        run: |
          make build" "$GATE"
want "a build is stored only when the store was reachable and missed" "if: steps.stored.outputs.hit != 'true' && steps.stored.outputs.reachable == 'true'" "$GATE"

tl_summary
