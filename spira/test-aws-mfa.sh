#!/usr/bin/env bash
# test-aws-mfa.sh — aws-mfa.sh refuses missing config or a malformed code, writes the session
# profile from the assume-role reply, and never prints the credentials.
#
# tier: T1
# covers: spira/aws-mfa.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
echo "test-aws-mfa.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
cat > "$TMP/aws" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$FIX/calls"
case "$1 $2" in
    "sts assume-role") echo '{"Credentials":{"AccessKeyId":"AKIAZZ","SecretAccessKey":"SECRETZZ","SessionToken":"TOKENZZ","Expiration":"2030-01-01T00:00:00Z"}}' ;;
esac
STUB
chmod +x "$TMP/aws"
export FIX="$TMP" PATH="$TMP:$PATH"
envset() { env -i PATH="$PATH" FIX="$FIX" SPIRA_AWS_BASE_PROFILE=zbase SPIRA_AWS_SESSION_PROFILE=zsess \
    SPIRA_AWS_ROLE_ARN=arn:aws:iam::000000000000:role/zrole SPIRA_AWS_MFA_SERIAL=arn:aws:iam::000000000000:mfa/zdev \
    SPIRA_AWS_REGION=zz-test-9 "$@"; }

out="$(envset bash "$HERE/aws-mfa.sh" 123456 2>&1)"; rc=$?
wantrc "valid code succeeds" 0 "$rc"
want "prints profile and expiry" "zsess session valid until 2030-01-01T00:00:00Z" "$out"
nowant "never prints credentials" SECRETZZ "$out"
want "assumes configured role" "--role-arn arn:aws:iam::000000000000:role/zrole" "$(cat "$TMP/calls")"
want "uses configured MFA serial" "--serial-number arn:aws:iam::000000000000:mfa/zdev" "$(cat "$TMP/calls")"
want "writes token to session profile" "configure set aws_session_token TOKENZZ --profile zsess" "$(cat "$TMP/calls")"
want "writes configured region" "configure set region zz-test-9 --profile zsess" "$(cat "$TMP/calls")"

: > "$TMP/calls"
envset bash "$HERE/aws-mfa.sh" 12345 >/dev/null 2>&1; wantrc "short code refused" 2 "$?"
envset bash "$HERE/aws-mfa.sh" abcdef >/dev/null 2>&1; wantrc "non-numeric code refused" 2 "$?"
env -i PATH="$PATH" FIX="$FIX" bash "$HERE/aws-mfa.sh" 123456 >/dev/null 2>&1; wantrc "unset config refused" 2 "$?"
is "refusals call aws never" "" "$(cat "$TMP/calls")"
