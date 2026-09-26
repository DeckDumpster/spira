#!/usr/bin/env bash
# test-git-push-app.sh — git pushes use the App credential helper when configured.
#
# WHAT THIS COVERS
# 1. git-credential-app.sh exists and is executable (positive control).
# 2. The helper outputs the correct git credential protocol format with stub creds.
# 3. The helper exits non-zero when credentials are absent (positive control for the check).
# 4. store/erase actions are no-ops (transient tokens must not be cached by git).
# 5. spira_git_push adds credential.helper and URL rewriting when App creds are set.
# 6. spira_git_push passes through as a plain git push when no creds are configured.
# 7. SPIRA_GH_APP_* keys are in the conf.sh allowlist.
# 8. The covered scripts call spira_git_push rather than bare git push.
#
# covers: spira/git-credential-app.sh spira/lib.sh spira/landing.sh spira/batch.sh spira/sending.sh spira/verdict.sh spira/queue.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-git-push-app.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

CRED_HELPER="$HERE/git-credential-app.sh"
LIB="$HERE/lib.sh"
CONF_SH="$HERE/conf.sh"
BIN="$TMP/bin"
mkdir -p "$BIN"

# =========================================================================
echo
echo "1. POSITIVE CONTROL — credential helper exists and is executable"
# =========================================================================
if [ -x "$CRED_HELPER" ]; then
    ok "git-credential-app.sh is executable"
else
    bad "git-credential-app.sh" "not found or not executable (positive control fails)"
    printf '\n0 passed, 1 failed\n'; exit 1
fi

# =========================================================================
echo
echo "2. CREDENTIAL HELPER — outputs correct format with stub credentials"
# =========================================================================
# Stub openssl: drains stdin, outputs a fake binary blob the b64url encoder accepts.
cat > "$BIN/openssl" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
printf 'fakesig'
EOF
chmod +x "$BIN/openssl"

# Stub curl: returns a valid JSON App token response, regardless of arguments.
cat > "$BIN/curl" <<'EOF'
#!/usr/bin/env bash
printf '{"token":"ghs_fakeAppToken9999","expires_at":"2099-01-01T00:00:00Z"}'
EOF
chmod +x "$BIN/curl"

printf 'fake-key-content\n' > "$TMP/fake-key.pem"

# SPIRA_PATH puts stub binaries first in PATH even after conf.sh rewrites PATH.
out="$(env -i \
    HOME="$TMP" \
    SPIRA_PATH="$BIN" \
    SPIRA_CONF=/nonexistent \
    SPIRA_GH_APP_ID=12345 \
    SPIRA_GH_APP_INSTALLATION_ID=67890 \
    SPIRA_GH_APP_KEY="$TMP/fake-key.pem" \
    bash "$CRED_HELPER" get 2>/dev/null)"
want "protocol=https in output"            "protocol=https"            "$out"
want "host=github.com in output"           "host=github.com"           "$out"
want "username=x-access-token in output"   "username=x-access-token"  "$out"
want "password=ghs_fakeAppToken in output" "password=ghs_fakeAppToken" "$out"

# =========================================================================
echo
echo "3. CREDENTIAL HELPER — exits non-zero without credentials"
# =========================================================================
# This proves the check is real: if SPIRA_GH_APP_ID is absent the helper
# must fail rather than silently emitting a blank or recycled credential.
rc_nc=0
out_nc="$(env -i \
    HOME="$TMP" \
    SPIRA_PATH="$BIN" \
    SPIRA_CONF=/nonexistent \
    bash "$CRED_HELPER" get 2>&1)" || rc_nc=$?
wantrc "no credentials → non-zero exit" 1 "$rc_nc"
want   "explains which var is missing"  "SPIRA_GH_APP_ID" "$out_nc"

# =========================================================================
echo
echo "4. CREDENTIAL HELPER — store and erase are no-ops (exit 0, no output)"
# =========================================================================
for action in store erase; do
    rc_noop=0
    out_noop="$(env -i HOME="$TMP" SPIRA_PATH="$BIN" SPIRA_CONF=/nonexistent \
        bash "$CRED_HELPER" "$action" 2>&1)" || rc_noop=$?
    wantrc "$action exits 0" 0 "$rc_noop"
    is     "$action emits nothing" "" "$out_noop"
done

# =========================================================================
echo
echo "5. spira_git_push — adds credential.helper and URL rewriting when App creds are set"
# =========================================================================
# Stub git records all arguments. SPIRA_PATH keeps the stub in PATH after conf.sh
# rewrites it. We clear the args file inside the subshell AFTER lib.sh is sourced
# so only the spira_git_push call is captured, not conf.sh's git calls.
cat > "$BIN/git" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$@" >> "$TMP/git-args"
exit 0
EOF
chmod +x "$BIN/git"

rm -f "$TMP/git-args"
(
    export SPIRA_PATH="$BIN"
    export SPIRA_HOME="$HERE/.."
    export SPIRA_CONF=/nonexistent
    export SPIRA_REPO="$TMP/fake-repo"
    export SPIRA_RUN="$TMP/run"
    export SPIRA_GH_APP_ID=99999
    export SPIRA_GH_APP_INSTALLATION_ID=11111
    # shellcheck disable=SC1090
    . "$LIB" 2>/dev/null || true
    rm -f "$TMP/git-args"
    spira_git_push "$TMP/fake-repo" -q origin main
) 2>/dev/null || true

args_with="$(cat "$TMP/git-args" 2>/dev/null)"
want "credential.helper flag present"     "credential.helper"              "$args_with"
want "URL rewriting flag present"         "insteadOf=git@github.com:"      "$args_with"
want "push is the subcommand"             "push"                           "$args_with"
want "original push args passed through"  "main"                           "$args_with"

# =========================================================================
echo
echo "6. spira_git_push — plain git push when App creds are not configured"
# =========================================================================
rm -f "$TMP/git-args"
(
    export SPIRA_PATH="$BIN"
    export SPIRA_HOME="$HERE/.."
    export SPIRA_CONF=/nonexistent
    export SPIRA_REPO="$TMP/fake-repo"
    export SPIRA_RUN="$TMP/run"
    unset SPIRA_GH_APP_ID           2>/dev/null || true
    unset SPIRA_GH_APP_INSTALLATION_ID 2>/dev/null || true
    # shellcheck disable=SC1090
    . "$LIB" 2>/dev/null || true
    rm -f "$TMP/git-args"
    spira_git_push "$TMP/fake-repo" -q origin main
) 2>/dev/null || true

args_without="$(cat "$TMP/git-args" 2>/dev/null)"
nowant "no credential.helper without App creds"  "credential.helper"  "$args_without"
nowant "no insteadOf without App creds"          "insteadOf"          "$args_without"
want "push still passes through"               "push"               "$args_without"

# =========================================================================
echo
echo "7. CONF.SH — App credential keys are in the allowlist"
# =========================================================================
for key in SPIRA_GH_APP_ID SPIRA_GH_APP_INSTALLATION_ID SPIRA_GH_APP_KEY \
           SPIRA_GH_APP_PRIVATE_KEY SPIRA_GH_APP_CONFIG; do
    if grep -q "$key" "$CONF_SH"; then
        ok "$key in conf.sh"
    else
        bad "$key in conf.sh" "not found"
    fi
done

# =========================================================================
echo
echo "8. STATIC CHECK — covered scripts use spira_git_push"
# =========================================================================
for f in landing.sh batch.sh sending.sh verdict.sh queue.sh; do
    if grep -qE 'spira_git_push' "$HERE/$f"; then
        ok "$f: spira_git_push present"
    else
        bad "$f: spira_git_push present" "not found"
    fi
    # No bare 'git ... push' should remain in the push paths of these files.
    bare="$(grep -nE '^\s+git\s+-C\s+\S+\s+push\b|^\s+git\s+push\b' "$HERE/$f" 2>/dev/null \
            | grep -v '^\s*#' | head -2 || true)"
    if [ -n "${bare:-}" ]; then
        bad "$f: no uncovered bare git push" "found: ${bare}"
    else
        ok "$f: no uncovered bare git push"
    fi
done

tl_summary
