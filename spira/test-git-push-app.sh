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
# 8. (retired, sp-uwhx0) — every script this once grepped is either Rust now or deleted;
#    see the section itself for why.
#
# tier: T1
# covers: spira/git-credential-app.sh spira/lib.sh landing-pass/src/* sending/src/* queue/src/* spira/conf.sh
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
# conf.sh keeps the caller's PATH first and only APPENDS SPIRA_PATH (sp-gypjk), so a stub
# goes first on the PATH this suite hands the run.
TOOLS="$(dirname "$(command -v spira-config)"):$HERE"

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

tl_config SPIRA_GH_APP_ID=12345 SPIRA_GH_APP_INSTALLATION_ID=67890 SPIRA_GH_APP_KEY="$TMP/fake-key.pem"
out="$(env -i \
    HOME="$TMP" \
    PATH="$BIN:$TOOLS:/usr/bin:/bin" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_CONF=/nonexistent \
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
tl_config SPIRA_GH_APP_ID="" SPIRA_GH_APP_INSTALLATION_ID="" SPIRA_GH_APP_KEY=""
out_nc="$(env -i \
    HOME="$TMP" \
    PATH="$BIN:$TOOLS:/usr/bin:/bin" \
    SPIRA_TOML="$SPIRA_TOML" \
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
    out_noop="$(env -i HOME="$TMP" PATH="$BIN:$TOOLS:/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF=/nonexistent \
        bash "$CRED_HELPER" "$action" 2>&1)" || rc_noop=$?
    wantrc "$action exits 0" 0 "$rc_noop"
    is     "$action emits nothing" "" "$out_noop"
done

# =========================================================================
echo
echo "5. spira_git_push — adds credential.helper and URL rewriting when App creds are set"
# =========================================================================
# Stub git records all arguments; it is first on PATH (conf.sh no longer rewrites PATH).
# We clear the args file inside the subshell AFTER lib.sh is sourced
# so only the spira_git_push call is captured, not conf.sh's git calls.
cat > "$BIN/git" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$@" >> "$TMP/git-args"
exit 0
EOF
chmod +x "$BIN/git"

rm -f "$TMP/git-args"
tl_config SPIRA_RUN="$TMP/run" SPIRA_GH_APP_ID=99999 SPIRA_GH_APP_INSTALLATION_ID=11111
(
    export PATH="$BIN:$PATH"
    export SPIRA_HOME="$HERE/.."
    export SPIRA_CONF=/nonexistent
    export SPIRA_REPO="$TMP/fake-repo"
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
tl_config SPIRA_RUN="$TMP/run" SPIRA_GH_APP_ID="" SPIRA_GH_APP_INSTALLATION_ID=""
(
    export PATH="$BIN:$PATH"
    export SPIRA_HOME="$HERE/.."
    export SPIRA_CONF=/nonexistent
    export SPIRA_REPO="$TMP/fake-repo"
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
# sp-g3uwp: conf.sh no longer carries its allowlist as literal text — a key's membership
# is now the existence of its own file under conf.d/, which conf-gen.sh derives the
# allowlist from directly.
for key in SPIRA_GH_APP_ID SPIRA_GH_APP_INSTALLATION_ID SPIRA_GH_APP_KEY \
           SPIRA_GH_APP_PRIVATE_KEY SPIRA_GH_APP_CONFIG; do
    if [ -f "$HERE/conf.d/$key" ]; then
        ok "$key in conf.sh"
    else
        bad "$key in conf.sh" "not found"
    fi
done

# =========================================================================
# 8. STATIC CHECK — covered scripts use spira_git_push — RETIRED (sp-uwhx0). Its own
# subjects: landing.sh (already handled above — the landing-pass binary, pushes through its
# own seam, source-grep cannot see it), sending.sh (now the `sending` binary, sp-arpjt),
# queue.sh/verdict.sh (now the `queue` binary, sp-flj4a; verdict runs in process inside it),
# and batch.sh (deleted, sp-uwhx0 — the batcher crate cuts and the queue binary pushes the
# batch now). Every one of them is either Rust — a source grep cannot see through a seam
# regardless of language — or gone outright. Nothing left for a bash grep to check.
# =========================================================================

tl_summary
