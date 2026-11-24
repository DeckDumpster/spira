#!/usr/bin/env bash
#
# pre-activate.sh <release-dir> — five checks a release must pass before
# releases/current moves onto it. `release verify` runs this against the
# release it checks (a landing, skew refresh in release mode and rollback-local
# all verify before `release activate`), before the symlink flips. Exit 0 only if every check passes; on any failure, print each failed
# check to stderr and exit 1 — the caller leaves current where it was.
#
# Checks:
#   config     spira.toml, if one is in force on this host, validates against
#              THIS release's spira-config schema — not the host's
#   registry   every key a binary in the release reads (`check-config`) is registered in the
#              release's own spira/conf.d, so a binary is never promoted ahead of its keys
#   store      the bead store is reachable and its schema matches the bd on PATH
#   units      every systemd unit renders with no unresolved placeholder
#   self-test  the release's own spira/self-test.sh
#   ami        a round AMI exists for the release's recipe-sha; only warns
#
# Runtime dependencies are not checked here: `release verify` settles them from files alone.
set -uo pipefail

REL="${1:?usage: pre-activate.sh <release-dir>}"
[ -d "$REL" ] || { printf 'pre-activate: not a directory: %s\n' "$REL" >&2; exit 1; }

FAIL=0
fail() { printf 'pre-activate: FAIL %s: %s\n' "$1" "$2" >&2; FAIL=1; }
ok()   { printf 'pre-activate: ok   %s\n' "$1"; }

toml_in_force() {
    local c
    for c in "${SPIRA_TOML:-}" \
             "${SPIRA_REPO:-}/spira.toml" \
             "${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml" \
             /etc/spira/spira.toml; do
        [ -n "$c" ] && [ -f "$c" ] && { printf '%s' "$c"; return; }
    done
}

# --- config: spira.toml (if any is in force) validates against the release's schema ---
check_config() {
    local toml
    toml="$(toml_in_force)"
    if [ -z "$toml" ]; then
        ok "config (no spira.toml in force — nothing to validate)"
        return
    fi
    local bin="$REL/bin/spira-config"
    if [ ! -x "$bin" ]; then
        fail config "$bin not found in release"
        return
    fi
    # sp-oppza ONE-TIME UPGRADE MIGRATION, ahead of validate: a box whose config predates
    # sp-k6m1m (goal set, no id_prefix) is repaired in place instead of failing activation on
    # every such box. Idempotent — a no-op once id_prefix is set, which includes production's
    # own state, set by hand — so this runs unconditionally, every pre-activate.
    local mig
    mig="$("$bin" migrate "$toml" 2>&1)"
    [ -n "$mig" ] && printf 'pre-activate: %s\n' "$mig"
    local out rc
    out="$("$bin" validate "$toml" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        fail config "$toml: $out"
        return
    fi
    out="$(SPIRA_HOME="$REL/spira" SPIRA_REPO="${SPIRA_REPO:-$REL}" "$bin" resolve --sh-all --conf-d "$REL/spira/conf.d" "$toml" 2>&1 >/dev/null)"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        ok config
    else
        fail config "$toml does not resolve under this release: $out"
    fi
}

# --- registry: every key a binary reads is registered in the conf.d it will resolve against ---
check_registry() {
    local toml bin out rc
    toml="$(toml_in_force)"
    if [ -z "$toml" ]; then
        ok "registry (no spira.toml in force — nothing to resolve)"
        return
    fi
    for bin in queue; do
        [ -x "$REL/bin/$bin" ] || continue
        out="$(SPIRA_RELEASE="$REL" SPIRA_TOML="$toml" "$REL/bin/$bin" check-config 2>&1)"
        rc=$?
        [ "$rc" -eq 0 ] || fail registry "$bin reads a key $REL/spira/conf.d does not register: $out"
    done
    [ "$FAIL" -ne 0 ] || ok registry
}

# --- store: the bead store is reachable and its schema matches the bd on PATH ---
check_store() {
    if ! command -v spira-lc >/dev/null 2>&1; then
        fail store "spira-lc not on PATH"
        return
    fi
    local t="${SPIRA_STORE_CHECK_TIMEOUT:-30}" out rc attempt
    local conn_re='i/o timeout|connection refused|connection reset|broken pipe|failed to open database|driver: bad connection|packets\.go|dial tcp'
    for attempt in 1 2; do
        out="$(timeout "$t" spira-lc admin-store migrate-status 2>&1)"
        rc=$?
        [ "$rc" -eq 0 ] && break
        [ "$rc" -ne 124 ] && ! grep -qiE "$conn_re" <<<"$out" && break
    done
    if [ "$rc" -eq 0 ]; then
        ok store
    elif [ "$rc" -eq 124 ]; then
        fail store "store slow: no answer in ${t} s (2 attempts)"
    elif grep -qiE "$conn_re" <<<"$out"; then
        fail store "store slow/unreachable: $out"
    else
        fail store "schema mismatch: $out"
    fi
}

# --- units: every systemd unit renders with no unresolved placeholder ---
check_units() {
    # sp-31dm0: units-install is a compiled binary in the release's bin/ now, not
    # systemd/install.sh.
    local inst="$REL/bin/units-install"
    if [ ! -x "$inst" ]; then
        fail units "$inst not found"
        return
    fi
    local out rc
    out="$(SPIRA_HOME="$REL/spira" SPIRA_REPO="$REL" "$inst" --render 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        fail units "units-install --render exited $rc: $(printf '%s' "$out" | tail -5)"
        return
    fi
    local left
    left="$(printf '%s\n' "$out" | grep -oE '@[A-Z_]+@' | sort -u | tr '\n' ' ')"
    if [ -n "$left" ]; then
        fail units "unresolved placeholders: $left"
    else
        ok units
    fi
}

# --- self-test: the release's own smoke test ---
check_self_test() {
    local st="$REL/spira/self-test.sh"
    if [ ! -x "$st" ]; then
        fail self-test "$st not found"
        return
    fi
    local out rc
    out="$("$st" "$REL" 2>&1)"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        ok self-test
    else
        fail self-test "$out"
    fi
}

# --- ami: an AMI exists for this release's recipe-sha. A warning, never a refusal: the image
# is built from main after the recipe lands, so refusing here could never be satisfied.
check_ami() {
    local rv="$REL/bin/round-vm" out rc
    [ -x "$rv" ] || { ok "ami (release carries no round-vm)"; return; }
    out="$(timeout "${SPIRA_AMI_CHECK_TIMEOUT:-60}" "$rv" ami-check 2>&1)"
    rc=$?
    case "$rc" in
        0|3) ok "ami" ;;
        *) printf 'pre-activate: warn ami: no round AMI matches this release'"'"'s recipe (rc %s); EC2 rounds are refused until the round-ami-build workflow publishes one: %s\n' "$rc" "$(printf '%s' "$out" | tail -2 | tr '\n' ' ')" >&2 ;;
    esac
}

check_config
check_registry
check_store
check_units
check_self_test
check_ami

exit "$FAIL"
