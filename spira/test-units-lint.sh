#!/usr/bin/env bash
#
# test-units-lint.sh — one install pass against a stubbed systemctl: install enables each
# timer, never the service behind it. The render and template-text checks (placeholders,
# configured paths, fences, ExecStart/SPIRA_TOML/store ordering) are cargo tests in
# install/tests/units_lint.rs.
#
# tier: T0
# covers: systemd/*.service systemd/*.timer install/src/bin/units_install.rs install/tests/units_lint.rs UC-operator-channel-37
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM

has()   { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "$2" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home"
RUN="$TMP/elsewhere/run"; mkdir -p "$RUN/watchd"
MAN="$TMP/watchers"
printf 'alpha|daemon|/usr/bin/true|\n' > "$MAN"

printf 'test-units-lint.sh\n'

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
# SPIRA_RUN/SPIRA_COCKPIT/SPIRA_PROD IN THE ENVIRONMENT TOO (sp-31dm0), units-install never sources conf.sh, so $TMP/install.conf alone never
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

period="$(sed -n 's/^OnUnitActiveSec=\([0-9]*\)min$/\1/p' "$ROOT/systemd/spira-notify.timer")"
default_age="$(env -i HOME="$TMP/home" PATH="$PATH" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$TMP/nonexistent" \
    bash -c ". '$HERE/conf.sh'; printf '%s' \"\$SPIRA_NOTIFY_AGE\"")"
is "the notify timer states a period in minutes" "yes" "$([ -n "$period" ] && echo yes || echo no)"
is "the threshold has a default"                 "yes" "$([ -n "$default_age" ] && echo yes || echo no)"
is "and a pass happens several times inside it"  "yes" \
   "$([ "$(( period * 60 * 3 ))" -le "$default_age" ] && echo yes || echo no)"

tl_summary
