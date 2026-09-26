#!/usr/bin/env bash
#
# test-conf-writeback.sh — a worktree's own conf.sh never regenerates a config file that
# lives outside that worktree, however it got HOME.
#
# THE INCIDENT THIS GUARDS AGAINST. A cutover branch's conf.sh, sourced by an aeon whose
# HOME is the real operator's, resolved a write-back target beside the operator's real
# legacy spira.conf and regenerated spira.toml from worktree-local state — three times in
# one day, each time blinding queue-watch until the file was restored by hand.
# spira_config_writeback is the guard: any code about to regenerate a config file resolves
# its target through it first.
#
# THE POSITIVE CONTROLS ARE FIRST (law-absence-needs-a-positive-control): both escape
# hatches (SPIRA_CONFIG_WRITE=1, and running as the installed release itself) return the
# candidate unchanged, so the redirect case below is not "this function always redirects."
#
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# testlib.sh has no inequality assertion (every suite that needs one has carried its own);
# named and shaped exactly like its sibling `is`.
isne() { [ "$2" != "$3" ] && ok "$1" || bad "$1" "wanted NOT [$2] got [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# A minimal harness tree, same shape test-conf.sh already builds: no .git, so SPIRA_REPO
# derives to the harness root itself rather than any real checkout.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"

# A fixture HOME holding a "full" spira.toml — the shape the operator's real box carries —
# at exactly the path spira_toml_resolve derives a write-back target beside
# ($HOME/.config/spira, the XDG tier of spira_conf_file's own search order).
FIXHOME="$TMP/home"
mkdir -p "$FIXHOME/.config/spira"
REAL_TOML="$FIXHOME/.config/spira/spira.toml"
cat > "$REAL_TOML" <<'EOF'
[spira]
fayths = ["builder", "ops"]

[repo.home]
path = "/srv/checkouts/home"
mode = "push"

[repo.service]
path = "/srv/checkouts/service"
mode = "pr"

[persona.builder]
model = "claude-sonnet-5"
EOF
ORIG_SUM="$(sha256sum "$REAL_TOML" | awk '{print $1}')"

# Runs `spira_config_writeback "$REAL_TOML"` in a subshell sourcing the harness's conf.sh
# under a controlled, explicit environment, and prints the result.
writeback() {
    env -i PATH="$PATH" HOME="$FIXHOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        "$@" \
        bash -c ". '$HARNESS/spira/conf.sh'; spira_config_writeback '$REAL_TOML'" 2>/dev/null
}

echo
echo "positive control — SPIRA_CONFIG_WRITE=1 returns the candidate unchanged:"
got="$(writeback env SPIRA_CONFIG_WRITE=1)"
is "SPIRA_CONFIG_WRITE=1 leaves the candidate alone" "$REAL_TOML" "$got"

echo
echo "positive control — the installed release (SPIRA_HOME == SPIRA_PROD) is unguarded:"
got="$(writeback env SPIRA_PROD="$HARNESS/spira")"
is "SPIRA_HOME == SPIRA_PROD leaves the candidate alone" "$REAL_TOML" "$got"

echo
echo "the guard — a worktree that is neither of those redirects under SPIRA_REPO:"
got="$(writeback env)"
isne "an unprivileged worktree does not return the real path" "$REAL_TOML" "$got"
want "the redirect lives under the harness root" "$HARNESS" "$got"
is   "the redirect keeps the candidate's basename" "$HARNESS/spira.toml" "$got"

echo
echo "an empty/unresolved SPIRA_PROD fails closed into the redirect, not the real path:"
got="$(writeback env SPIRA_PROD=)"
isne "empty SPIRA_PROD still redirects" "$REAL_TOML" "$got"

echo
echo "the redirect is not a fiction — writing there truly leaves the real file untouched:"
redirected="$(writeback env)"
mkdir -p "$(dirname "$redirected")"
printf '[spira]\nfayths = []\n\n[repo]\n' > "$redirected"
now_sum="$(sha256sum "$REAL_TOML" | awk '{print $1}')"
is "the operator's real spira.toml is byte-identical after the redirected write" "$ORIG_SUM" "$now_sum"

tl_summary
