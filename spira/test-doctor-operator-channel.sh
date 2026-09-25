#!/usr/bin/env bash
#
# test-doctor-operator-channel.sh — doctor.sh's doctor_check_operator_channel: inotifywait,
# the configured mail client, hunk and go are FAIL on an operated instance, not the "minor,
# optional" warning doctor.sh used to give a box whose whole operator channel was dead
# (sp-u3pb6).
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL. hunk is absent from every environment this suite runs in (waived from
#    the testenv image as an operator-only tool no suite invokes). With COCKPIT_SESSIONS
#    naming hunk and SPIRA_OPERATED=1 (the default), absence must FAIL, not warn — proving
#    the check can find something before the clean case below is trusted.
# 2. SPIRA_OPERATED=0 downgrades the same absence to warn: a headless fixture declares
#    itself honestly instead of tripping a false alarm.
# 3. STUB PRESENT. A stubbed hunk on PATH reads as ok — the check is not always-FAIL.
# 4. COCKPIT_MAIL names the CONFIGURED client. A present, non-default client (mutt) is ok;
#    aerc's own presence or absence is never inspected once a different client is set.
# 5. inotifywait and aerc, both installed wherever this suite runs (real packages, not
#    stubs), read as ok. This is the suite's positive control for the "present" path on the
#    two tools this suite cannot fake absent: conf.sh always appends the box's real
#    /usr/bin ahead of nothing, so a system package already installed cannot be hidden from
#    inside a suite that needs the rest of doctor.sh's real PATH resolution to run at all.
# 6. GO FAILS exactly on the case that matters: bd mismatched AND no prebuilt for this
#    architecture. A stubbed uname reports an unsupported architecture and a stubbed bd
#    reports the wrong version; the FAIL names the install hint. The inverse — a matching
#    bd, or a supported architecture — is silent ok (checked against this suite's real bd
#    and real architecture).
#
# covers: spira/doctor.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-operator-channel.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home/.local/bin" "$TMP/run"

run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_DOCTOR_INSTALLING=1 \
        "$@" \
        bash "$HERE/doctor.sh" 2>&1
}
op_section() { sed -n '/^operator channel$/,/^$/p' <<< "$1"; }

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — hunk absent, SPIRA_OPERATED=1: FAILs, names the consequence:"
# ==========================================================================
out1="$(run_doctor SPIRA_OPERATED=1 "COCKPIT_SESSIONS=brain hunk chat" || true)"
sec1="$(op_section "$out1")"
want "positive control: FAIL fires" "FAIL" "$sec1"
want "positive control: names hunk" "hunk" "$sec1"
want "positive control: names the consequence" "review pane is unavailable" "$sec1"
want "positive control: carries the install hint" "npm install -g --prefix" "$sec1"

# ==========================================================================
echo
echo "2. SPIRA_OPERATED=0 — same absence downgrades to warn, not FAIL:"
# ==========================================================================
out2="$(run_doctor SPIRA_OPERATED=0 "COCKPIT_SESSIONS=brain hunk chat" || true)"
sec2="$(op_section "$out2")"
want   "operated=0: hunk absence is warn" "warn  hunk" "$sec2"
nowant "operated=0: hunk absence is not FAIL" "FAIL  hunk" "$sec2"

# ==========================================================================
echo
echo "3. STUB PRESENT — a stubbed hunk on PATH reads as ok:"
# ==========================================================================
printf '#!/bin/sh\n' > "$TMP/home/.local/bin/hunk"
chmod +x "$TMP/home/.local/bin/hunk"
out3="$(run_doctor SPIRA_OPERATED=1 "COCKPIT_SESSIONS=brain hunk chat" || true)"
sec3="$(op_section "$out3")"
rm -f "$TMP/home/.local/bin/hunk"
want   "stub present: hunk ok" "ok    hunk" "$sec3"
nowant "stub present: no FAIL naming hunk" "FAIL  hunk" "$sec3"

# ==========================================================================
echo
echo "4. COCKPIT_MAIL names the configured client, not literally aerc:"
# ==========================================================================
printf '#!/bin/sh\n' > "$TMP/home/.local/bin/mutt"
chmod +x "$TMP/home/.local/bin/mutt"
out4="$(run_doctor SPIRA_OPERATED=1 COCKPIT_MAIL=mutt || true)"
sec4="$(op_section "$out4")"
rm -f "$TMP/home/.local/bin/mutt"
want   "COCKPIT_MAIL=mutt present: ok" "ok    mutt" "$sec4"
nowant "COCKPIT_MAIL=mutt present: aerc is never inspected" "aerc" "$sec4"

# ==========================================================================
echo
echo "5. inotifywait and aerc, real packages present wherever this suite runs: ok:"
# ==========================================================================
out5="$(run_doctor SPIRA_OPERATED=1 || true)"
sec5="$(op_section "$out5")"
want "inotifywait present: ok" "ok    inotifywait" "$sec5"
want "aerc present (default COCKPIT_MAIL): ok" "ok    aerc" "$sec5"

# ==========================================================================
echo
echo "6a. GO — mismatched bd, unsupported architecture: FAILs, names the install hint:"
# ==========================================================================
STUBBD="$TMP/stubbd"; mkdir -p "$STUBBD"
cat > "$STUBBD/bd" <<'FAKEBD'
#!/usr/bin/env bash
case "$*" in
    version) echo "bd version 0.0.1" ;;
    *) exit 0 ;;
esac
FAKEBD
chmod +x "$STUBBD/bd"
cat > "$TMP/home/.local/bin/uname" <<'FAKEUNAME'
#!/usr/bin/env bash
[ "$1" = -m ] && { echo sparc64; exit 0; }
exec /usr/bin/uname "$@"
FAKEUNAME
chmod +x "$TMP/home/.local/bin/uname"
out6a="$(run_doctor SPIRA_OPERATED=1 "SPIRA_BD=$STUBBD/bd" || true)"
sec6a="$(op_section "$out6a")"
want "go FAIL: mismatch + unsupported arch fails" "FAIL  go" "$sec6a"
want "go FAIL: names the install hint" "go.dev/dl" "$sec6a"
rm -f "$TMP/home/.local/bin/uname"

# ==========================================================================
echo
echo "6b. GO — this suite's real bd and real architecture: silent ok, never FAIL:"
# ==========================================================================
out6b="$(run_doctor SPIRA_OPERATED=1 || true)"
sec6b="$(op_section "$out6b")"
nowant "go: real bd/arch never FAILs" "FAIL  go" "$sec6b"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
