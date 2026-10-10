#!/usr/bin/env bash
#
# first-run.sh — run each newly shipped oneshot unit once after a release activates.
#
#   first-run.sh --range <A>..<B> --repo <path>
#
# Every systemd/ template (.service, or the .service of a changed .timer) that differs
# between A and B and is Type=oneshot is started once, bounded (SPIRA_FIRSTRUN_TIMEOUT). A
# non-zero exit files one P1 against the bead named in the subject of the last commit that
# touched the template, carrying the exit and the journal tail. A template opts out with a
# line `# first-run: skip <reason>`; the reason is logged. A unit the instance does not
# install is skipped. Never fails the deploy; exits 1 only if a filing itself failed.
#
# covers: spira/first-run.sh spira/deploy.sh systemd/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/conf.sh"
. "$HERE/lib.sh"

SC="${SPIRA_SYSTEMCTL:-systemctl}"
JC="${SPIRA_JOURNALCTL:-journalctl}"
BD="${SPIRA_BD:-bd}"
BEAD="${SPIRA_FIRSTRUN_BEAD_SH:-$HERE/bead.sh}"
TIMEOUT="${SPIRA_FIRSTRUN_TIMEOUT:-300}"
TEMPLATES="${SPIRA_FIRSTRUN_TEMPLATES:-systemd}"
RANGE=""; REPO="${SPIRA_REPO:-}"
while [ $# -gt 0 ]; do
    case "$1" in
        --range) RANGE="${2:-}"; shift ;;
        --repo) REPO="${2:-}"; shift ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        *) printf 'first-run.sh: unknown argument %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done
[ -n "$RANGE" ] && [ -n "$REPO" ] || { printf 'first-run.sh: needs --range and --repo\n' >&2; exit 2; }
release="${RANGE##*..}"
marker="first-run [$release]"

inst_name() {
    case "$1" in spira-watch@.service) printf '%s\n' "$1" ;;
        spira-*.service) printf '%s-%s.service\n' "${1%.service}" "$SPIRA_INSTANCE" ;;
        *) printf '%s\n' "$1" ;; esac
}

changed="$(git -C "$REPO" diff --name-only --diff-filter=AM "$RANGE" -- "$TEMPLATES" 2>/dev/null)" || {
    printf 'first-run: cannot read range %s in %s\n' "$RANGE" "$REPO" >&2; exit 1; }
stems="$(printf '%s\n' "$changed" | sed -n 's#^.*/\([^/]*\)\.\(service\|timer\)$#\1#p' | sort -u)"

rc=0; ran=0; failed=0
for stem in $stems; do
    tpl="$TEMPLATES/$stem.service"
    text="$(git -C "$REPO" show "$release:$tpl" 2>/dev/null)" || continue
    printf '%s\n' "$text" | grep -q '^Type=oneshot' || continue
    unit="$(inst_name "$stem.service")"
    skip="$(printf '%s\n' "$text" | sed -n 's/^#[[:space:]]*first-run:[[:space:]]*skip[[:space:]]*//p' | head -n 1)"
    if [ -n "$skip" ]; then
        printf 'first-run: skip %s — %s\n' "$unit" "$skip"; continue
    fi
    "$SC" --user cat "$unit" >/dev/null 2>&1 || {
        printf 'first-run: skip %s — not installed on this instance\n' "$unit"; continue; }

    subject="$(git -C "$REPO" log -n 1 --format=%s "$RANGE" -- "$tpl" 2>/dev/null)"
    bead="$(printf '%s\n' "$subject" | grep -oE "\b${SPIRA_ID_PREFIX:-sp}-[a-z0-9]+(\.[0-9]+)*\b" | head -n 1)"
    ran=$((ran+1))
    "$SC" --user reset-failed "$unit" >/dev/null 2>&1 || true
    out="$(timeout "$TIMEOUT" "$SC" --user start "$unit" 2>&1)"; crc=$?
    if [ "$crc" -eq 0 ]; then
        printf 'first-run: PASS %s\n' "$unit"; continue
    fi
    failed=$((failed+1))
    printf 'first-run: FAIL %s (exit %s)\n' "$unit" "$crc"
    if [ -n "$bead" ] && timeout 5 "$BD" -C "$SPIRA_DB" comments "$bead" 2>/dev/null \
            | grep -qF "$marker $unit"; then
        printf 'first-run: %s already filed for %s\n' "$unit" "$release"; continue
    fi
    tail="$("$JC" --user -u "$unit" -n 20 --no-pager 2>&1 | tail -c 3000)"
    bodyf="$(mktemp)"
    {
        printf 'Unit %s, shipped by %s, failed its first run after release %s.\n\n' "$unit" "${bead:-an unnamed bead}" "$release"
        printf 'exit: %s\n\nsystemctl: %s\n\njournal tail:\n%s\n' "$crc" "$out" "$tail"
    } > "$bodyf"
    extra=()
    if [ -n "$bead" ]; then
        extra=(--parent "$bead")
        repo="$(timeout 5 "$BD" -C "$SPIRA_DB" show "$bead" --json 2>/dev/null | python3 -c 'import json,sys
d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d
print(next((l[5:] for l in d.get("labels") or [] if l.startswith("repo:")), ""))' 2>/dev/null)"
        [ -z "$repo" ] || extra+=(--repo "$repo")
    fi
    "$BEAD" file "unit $unit failed its first run after $release" \
        --for builder --priority 1 "${extra[@]}" --body-file "$bodyf" >/dev/null || rc=1
    rm -f "$bodyf"
    [ -z "$bead" ] || timeout 5 "$BD" -C "$SPIRA_DB" comments add "$bead" \
        "$marker $unit FAIL (exit $crc); follow-up filed." >/dev/null 2>&1 || true
done
printf 'first-run: %s unit(s) run, %s failed\n' "$ran" "$failed"
exit "$rc"
