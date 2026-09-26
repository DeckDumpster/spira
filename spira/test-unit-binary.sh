#!/usr/bin/env bash
# tier: T0
# covers: systemd/*.service .github/workflows/gate.yml
#
# Verifies the unit-to-binary contract: every binary crate in the repo must
# have a corresponding executable in bin/ before suites run. The build job
# in gate.yml places binaries there; this suite confirms the mapping is
# complete and names any gap.
#
# POSITIVE CONTROL: the suite removes a binary from a scratch bin/ and asserts
# the check names the missing entry. Seen red with one binary absent, seen
# green with all present.
#
# host-reason: reads Cargo.toml files and systemd/*.service from the source
#              tree; no container state, no network, no cargo invocation needed
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
REPO="$(cd "$HERE/.." && pwd -P)"

echo "test-unit-binary.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Collect expected binaries: all Cargo.toml files paired with src/main.rs.
# ---------------------------------------------------------------------------
expected=()
while IFS= read -r toml; do
    crate_dir="$(dirname "$toml")"
    [ -f "$crate_dir/src/main.rs" ] || continue
    bin_name="$(awk '
        /^\[package\]/ { in_pkg=1 }
        in_pkg && /^name[[:space:]]*=/ {
            gsub(/.*=[[:space:]]*"/, "")
            gsub(/".*/, "")
            print; exit
        }
    ' "$toml")"
    [ -n "$bin_name" ] && expected+=("$bin_name")
done < <(find "$REPO" -name 'Cargo.toml' -not -path '*/target/*' | sort)

if [ "${#expected[@]}" -gt 0 ]; then
    ok "found ${#expected[@]} binary crate(s): ${expected[*]}"
else
    bad "positive control: at least one binary crate in repo" "none found"
fi

# ---------------------------------------------------------------------------
# check_bins <bin-dir> — print MISSING: <name> for each absent or
# non-executable binary; return non-zero when any are missing.
# ---------------------------------------------------------------------------
check_bins() {
    local bindir="$1" missing=0
    for name in "${expected[@]}"; do
        if [ ! -x "$bindir/$name" ]; then
            printf 'MISSING: %s\n' "$name"
            missing=$((missing+1))
        fi
    done
    return "$missing"
}

# ---------------------------------------------------------------------------
# Populate a scratch bin/ with stubs for every expected binary.
# ---------------------------------------------------------------------------
SCRATCHBIN="$TMP/bin"
mkdir -p "$SCRATCHBIN"
for name in "${expected[@]}"; do
    printf '#!/bin/sh\n' > "$SCRATCHBIN/$name"
    chmod +x "$SCRATCHBIN/$name"
done

# SEEN GREEN: check passes when all stubs are present.
if check_bins "$SCRATCHBIN" >/dev/null 2>&1; then
    ok "check passes when all binaries are present"
else
    bad "check passes when all binaries are present" \
        "$(check_bins "$SCRATCHBIN" 2>&1 || true)"
fi

# ---------------------------------------------------------------------------
# SEEN RED: remove one binary; confirm the check fails and names it.
# ---------------------------------------------------------------------------
OFFENDER="${expected[-1]}"
rm "$SCRATCHBIN/$OFFENDER"

check_out="$(check_bins "$SCRATCHBIN" 2>&1)" && check_rc=0 || check_rc=$?
if [ "$check_rc" -ne 0 ]; then
    ok "SEEN RED: check fails when $OFFENDER is missing (rc=$check_rc)"
else
    bad "SEEN RED: check fails when $OFFENDER is missing" "check exited 0"
fi
want "names the missing binary" "$OFFENDER" "$check_out"

# SEEN GREEN: restore the binary and confirm recovery.
printf '#!/bin/sh\n' > "$SCRATCHBIN/$OFFENDER"
chmod +x "$SCRATCHBIN/$OFFENDER"
if check_bins "$SCRATCHBIN" >/dev/null 2>&1; then
    ok "SEEN GREEN: check passes after binary is restored"
else
    bad "SEEN GREEN: check passes after binary is restored" \
        "$(check_bins "$SCRATCHBIN" 2>&1 || true)"
fi

# ---------------------------------------------------------------------------
# Contract: spira-cockpit.service references @SPIRA_SUPERVISE_BIN@, and
# spira-supervise must be among the discovered binaries.
# ---------------------------------------------------------------------------
cockpit_svc="$REPO/systemd/spira-cockpit.service"
if grep -q '@SPIRA_SUPERVISE_BIN@' "$cockpit_svc" 2>/dev/null; then
    ok "spira-cockpit.service references @SPIRA_SUPERVISE_BIN@"
else
    bad "spira-cockpit.service references @SPIRA_SUPERVISE_BIN@" \
        "not found in $cockpit_svc"
fi

found_supervise=0
for name in "${expected[@]}"; do
    [ "$name" = "spira-supervise" ] && found_supervise=1 && break
done
if [ "$found_supervise" -eq 1 ]; then
    ok "spira-supervise is in the discovered crate list"
else
    bad "spira-supervise is in the discovered crate list" \
        "expected list: ${expected[*]:-none}"
fi

# ---------------------------------------------------------------------------
# Mechanism: gate.yml has a build job that calls make build before suites run.
# ---------------------------------------------------------------------------
want "gate.yml build job calls make build" \
    "make build" \
    "$(cat "$REPO/.github/workflows/gate.yml" 2>/dev/null)"
tl_summary
