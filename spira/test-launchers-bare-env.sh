#!/usr/bin/env bash
# test-launchers-bare-env.sh — every entry point an operator or a client starts OUTSIDE a Spira
# unit, run from a bare login environment: HOME, PATH and the two exports an operator's profile
# carries (SPIRA_TOML, SPIRA_RELEASE), nothing else. A unit's environment is rendered for it; these have none,
# so a launcher that quietly depends on a variable only a unit sets passes every other suite.
#
# Each launcher (the ops pane aside, which renders "?" for what it cannot read) is run twice: with the profile export, where it must run, and with nothing, where
# it must refuse — the second proves the bare environment is bare and the first could have failed.
# The client-registered commands (status line, session hook) are run by
# release/tests/session_hook_minimal_env.rs, which names their use cases.
#
# tier: T3
# covers: aerc/* cockpit.sh concierge.sh cockpit/panel-run.sh cockpit/ops/src/* mail/src/* spira/conf.sh UC-operator-launchers-01 UC-operator-launchers-02 UC-operator-launchers-03 UC-operator-launchers-04
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-launchers-bare-env.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/bin"

tl_config SPIRA_MAIL="$TMP/mail" SPIRA_RUN="$TMP/run" SPIRA_DB=""
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/claude"; chmod +x "$TMP/bin/claude"

# profile <cmd...> — the operator's login environment; bare <cmd...> — a process given nothing.
profile() { env -i HOME="$TMP/home" PATH="$TMP/bin:$PATH" SPIRA_TOML="$SPIRA_TOML" SPIRA_RELEASE="${SPIRA_RELEASE:-$ROOT}" "$@"; }
bare()    { env -i HOME="$TMP/home" PATH="$TMP/bin:$PATH" "$@"; }

refuses_bare() {  # refuses_bare <name> <cmd...>
    local name="$1"; shift
    local out rc
    out="$(bare "$@" </dev/null 2>&1)"; rc=$?
    [ "$rc" != 0 ] && ok "$name refuses with no SPIRA_TOML" || bad "$name refuses with no SPIRA_TOML" "exit 0: $out"
}

# --- UC-operator-launchers-01: the mail client's outgoing command ---------------------------
mkdir -p "$TMP/mail/concierge/"{new,cur,tmp} "$TMP/mail/operator/"{new,cur,tmp}
id="$(date +%s).$$.$RANDOM"
printf 'From: Operator <operator@spira>\nSubject: Please reply\nMessage-ID: <%s@spira>\n\nbody\n' "$id" > "$TMP/mail/concierge/cur/$id"
reply="$(printf 'In-Reply-To: <%s@spira>\nFrom: Concierge <concierge@spira>\nSubject: Re: Please reply\n\nDone.\n' "$id")"
out="$(printf '%s\n' "$reply" | profile mail sendmail 2>&1)"; rc=$?
wantrc "mail sendmail runs from a bare login environment" 0 "$rc"
[ "$rc" = 0 ] || echo "# sendmail said: $out"
nowant "mail sendmail names no missing config" "SPIRA_TOML" "$out"
refuses_bare "mail sendmail" mail sendmail

# --- UC-operator-launchers-02: cockpit.sh --------------------------------------------------
out="$(profile "$ROOT/cockpit.sh" status </dev/null 2>&1)"; rc=$?
nowant "cockpit.sh status names no missing config" "SPIRA_TOML" "$out"
case "$rc" in 0|1) ok "cockpit.sh status reports a state (exit $rc)" ;; *) bad "cockpit.sh status reports a state" "exit $rc: $out" ;; esac
refuses_bare "cockpit.sh" "$ROOT/cockpit.sh" status

# --- UC-operator-launchers-03: concierge.sh ------------------------------------------------
out="$(profile "$ROOT/concierge.sh" status </dev/null 2>&1)"; rc=$?
nowant "concierge.sh status names no missing config" "SPIRA_TOML" "$out"
case "$rc" in 0|1) ok "concierge.sh status reports a state (exit $rc)" ;; *) bad "concierge.sh status reports a state" "exit $rc: $out" ;; esac
refuses_bare "concierge.sh" "$ROOT/concierge.sh" status

# --- UC-operator-launchers-04: the ops pane ------------------------------------------------
out="$(profile SPIRA_HOME="$HERE" health once </dev/null 2>&1)"; rc=$?
nowant "ops pane names no missing config" "SPIRA_TOML" "$out"
wantrc "ops pane renders once from a bare login environment" 0 "$rc"

echo
tl_summary
