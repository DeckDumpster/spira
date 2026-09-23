#!/usr/bin/env bash
# git-credential-app.sh — git credential helper for GitHub App identity.
# Mints a GitHub App installation token and outputs it in git credential format.
#
# Usage: git -c credential.helper=/path/to/git-credential-app.sh push ...
#
# Credentials are read from: SPIRA_GH_APP_ID, SPIRA_GH_APP_INSTALLATION_ID,
# SPIRA_GH_APP_KEY (file path) or SPIRA_GH_APP_PRIVATE_KEY (PEM inline).
# Falls back to SPIRA_GH_APP_CONFIG (default ~/.config/spira/github-app.env).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

# Drain stdin; git sends context (key=value lines, blank-line-terminated) on every call.
while IFS= read -r _line && [ -n "$_line" ]; do :; done

[ "${1:-}" = "get" ] || exit 0

# Load credentials from config file when not already in the environment.
_conf="${SPIRA_GH_APP_CONFIG:-$HOME/.config/spira/github-app.env}"
if [ -f "$_conf" ]; then
    # shellcheck disable=SC1090
    . "$_conf" 2>/dev/null || true
fi

_app_id="${SPIRA_GH_APP_ID:-}"
_install_id="${SPIRA_GH_APP_INSTALLATION_ID:-}"
[ -n "$_app_id" ]     || { printf 'git-credential-app: SPIRA_GH_APP_ID not set\n' >&2; exit 1; }
[ -n "$_install_id" ] || { printf 'git-credential-app: SPIRA_GH_APP_INSTALLATION_ID not set\n' >&2; exit 1; }

_key_file=""
_key_tmp=""
if [ -n "${SPIRA_GH_APP_KEY:-}" ] && [ -f "$SPIRA_GH_APP_KEY" ]; then
    _key_file="$SPIRA_GH_APP_KEY"
elif [ -n "${SPIRA_GH_APP_PRIVATE_KEY:-}" ]; then
    _key_tmp="$(mktemp)"
    printf '%s' "$SPIRA_GH_APP_PRIVATE_KEY" > "$_key_tmp"
    _key_file="$_key_tmp"
    trap 'rm -f "$_key_tmp"' EXIT
else
    printf 'git-credential-app: no key (set SPIRA_GH_APP_KEY or SPIRA_GH_APP_PRIVATE_KEY)\n' >&2
    exit 1
fi

_b64url() { base64 -w0 | tr '+/' '-_' | tr -d '='; }
_now="$(date +%s)"
_iat="$((_now - 60))"
_exp="$((_now + 540))"
_hdr="$(printf '{"alg":"RS256","typ":"JWT"}' | _b64url)"
_pay="$(printf '{"iat":%d,"exp":%d,"iss":"%s"}' "$_iat" "$_exp" "$_app_id" | _b64url)"
_sig="$(printf '%s' "${_hdr}.${_pay}" | openssl dgst -sha256 -sign "$_key_file" -binary | _b64url)"
_jwt="${_hdr}.${_pay}.${_sig}"

_tok="$(curl -sf --max-time 30 \
    -X POST \
    -H "Authorization: Bearer $_jwt" \
    -H "Accept: application/vnd.github.v3+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "https://api.github.com/app/installations/${_install_id}/access_tokens" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])' 2>/dev/null)" \
    || { printf 'git-credential-app: token request failed\n' >&2; exit 1; }
[ -n "$_tok" ] || { printf 'git-credential-app: empty token from API\n' >&2; exit 1; }

printf 'protocol=https\nhost=github.com\nusername=x-access-token\npassword=%s\n' "$_tok"
