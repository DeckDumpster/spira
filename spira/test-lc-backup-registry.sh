#!/usr/bin/env bash
#
# test-lc-backup-registry.sh — lc-backup.sh run copies the test registry through its binary and
# puts it off-box, stamps it, and age reports it beside the databases.
#
#   ./test-lc-backup-registry.sh
#
# tier: T1
# covers: spira/lc-backup.sh test-registry/** spira/conf.d/SPIRA_LC_BACKUP_* docs/operating.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin" "$T/run" "$T/dest"
cat > "$T/bin/test-registry" <<'STUB'
#!/usr/bin/env bash
echo "test-registry $*" >> "$STUB_LOG"
[ "${STUB_BACKUP_FAIL:-}" = 1 ] && { echo "disk full" >&2; exit 1; }
[ "$1" = backup ] && [ "$2" = --out ] && echo data > "$3" && echo "$3 5 2"
STUB
cat > "$T/bin/spira-lc" <<'STUB'
#!/usr/bin/env bash
echo "spira-lc $*" >> "$STUB_LOG"
[ "${STUB_PUT_FAIL:-}" = 1 ] && { echo "denied" >&2; exit 1; }
[ "$1" = admin-archive-put ] && echo "archived"
exit 0
STUB
chmod +x "$T/bin/"*
tl_config SPIRA_RUN="$T/run" SPIRA_LC_BACKUP_DB="" SPIRA_LC_BACKUP_URL="file://$T/dest" \
    SPIRA_LC_BACKUP_SYNC_DIR="$T/sync" SPIRA_LC_BACKUP_ALLOW_LOCAL=1 SPIRA_LC_BACKUP_MAX_AGE=1000
SPIRA_RUN="$T/run"
# shellcheck disable=SC1091
. "$HERE/lc-backup.sh"
_lcb_dbs() { echo alpha; }
_lcb_run_db() { echo "ran $1" >> "$T/dbs"; return "${DB_RC:-0}"; }
export PATH="$T/bin:$PATH" STUB_LOG="$T/log"
stamp="$SPIRA_RUN/lc-backup.test-registry.ok"

out="$(cmd_age 2>&1)"; rc=$?
grep -q '^test-registry: never' <<<"$out" && ok "age prints never with no stamp" || bad "age prints never with no stamp" "$out"
[ "$rc" -ne 0 ] && ok "age exits non-zero with no stamp" || bad "age exits non-zero with no stamp" "rc=$rc"

STUB_BACKUP_FAIL=1 cmd_run >"$T/out" 2>&1; rc=$?
[ "$rc" -eq 1 ] && ok "a failing backup makes run exit 1" || bad "a failing backup makes run exit 1" "rc=$rc"
grep -q '^lc-backup: test-registry: backup step failed' "$T/out" && ok "the failed step is named" || bad "the failed step is named" "$(cat "$T/out")"
[ ! -e "$stamp" ] && ok "a failing backup writes no stamp" || bad "a failing backup writes no stamp" "stamp exists"
grep -q admin-archive-put "$T/log" && bad "no upload after a failed backup" "uploaded" || ok "no upload after a failed backup"

: > "$T/log"; cmd_run >"$T/out" 2>&1; rc=$?
[ "$rc" -eq 0 ] && ok "run exits 0" || bad "run exits 0" "rc=$rc $(cat "$T/out")"
grep -n 'test-registry backup --out' "$T/log" | head -1 | grep -q "$T/sync/test-registry/.*\.sqlite" && ok "run calls backup into the sync dir" || bad "run calls backup into the sync dir" "$(cat "$T/log")"
grep -q 'spira-lc admin-archive-put --file .* --key test-registry-[0-9T]*Z\.sqlite$' "$T/log" && ok "run puts the copy under test-registry-<stamp>.sqlite" || bad "run puts the copy under test-registry-<stamp>.sqlite" "$(cat "$T/log")"
[ "$(grep -o '^[a-z-]* [a-z-]*' "$T/log" | tr '\n' ,)" = "test-registry backup,spira-lc admin-archive-put," ] && ok "backup precedes the upload" || bad "backup precedes the upload" "$(cat "$T/log")"
read -r t key dash < "$stamp" 2>/dev/null
[[ "$key" == test-registry-*.sqlite && "$dash" = - ]] && ok "the stamp is <epoch> <key> -" || bad "the stamp is <epoch> <key> -" "$(cat "$stamp" 2>/dev/null)"

cp "$stamp" "$T/prev"; sleep 1
STUB_PUT_FAIL=1 cmd_run >"$T/out" 2>&1; rc=$?
[ "$rc" -eq 1 ] && grep -q '^lc-backup: test-registry: upload step failed' "$T/out" && ok "a failing upload exits 1 naming the step" || bad "a failing upload exits 1 naming the step" "rc=$rc $(cat "$T/out")"
cmp -s "$stamp" "$T/prev" && ok "a failing upload leaves the previous stamp" || bad "a failing upload leaves the previous stamp" "changed"
DB_RC=1 cmd_run >"$T/out" 2>&1; rc=$?
[ "$rc" -eq 1 ] && [ "$(cmp -s "$stamp" "$T/prev"; echo $?)" = 1 ] && ok "a failing database still lets the registry ship, and run exits 1" || bad "a failing database still lets the registry ship, and run exits 1" "rc=$rc"

touch "$T/sync/test-registry/orphan.sqlite.tmp"
sleep 1; cmd_run >/dev/null 2>&1
[ "$(ls "$T/sync/test-registry" | wc -l)" -eq 1 ] && ok "only the newest local copy is kept" || bad "only the newest local copy is kept" "$(ls "$T/sync/test-registry")"

out="$(cmd_age 2>&1)"
grep -q '^test-registry: ok ' <<<"$out" && ok "age prints test-registry ok" || bad "age prints test-registry ok" "$out"
out="$(cmd_age --now $(( $(date +%s) + 5000 )) 2>&1)"; rc=$?
grep -q '^test-registry: stale ' <<<"$out" && [ "$rc" -ne 0 ] && ok "an old stamp is stale and red" || bad "an old stamp is stale and red" "rc=$rc $out"
cp "$stamp" "$T/good"; echo garbage > "$stamp"
out="$(cmd_age 2>&1)"; rc=$?
grep -q '^test-registry: unreadable' <<<"$out" && [ "$rc" -ne 0 ] && ok "a garbage stamp is unreadable and red" || bad "a garbage stamp is unreadable and red" "rc=$rc $out"
cp "$T/good" "$stamp"
_lcb_measure_db() { case "$1" in alpha) echo "stale 9999" ;; *) echo "ok 1" ;; esac; }
out="$(cmd_age 2>&1)"; rc=$?
[ "$out" = "$(printf 'alpha: stale 9999\ntest-registry: ok 1')" ] && [ "$rc" -ne 0 ] && ok "a stale database is listed before a fresh registry and stays red" || bad "a stale database is listed before a fresh registry and stays red" "rc=$rc $out"
tl_summary
