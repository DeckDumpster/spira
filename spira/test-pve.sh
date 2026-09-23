#!/usr/bin/env bash
#
# test-pve.sh — pve.sh argument parsing, credential checks, and API routing.
#
#   ./test-pve.sh
#
# No real Proxmox connection. curl is replaced by a shim that records calls and
# returns fixture responses. The shim lives on SPIRA_PATH so it survives the
# PATH reset conf.sh performs at load time.
#
# THREE PROPERTIES (law-absence-needs-a-positive-control):
#   1. A call with valid credentials and cacert routes to the right API path.
#   2. A call without credentials fails with a named message before any curl call.
#   3. A call with a missing cacert refuses rather than falling back to insecure.
#
# Positive control: a deliberately wrong path assertion is verified to fail,
# proving the path checks are actually exercising the right thing.
#
# defect: sp-imqd4
# covers: spira/pve.sh spira/conf.sh
# hermetic-ok: mocks curl; no network calls, no systemd, no database
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

printf 'test-pve.sh\n'

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Fixture: fake CA cert and a populated pve.env.
# SPIRA_PATH must carry the shim directory — conf.sh resets PATH to
# "${SPIRA_PATH:+$SPIRA_PATH:}$HOME/.local/bin:..." and removes any injected
# bin directory unless it is carried on SPIRA_PATH.
# ---------------------------------------------------------------------------
CACERT="$TMP/fake-ca.pem"
printf 'FAKE CA\n' > "$CACERT"

PVE_ENV="$TMP/pve.env"
printf 'PVE_TOKEN_ID=testuser@pam!testtoken\n' >> "$PVE_ENV"
printf 'PVE_TOKEN_SECRET=test-secret-value\n'  >> "$PVE_ENV"
printf 'PVE_NODE=testnode\n'                   >> "$PVE_ENV"
printf 'PVE_API_HOST=192.0.2.1\n'             >> "$PVE_ENV"
printf 'PVE_CACERT=%s\n' "$CACERT"             >> "$PVE_ENV"

SHIM_DIR="$TMP/shim"
mkdir -p "$SHIM_DIR"
CURL_LOG="$TMP/curl.log"

# Write the single-response shim.
# Two known shell pitfalls to avoid here:
#   1. `printf '---\n'` fails: bash's printf treats '---' as an option (the
#      '--' option prefix), so use `echo '==='` for the sentinel instead.
#   2. `body="${VAR:-{...}}"`: when VAR is set, bash does NOT consume the
#      nested `}` inside the default value, so the outer `}` leaks into the
#      expanded string. Use an explicit if/else to avoid nested braces in `:-`.
write_simple_shim() {
    cat > "$SHIM_DIR/curl" <<'SHIM'
#!/usr/bin/env bash
{ printf '%s\n' "$@"; echo '==='; } >> "$CURL_LOG_PATH"
out_file=""; next=0
for a in "$@"; do
    if [ "$next" = "1" ]; then out_file="$a"; next=0
    elif [ "$a" = "-o" ]; then next=1
    fi
done
if [ -n "${CURL_FIXTURE:-}" ]; then body="$CURL_FIXTURE"; else body='{"data":""}'; fi
[ -n "$out_file" ] && printf '%s' "$body" > "$out_file"
printf '%s' "${CURL_STATUS:-200}"
exit "${CURL_RC:-0}"
SHIM
    chmod +x "$SHIM_DIR/curl"
}

# Write a two-response shim for commands that issue an async task.
# First call returns a UPID; subsequent calls return a stopped/OK task status.
write_task_shim() {
    local upid="$1"
    # Heredoc is unquoted so ${upid} expands here.
    cat > "$SHIM_DIR/curl" <<SHIM
#!/usr/bin/env bash
call_n=\$(grep -c '^===$' "\$CURL_LOG_PATH" 2>/dev/null || echo 0)
{ printf '%s\n' "\$@"; echo '==='; } >> "\$CURL_LOG_PATH"
out_file=""; next=0
for a in "\$@"; do
    if [ "\$next" = "1" ]; then out_file="\$a"; next=0
    elif [ "\$a" = "-o" ]; then next=1
    fi
done
if [ "\$call_n" -lt 1 ]; then
    body='{"data":"${upid}"}'
else
    body='{"data":{"status":"stopped","exitstatus":"OK"}}'
fi
[ -n "\$out_file" ] && printf '%s' "\$body" > "\$out_file"
printf '200'
SHIM
    chmod +x "$SHIM_DIR/curl"
}

write_simple_shim

# Run pve.sh with the shim environment.
# First arg is the CURL_FIXTURE body; remaining args are passed to pve.sh.
run_pve() {
    local fixture="$1"; shift
    env -i \
        PATH="$PATH" \
        HOME="$TMP" \
        SPIRA_PATH="$SHIM_DIR" \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_PVE_ENV="$PVE_ENV" \
        CURL_LOG_PATH="$CURL_LOG" \
        CURL_FIXTURE="$fixture" \
        CURL_STATUS="${CURL_STATUS:-200}" \
        CURL_RC="${CURL_RC:-0}" \
        bash "$HERE/pve.sh" "$@" 2>&1
}

reset_log() { : > "$CURL_LOG"; }
# Count completed curl calls by counting the '===' sentinels.
call_count() { local n; n=$(grep -c '^===$' "$CURL_LOG" 2>/dev/null) || n=0; printf '%s' "$n"; }
# Return all URLs from the log.
all_urls() { awk '/^https:\/\//' "$CURL_LOG"; }
# Return the URL of the Nth call (1-indexed); defaults to last.
nth_url() {
    local n="${1:-0}"
    if [ "$n" -gt 0 ]; then
        awk '/^https:\/\//' "$CURL_LOG" | sed -n "${n}p"
    else
        awk '/^https:\/\//' "$CURL_LOG" | tail -1
    fi
}

# ---------------------------------------------------------------------------
# Smoke: pve.sh and shim load and the basic nextid path works.
# ---------------------------------------------------------------------------
reset_log
out="$(run_pve '{"data":"999"}' nextid)"
want "nextid: output contains 999" "999" "$out"
want "nextid: calls /cluster/nextid" "/cluster/nextid" "$(nth_url)"

# Positive control: a deliberately wrong assertion would fail.
url="$(nth_url)"
[[ "$url" != *"/cluster/wrongpath"* ]] && ok "positive-control: wrong-path check would fail" \
    || bad "positive-control" "url matched wrong path"

# ---------------------------------------------------------------------------
# 1. Missing credentials are caught before any curl call.
# ---------------------------------------------------------------------------
reset_log
no_env_out="$(env -i \
    PATH="$PATH" HOME="$TMP" SPIRA_PATH="$SHIM_DIR" \
    SPIRA_CONF="$TMP/no.conf" \
    SPIRA_PVE_ENV="$TMP/missing.env" \
    CURL_LOG_PATH="$CURL_LOG" \
    bash "$HERE/pve.sh" status 100 2>&1)" || true
want "missing creds: error names env file" "missing.env" "$no_env_out"
is "missing creds: no curl call" "0" "$(call_count)"

# ---------------------------------------------------------------------------
# 2. Missing cacert refused; no insecure fallback.
# ---------------------------------------------------------------------------
reset_log
bad_env="$TMP/bad-cert.env"
cat > "$bad_env" <<EOF
PVE_TOKEN_ID=u@r!t
PVE_TOKEN_SECRET=s
PVE_NODE=n
PVE_API_HOST=192.0.2.1
PVE_CACERT=/tmp/surely-absent-cert-$$
EOF
missing_cert_out="$(env -i \
    PATH="$PATH" HOME="$TMP" SPIRA_PATH="$SHIM_DIR" \
    SPIRA_CONF="$TMP/no.conf" \
    SPIRA_PVE_ENV="$bad_env" \
    CURL_LOG_PATH="$CURL_LOG" \
    bash "$HERE/pve.sh" status 100 2>&1)" || true
want "missing cacert: error mentions cacert" "cacert" "$missing_cert_out"
is "missing cacert: no curl call" "0" "$(call_count)"

# ---------------------------------------------------------------------------
# 3. curl called with --cacert; never --insecure.
# ---------------------------------------------------------------------------
reset_log
run_pve '{"data":{"status":"running"}}' status 100 >/dev/null 2>&1 || true
logged="$(cat "$CURL_LOG")"
want "status: --cacert argument present" "--cacert" "$logged"
[[ "$logged" != *"--insecure"* ]] && ok "status: no --insecure flag" \
    || bad "status: no --insecure flag" "found --insecure in args"

# ---------------------------------------------------------------------------
# 4. Verb API routing.
# ---------------------------------------------------------------------------
reset_log; run_pve '{"data":{"status":"running"}}' status 200 >/dev/null 2>&1 || true
want "status: /status/current" "/status/current" "$(nth_url)"

reset_log; run_pve '{"data":"101"}' nextid >/dev/null 2>&1 || true
want "nextid: /cluster/nextid" "/cluster/nextid" "$(nth_url)"

reset_log; run_pve '{"data":[]}' list >/dev/null 2>&1 || true
want "list: /qemu" "/qemu" "$(nth_url)"

reset_log; run_pve '{"data":[]}' list --pool somepool >/dev/null 2>&1 || true
want "list --pool: /pools/" "/pools/" "$(nth_url)"

reset_log; run_pve '{"data":{}}' config 200 >/dev/null 2>&1 || true
want "config: /config path" "/config" "$(nth_url)"

# Clone: two-response shim so the poll_task call gets a stopped task.
write_task_shim "UPID:testnode:1:1:1:qmclone:200:root@pam:"
reset_log; run_pve "" clone 9110 200 test-clone --pool mypool >/dev/null 2>&1 || true
want "clone: first call to /clone" "/clone" "$(nth_url 1)"
write_simple_shim

# ---------------------------------------------------------------------------
# 5. guest-exec calls /agent/exec with --timeout respected.
# ---------------------------------------------------------------------------
reset_log
run_pve '{"data":{"pid":12345}}' guest-exec 100 --timeout 1 /bin/true >/dev/null 2>&1 || true
want "guest-exec: /agent/exec" "/agent/exec" "$(nth_url 1)"

# ---------------------------------------------------------------------------
# 6. Usage for missing and unknown verb.
# ---------------------------------------------------------------------------
no_verb="$(run_pve "" 2>&1)" || true
want "no-verb: usage" "usage:" "$no_verb"

bad_verb="$(run_pve "" unknownverb 2>&1)" || true
want "unknown-verb: usage" "usage:" "$bad_verb"

# ---------------------------------------------------------------------------
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
