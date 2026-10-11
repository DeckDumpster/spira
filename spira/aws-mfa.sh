#!/usr/bin/env bash
# aws-mfa.sh — exchange an MFA code for a one-hour role session.
#
# usage: aws-mfa.sh <6-digit code>
# env (all required, no fallback): SPIRA_AWS_BASE_PROFILE SPIRA_AWS_SESSION_PROFILE
#   SPIRA_AWS_ROLE_ARN SPIRA_AWS_MFA_SERIAL SPIRA_AWS_REGION
# The base profile holds a key that may only assume the role, and only with MFA. Temporary
# credentials are written to the session profile; the expiry is printed, the credentials never.
set -euo pipefail
code="${1:?usage: aws-mfa.sh <6-digit code>}"
case "$code" in [0-9][0-9][0-9][0-9][0-9][0-9]) ;; *) echo "aws-mfa.sh: code must be 6 digits" >&2; exit 2 ;; esac
for v in SPIRA_AWS_BASE_PROFILE SPIRA_AWS_SESSION_PROFILE SPIRA_AWS_ROLE_ARN SPIRA_AWS_MFA_SERIAL SPIRA_AWS_REGION; do
    [ -n "${!v:-}" ] || { echo "aws-mfa.sh: $v is not set" >&2; exit 2; }
done
out="$(aws sts assume-role --profile "$SPIRA_AWS_BASE_PROFILE" --region "$SPIRA_AWS_REGION" \
    --role-arn "$SPIRA_AWS_ROLE_ARN" --role-session-name spira-mfa \
    --serial-number "$SPIRA_AWS_MFA_SERIAL" --token-code "$code" --duration-seconds 3600 --output json)"
read -r k s t e < <(printf '%s' "$out" | python3 -I -c 'import json,sys;c=json.load(sys.stdin)["Credentials"];print(c["AccessKeyId"],c["SecretAccessKey"],c["SessionToken"],c["Expiration"])')
p="$SPIRA_AWS_SESSION_PROFILE"
aws configure set aws_access_key_id "$k" --profile "$p"
aws configure set aws_secret_access_key "$s" --profile "$p"
aws configure set aws_session_token "$t" --profile "$p"
aws configure set region "$SPIRA_AWS_REGION" --profile "$p"
echo "$p session valid until $e"
