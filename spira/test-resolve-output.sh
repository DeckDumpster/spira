#!/usr/bin/env bash
#
# test-resolve-output.sh — failed bd close must surface bd's own complaint to the caller.
#
#   ./test-resolve-output.sh
#
# THE DEFECT THIS REPRODUCES (sp-ve5s). cockpit/resolve.sh discarded both stdout and stderr
# of the inner `bd close` invocation, so a database-wide write refusal — bd exits 0 and
# prints the complaint to STDOUT — appeared only as "failed to close <id> in <db>" with no
# cause. A five-turn hunt traced the real message; this suite ensures it propagates.
#
# THREE CASES (law-absence-needs-a-positive-control):
#   1. bd fails with a known complaint on stdout → resolve.sh surfaces that text on stderr.
#   2. bd fails with a known complaint on stderr → resolve.sh surfaces that text on stderr.
#   3. bd succeeds → resolve.sh exits 0 and emits no failure text (healthy path stays silent).
#
# Driven through BD_BIN and COCKPIT_DB overrides — no database build, under a second.
#
# covers: cockpit/resolve.sh
# defect: sp-ve5s
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(cd "$HERE/../cockpit" && pwd)"
pass=0; fail=0

ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A GUARD ON THE GUARD: the script must exist or a missing file gives a false pass via a
# different error path.
[ -f "$COCKPIT/resolve.sh" ] || {
  echo "SKIP: cockpit/resolve.sh not found at $COCKPIT/resolve.sh" >&2; exit 77
}

# Two separate paths: COCKPIT_DB has .beads/ so cockpit_db() accepts it; SPIRA_DB has no
# .beads/ so conf.sh's schema-migration check (line 779: `if [ -d "${SPIRA_DB:-}/.beads" ]`)
# is skipped entirely. Both paths matter — mixing them triggers the migration guard against a
# directory that has no real beads project and fails every case before the stub bd is reached.
DB="$TMP/testdb"
SPIRA_DB_PATH="$TMP/spira-nobeads"
mkdir -p "$DB/.beads" "$SPIRA_DB_PATH"

# resolve() — invoke resolve.sh with a given stub bd binary, capture its combined stderr.
# stdout is discarded (we only care about the error path). Exit code is in $rc after return.
run_resolve() {
  local stub="$1" id="${2:-sp-test-id}" reason="${3:-close reason}"
  local rc=0
  RESULT=$(
    BD_BIN="$stub" \
    COCKPIT_DB="$DB" \
    SPIRA_DB="$SPIRA_DB_PATH" \
    SELF_CLOSED="$TMP/self-closed-$$" \
    bash "$COCKPIT/resolve.sh" "$id" "$reason" 2>&1 >/dev/null
  ) || rc=$?
  return "$rc"
}

# ======================================================================================
echo
echo "CASE 1: bd exits nonzero with complaint on STDOUT — resolve.sh must surface it:"
# ======================================================================================
COMPLAINT="refusing to auto-apply 8 pending schema migrations to a remote-backed database"
BD_STDOUT="$TMP/bin/bd-fail-stdout"
mkdir -p "$TMP/bin"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "%s"\nexit 1\n' "$COMPLAINT" > "$BD_STDOUT"
chmod +x "$BD_STDOUT"

run_resolve "$BD_STDOUT" && rc1=0 || rc1=$?

if [ "$rc1" -ne 0 ]; then
  ok "resolve.sh exits nonzero when bd fails"
else
  bad "resolve.sh exits nonzero when bd fails" "expected nonzero, got 0"
fi
if printf '%s' "$RESULT" | grep -qF "failed to close"; then
  ok "resolve.sh prints its own failure header"
else
  bad "resolve.sh prints its own failure header" "stderr: $RESULT"
fi
if printf '%s' "$RESULT" | grep -qF "$COMPLAINT"; then
  ok "bd's stdout complaint reaches the caller's stderr"
else
  bad "bd's stdout complaint reaches the caller's stderr" "stderr: $RESULT"
fi

# ======================================================================================
echo
echo "CASE 2: bd exits nonzero with complaint on STDERR — resolve.sh must surface it:"
# ======================================================================================
COMPLAINT2="bd: unknown command: close"
BD_STDERR="$TMP/bin/bd-fail-stderr"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "%s" >&2\nexit 1\n' "$COMPLAINT2" > "$BD_STDERR"
chmod +x "$BD_STDERR"

run_resolve "$BD_STDERR" && rc2=0 || rc2=$?

if printf '%s' "$RESULT" | grep -qF "$COMPLAINT2"; then
  ok "bd's stderr complaint reaches the caller's stderr"
else
  bad "bd's stderr complaint reaches the caller's stderr" "stderr: $RESULT"
fi

# ======================================================================================
echo
echo "CASE 3: bd succeeds — resolve.sh exits 0, no failure text (healthy path stays silent):"
# ======================================================================================
BD_OK="$TMP/bin/bd-ok"
printf '#!/usr/bin/env bash\nprintf "Closed.\\n"\nexit 0\n' > "$BD_OK"
chmod +x "$BD_OK"

stdout_out=$(
  BD_BIN="$BD_OK" \
  COCKPIT_DB="$DB" \
  SPIRA_DB="$SPIRA_DB_PATH" \
  SELF_CLOSED="$TMP/self-closed-ok" \
  bash "$COCKPIT/resolve.sh" sp-test-id "close reason" 2>/dev/null
) && rc3=0 || rc3=$?

if [ "$rc3" -eq 0 ]; then
  ok "resolve.sh exits 0 on success"
else
  bad "resolve.sh exits 0 on success" "exit code: $rc3"
fi
if ! printf '%s' "$stdout_out" | grep -qF "failed"; then
  ok "no failure text on the healthy path"
else
  bad "no failure text on the healthy path" "output: $stdout_out"
fi
if printf '%s' "$stdout_out" | grep -qF "resolved"; then
  ok "success message is present on the healthy path"
else
  bad "success message is present on the healthy path" "stdout: $stdout_out"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
