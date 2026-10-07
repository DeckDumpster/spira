#!/usr/bin/env bash
#
# test-units-lint.sh — one render pass and one install pass, checking every watcher and
# notifier unit's own contract instead of each suite paying for its own checkout clone and
# systemctl stub to ask the same questions (D8): did every @PLACEHOLDER@ resolve, and does
# every path in it come from configuration rather than a literal. It also holds the whole
# render to law-isolate-greedy-work-in-vms (sp-b4oct): no unit carries CPUQuota=, Nice= or
# IOSchedulingClass= — the OS schedules, greedy work goes to a VM. test-watch-notify.sh, test-watch-refresh.sh and test-pr-notify.sh each used to
# render+install their own unit to ask this; consolidated here it is one clone and one pass.
#
# tier: T0
# covers: systemd/*.service systemd/*.timer install/src/bin/units_install.rs UC-operator-channel-37
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
# SPIRA_HOME IS THE HOME NOW (locate_home no longer searches): every binary reads
# <home>/conf.d directly, and the *.sh glob above never matches the conf.d directory.
ln -s "$HERE/conf.d" "$CLONE/spira/conf.d"

# conf.sh's spira.toml auto-convert shells out to spira-config (sp-zs04v.2), found by name
# on the suite's PATH (sp-gypjk).
command -v spira-config >/dev/null 2>&1 || bail "spira-config is not on PATH"
cp -r "$ROOT/systemd" "$CLONE/systemd"
ln -s "$ROOT/beads-push.sh" "$ROOT/concierge.sh" "$CLONE/"

COCKPIT="$TMP/elsewhere/cockpit"; RUN="$TMP/elsewhere/run"
mkdir -p "$COCKPIT" "$RUN/watchd"
MAN="$TMP/watchers"
# A daemon row, not a log row: only "daemon" watchers get a spira-watch@ unit
# (watchd units), which is the one this suite needs rendered.
printf 'alpha|daemon|/usr/bin/true|\n' > "$MAN"
RENDER_DIR="$TMP/render"; mkdir -p "$RENDER_DIR"
CONF="$RENDER_DIR/spira.conf"
# ITS OWN DIRECTORY, SEPARATE FROM THE INSTALL PASS'S CONF BELOW: spira_toml_resolve's
# pinned-conf auto-convert writes its spira.toml beside the pinned conf (sp-zs04v.4), so two
# unrelated conf files sharing one directory would collide on that one toml — the render
# pass's (SPIRA_PROD pinned empty) landing first and the install pass silently inheriting
# it instead of converting its own conf, exactly the cross-contamination
# law-absence-needs-a-positive-control's sibling scar (sp-zs04v.2) warns about.
#
# SPIRA_PROD pinned to empty: render() then falls back to SPIRA_HOME ($CLONE/spira), so
# ExecStart resolves from the clone rather than this box's own derived release path.
printf 'SPIRA_ID_PREFIX = sp\nSPIRA_RUN = %s\nSPIRA_COCKPIT = %s\nSPIRA_WATCHERS = %s\nSPIRA_PROD = \n' \
    "$RUN" "$COCKPIT" "$MAN" > "$CONF"

printf 'test-units-lint.sh\n'

# =======================================================================================
# THE RENDER PASS. Cheap (no systemctl, no install run) — one substitution pass per unit,
# which is what every removed per-suite section actually needed to check content.
# =======================================================================================
# SPIRA_WATCHERS IN THE ENVIRONMENT, not only the conf: units.sh runs `watchd` by name
# (sp-gypjk), i.e. the tree's own copy, whose conf.sh would otherwise find the tree's
# spira.toml before this suite's pinned conf. The environment wins over any config file.
#
# SPIRA_HOME/SPIRA_RUN/SPIRA_COCKPIT/SPIRA_PROD IN THE ENVIRONMENT TOO (sp-31dm0): $CONF
# above still exists for the tools that source conf.sh (watchd.sh), but units-install is a
# compiled binary that never sources conf.sh and so never reads a spira.conf file at all —
# it only reads real environment variables (install::bootstrap::host_from_env). Leaving
# these four to the conf file alone rendered every @SPIRA_HOME@/@SPIRA_RUN@/@SPIRA_PROD@
# path empty or wrong (e.g. "/spira/watchd.sh", "append:/watch-notify.log") without units-
# install ever saying so — paths_are_configured caught it as a stray path on
# spira-notify-prod.service, the one unit here whose [Service] block leans on all
# three. SPIRA_HOME = $CLONE/spira (this suite's own clone, never the real checkout, per
# the comment above); SPIRA_PROD is left empty on purpose (render()'s own fallback to
# SPIRA_HOME is exactly what the comment below this block is testing).
tl_config SPIRA_WATCHERS="$MAN" SPIRA_RUN="$RUN" SPIRA_COCKPIT="$COCKPIT" SPIRA_PROD="" \
    SPIRA_LC_PASSWORD_FILE="$RUN/lc.credential" SPIRA_DB="$RUN/db" SPIRA_DOLT_DATA=""
# SPIRA_LC_PASSWORD_FILE above no longer reaches units-install's render for this key: it is
# PROCEDURAL (spira_config::resolve::lc_credential_default), computed straight from
# XDG_CONFIG_HOME/HOME with no environment-override rung at all (one source of config, per
# Ryan 2026-10-05) — point XDG_CONFIG_HOME under $RUN so the computed default lands
# somewhere paths_are_configured() accepts, instead of this suite's own $TMP/home.
# SPIRA_DOLT_DATA="" (one source of config, per Ryan 2026-10-05): the complete fixture
# declares a non-empty dolt_data, so manifest.rs's dolt-data gate — previously closed by an
# unset/derived-empty default — now renders dolt-tmp-prune.service/.timer into this pass too.
# That unit is a sanctioned, documented exception to law-isolate-greedy-work-in-vms (it
# genuinely carries CPUQuota=/Nice=/IOSchedulingClass= on purpose, sp-n1l7y) that this
# suite's render fixture never meant to exercise — it checks three specific "prod" units plus
# the watch template, not dolt's own unit. Declaring it empty restores this suite's original,
# pre-migration scope instead of weakening the global "no CPUQuota=" assertion below.
rendered="$(env -i HOME="$TMP/home" PATH="$PATH" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$CONF" \
    XDG_CONFIG_HOME="$RUN/xdg-config" \
    SPIRA_HOME="$CLONE/spira" units-install --render 2>"$TMP/render.err")"
is "the render pass produced units" "yes" "$([ -n "$rendered" ] && echo yes || echo no)"
# `note:` lines are install.sh commenting on units this suite does not touch (an unbuilt
# Rust binary elsewhere in UNITS, not rendered here) — informational, not a render failure.
is "and reported no error" "" "$(grep -v '^note:\|^      Build it:' "$TMP/render.err")"

block() {  # block <unit-name> -> its rendered content
    awk -v m="===== $1 =====" 'index($0,m)==1{f=1;next} /^===== /{f=0} f' <<< "$rendered"
}

# EVERY PATH IN A RENDERED UNIT MUST COME FROM CONFIGURATION. Both roots are pinned to
# non-defaults above, so a path written as a literal in a template cannot pass by coincidence.
#
# $SPIRA_TOML ITSELF IS ALSO AN ALLOWED VALUE (one source of config, per Ryan 2026-10-05):
# every unit now carries `Environment=SPIRA_TOML=@SPIRA_TOML@` (sp-v62vn follow-up — the
# launched process reaches its config the same way this installer did), so its literal value
# — this suite's own base:override pair, neither of which lives under $CLONE or $RUN — shows
# up as a "path" in every rendered unit. That is the suite's own configuration passed through
# unchanged, not a template literal, so it is exempted by exact match only — a stray real path
# that merely starts with one half of $SPIRA_TOML still gets caught.
paths_are_configured() {  # paths_are_configured <label> <unit-text>
    local stray="" p
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        case "$p" in
            "$CLONE"|"$CLONE"/*|"$RUN"|"$RUN"/*) ;;
            "$SPIRA_TOML") ;;
            *) stray="$stray $p" ;;
        esac
    done < <(sed 's|file://|file:|' <<< "$2" | grep -oE '[=:]/[^ ]+' | sed 's/^[=:]//')
    is "$1: every path in it came from configuration" "" "$stray"
}

# NO UNIT IS CPU-FENCED OR NICED (sp-b4oct, law-isolate-greedy-work-in-vms). Checked over
# EVERY rendered unit, not just the watchers. POSITIVE CONTROL first
# (law-absence-needs-a-positive-control): the same grep must find the directives in a planted
# unit, and the render must actually hold many units' [Service] lines, or an empty or
# truncated render would pass the absence check for free.
_fence_re='^(CPUQuota|Nice|IOSchedulingClass)='
is "positive control: the fence grep finds a planted CPUQuota=/Nice=" "2" \
    "$(printf '[Service]\nCPUQuota=35%%\nNice=10\nType=oneshot\n' | grep -cE "$_fence_re")"
_n_exec="$(grep -c '^ExecStart=' <<< "$rendered")"
is "positive control: the render holds at least 20 units' ExecStart= lines" "yes" \
    "$([ "${_n_exec:-0}" -ge 20 ] && echo yes || echo "no ($_n_exec)")"
_io_fenced='spira-landing-pass-prod.service spira-sop-lint-prod.service'
_fenced_out="$(awk -v ok="$_io_fenced" -v re="$_fence_re" '
    /^===== /{n=split(ok,a," ");skip=0;for(i=1;i<=n;i++)if(index($0,"===== " a[i] " =====")==1)skip=1;next}
    !skip && $0 ~ re' <<< "$rendered")"
is "no rendered unit outside the IO-fenced pair carries CPUQuota=, Nice= or IOSchedulingClass=" "" "$_fenced_out"
is "positive control: the exemption skips only the named units" "1" \
    "$(printf '===== spira-sop-lint-prod.service =====\nNice=19\n===== other.service =====\nNice=5\n' |
        awk -v ok="$_io_fenced" -v re="$_fence_re" '
        /^===== /{n=split(ok,a," ");skip=0;for(i=1;i<=n;i++)if(index($0,"===== " a[i] " =====")==1)skip=1;next}
        !skip && $0 ~ re' | grep -c .)"
for svc in $_io_fenced; do
    has "$svc: IO-fenced idle" "$(block "$svc")" "IOSchedulingClass=idle"
done

for svc in spira-notify-prod.service spira-refresh-prod.service \
           spira-mail-tidy-prod.service; do
    unit="$(block "$svc")"
    has   "$svc: it rendered"                     "$unit" "ExecStart="
    hasnt "$svc: no placeholder survives into it" "$unit" "@"
    paths_are_configured "$svc" "$unit"
done

for tmr in spira-notify-prod.timer spira-refresh-prod.timer \
           spira-mail-tidy-prod.timer; do
    unit="$(block "$tmr")"
    hasnt "$tmr: no placeholder survives into it" "$unit" "@"
    has   "$tmr: it fires periodically"           "$unit" "OnUnitActiveSec="
done

# THE WATCHER TEMPLATE. Rendered once per manifest row (here, "alpha"), so %i is gone and
# replaced by the watcher's own name — a real per-instance unit, not a systemd template.
watch_unit="$(block "spira-watch-alpha-prod.service")"
has   "spira-watch@ (alpha): it rendered"               "$watch_unit" "ExecStart="
hasnt "spira-watch@ (alpha): no placeholder survives, and %i is gone" "$watch_unit" "@"
hasnt "spira-watch@ (alpha): and the template specifier is gone too"  "$watch_unit" "%i"
paths_are_configured "spira-watch@ (alpha)" "$watch_unit"

# THE NOTIFY PERIOD MUST BE WELL UNDER THE THRESHOLD, or the granularity with which
# staleness is noticed doubles the wait the threshold was set to allow (UC-operator-channel-28).
notify_tmr="$(block spira-notify-prod.timer)"
period="$(sed -n 's/^OnUnitActiveSec=\([0-9]*\)min$/\1/p' <<< "$notify_tmr")"
default_age="$(env -i HOME="$TMP/home" PATH="$PATH" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$TMP/nonexistent" \
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
chmod +x "$STUB/systemctl" "$STUB/loginctl"
# A RELEASE-SHAPED PROD ROOT (sp-gypjk): units ExecStart @SPIRA_PROD_ROOT@/bin/<tool>, and
# install refuses a unit whose target is not executable. The root links this tree's own
# top-level entries and adds a bin/ of no-op stubs, one per binary any unit names.
PRODROOT="$TMP/prodroot"; mkdir -p "$PRODROOT/bin"
for _e in "$ROOT"/*; do ln -s "$_e" "$PRODROOT/"; done
for _b in $(grep -oh '@SPIRA_PROD_ROOT@/bin/[a-z0-9-]*' "$ROOT"/systemd/*.service | sed 's#.*/bin/##' | sort -u); do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$PRODROOT/bin/$_b"; chmod +x "$PRODROOT/bin/$_b"
done; unset _b _e
IHOME="$TMP/ihome"; mkdir -p "$IHOME"
: > "$TMP/systemctl.log"
printf 'SPIRA_RUN = %s\nSPIRA_COCKPIT = %s\nSPIRA_WATCHERS = %s\nSPIRA_PATH = %s\nSPIRA_PROD = %s\n' \
    "$RUN" "$ROOT/cockpit" "$MAN" "$STUB" "$PRODROOT/spira" > "$TMP/install.conf"
# SPIRA_RUN/SPIRA_COCKPIT/SPIRA_PROD IN THE ENVIRONMENT TOO (sp-31dm0), same reason as the
# render pass above: units-install never sources conf.sh, so $TMP/install.conf alone never
# reaches it. Left to the conf file alone, @SPIRA_PROD_ROOT@ fell back to dirname(SPIRA_HOME)
# — this container's own real checkout root, not PRODROOT — so ExecStart pointed at this
# box's /workspace/bin/sentinel (not yet built in this pass) instead of PRODROOT/bin's stub.
# SPIRA_MAIL no longer derives from SPIRA_RUN (one source of config, per Ryan 2026-10-05):
# left to the fixture's own default, install's own "mail ensure concierge" step tries to
# create the mailbox under the fictional /fixture/userhome/... tree and refuses with
# "Permission denied".
tl_config SPIRA_WATCHERS="$MAN" SPIRA_RUN="$RUN" SPIRA_COCKPIT="$ROOT/cockpit" \
    SPIRA_PROD="$PRODROOT/spira" SPIRA_MAIL="$RUN/mail"
env -i HOME="$IHOME" PATH="$STUB:$PATH" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$TMP/install.conf" \
    SPIRA_INSTALL_FORCE=1 SPIRA_HOME="$HERE" \
    units-install > "$TMP/install.out" 2>&1
ilog="$(cat "$TMP/systemctl.log")"
has "the install ran" "$ilog" "daemon-reload"
case "$ilog" in *daemon-reload*) ;; *) tail -20 "$TMP/install.out" | sed 's/^/# install: /' ;; esac
for pair in "spira-notify-prod" "spira-refresh-prod" \
            "spira-mail-tidy-prod"; do
    has   "$pair: install enabled its timer"        "$ilog" "enable --now $pair.timer"
    hasnt "$pair: but not the service behind it"    "$ilog" "enable --now $pair.service"
done

# sp-rgfi8: cert-sweep needs a git checkout; @SPIRA_HOME@ is a release dir with no .git.
for _u in full sample; do
    if grep -q '^ExecStart=.*cert-sweep pass.*--repo @SPIRA_REPO@' "$ROOT/systemd/spira-cert-sweep-$_u.service"; then
        ok "cert-sweep-$_u passes --repo @SPIRA_REPO@"
    else
        bad "cert-sweep-$_u --repo must be @SPIRA_REPO@ (git checkout)" "not @SPIRA_REPO@"
    fi
done

lone_pct() { grep -E '^Exec[A-Za-z]*=' "$1" | sed 's/%%//g' | grep -c '%[A-Za-z]'; }
printf 'ExecCondition=/bin/bash -c %s\n' "'printf \"%s\"'" > "$TMP/planted.service"
[ "$(lone_pct "$TMP/planted.service")" -gt 0 ] && ok "lone-% matcher flags a planted unescaped specifier" \
    || bad "lone-% matcher flags a planted unescaped specifier" "matcher silent on planted offender"
n="$(lone_pct "$ROOT/systemd/spira-ops.service")"
[ "$n" -eq 0 ] && ok "spira-ops.service Exec lines carry no unescaped % specifier" \
    || bad "spira-ops.service Exec lines carry no unescaped % specifier" "$n line(s) with a lone %"

# EVERY ExecStart BEGINS WITH A PLACEHOLDER (sp-ycxt2). doctor's check_prod_checkout — which
# deploy's pre-health runs — reads ExecStart's first path and refuses any installed unit
# running from outside $SPIRA_RELEASES; a literal /bin/bash wrapper in spira-ops.service
# refused every deploy. Positive control: the matcher flags a planted offender.
bare_exec() { grep -hE '^ExecStart=-?/' "$@" 2>/dev/null; }
printf 'ExecStart=/bin/bash -c %s\n' "'exec x'" > "$TMP/planted-exec.service"
[ -n "$(bare_exec "$TMP/planted-exec.service")" ] && ok "bare-ExecStart matcher flags a planted literal path" \
    || bad "bare-ExecStart matcher flags a planted literal path" "matcher silent on planted offender"
_bare="$(cd "$ROOT/systemd" && grep -lE '^ExecStart=-?/' *.service 2>/dev/null | tr '\n' ' ')"
is "every template's ExecStart begins with a placeholder (no literal path)" "" "${_bare% }"

tl_summary
