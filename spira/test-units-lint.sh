#!/usr/bin/env bash
#
# test-units-lint.sh — one render pass and one install pass, checking every watcher and
# notifier unit's own contract instead of each suite paying for its own checkout clone and
# systemctl stub to ask the same four questions (D8): is it CPU-fenced, is it niced, did every
# @PLACEHOLDER@ resolve, and does every path in it come from configuration rather than a
# literal. test-watch-notify.sh, test-watch-refresh.sh and test-pr-notify.sh each used to
# render+install their own unit to ask this; consolidated here it is one clone and one pass.
#
# tier: T0
# covers: systemd/*.service systemd/*.timer systemd/install.sh UC-operator-channel-37
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

has()   { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "$2" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home"

# A harness tree that is NOT this checkout, so nothing here can read the operator's own
# configuration or units and report a pass it did not earn. Symlinked rather than copied
# (D8/row 32): the content only needs to be reachable, never mutated, so a second full copy
# of spira/*.sh buys nothing a symlink does not.
CLONE="$TMP/clone"
mkdir -p "$CLONE/spira" "$CLONE/cockpit"
ln -s "$HERE"/*.sh "$CLONE/spira/"
cp -r "$ROOT/systemd" "$CLONE/systemd"
ln -s "$ROOT/beads-push.sh" "$ROOT/concierge.sh" "$CLONE/"

COCKPIT="$TMP/elsewhere/cockpit"; RUN="$TMP/elsewhere/run"
mkdir -p "$COCKPIT" "$RUN/watchd"
MAN="$TMP/watchers"
# A daemon row, not a log row: only "daemon" watchers get a spira-watch@ unit
# (watchd.sh cmd_units), which is the one this suite needs rendered.
printf 'alpha|daemon|/usr/bin/true|\n' > "$MAN"
CONF="$TMP/spira.conf"
# SPIRA_PROD pinned to empty: render() then falls back to SPIRA_HOME ($CLONE/spira), so
# ExecStart resolves from the clone rather than this box's own derived release path.
printf 'SPIRA_RUN = %s\nSPIRA_COCKPIT = %s\nSPIRA_WATCHERS = %s\nSPIRA_PROD = \n' \
    "$RUN" "$COCKPIT" "$MAN" > "$CONF"

printf 'test-units-lint.sh\n'

# =======================================================================================
# THE RENDER PASS. Cheap (no systemctl, no install run) — one Python substitution per unit,
# which is what every removed per-suite section actually needed to check content.
# =======================================================================================
rendered="$(env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$CONF" \
    bash "$CLONE/systemd/install.sh" --render 2>"$TMP/render.err")"
is "the render pass produced units" "yes" "$([ -n "$rendered" ] && echo yes || echo no)"
# `note:` lines are install.sh commenting on units this suite does not touch (an unbuilt
# Rust binary elsewhere in UNITS, not rendered here) — informational, not a render failure.
is "and reported no error" "" "$(grep -v '^note:\|^      Build it:' "$TMP/render.err")"

block() {  # block <unit-name> -> its rendered content
    awk -v m="===== $1 =====" 'index($0,m)==1{f=1;next} /^===== /{f=0} f' <<< "$rendered"
}

# EVERY PATH IN A RENDERED UNIT MUST COME FROM CONFIGURATION. Both roots are pinned to
# non-defaults above, so a path written as a literal in a template cannot pass by coincidence.
paths_are_configured() {  # paths_are_configured <label> <unit-text>
    local stray="" p
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        case "$p" in "$CLONE"|"$CLONE"/*|"$RUN"|"$RUN"/*) ;; *) stray="$stray $p" ;; esac
    done < <(sed 's|file://|file:|' <<< "$2" | grep -oE '[=:]/[^ ]+' | sed 's/^[=:]//')
    is "$1: every path in it came from configuration" "" "$stray"
}

for svc in spira-watch-notify-prod.service spira-watch-refresh-prod.service \
           spira-pr-notify-prod.service spira-mail-tidy-prod.service; do
    unit="$(block "$svc")"
    has   "$svc: it is CPU-fenced"                "$unit" "CPUQuota="
    has   "$svc: and niced"                       "$unit" "Nice="
    hasnt "$svc: no placeholder survives into it" "$unit" "@"
    paths_are_configured "$svc" "$unit"
done

for tmr in spira-watch-notify-prod.timer spira-watch-refresh-prod.timer \
           spira-pr-notify-prod.timer spira-mail-tidy-prod.timer; do
    unit="$(block "$tmr")"
    hasnt "$tmr: no placeholder survives into it" "$unit" "@"
    has   "$tmr: it fires periodically"           "$unit" "OnUnitActiveSec="
done

# THE WATCHER TEMPLATE. Rendered once per manifest row (here, "alpha"), so %i is gone and
# replaced by the watcher's own name — a real per-instance unit, not a systemd template.
watch_unit="$(block "spira-watch-alpha-prod.service")"
has   "spira-watch@ (alpha): it is CPU-fenced"          "$watch_unit" "CPUQuota="
has   "spira-watch@ (alpha): and niced"                 "$watch_unit" "Nice="
hasnt "spira-watch@ (alpha): no placeholder survives, and %i is gone" "$watch_unit" "@"
hasnt "spira-watch@ (alpha): and the template specifier is gone too"  "$watch_unit" "%i"
paths_are_configured "spira-watch@ (alpha)" "$watch_unit"

# THE NOTIFY PERIOD MUST BE WELL UNDER THE THRESHOLD, or the granularity with which
# staleness is noticed doubles the wait the threshold was set to allow (UC-operator-channel-28).
notify_tmr="$(block spira-watch-notify-prod.timer)"
period="$(sed -n 's/^OnUnitActiveSec=\([0-9]*\)min$/\1/p' <<< "$notify_tmr")"
default_age="$(env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$TMP/nonexistent" \
    bash -c ". '$CLONE/spira/conf.sh'; printf '%s' \"\$SPIRA_NOTIFY_AGE\"")"
is "the notify timer states a period in minutes" "yes" "$([ -n "$period" ] && echo yes || echo no)"
is "the threshold has a default"                 "yes" "$([ -n "$default_age" ] && echo yes || echo no)"
is "and a pass happens several times inside it"  "yes" \
   "$([ "$(( period * 60 * 3 ))" -le "$default_age" ] && echo yes || echo no)"

# =======================================================================================
# THE INSTALL PASS. One run, stubbed systemctl, checked for all four timers at once: install
# enables the TIMER, never the SERVICE behind it — enabling the service as well would also
# run it once at boot, outside the schedule (law-fence-loops-on-shared-hardware).
# =======================================================================================
STUB="$TMP/stub"; mkdir -p "$STUB"
cat > "$STUB/systemctl" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/systemctl.log"
exit 0
EOF
chmod +x "$STUB/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$STUB/loginctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$STUB/spira-supervise"
chmod +x "$STUB/systemctl" "$STUB/loginctl" "$STUB/spira-supervise"
IHOME="$TMP/ihome"; mkdir -p "$IHOME"
: > "$TMP/systemctl.log"
printf 'SPIRA_RUN = %s\nSPIRA_COCKPIT = %s\nSPIRA_WATCHERS = %s\nSPIRA_PATH = %s\nSPIRA_PROD = %s\n' \
    "$RUN" "$ROOT/cockpit" "$MAN" "$STUB" "$HERE" > "$TMP/install.conf"
env -i HOME="$IHOME" PATH="$STUB:$PATH" SPIRA_CONF="$TMP/install.conf" \
    SPIRA_INSTALL_FORCE=1 SPIRA_HOME="$HERE" \
    "SPIRA_SUPERVISE_BIN=$STUB/spira-supervise" \
    bash "$CLONE/systemd/install.sh" > "$TMP/install.out" 2>&1
ilog="$(cat "$TMP/systemctl.log")"
has "the install ran" "$ilog" "daemon-reload"
for pair in "spira-watch-notify-prod" "spira-watch-refresh-prod" \
            "spira-pr-notify-prod" "spira-mail-tidy-prod"; do
    has   "$pair: install enabled its timer"        "$ilog" "enable --now $pair.timer"
    hasnt "$pair: but not the service behind it"    "$ilog" "enable --now $pair.service"
done

tl_summary
