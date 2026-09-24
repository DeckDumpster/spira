#!/usr/bin/env bash
# app-token.sh — mint a GitHub App installation token via JWT + GitHub API.
# For CI use; the broker binary is the canonical minter for server-side scripts.
#
# Reads from environment:
#   SPIRA_GH_APP_ID              numeric App ID
#   SPIRA_GH_APP_INSTALLATION_ID numeric installation ID
#   SPIRA_GH_APP_KEY             path to RSA private key PEM (takes precedence)
#   SPIRA_GH_APP_PRIVATE_KEY     PEM content as a string (used when KEY path absent)
#
# Prints the installation token to stdout. Never logs the key or the token.
# Requires: openssl, curl, python3 (or jq).
set -uo pipefail

_app_id="${SPIRA_GH_APP_ID:-}"
_install_id="${SPIRA_GH_APP_INSTALLATION_ID:-}"

[ -n "$_app_id" ]     || { printf 'app-token.sh: SPIRA_GH_APP_ID is required\n' >&2;              exit 1; }
[ -n "$_install_id" ] || { printf 'app-token.sh: SPIRA_GH_APP_INSTALLATION_ID is required\n' >&2; exit 1; }

# Key: file path takes precedence over inline content.
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
    printf 'app-token.sh: no key (set SPIRA_GH_APP_KEY or SPIRA_GH_APP_PRIVATE_KEY)\n' >&2
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

curl -sf --max-time 30 \
    -X POST \
    -H "Authorization: Bearer $_jwt" \
    -H "Accept: application/vnd.github.v3+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "https://api.github.com/app/installations/${_install_id}/access_tokens" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])'
