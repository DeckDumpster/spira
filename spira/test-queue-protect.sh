#!/usr/bin/env bash
# test-queue-protect.sh — queue.sh protect and doctor queue-protection check.
#
# doctor.sh warns when a queue-mode repository has no protection receipt, and
# is silent for push-mode repositories and for protected queue-mode ones.
# queue.sh protect writes the receipt on a successful forge call.
#
# covers: spira/queue.sh spira/doctor.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-queue-protect.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

BIN="$TMP/bin"; mkdir -p "$BIN" "$TMP/db/.beads" "$TMP/run"
touch "$TMP/watchers-empty"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

FAKE_HOME="$TMP/home"; mkdir -p "$FAKE_HOME/.config/systemd/user"

cat > "$BIN/sc" <<'MOCK'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n' ;;
    *"is-enabled"*)          printf 'enabled\n'; exit 0 ;;
esac
exit 0
MOCK
chmod +x "$BIN/sc"

# A minimal git repo used as the mapped checkout.
QREPO="$TMP/repo"
git init -q -b main "$QREPO"
git -C "$QREPO" commit -q --allow-empty -m "init"

QNAME=fixture-queue-repo
PNAME=fixture-push-repo
RMAP="$TMP/repo-map"

run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/sc" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP="$RMAP" \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

repos_section() { sed -n '/^repositories$/,/^$/p'; }

# ===========================================================================
echo
echo "positive control — queue-mode with no protection record warns:"
# ===========================================================================
printf '%s | %s | queue | main | | |\n' "$QNAME" "$QREPO" > "$RMAP"
# No receipt file yet — offender planted.
out="$(run_doctor | repos_section)"
want "queue-mode no receipt: warns"         "warn"      "$out"
want "warn names the repo"                  "$QNAME"    "$out"
want "warn mentions protection record"      "protection record" "$out"
want "warn suggests the command"            "queue.sh protect"  "$out"

# ===========================================================================
echo
echo "queue-mode with protection record — no warning:"
# ===========================================================================
printf 'main\n' > "$TMP/run/queue-protected-$QNAME"
out2="$(run_doctor | repos_section)"
nowant "queue-mode with receipt: no protection warn" \
    "has no protection record" "$out2"
want "queue-mode with receipt: ok shown" \
    "protection record present" "$out2"
rm -f "$TMP/run/queue-protected-$QNAME"

# ===========================================================================
echo
echo "push-mode — no protection warning:"
# ===========================================================================
printf '%s | %s | push | main | | |\n' "$PNAME" "$QREPO" > "$RMAP"
out3="$(run_doctor | repos_section)"
nowant "push-mode: no protection warn" "protection record" "$out3"

# ===========================================================================
echo
echo "queue.sh protect — writes the receipt on a successful forge call:"
# ===========================================================================
# Forge fixture: accepts branch-protect and exits 0.
FORGE_LOG="$TMP/forge-protect-log"
cat > "$BIN/forge-fixture.sh" <<FSCRIPT
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    branch-protect)
        base="\${1:-}"
        printf 'branch-protect %s %s\n' "\$repo" "\$base" >> "$FORGE_LOG"
        exit 0
        ;;
    *) printf 'fixture: unknown: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FSCRIPT
chmod +x "$BIN/forge-fixture.sh"
: > "$FORGE_LOG"

# Write the queue-mode repo-map for protect.
printf '%s | %s | queue | main | | |\n' "$QNAME" "$QREPO" > "$RMAP"

protect_out="$(
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_REPO_MAP="$RMAP" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_FORGE="$BIN/forge-fixture.sh" \
        bash "$HERE/queue.sh" protect "$QNAME" 2>&1 || true
)"

want "protect: calls forge"           "branch-protect" "$(cat "$FORGE_LOG")"
want "protect: forge gets base branch" "main"           "$(cat "$FORGE_LOG")"
[ -f "$TMP/run/queue-protected-$QNAME" ] \
    && ok  "protect: receipt written" \
    || bad "protect: receipt written" "file missing"
want "protect: receipt contains branch" "main" \
    "$(cat "$TMP/run/queue-protected-$QNAME" 2>/dev/null || true)"
want "protect: reports success" "protection set" "$protect_out"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
