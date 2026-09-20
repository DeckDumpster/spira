#!/usr/bin/env bash
# covers: spira/acceptance-run.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SCRIPT="$HERE/acceptance-run.sh"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { grep -qF  "$2" "$SCRIPT" && ok "$1" || bad "$1" "not found: [$2]"; }
wantre() { grep -qE  "$2" "$SCRIPT" && ok "$1" || bad "$1" "pattern not found: $2"; }

echo "test-acceptance-run.sh"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

echo
echo "1. POSITIVE CONTROL — acceptance-run.sh exists and is executable"

if [ -f "$SCRIPT" ] && [ -x "$SCRIPT" ]; then
    ok "acceptance-run.sh exists and is executable"
else
    bad "acceptance-run.sh exists and is executable" "missing or not executable"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

echo
echo "2. Structural: SPIRA_OPERATED=0 in each phase's install environment"

wantre "phase A _install_env includes SPIRA_OPERATED=0" \
    '_install_env=\(.*SPIRA_OPERATED=0'
wantre "phase B passes SPIRA_OPERATED=0 to install.sh directly" \
    'SPIRA_OPERATED=0 bash.*_prev_clone.*install\.sh'
wantre "phase D _aged_env includes SPIRA_OPERATED=0" \
    '_aged_env=\(.*SPIRA_OPERATED=0'
want "SPIRA_OPERATED = 0 written to spira.conf" \
    "SPIRA_OPERATED = 0"

echo
echo "3. Pair: stub doctor FAILs without SPIRA_OPERATED=0, passes with it"

# Stub doctor.sh: exits 1 and emits FAIL lines when SPIRA_OPERATED is not 0.
cat > "$SCRATCH/doctor.sh" <<'DOCTOR'
#!/usr/bin/env bash
if [ "${SPIRA_OPERATED:-1}" != "0" ]; then
    printf 'FAIL  aerc (COCKPIT_MAIL) is not on PATH\n'
    printf 'FAIL  hunk is not on PATH\n'
    exit 1
fi
exit 0
DOCTOR
chmod +x "$SCRATCH/doctor.sh"

# Stub install.sh: calls the stub doctor as preflight; refuses on non-zero.
cat > "$SCRATCH/install.sh" <<INSTALL
#!/usr/bin/env bash
_out="\$("$SCRATCH/doctor.sh" 2>&1)"
_rc=\$?
printf '%s\n' "\$_out"
[ "\$_rc" = 0 ] || { printf 'install: preflight failed\n' >&2; exit 1; }
printf 'install: ok\n'
exit 0
INSTALL
chmod +x "$SCRATCH/install.sh"

# Positive control: stub install refuses when SPIRA_OPERATED is unset.
_out="$(bash "$SCRATCH/install.sh" 2>&1)" && _rc=0 || _rc=$?
if [ "$_rc" -ne 0 ] && printf '%s' "$_out" | grep -qF 'COCKPIT_MAIL'; then
    ok "positive-control: stub install refuses when SPIRA_OPERATED unset (names operator tool)"
else
    bad "positive-control: stub install refuses when SPIRA_OPERATED unset" \
        "exit=$_rc"
fi

# With SPIRA_OPERATED=0, stub install proceeds.
_out2="$(SPIRA_OPERATED=0 bash "$SCRATCH/install.sh" 2>&1)" && _rc2=0 || _rc2=$?
if [ "$_rc2" -eq 0 ]; then
    ok "with SPIRA_OPERATED=0: stub install proceeds"
else
    bad "with SPIRA_OPERATED=0: stub install proceeds" \
        "exit=$_rc2 output=$_out2"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
