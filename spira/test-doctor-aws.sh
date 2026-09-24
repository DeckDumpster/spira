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

# make_aws VERSION — write a stub aws to $BIN/aws that reports the given version string.
make_aws() {
    local ver="$1"
    cat > "$BIN/aws" <<STUB
#!/usr/bin/env bash
case "\$*" in
    "--version") printf 'aws-cli/%s Python/3.12.7 Linux/6.1.0 exe/x86_64\n' "$ver"; exit 0 ;;
    *) exit 0 ;;
esac
STUB
    chmod +x "$BIN/aws"
}

# run_doctor — run doctor.sh in a clean env with BIN prepended.
run_doctor() {
    env -i \
        PATH="$BIN:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_DOCTOR_INSTALLING=1 \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
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
rm -f "$BIN/aws"
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
