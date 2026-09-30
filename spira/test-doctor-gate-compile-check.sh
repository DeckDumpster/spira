#!/usr/bin/env bash
#
# test-doctor-gate-compile-check.sh — doctor.sh fails a repo whose configured gate command
#   cannot reach a compile check, for any repo carrying Rust crates.
#
# THE CASE (sp-1hmrm, 2026-09-28). An interim hand-cut of the spira row's gate command, live
# only in the repo-map (no committed tree carries it), dropped gate-touched.sh and with it
# build-fence.sh — the one compile check on the certification path (sp-9uro3). sp-0inic
# certified green under the cut gate and broke the build for its whole round on its own diff.
# No container-run suite can catch a hand-cut to live operator state; only a runtime check
# reading the same repo-map the gate itself reads can.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the cut row must be seen
# FAIL before the same row, repaired, is trusted to report OK.
#
# SINCE sp-quu2w THE TREE OWNS ITS GATE: a repository whose landing ref carries gate.steps is
# gated by that file, and doctor asks the gate binary (`gate --definition`) for whichever
# definition is in force, so the same check covers both a tree definition and a legacy row.
#
# tier: T1
# covers: spira/doctor.sh spira/build-fence.sh gate/src/def.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-doctor-gate-compile-check.sh"

SPIRA_CONFIG_BIN="$(testlib_spira_config_bin)" || skip "no spira-config binary found — cannot be built here"
GATE_BIN="${SPIRA_GATE_BIN:-${SPIRA_ARTIFACTS:-}/gate}"
[ -x "$GATE_BIN" ] || GATE_BIN="$HERE/../bin/gate"
[ -x "$GATE_BIN" ] || skip "no gate binary found — cannot be built here"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/bin" "$TMP/db/.beads" "$TMP/run" "$TMP/home" "$TMP/chamber"
mkdir -p "$TMP/rustrepo/crate/src" "$TMP/shellrepo"
printf '[package]\nname = "fixture"\n' > "$TMP/rustrepo/crate/Cargo.toml"
# Both fixtures are checkouts with the landing ref their rows declare: the gate resolves the
# definition at that ref, exactly as a trial does.
for r in rustrepo shellrepo; do
    git -C "$TMP/$r" init -q -b main
    git -C "$TMP/$r" add -A
    git -C "$TMP/$r" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m init
    git -C "$TMP/$r" update-ref refs/remotes/origin/main HEAD
done
# commit_steps <content> — land a gate.steps on rustrepo's landing ref.
commit_steps() {
    printf '%s\n' "$1" > "$TMP/rustrepo/gate.steps"
    git -C "$TMP/rustrepo" add gate.steps
    git -C "$TMP/rustrepo" -c user.name=t -c user.email=t@example.com commit -q -m steps
    git -C "$TMP/rustrepo" update-ref refs/remotes/origin/main HEAD
}

cat > "$TMP/bin/bd" <<'FAKEBD'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*)         printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--json"*)          printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *"show"*"sp-test"*)         printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *)                          exit 0 ;;
esac
FAKEBD
chmod +x "$TMP/bin/bd"

cat > "$TMP/bin/fake-notify" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$TMP/bin/fake-notify"

cat > "$TMP/bin/systemctl" <<'SH'
#!/usr/bin/env bash
printf 'enabled\n'; exit 0
SH
chmod +x "$TMP/bin/systemctl"

TOML="$TMP/spira.toml"
: > "$TOML"

# run_doctor <repo-map> — SPIRA_REPO pinned to $TMP so nothing this reads or writes escapes
# the fixture. repo_field resolves paths relative to nothing (rows carry absolute paths), so
# the fixture repos above are addressed directly.
run_doctor() {
    env -i \
        PATH="$TMP/bin:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_TOML="$TOML" \
        SPIRA_REPO="$TMP" \
        SPIRA_REPO_MAP="$1" \
        SPIRA_CHAMBER="$TMP/chamber" \
        SPIRA_CONFIG_BIN="$SPIRA_CONFIG_BIN" \
        SPIRA_GATE_BIN="$GATE_BIN" \
        SPIRA_PATH="$TMP/bin" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD="$TMP/bin/bd" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_NOTIFY="$TMP/bin/fake-notify" \
        SPIRA_SYSTEMCTL="$TMP/bin/systemctl" \
        SPIRA_GOAL=sp-test \
        SPIRA_COCKPIT="$TMP/run" \
        SPIRA_SNAP_STALE_S=60 \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

# ==========================================================================
echo
echo "SEEN RED FIRST — a Rust repo whose gate names neither compile-check script:"
# ==========================================================================
RMAP="$TMP/repo-map-cut"
cat > "$RMAP" <<EOF
rustrepo | $TMP/rustrepo | queue | origin/main | | bash spira/inventory.sh && bash spira/literal-lint.sh
EOF
cut_out="$(run_doctor "$RMAP")"
want   "cut row: FAIL names the repo"            "FAIL  rustrepo: gate command has no reachable compile check" "$cut_out"
want   "cut row: names the fix"                  "build-fence.sh"                                              "$cut_out"
nowant "cut row: no false OK for this repo"       "ok    rustrepo"                                             "$cut_out"

# ==========================================================================
echo
echo "GREEN AFTER — the same row, repaired, reports OK (positive control):"
# ==========================================================================
RMAP_FIXED="$TMP/repo-map-fixed"
cat > "$RMAP_FIXED" <<EOF
rustrepo | $TMP/rustrepo | queue | origin/main | | bash spira/inventory.sh && bash spira/build-fence.sh
EOF
fixed_out="$(run_doctor "$RMAP_FIXED")"
want   "fixed row: OK reports the repo by name"  "ok    rustrepo: gate command reaches a compile check" "$fixed_out"
nowant "fixed row: no FAIL survives the fix"     "FAIL  rustrepo"                                       "$fixed_out"

# ==========================================================================
echo
echo "the selector alone reaches none (sp-aprxm; the suite-select binary since sp-wx2tw):"
# ==========================================================================
RMAP_TOUCHED="$TMP/repo-map-touched"
cat > "$RMAP_TOUCHED" <<EOF
rustrepo | $TMP/rustrepo | queue | origin/main | | "\$SPIRA_SELECT_BIN" gate "\$SPIRA_GATE_BASE" "\$SPIRA_GATE_BRANCH"
EOF
touched_out="$(run_doctor "$RMAP_TOUCHED")"
want "selector-only row: FAIL names the repo" "FAIL  rustrepo: gate command has no reachable compile check" "$touched_out"

# ==========================================================================
echo
echo "a repo with no Rust crates is not checked at all — absence of Cargo.toml is not a fault:"
# ==========================================================================
RMAP_SHELL="$TMP/repo-map-shell"
cat > "$RMAP_SHELL" <<EOF
shellrepo | $TMP/shellrepo | queue | origin/main | | bash spira/inventory.sh
EOF
shell_out="$(run_doctor "$RMAP_SHELL")"
nowant "shell-only repo: no FAIL for it" "FAIL  shellrepo" "$shell_out"
nowant "shell-only repo: no OK for it either — it was never checked" "ok    shellrepo" "$shell_out"
want   "shell-only repo: the section says nothing carries Rust crates" \
       "no repository in the map carries Rust crates" "$shell_out"

# ==========================================================================
echo
echo "THE TREE OWNS ITS GATE (sp-quu2w) — the landing ref's gate.steps is read, the column is not:"
# ==========================================================================
# Seen red first: a tree definition with no build fence FAILs even though the row's column
# (ignored now) still names one.
commit_steps "step bash spira/inventory.sh"
tree_cut_out="$(run_doctor "$RMAP_FIXED")"
want   "tree definition without build-fence: FAIL" "FAIL  rustrepo: gate command has no reachable compile check" "$tree_cut_out"
nowant "the ignored column does not rescue it"       "ok    rustrepo"                                             "$tree_cut_out"
# Green after: the tree definition names it, and the row's column is empty.
commit_steps "step bash spira/inventory.sh
step bash spira/build-fence.sh"
RMAP_EMPTY="$TMP/repo-map-empty"
printf 'rustrepo | %s | queue | origin/main | |\n' "$TMP/rustrepo" > "$RMAP_EMPTY"
tree_ok_out="$(run_doctor "$RMAP_EMPTY")"
want   "tree definition with build-fence, empty column: OK" "ok    rustrepo: gate command reaches a compile check" "$tree_ok_out"
nowant "no FAIL for it"                                     "FAIL  rustrepo"                                        "$tree_ok_out"

tl_summary
