#!/usr/bin/env bash
# test-install-naming.sh — install.sh renders per-instance unit names (UC-instance-lifecycle-24).
#
# Demoted from test-install-instance.sh's PER-INSTANCE NAMING section
# (docs/test-plan/instance-lifecycle.md row 24): naming is a pure function of
# `install.sh --render` and the ENABLE array units.sh computes — neither needs a
# live install against real systemd, so this suite needs no testenv container.
# test-install-instance.sh keeps the PRUNE, WATCHER INSTALL and AEON ISOLATION
# sections, which do exercise real systemd.
#
# tier: T1
# covers: systemd/install.sh systemd/units.sh UC-instance-lifecycle-24
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/run"
WATCHERS="$TMP/watchers"; printf '# empty\n' > "$WATCHERS"

rendered="$(env -i \
    PATH="$PATH" \
    HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$WATCHERS" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    units-install test --render 2>&1)"
render_rc=$?
is "render: install.sh test --render exits 0" "0" "$render_rc"

# Non-shared units (everything named spira-*) must carry the -test suffix; shared
# units (cockpit-ensure, concierge, beads-push, dolt-beads) keep plain names.
bad_rendered="$(printf '%s\n' "$rendered" \
    | grep '^===== spira-' \
    | sed 's/^===== \(spira-[^ ]*\) =====/\1/' \
    | grep -v '^spira-.*-test\.' || true)"
is "naming: all rendered spira-* units have -test suffix" "" "$bad_rendered"

n="$(printf '%s\n' "$rendered" | grep -c '^===== spira-.*-test\.' || true)"
if [ "${n:-0}" -ge 14 ]; then
    ok "naming: at least 14 spira-*-test units rendered ($n)"
else
    bad "naming: at least 14 spira-*-test units rendered" "got ${n:-0}"
fi

want "naming: cockpit-ensure.service present (shared, plain name)" \
     "===== cockpit-ensure.service =====" "$rendered"
want "naming: concierge.service present (shared, plain name)" \
     "===== concierge.service =====" "$rendered"
want "naming: beads-push.service present (shared, plain name)" \
     "===== beads-push.service =====" "$rendered"

# ENABLE is the array units-install's own enable/restart loop reads to decide what to
# enable — already instance-qualified (inst_name applied per manifest entry, install/src/
# manifest.rs, sp-31dm0: systemd/units.sh is retired). `--list-enable` is the T1 way to
# prove the enable-time name, without a live install run against real systemd.
enable_arr="$(env -i PATH="$PATH" SPIRA_HOME="$HERE" SPIRA_REPO="$(cd "$HERE/.." && pwd -P)" \
    SPIRA_INSTANCE=test \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
    SPIRA_WATCHERS="$WATCHERS" \
    units-install --list-enable 2>/dev/null | tr '\n' ' ')"
want   "naming: ENABLE includes spira-sentinel-test.timer" "spira-sentinel-test.timer" "$enable_arr"
nowant "naming: ENABLE does not include plain spira-sentinel.timer" "spira-sentinel.timer" "$enable_arr"

tl_summary
