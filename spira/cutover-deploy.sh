#!/usr/bin/env bash
#
# cutover-deploy.sh — design §5.3: the migration step of the cutover deploy, run between
# drain and resume. Idempotent per step (schema.sql is IF NOT EXISTS throughout, grants.sql
# is CREATE USER IF NOT EXISTS, classify skips a bead that already has a row, and the config
# step is a plain unset), so a retry after a partial failure picks up where it left off.
#
#   cutover-deploy.sh --repo NAME [--repo NAME]... [--dry-run]
#
# --repo is repeatable and forwarded straight to `spira-lc classify`; this script never reads
# the repository map itself (spira-config is the only reader/writer config-fence admits) —
# the caller already has the repository list and passes it in.
#
# DRAIN IS THE CALLER'S JOB, NOT THIS SCRIPT'S: whatever stopped spira-lc.socket and the
# queue/sentinel/batch timers before calling this also resumes them afterwards — the same
# split spira-install keeps between creating the credential and this script (spending it). This script only refuses to run while spira-lc.socket still looks active,
# because a live socket means a writer could still reach the store while it is reclassified
# and re-granted underneath it.
#
# Steps, in order (each is the one place its own resource is written outside a test):
#   1. apply lifecycle/schema.sql, as the database ADMIN (SPIRA_LC_ADMIN_USER/PASSWORD, else
#      root with the password spira-install provisioned) — spira_lc's own grant does not
#      yet exist to do this with
#   2. apply lifecycle/grants.sql, same admin connection, with @SPIRA_LC_PASSWORD@/
#      @SPIRA_LC_RO_PASSWORD@ substituted from the credentials spira-install
#      generated (the spira_lc_password_file config key, and its -ro sibling)
#   3. run `spira-lc classify` once per --repo given, AS spira_lc (SPIRA_LC_PASSWORD_FILE) —
#      the same restricted user everything else now writes through, so a grants.sql mistake
#      fails this step instead of production's first real write
#   4. remove the retired lifecycle_enforce key through spira-config — the only writer the
#      config document admits (config-fence refuses any other). The lifecycle machine is the
#      only mode (sp-v62vn): there is no switch to flip, and a leftover `true` only warns.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

DRY_RUN=0
REPOS=()
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --repo) REPOS+=("${2:?--repo needs a name}"); shift 2 ;;
        *) printf 'cutover-deploy: unknown argument %s\n' "$1" >&2; exit 2 ;;
    esac
done
[ "${#REPOS[@]}" -gt 0 ] || { printf 'cutover-deploy: at least one --repo NAME is required\n' >&2; exit 2; }

if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet spira-lc.socket 2>/dev/null; then
    printf 'cutover-deploy: spira-lc.socket is still active — drain it first (systemctl stop spira-lc.socket spira-lc.service)\n' >&2
    exit 1
fi

SPIRA_LC_CRED_FILE="${SPIRA_LC_CRED_FILE:-${SPIRA_LC_PASSWORD_FILE:-}}"
[ -n "$SPIRA_LC_CRED_FILE" ] || { printf 'cutover-deploy: no spira_lc credential file configured (spira_lc_password_file) and none at the default path — run spira-install first\n' >&2; exit 1; }
SPIRA_LC_RO_CRED_FILE="${SPIRA_LC_RO_CRED_FILE:-$SPIRA_LC_CRED_FILE-ro}"
if [ -z "${SPIRA_LC_PASSWORD:-}" ] && [ -r "$SPIRA_LC_CRED_FILE" ]; then
    SPIRA_LC_PASSWORD="$(cat "$SPIRA_LC_CRED_FILE")"
fi
if [ -z "${SPIRA_LC_RO_PASSWORD:-}" ] && [ -r "$SPIRA_LC_RO_CRED_FILE" ]; then
    SPIRA_LC_RO_PASSWORD="$(cat "$SPIRA_LC_RO_CRED_FILE")"
fi
[ -n "${SPIRA_LC_PASSWORD:-}" ] || { printf 'cutover-deploy: no spira_lc credential (SPIRA_LC_PASSWORD or %s) — refusing\n' "$SPIRA_LC_CRED_FILE" >&2; exit 1; }
[ -n "${SPIRA_LC_RO_PASSWORD:-}" ] || { printf 'cutover-deploy: no spira_lc_ro credential (SPIRA_LC_RO_PASSWORD or %s) — refusing\n' "$SPIRA_LC_RO_CRED_FILE" >&2; exit 1; }

command -v spira-lc >/dev/null 2>&1 || { printf 'cutover-deploy: spira-lc is not on PATH\n' >&2; exit 1; }
command -v spira-config >/dev/null 2>&1 || { printf 'cutover-deploy: spira-config is not on PATH\n' >&2; exit 1; }

say() { printf 'cutover-deploy: %s\n' "$1"; }

: "${SPIRA_LC_ADMIN_USER:=root}"
if [ -z "${SPIRA_LC_ADMIN_PASSWORD:-}" ]; then
    _admin_file="${SPIRA_LC_ADMIN_PASSWORD_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira-lc-admin.credential}"
    [ -r "$_admin_file" ] && SPIRA_LC_ADMIN_PASSWORD="$(cat "$_admin_file")"
fi
: "${SPIRA_LC_ADMIN_PASSWORD:=}"
admin_lc() { SPIRA_LC_ADMIN_USER="$SPIRA_LC_ADMIN_USER" SPIRA_LC_ADMIN_PASSWORD="$SPIRA_LC_ADMIN_PASSWORD" spira-lc "$@"; }

say "applying schema.sql (as $SPIRA_LC_ADMIN_USER)"
if [ "$DRY_RUN" != 1 ]; then
    admin_lc admin-apply-ddl "$HERE/../lifecycle/schema.sql" || exit 1
fi

say "applying grants.sql (as $SPIRA_LC_ADMIN_USER)"
TMP_GRANTS="$(mktemp)"
trap 'rm -f "$TMP_GRANTS"' EXIT INT TERM
sed -e "s/@SPIRA_LC_PASSWORD@/$SPIRA_LC_PASSWORD/" -e "s/@SPIRA_LC_RO_PASSWORD@/$SPIRA_LC_RO_PASSWORD/" \
    "$HERE/../lifecycle/grants.sql" > "$TMP_GRANTS"
if [ "$DRY_RUN" != 1 ]; then
    admin_lc admin-apply-ddl "$TMP_GRANTS" || exit 1
fi

say "classifying the quiesced store"
for name in "${REPOS[@]}"; do
    if [ "$DRY_RUN" = 1 ]; then
        say "  would classify $name"
        continue
    fi
    SPIRA_LC_USER=spira_lc SPIRA_LC_PASSWORD_FILE="$SPIRA_LC_CRED_FILE" spira-lc classify \
        --home "$SPIRA_CONFIG_HOME" --bd-bin "${SPIRA_BD:-bd}" --bd-db "$SPIRA_DB" \
        --landstate-dir "$SPIRA_RUN/landstate" --queue-dir "$SPIRA_QUEUE_DIR" --repo "$name" || exit 1
done

say "retiring lifecycle_enforce"
if [ "$DRY_RUN" != 1 ]; then
    # spira_config_unset (conf.sh) is the one place a [spira] key's dotted path is derived and
    # spira-config invoked — never a direct write to the config document itself, which the
    # config-fence lint refuses (it is a multi-tenant store; spira-config is its only writer).
    spira_config_unset SPIRA_LIFECYCLE_ENFORCE || exit 1
fi

say "done — resume spira-lc.socket and the timers"
