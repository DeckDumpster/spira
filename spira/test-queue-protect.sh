#!/usr/bin/env bash
# test-queue-protect.sh — queue.sh protect writes a protection receipt.
#
# queue.sh protect writes the receipt on a successful forge call. Doctor's own
# queue-protection check (which read that receipt back) was part of the "repositories"
# section removed by sp-utt1i; repo-map/config validation is the config-store-preflight
# area's job now (sp-n071y), not doctor's.
#
# NOT YET DEMOTED TO T1 (sp-s088v.16, UC-32): the plan's precondition — doctor.sh
# gaining a --section repositories selector — does not hold; doctor.sh has no
# repositories section at all post sp-utt1i, let alone a selector for one.
#
# covers: spira/queue.sh
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

FAKE_HOME="$TMP/home"; mkdir -p "$FAKE_HOME/.config/systemd/user"

# A minimal git repo used as the mapped checkout.
QREPO="$TMP/repo"
git init -q -b main "$QREPO"
git -C "$QREPO" commit -q --allow-empty -m "init"

QNAME=fixture-queue-repo
RMAP="$TMP/repo-map"

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
want "protect: attribution note present" "attribution note" "$protect_out"
want "protect: bisect fallback mentioned" "bisect" "$protect_out"

# ===========================================================================
# GAP G5 — forge exits non-zero: no receipt is written, and the error names
# the failure (not a silent return or a receipt written on a call that failed).
# ===========================================================================
echo
echo "gap G5: forge branch-protect fails — no receipt, named error:"
cat > "$BIN/forge-fixture-fail.sh" <<FAILSCRIPT
#!/usr/bin/env bash
cmd="\${1:-}"
case "\$cmd" in
    branch-protect) exit 1 ;;
    *) exit 1 ;;
esac
FAILSCRIPT
chmod +x "$BIN/forge-fixture-fail.sh"
rm -f "$TMP/run/queue-protected-$QNAME"

fail_out="$(
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_REPO_MAP="$RMAP" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_FORGE="$BIN/forge-fixture-fail.sh" \
        bash "$HERE/queue.sh" protect "$QNAME" 2>&1
)"; fail_rc=$?

[ "$fail_rc" -ne 0 ] && ok "G5: exits non-zero when forge fails" \
    || bad "G5: exits non-zero when forge fails" "rc=$fail_rc"
want "G5: names the failure" "forge branch-protect failed" "$fail_out"
[ ! -f "$TMP/run/queue-protected-$QNAME" ] \
    && ok  "G5: no receipt written on forge failure" \
    || bad "G5: no receipt written on forge failure" "receipt exists"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
