#!/usr/bin/env bash
# test-first-run.sh — a release's newly shipped oneshot units run once; a failure files one P1
# against the shipping bead with the error text, a pass files nothing, an opt-out is logged.
#
# tier: T1
# covers: spira/first-run.sh spira/deploy.sh systemd/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
export SPIRA_CONF=/nonexistent SPIRA_DB=/nonexistent
tl_config SPIRA_DB=/nonexistent SPIRA_INSTANCE=zz SPIRA_ID_PREFIX=sp

echo "test-first-run.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
FIX="$TMP/fix"; mkdir -p "$FIX"; export FIX

cat > "$TMP/systemctl" <<'STUB'
#!/usr/bin/env bash
shift
case "$1" in
    cat) [ "$2" != spira-absent-zz.service ] ;;
    reset-failed) ;;
    start) echo "$2" >> "$FIX/started"
        case "$2" in spira-bad-zz.service) echo "Job failed: exit-code 2" >&2; exit 2 ;; esac ;;
esac
STUB
cat > "$TMP/journalctl" <<'STUB'
#!/usr/bin/env bash
echo "spira-bad: SPIRA_TOML is not set"
STUB
cat > "$TMP/bd" <<'STUB'
#!/usr/bin/env bash
shift 2
case "$1" in
    show) echo '[{"id":"sp-ship1","labels":["repo:zzrepo"]}]' ;;
    comments)
        if [ "${2:-}" = add ]; then printf '%s\n' "$4" >> "$FIX/comments"; else cat "$FIX/comments" 2>/dev/null; fi ;;
esac
STUB
cat > "$TMP/bead.sh" <<'STUB'
#!/usr/bin/env bash
echo "FILE $*" >> "$FIX/filed"
for a in "$@"; do [ -f "$a" ] && cat "$a" >> "$FIX/filed"; done
STUB
chmod +x "$TMP"/systemctl "$TMP"/journalctl "$TMP"/bd "$TMP"/bead.sh
export SPIRA_SYSTEMCTL="$TMP/systemctl" SPIRA_JOURNALCTL="$TMP/journalctl" SPIRA_BD="$TMP/bd" SPIRA_FIRSTRUN_BEAD_SH="$TMP/bead.sh"
tl_config SPIRA_BD="$SPIRA_BD"

R="$TMP/repo"; git init -q "$R"
g() { git -C "$R" -c user.name=t -c user.email=t@t "$@"; }
mkdir "$R/systemd"
g commit -q --allow-empty -m base; g tag a
unit() { printf '[Service]\nType=oneshot\n%sExecStart=/bin/true\n' "${2:-}" > "$R/systemd/$1"; }
unit spira-bad.service; unit spira-good.service; unit spira-skipme.service $'# first-run: skip needs a live broker\n'
unit spira-absent.service
printf '[Service]\nType=simple\nExecStart=/bin/true\n' > "$R/systemd/spira-daemon.service"
g add -A; g commit -q -m "spira: land sp-ship1 — new units"; g tag b

out="$("$HERE/first-run.sh" --range a..b --repo "$R" 2>&1)"; rc=$?
want "failing unit is reported"           "FAIL spira-bad-zz.service (exit 2)" "$out"
want "passing unit is reported"           "PASS spira-good-zz.service" "$out"
want "opt-out logs its reason"            "skip spira-skipme-zz.service — needs a live broker" "$out"
want "uninstalled unit is skipped"        "skip spira-absent-zz.service — not installed" "$out"
nowant "opted-out unit is never started"  "spira-skipme-zz.service" "$(cat "$FIX/started")"
nowant "long-running unit is never started" "spira-daemon" "$(cat "$FIX/started")"
filed="$(cat "$FIX/filed")"
is   "exactly one filing"                 "1" "$(grep -c '^FILE' "$FIX/filed")"
want "filing is P1"                       "--priority 1" "$filed"
want "filing names the shipping bead"     "--parent sp-ship1" "$filed"
want "filing takes the bead's repo"       "--repo zzrepo" "$filed"
want "filing carries the error text"      "SPIRA_TOML is not set" "$filed"
want "filing carries the exit"            "exit: 2" "$filed"

: > "$FIX/filed"
"$HERE/first-run.sh" --range a..b --repo "$R" >/dev/null 2>&1
is   "a re-run does not file twice"       "" "$(cat "$FIX/filed")"

: > "$FIX/filed"; : > "$FIX/comments"; rm -f "$FIX/started"
g rm -q --cached -r systemd >/dev/null; rm -f "$R/systemd/spira-bad.service"; g add -A; g commit -q -m "drop bad"; g tag c
"$HERE/first-run.sh" --range b..c --repo "$R" >/dev/null 2>&1
is   "an unchanged unit files nothing"    "" "$(cat "$FIX/filed")"
is   "an unchanged unit is not started"   "" "$(cat "$FIX/started" 2>/dev/null)"
