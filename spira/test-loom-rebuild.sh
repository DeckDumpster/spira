#!/usr/bin/env bash
#
# test-loom-rebuild.sh — layout.sh rebuild_loom_if_stale builds when source is newer
# than binary; restart_loom_if_stale restarts the service when binary is newer than
# the running process.
#
# THE FAILURE THIS SUITE EXISTS FOR. loom/target/release/loom is in .gitignore, so
# landing a source change ships nothing to the running server — the unit restarts into
# the same old binary forever. Without this check, a change to loom/src is invisible
# to the running process until someone hand-runs cargo, and the symptom ("change had
# no effect") is indistinguishable from a broken implementation.
#
# sp-vo6r
# tier: T1
# covers: cockpit/layout.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
LAYOUT="$HERE/../cockpit/layout.sh"


TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

MOCK_BIN="$TMP/mock-bin"
mkdir -p "$MOCK_BIN"

# Mock cargo: records the call and exits 0.
cat > "$MOCK_BIN/cargo" <<'MOCK'
#!/usr/bin/env bash
printf 'cargo %s\n' "$*" >> "${CARGO_LOG}"
exit 0
MOCK
chmod +x "$MOCK_BIN/cargo"

# Mock systemctl: records the call and emulates the queries restart_loom_if_stale uses.
cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf 'systemctl %s\n' "$*" >> "${SYSTEMCTL_LOG}"
case "$*" in
    *is-active*) printf 'active\n' ;;
    *MainPID*)   printf '%s\n' "${MOCK_MAIN_PID:-1}" ;;
    *cat*)       exit 0 ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

# Loom source tree fixture.
LOOM_SRC="$TMP/loom"
mkdir -p "$LOOM_SRC/src" "$LOOM_SRC/target/release"
LOOM_BIN="$LOOM_SRC/target/release/loom"

# Stub conf.sh layout.sh sources at startup. Pin every key to a non-default so no
# ambient spira.conf silently decides a verdict (law-gates-run-in-a-clean-environment).
mkdir -p "$TMP/cockpit" "$TMP/spira" "$TMP/.runtime"
cp "$LAYOUT" "$TMP/cockpit/layout.sh"
chmod +x "$TMP/cockpit/layout.sh"
cat > "$TMP/spira/conf.sh" <<CONF
SPIRA_HOME="$HERE"
SPIRA_COCKPIT="\${SPIRA_COCKPIT:-}"
SPIRA_PANEL="\${SPIRA_PANEL:-/dev/null}"
SPIRA_REPO="$TMP"
SPIRA_RUN="$TMP/.runtime"
SPIRA_LOOM_BIN="$LOOM_BIN"
COCKPIT_CWD="/tmp"
COCKPIT_BOTTOM_PCT=30
COCKPIT_RIGHT_PCT=33
COCKPIT_HEAL_COOLDOWN=60
SPIRA_TZ="UTC"
CONF

# Helper: invoke a function from layout.sh in a fresh subshell.
run_fn() {
    local fn="$1"
    > "$TMP/cargo.log"; > "$TMP/systemctl.log"
    env SPIRA_COCKPIT="$TMP/cockpit" \
        SPIRA_CONF="$TMP/no.conf" \
        PATH="$MOCK_BIN:$PATH" \
        CARGO_LOG="$TMP/cargo.log" \
        SYSTEMCTL_LOG="$TMP/systemctl.log" \
        MOCK_MAIN_PID="${MOCK_MAIN_PID:-1}" \
        bash -c ". '$TMP/cockpit/layout.sh' 2>/dev/null; $fn" 2>/dev/null || true
}

# ── POSITIVE CONTROL: source newer than binary → cargo must be called ─────────
echo "case: source newer than binary — build must fire"

printf 'content' > "$LOOM_SRC/Cargo.toml"
printf 'content' > "$LOOM_SRC/src/main.rs"
printf 'binary' > "$LOOM_BIN"; chmod +x "$LOOM_BIN"
touch -d 'now - 60 seconds' "$LOOM_BIN"
touch "$LOOM_SRC/src/main.rs"

run_fn rebuild_loom_if_stale
want "cargo build --release called when source is newer" \
    "build --release" "$(cat "$TMP/cargo.log")"

# ── NEGATIVE CONTROL: binary newer than all source → cargo must NOT be called ─
echo "case: binary newer than source — build must NOT fire"

# ALL source files must be older than the binary; a freshly written Cargo.toml would
# trigger the build even when main.rs is old.
printf 'content' > "$LOOM_SRC/Cargo.toml"
printf 'content' > "$LOOM_SRC/src/main.rs"
# Set source to old first, then touch binary to now.
touch -d 'now - 60 seconds' "$LOOM_SRC/src/main.rs" "$LOOM_SRC/Cargo.toml"
touch "$LOOM_BIN"; chmod +x "$LOOM_BIN"

run_fn rebuild_loom_if_stale
cargo_out2="$(cat "$TMP/cargo.log")"
[ -z "$cargo_out2" ] && ok "cargo not called when binary is current" \
    || bad "cargo not called when binary is current" "got: $cargo_out2"

# ── MISSING BINARY: no binary exists → build must fire ────────────────────────
echo "case: binary absent — build must fire"

rm -f "$LOOM_BIN"
touch "$LOOM_SRC/src/main.rs"

run_fn rebuild_loom_if_stale
want "cargo build --release called when binary is missing" \
    "build --release" "$(cat "$TMP/cargo.log")"

# ── restart_loom_if_stale: function queries the service before acting ──────────
echo "case: restart_loom_if_stale checks service state before restarting"

printf 'binary' > "$LOOM_BIN"; chmod +x "$LOOM_BIN"
# Make binary very new so it is newer than PID 1's start time (which is epoch).
touch "$LOOM_BIN"

MOCK_MAIN_PID=1 run_fn restart_loom_if_stale
systemctl_out="$(cat "$TMP/systemctl.log")"
want "systemctl queries is-active before deciding" "is-active" "$systemctl_out"
want "systemctl queries MainPID before deciding"   "MainPID"   "$systemctl_out"

# ── restart_loom_if_stale: skips when binary is not executable ─────────────────
echo "case: restart_loom_if_stale skips when binary lacks exec bit"

rm -f "$LOOM_BIN"
printf 'not-exec' > "$LOOM_BIN"   # created without execute permission
> "$TMP/systemctl.log"

MOCK_MAIN_PID=1 run_fn restart_loom_if_stale
sysctl_skip="$(cat "$TMP/systemctl.log")"
[ -z "$sysctl_skip" ] && ok "restart skipped when binary is not executable" \
    || bad "restart skipped when binary is not executable" "got: $sysctl_skip"
tl_summary
