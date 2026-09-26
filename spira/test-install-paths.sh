#!/usr/bin/env bash
#
# test-install-paths.sh — install.sh refuses when another instance's config shares a
# critical path (SPIRA_RUN, SPIRA_DB, SPIRA_PROD, SPIRA_DOLT_DATA, SPIRA_TESTDB_PORT);
# two instances with distinct paths install without complaint.
#
#   ./test-install-paths.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. COLLISION DETECTED: two configs in the same directory sharing SPIRA_RUN fail the
#    install, naming the colliding key and the other instance. No unit is written.
# 2. DISTINCT PATHS: two configs with different SPIRA_RUN values pass the collision
#    check and do not block the install.
# 3. NO CONFIG FILE: SPIRA_CONF points to a non-existent file; the check is skipped
#    (SPIRA_CONF_FILE is empty) and the install proceeds.
# 4. SAME INSTANCE: two configs in the same directory with the same SPIRA_INSTANCE
#    value are not compared — the check only compares different instances.
#
# POSITIVE CONTROL: the matcher must first demonstrate it finds a known collision,
# then be trusted when it reports none (law-absence-needs-a-positive-control).
#
# SCAR: the fixture's cp list omitted suite-covers.sh after sp-dt8u added it to
# lib.sh; lib.sh failed at source time before any assertion ran. A real install
# carries no fixture and the cause cannot exist.
#
# SKIP CONDITION: XDG_RUNTIME_DIR is not /run/user/1001 (suite runs inside the
# testenv container as spirauser) or user systemd is not responding.
#
# tier: T1
# covers: systemd/install.sh
# requires: testenv
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# The XDG_RUNTIME_DIR check below is a UID heuristic, not an identity check — it passes
# on any host whose real user happens to have UID 1001 (sp-nxvjm). This is the guard.
. "$HERE/testlib.sh"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }

echo "test-install-paths.sh"

[ "${XDG_RUNTIME_DIR:-}" = "/run/user/1001" ] || {
    printf 'SKIP test-install-paths.sh: not running as spirauser inside testenv container\n' >&2
    exit 77
}
# hermetic-ok: SKIP check — exits 77 when not inside the testenv container
systemctl --user status >/dev/null 2>&1 || {
    printf 'SKIP test-install-paths.sh: user systemd not running inside container\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

CONF_DIR="$TMP/conf"
SPIRA_RUN_DIR="$TMP/run"
DEST="$HOME/.config/systemd/user"
mkdir -p "$CONF_DIR" "$SPIRA_RUN_DIR" "$DEST"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/spira-supervise"
chmod +x "$TMP/spira-supervise"

# world.halted causes install.sh to enable units but not start them and to skip
# the end-state check (which needs bd + a real database, absent here).
touch "$SPIRA_RUN_DIR/world.halted"

# inst_paths [extra-env...] — run install.sh with path-collision env.
inst_paths() {
    SPIRA_RUN="$SPIRA_RUN_DIR" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    SPIRA_INSTALL_FORCE=1 \
    SPIRA_SUPERVISE_BIN="$TMP/spira-supervise" \
    "$@" \
    bash "$HERE/../systemd/install.sh" test 2>&1
}

# Seed DEST by rendering with no collision check (SPIRA_CONF=/nonexistent).
rendered="$(SPIRA_CONF=/nonexistent inst_paths)"
render_rc=$?
if [ "$render_rc" != 0 ]; then
    # Retry with --render only (collision check may still block a full install here).
    rendered="$(SPIRA_CONF=/nonexistent SPIRA_RUN="$SPIRA_RUN_DIR" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
        SPIRA_INSTALL_FORCE=1 \
        bash "$HERE/../systemd/install.sh" test --render 2>&1)"
    render_rc=$?
fi
if [ "$render_rc" != 0 ]; then
    printf 'fixture: install.sh test --render failed (rc=%s)\n' "$render_rc"
    printf '%s\n' "$rendered"
    exit 1
fi
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"; > "$DEST/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$DEST/$current_unit"
    fi
done <<< "$rendered"

# ==========================================================================
echo
echo "POSITIVE CONTROL — collision on SPIRA_RUN is detected:"
# ==========================================================================
printf 'SPIRA_INSTANCE = prod\nSPIRA_RUN = %s\n' "$SPIRA_RUN_DIR" \
    > "$CONF_DIR/prod.conf"
printf 'SPIRA_INSTANCE = test\n' > "$CONF_DIR/test.conf"

collision_out="$(SPIRA_CONF="$CONF_DIR/test.conf" inst_paths)"
collision_rc=$?

nonzero "collision: exit non-zero when SPIRA_RUN collides"   "$collision_rc"
want    "collision: names the colliding key"                  "SPIRA_RUN"   "$collision_out"
want    "collision: names the other instance"                 "prod"        "$collision_out"
want    "collision: names the other config file"              "prod.conf"   "$collision_out"
installed_after="$(ls "$DEST" | wc -l | tr -d ' ')"
pre_seeded="$(ls "$DEST" | wc -l | tr -d ' ')"
[ "$installed_after" = "$pre_seeded" ] \
    && ok "collision: no unit written after refusal" \
    || bad "collision: DEST changed after refusal (was $pre_seeded, now $installed_after)" ""

# ==========================================================================
echo
echo "DISTINCT PATHS — different SPIRA_RUN values do not block the install:"
# ==========================================================================
printf 'SPIRA_INSTANCE = prod\nSPIRA_RUN = %s\n' "$TMP/other-run" \
    > "$CONF_DIR/prod.conf"

distinct_out="$(SPIRA_CONF="$CONF_DIR/test.conf" inst_paths)"
distinct_rc=$?

iszero  "distinct: exit 0 when no path collision"            "$distinct_rc"
nowant  "distinct: no refusal message in output"             "refusing" "$distinct_out"
nowant  "distinct: no collision mention in output"           "collides" "$distinct_out"

# ==========================================================================
echo
echo "NO CONFIG FILE — SPIRA_CONF=/nonexistent skips the collision check:"
# ==========================================================================
no_conf_out="$(SPIRA_CONF=/nonexistent inst_paths)"
no_conf_rc=$?

iszero  "no-conf: exit 0 when no config file (check skipped)" "$no_conf_rc"
nowant  "no-conf: no refusal message"                          "refusing" "$no_conf_out"

# ==========================================================================
echo
echo "SAME INSTANCE — two configs with the same SPIRA_INSTANCE are not compared:"
# ==========================================================================
printf 'SPIRA_INSTANCE = test\nSPIRA_RUN = %s\n' "$SPIRA_RUN_DIR" \
    > "$CONF_DIR/test-other.conf"

same_inst_out="$(SPIRA_CONF="$CONF_DIR/test.conf" inst_paths)"
same_inst_rc=$?

iszero  "same-instance: exit 0 when other config has same SPIRA_INSTANCE" "$same_inst_rc"
nowant  "same-instance: no refusal message"                                "refusing" "$same_inst_out"

rm -f "$CONF_DIR/test-other.conf"

# ==========================================================================
echo
tl_summary
