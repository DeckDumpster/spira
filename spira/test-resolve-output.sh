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
# tier: T1
# covers: cockpit/ops/src/resolve.rs UC-cockpit-observability-40
# defect: sp-ve5s
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
COCKPIT="$(cd "$HERE/../cockpit" && pwd)"
pass=0; fail=0


TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A GUARD ON THE GUARD: the binary must exist or a missing file gives a false pass via a
# different error path. `resolve` is a binary now (sp-llbmi), invoked by name from the
# tree's build on PATH.
command -v resolve >/dev/null 2>&1 || {
  echo "SKIP: 'resolve' binary not found on PATH" >&2; exit 77
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
  # COCKPIT_DB/SPIRA_DB are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG):
  # declare via tl_config, not the env prefix below, which no process reads any more.
  tl_config COCKPIT_DB="$DB" SPIRA_DB="$SPIRA_DB_PATH"
  # resolve closes through spira-lc (sp-3fue0j); with no lifecycle store here, that closes the
  # store through the case's stub bd, whose complaint must still reach the caller.
  lc_close_stub "$TMP/lc" "$stub" "$DB"
  RESULT=$(
    LC_STUB_NOROW=1 BD_BIN="$stub" \
    resolve "$id" "$reason" 2>&1 >/dev/null
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

# COCKPIT_DB/SPIRA_DB unchanged from run_resolve's tl_config declaration above; the close goes
# through spira-lc (sp-3fue0j), so its stand-in closes through this case's healthy bd.
lc_close_stub "$TMP/lc" "$BD_OK" "$DB"
stdout_out=$(
  LC_STUB_NOROW=1 BD_BIN="$BD_OK" \
  resolve sp-test-id "close reason" 2>/dev/null
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

# ======================================================================================
echo
echo "CASE 4: a work bead (has a lifecycle row) is refused, naming reply; bd is never called:"
# ======================================================================================
BD_MARK="$TMP/bd-called"
BD_TRAP="$TMP/bin/bd-trap"
printf '#!/usr/bin/env bash\ncase " $* " in *" show "*) exit 0;; esac\ntouch "%s"\nexit 0\n' "$BD_MARK" > "$BD_TRAP"
chmod +x "$BD_TRAP"
lc_close_stub "$TMP/lc" "$BD_TRAP" "$DB"
work_out=$(BD_BIN="$BD_TRAP" LC_STUB_ROW=1 resolve sp-test-id "close reason" 2>&1) && rc4=0 || rc4=$?
if [ "$rc4" -ne 0 ] && printf '%s' "$work_out" | grep -qF "reply" && [ ! -e "$BD_MARK" ]; then
  ok "work bead refused with reply named, nothing written"
else
  bad "work bead refused with reply named, nothing written" "rc=$rc4 out: $work_out"
fi

echo
tl_summary
