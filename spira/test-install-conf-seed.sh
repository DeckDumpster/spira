#!/usr/bin/env bash
#
# test-install-conf-seed.sh — install.sh seeds dirname($SPIRA_PROD)/spira.conf with
# SPIRA_INSTANCE=<instance> when installing a non-prod instance, so the test sentinel
# reads test config instead of falling through to ~/.config/spira/spira.conf (prod).
#
#   ./test-install-conf-seed.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. SEED ON NON-PROD: installing a 'test' instance writes SPIRA_INSTANCE=test to
#    dirname($SPIRA_PROD)/spira.conf, which is $SPIRA_REPO/spira.conf from the sentinel's
#    perspective. The line is appended so operator settings already in the file are preserved.
# 2. NO SEED ON PROD: installing the 'prod' instance does NOT write this file — prod
#    is already the default and seeding is not needed.
# 3. IDEMPOTENT: running install.sh a second time does not append a duplicate line when
#    the exact SPIRA_INSTANCE=<instance> line already exists.
#
# POSITIVE CONTROL: the seed must be verified to have fired BEFORE claiming idempotency.
# An install that never writes anything appears identical to one that is idempotent.
#
# THE FIXTURE USES A FAKE PROD DIR. $SPIRA_PROD must point at a directory that exists
# and contains executable stubs for every ExecStart target in the unit templates, because
# install.sh refuses to write a unit whose ExecStart target is absent or non-executable.
# All stubs exit 0; their content is not under test here.
#
# defect: sp-3nnd.1
# covers: systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
filehas() { grep -qxF "$2" "$3" 2>/dev/null && ok "$1" || bad "$1" "wanted line [$2] in $3"; }
filelacks() { grep -qxF "$2" "$3" 2>/dev/null && bad "$1" "did not want line [$2] in $3" || ok "$1"; }
filecount() { local n; n="$(grep -cxF "$2" "$3" 2>/dev/null || echo 0)"; [ "$n" = "$4" ] && ok "$1" || bad "$1" "wanted $4 occurrences of [$2], got $n in $3"; }

echo "test-install-conf-seed.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture: minimal harness tree.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$FIXTURE/systemd/$(basename "$f")"
done
ln -s "$HERE/../systemd/install.sh" "$FIXTURE/systemd/install.sh"
for f in conf.sh watchd.sh lib.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$FIXTURE/spira/$f"
done
printf '# empty\n' > "$FIXTURE/spira/watchers"
printf '# empty\n' > "$FIXTURE/spira/repo-map.example"

DEST="$TMP/home/.config/systemd/user"
SPIRA_RUN_DIR="$TMP/run"
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$DEST" "$SPIRA_RUN_DIR" "$MOCK_BIN"
MOCK_LOG="$TMP/systemctl.log"

cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_LOG}"
case "$*" in
    *list-units*active*spira-aeon*) true ;;
    *list-unit-files*spira-watch*) true ;;
    *list-units*spira-watch*) true ;;
    *is-active*) printf 'active\n' ;;
    *list-timers*) true ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/loginctl"
chmod +x "$MOCK_BIN/loginctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/spira-supervise"
chmod +x "$MOCK_BIN/spira-supervise"

# ---------------------------------------------------------------------------
# FAKE PROD DIR: a directory that stands in for the prod checkout's spira/ subdir.
# Every script referenced in ExecStart=@SPIRA_PROD@/... must exist and be executable.
# These are stubs only — the test is about the conf file that is written beside them.
# Derived from the unit templates so a new template cannot silently break this test.
# ---------------------------------------------------------------------------
FAKE_PROD="$TMP/prod-checkout/spira"
mkdir -p "$FAKE_PROD"
for _s in $(grep -h "ExecStart=\|ExecStartPre=" "$REAL_REPO/systemd/"*.service 2>/dev/null \
            | grep "@SPIRA_PROD@" | sed 's|.*@SPIRA_PROD@/||' | sed 's/ .*//' | sort -u); do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_PROD/$_s"
    chmod +x "$FAKE_PROD/$_s"
done
unset _s
FAKE_PROD_COCK="$(dirname "$FAKE_PROD")/cockpit"
mkdir -p "$FAKE_PROD_COCK"
for _s in $(grep -h "ExecStart=" "$REAL_REPO/systemd/"*.service 2>/dev/null \
            | grep "@SPIRA_PROD_COCK@" | sed 's|.*@SPIRA_PROD_COCK@/||' | sed 's/ .*//' | sort -u); do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_PROD_COCK/$_s"
    chmod +x "$FAKE_PROD_COCK/$_s"
done
unset _s

# The conf.sh seeding target: dirname($FAKE_PROD)/spira.conf.
PROD_CONF="$TMP/prod-checkout/spira.conf"

# ---------------------------------------------------------------------------
# Fake git repo for SPIRA_REPO (the landref check needs a repo on its base branch).
# ---------------------------------------------------------------------------
FAKE_ORIGIN="$TMP/origin.git"
FAKE_REPO="$TMP/repo"
git init -q --bare -b main "$FAKE_ORIGIN" 2>/dev/null
git init -q -b main "$FAKE_REPO" 2>/dev/null
git -C "$FAKE_REPO" config user.email t@t
git -C "$FAKE_REPO" config user.name test
printf 'seed\n' > "$FAKE_REPO/f"
git -C "$FAKE_REPO" add f
git -C "$FAKE_REPO" commit -qm "seed" 2>/dev/null
git -C "$FAKE_REPO" remote add origin "$FAKE_ORIGIN"
git -C "$FAKE_REPO" push -q origin main 2>/dev/null
git -C "$FAKE_REPO" fetch -q origin 2>/dev/null
git -C "$FAKE_REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main
for _s in concierge.sh beads-push.sh; do
    [ -f "$REAL_REPO/$_s" ] && ln -sf "$REAL_REPO/$_s" "$FAKE_REPO/$_s"
done
unset _s

# ---------------------------------------------------------------------------
# inst [instance] [args] — run install.sh in the controlled environment.
# The second positional argument passed to the script becomes the instance.
# ---------------------------------------------------------------------------
inst() {
    local instance="${1:-test}"; shift || true
    > "$MOCK_LOG"
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        "SPIRA_REPO_MAP=$FIXTURE/spira/repo-map.example" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=$FAKE_PROD" \
        "SPIRA_REPO=$FAKE_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_SUPERVISE_BIN=$MOCK_BIN/spira-supervise" \
        "MOCK_LOG=$MOCK_LOG" \
        SPIRA_INSTALL_FORCE=1 \
        bash "$FIXTURE/systemd/install.sh" "$instance" "$@" 2>&1
}

# Seed DEST with pre-rendered units so the install loop has existing files to compare.
rendered="$(inst test --render 2>&1)"; render_rc=$?
if [ "$render_rc" != 0 ]; then
    printf 'fixture: install.sh test --render failed (rc=%s) — cannot continue\n' "$render_rc"
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
echo "SEED ON NON-PROD — test install writes SPIRA_INSTANCE=test beside prod checkout:"
# ==========================================================================

rm -f "$PROD_CONF"   # start clean

out="$(inst test 2>&1)"; rc=$?
iszero "seed: install.sh test exits 0"     "$rc"
filehas "seed: SPIRA_INSTANCE=test written to prod-checkout/spira.conf" \
        "SPIRA_INSTANCE=test" "$PROD_CONF"
want "seed: install reports the seeding"   "seeded" "$out"

# ==========================================================================
echo
echo "IDEMPOTENT — second install does not append a duplicate SPIRA_INSTANCE line:"
# ==========================================================================

out2="$(inst test 2>&1)"; rc2=$?
iszero "idempotent: second install exits 0" "$rc2"
filecount "idempotent: SPIRA_INSTANCE=test appears exactly once" \
          "SPIRA_INSTANCE=test" "$PROD_CONF" "1"
nowant "idempotent: second install does not report seeding again" "seeded" "$out2"

# ==========================================================================
echo
echo "EXISTING FILE PRESERVED — prior operator settings survive the seed:"
# ==========================================================================

rm -f "$PROD_CONF"
printf 'SPIRA_DB=/some/path/db\n' > "$PROD_CONF"

inst test >/dev/null 2>&1
# Both the original line and the new line must be in the file.
filehas "existing: original SPIRA_DB line preserved" "SPIRA_DB=/some/path/db" "$PROD_CONF"
filehas "existing: SPIRA_INSTANCE=test appended"     "SPIRA_INSTANCE=test"    "$PROD_CONF"

# ==========================================================================
echo
echo "NO SEED ON PROD — prod install does not write the file:"
# ==========================================================================

rm -f "$PROD_CONF"
out_prod="$(inst prod 2>&1)"; rc_prod=$?
iszero "prod: install.sh prod exits 0" "$rc_prod"
[ ! -f "$PROD_CONF" ] \
    && ok "prod: no spira.conf written beside prod checkout" \
    || bad "prod: spira.conf was written when it should not be" ""
nowant "prod: no seeding message in prod install output" "seeded" "$out_prod"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
