#!/usr/bin/env bash
#
# test-doctor-aws.sh — doctor.sh detects the AWS CLI and requires v2.
#
# POSITIVE CONTROL FIRST. A fake aws reporting v1 must produce a WARN line before the
# passing cases are trusted. This satisfies law-a-regression-test-must-be-seen-to-fail
# and law-absence-needs-a-positive-control.
#
# WHAT IS TESTED
# 1. POSITIVE CONTROL: aws v1 produces a warn line — the defect visible before the fix.
# 2. Absent aws: doctor.sh produces a warn line naming the dependency.
# 3. aws v1 present: doctor.sh produces a warn line naming the version requirement.
# 4. aws v2 present: doctor.sh produces an ok line reporting the version.
#
# PATH NOTE. conf.sh (sourced by doctor.sh) rebuilds PATH as:
#   ${SPIRA_PATH:+$SPIRA_PATH:}$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin
# The initial PATH set by env -i is overwritten. Two consequences:
#   • bd/systemctl fakes go in $BIN; SPIRA_PATH=$BIN keeps them in the rebuilt PATH.
#   • aws stubs go in $HOME/.local/bin (= $TMP/home/.local/bin), which conf.sh places
#     before /usr/local/bin. This shadows any real aws installed in the testenv image.
# For the "absent aws" scenario the stub returns no output, so doctor.sh falls to the
# v-unknown WARN path — functionally equivalent to truly absent for all test assertions.
#
# covers: spira/doctor.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-aws.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/db/.beads" "$TMP/run"

BIN="$TMP/bin"
mkdir -p "$BIN"

# HOME_BIN: the directory conf.sh places before /usr/local/bin.
# aws stubs go here to shadow any real aws in the testenv image.
HOME_BIN="$TMP/home/.local/bin"
mkdir -p "$HOME_BIN"

# Fake bd: handles list and migrate schema so doctor.sh does not fail on unrelated sections.
cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

# Fake systemctl: everything active/disabled so unrelated checks pass.
cat > "$BIN/systemctl" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*) printf 'active\n' ;;
    *"is-enabled"*) printf 'disabled\n'; exit 1 ;;
    *"list-unit-files"*|*"list-units"*|*"list-timers"*) true ;;
esac
exit 0
FAKESCRIPT
chmod +x "$BIN/systemctl"

# make_aws VERSION — write a stub aws to $HOME_BIN/aws reporting the given version.
# Placed in HOME_BIN so it precedes /usr/local/bin after conf.sh rebuilds PATH.
make_aws() {
    local ver="$1"
    cat > "$HOME_BIN/aws" <<STUB
#!/usr/bin/env bash
case "\$*" in
    "--version") printf 'aws-cli/%s Python/3.12.7 Linux/6.1.0 exe/x86_64\n' "$ver"; exit 0 ;;
    *) exit 0 ;;
esac
STUB
    chmod +x "$HOME_BIN/aws"
}

# make_aws_absent — write a stub that returns no version output, simulating an
# unusable aws. Used in place of removing the stub so that any real aws installed
# in the testenv image (at /usr/local/bin/aws) is shadowed rather than exposed.
make_aws_absent() {
    printf '#!/bin/sh\n' > "$HOME_BIN/aws"
    chmod +x "$HOME_BIN/aws"
}

# run_doctor — run doctor.sh in a clean env.
# SPIRA_PATH=$BIN keeps bd and systemctl fakes in PATH after conf.sh's rebuild.
# HOME=$TMP/home means conf.sh prepends $TMP/home/.local/bin (= HOME_BIN) to PATH.
run_doctor() {
    env -i \
        HOME="$TMP/home" \
        SPIRA_PATH="$BIN" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_DOCTOR_INSTALLING=1 \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        bash "$HERE/doctor.sh" 2>&1 || true
}

# ============================================================================
echo
echo "positive control — aws v1 produces a warn line:"
# ============================================================================
make_aws "1.29.0"
out="$(run_doctor)"
want "POSITIVE CONTROL: v1 produces warn" "warn" "$out"
want "POSITIVE CONTROL: warn mentions v1" "v1" "$out"
nowant "POSITIVE CONTROL: no ok for aws v1" "ok    aws" "$out"

# ============================================================================
echo
echo "absent aws — warn line names the dependency:"
# ============================================================================
# A no-output stub shadows any real aws at /usr/local/bin/aws. Doctor.sh falls
# to the v-unknown WARN path, which is functionally equivalent to truly absent.
make_aws_absent
out="$(run_doctor)"
want "absent aws: warn line" "warn" "$out"
want "absent aws: names aws" "aws" "$out"
nowant "absent aws: no ok for aws" "ok    aws" "$out"

# ============================================================================
echo
echo "aws v1 present — warn names the version requirement:"
# ============================================================================
make_aws "1.29.0"
out="$(run_doctor)"
want "aws v1: warn line" "warn" "$out"
want "aws v1: mentions v2 requirement" "v2" "$out"

# ============================================================================
echo
echo "aws v2 present — ok line reports version:"
# ============================================================================
make_aws "2.37.1"
out="$(run_doctor)"
want "aws v2: ok line" "ok    aws" "$out"
want "aws v2: shows version" "aws-cli/2" "$out"
nowant "aws v2: no warn for aws" "warn  aws" "$out"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
