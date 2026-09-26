#!/usr/bin/env bash
#
# test-bead-repo-guard.sh — bead.sh file refuses a --repo absent from the repo map before
# bd is ever called, and lists every mapped repo name in the refusal.
#
#   ./test-bead-repo-guard.sh
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the unmapped case is
# proven to fire, and to fire before bd runs, before the mapped case is trusted to pass.
#
# --kind event (not --kind work) is enough to reach the check without a persona; the
# check runs on the parsed --repo before either kind's bd-create path is built, so it
# refuses identically whichever kind names a repo.
#
# defect: sp-pnhtt
# covers: spira/bead.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-bead-repo-guard.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A minimal repo map with two known entries — non-default names, so a check that only
# recognises the shipped example would pass for the wrong reason.
MAP="$TMP/repo-map"
printf 'spira    | /srv/spira     | push | origin/main |  |\n' > "$MAP"
printf 'widget   | /srv/widget    | pr   | origin/main |  |\n' >> "$MAP"

# A stub bd that records its own invocation, so an unmapped repo can be proven never to
# reach it.
STUB_BD="$TMP/bd"
printf '#!/usr/bin/env bash\nprintf "bd-called\\n"; exit 0\n' > "$STUB_BD"
chmod +x "$STUB_BD"

# Runs in this shell, not a command substitution: a subshell's exit status cannot be
# read back through a variable once the subshell that set it has exited.
run_bead_file() {   # run_bead_file <repo> -> writes combined output to $TMP/out, returns bead.sh's exit
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$MAP" SPIRA_BD="$STUB_BD" BD_TIMEOUT=10 \
        bash "$HERE/bead.sh" file "guard test" --kind event --repo "$1" > "$TMP/out" 2>&1
}

# ==========================================================================
echo
echo "POSITIVE CONTROL: unmapped repo is refused, exit 2, bd never called:"
# ==========================================================================

run_bead_file "nope"; rc=$?; out="$(cat "$TMP/out")"
is   "unmapped repo: exit code is 2"        "2"  "$rc"
nowant "unmapped repo: bd not called"       "bd-called"  "$out"
want   "unmapped repo: error names the offender" "nope"  "$out"

# ==========================================================================
echo
echo "error message lists every mapped repo name:"
# ==========================================================================

want "all keys: spira present"  "spira"   "$out"
want "all keys: widget present" "widget"  "$out"

# ==========================================================================
echo
echo "mapped repo passes through to bd:"
# ==========================================================================

run_bead_file "spira"; rc2=$?; out2="$(cat "$TMP/out")"
is   "mapped repo: exit code is 0"        "0"  "$rc2"
want "mapped repo: bd was called"         "bd-called"  "$out2"

run_bead_file "widget"; rc3=$?; out3="$(cat "$TMP/out")"
is   "second mapped repo: exit code is 0"  "0"  "$rc3"
want "second mapped repo: bd was called"   "bd-called"  "$out3"

echo
tl_summary
