#!/usr/bin/env bash
# test-landing-build.sh — land-build-ensure.sh stays quiet when no cargo source changed, and
# when cargo is absent. (The "binary absent with its unit enabled" trigger is deleted —
# sp-gypjk: a release always carries every binary. The source-changed trigger that does fire
# is test-land-build-ensure.sh's.)
#
# POSITIVE CONTROL FIRST. Each silent assertion is preceded by a case that proves
# the trigger fires before trusting that it is quiet (law-absence-needs-a-positive-control).
#
# WHAT IS TESTED
# 1. POSITIVE CONTROL: land-build-ensure.sh is present and executable.
# 2. No source change → build.sh is NOT invoked.
# 3. cargo absent → build.sh is NOT invoked.
#
# covers: spira/land-build-ensure.sh landing-pass/* spira/build.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-landing-build.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# =========================================================================
echo
echo "POSITIVE CONTROL — land-build-ensure.sh exists and is executable:"
# =========================================================================
if command -v land-build-ensure.sh >/dev/null 2>&1; then
    ok "land-build-ensure.sh is present and executable"
else
    bad "land-build-ensure.sh is present and executable" \
        "not found or not executable (positive control: fails before the fix)"
fi

# =========================================================================
# SHARED FIXTURE
# =========================================================================
# A minimal git repo with no reflog (so _be_src_changed stays 0) lets us test
# the no-change path in isolation.
REPO="$TMP/repo"
mkdir -p "$REPO/spira"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t
touch "$REPO/spira/.keep"
git -C "$REPO" add .
git -C "$REPO" commit -q -m "init"
# No second commit → no @{1} reflog entry → source-changed check is always 0.

# Stub build.sh, found by name on PATH (sp-gypjk): records that it was called.
mkdir -p "$TMP/stub" "$TMP/nocargo"
BUILD_MARKER="$TMP/build_called"
cat > "$TMP/stub/build.sh" <<BSTUB
#!/usr/bin/env bash
touch "$BUILD_MARKER"
printf 'build.sh: stub invoked\n'
BSTUB
chmod +x "$TMP/stub/build.sh"
cp "$TMP/stub/build.sh" "$TMP/nocargo/build.sh"

# Stub cargo: land-build-ensure.sh checks 'command -v cargo' before doing anything.
cat > "$TMP/stub/cargo" <<'CSTUB'
#!/usr/bin/env bash
exit 0
CSTUB
chmod +x "$TMP/stub/cargo"

# Stub systemctl: by default reports every unit as enabled.
SC_LOG="$TMP/sc.log"
cat > "$TMP/stub/systemctl" <<SCMOCK
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$SC_LOG"
case "\$*" in
    *"is-enabled"*) printf 'enabled\n'; exit 0 ;;
    *) exit 0 ;;
esac
SCMOCK
chmod +x "$TMP/stub/systemctl"

# run_ensure: invoke land-build-ensure.sh (by name) in a controlled environment.
run_ensure() {
    env -i \
        PATH="$TMP/stub:$HERE:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_REPO="$REPO" \
        SPIRA_INSTANCE=prod \
        SPIRA_SYSTEMCTL="$TMP/stub/systemctl" \
        land-build-ensure.sh 2>&1
}

# run_ensure_nocargo: same without cargo on PATH.
run_ensure_nocargo() {
    env -i \
        PATH="$TMP/nocargo:$HERE:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_REPO="$REPO" \
        SPIRA_INSTANCE=prod \
        SPIRA_SYSTEMCTL="$TMP/stub/systemctl" \
        land-build-ensure.sh 2>&1
}

# =========================================================================
echo
echo "no source change → build.sh is NOT invoked:"
# =========================================================================
rm -f "$BUILD_MARKER" "$SC_LOG"

present_out="$(run_ensure 2>&1)"; present_rc=$?

is "no change exits 0" "0" "$present_rc"
nowant "no change does not trigger build" "running build.sh" "$present_out"
[ ! -f "$BUILD_MARKER" ] \
    && ok  "build.sh marker absent (build.sh was not called)" \
    || bad "build.sh marker absent (build.sh was not called)" \
           "marker file present — build.sh was invoked when it should not have been"

# =========================================================================
echo
echo "cargo absent → build.sh is NOT invoked:"
# =========================================================================
rm -f "$BUILD_MARKER" "$SC_LOG"

nocargo_out="$(run_ensure_nocargo 2>&1)"; nocargo_rc=$?

is "no cargo exits 0" "0" "$nocargo_rc"
nowant "no cargo does not trigger build" "running build.sh" "$nocargo_out"
[ ! -f "$BUILD_MARKER" ] \
    && ok  "no cargo: build.sh marker absent" \
    || bad "no cargo: build.sh marker absent" \
           "marker present — build.sh invoked without cargo"

# =========================================================================
echo
tl_summary
