#!/usr/bin/env bash
#
# test-gate-unit.sh — lib.sh's host_cores(), the one gate seam still in bash.
#
# The pure functions this suite used to table-test (verdict()'s shape and its no-evidence
# downgrade, gate_key_hash(), cache_fresh(), gate_tree_key(), gate_attribute(), red/ran/
# timed-out suite parsing) moved with gate.sh into the Rust `gate` crate (sp-0tpcs) and are
# unit-tested there: `cargo test -p gate` (gate/src/{key,parse,engine,tests}.rs).
#
# host-reason: sources lib.sh only; no database, no systemd, no git
# tier: T1
# covers: spira/lib.sh UC-gate-verdict-11
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-gate-unit.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export SPIRA_CONF="$TMP/nonexistent.conf"
export SPIRA_HOME="$HERE"

# --- host_cores() (UC-gate-verdict-11): getconf, immune to cgroup-limited nproc ----------
real_cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null)"
got="$(bash -c '
    mkdir -p "'"$TMP"'/stubpath"
    printf "#!/usr/bin/env bash\necho 1\n" > "'"$TMP"'/stubpath/nproc"
    chmod +x "'"$TMP"'/stubpath/nproc"
    PATH="'"$TMP"'/stubpath:$PATH" bash -c ". \"'"$HERE"'/lib.sh\" 2>/dev/null; host_cores"
')"
is "host_cores(): returns getconf's count even with nproc stubbed to 1" "$real_cores" "$got"

tl_summary
