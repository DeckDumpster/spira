#!/usr/bin/env bash
#
# test-concierge-raw-pr-hook.sh — the Concierge PreToolUse fence refuses a raw PR create or
# merge against a registered repository, admits it with the override, admits an unregistered
# repository, and is wired into the session's settings.
#
# tier: T2
# covers: spira/hooks/concierge-raw-pr-fence.sh concierge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

HOOK="$HERE/hooks/concierge-raw-pr-fence.sh"
mkdir -p "$TMP/reg" "$TMP/run"
git -C "$TMP/reg" init -q && git -C "$TMP/reg" remote add origin git@github.com:acme-org/widgetry.git
cat > "$TMP/cfg" <<STUB
#!/usr/bin/env bash
[ -z "\${STUB_FAIL:-}" ] || { echo "no registry" >&2; exit 1; }
case "\$2" in names) echo widgetry ;; field) echo "$TMP/reg" ;; esac
STUB
chmod +x "$TMP/cfg"

fence() {   # fence <command> [cwd] [env...] -> stdout+stderr; $? is the hook's exit
    local c="$1" cwd="${2:-/}"; shift 2 2>/dev/null || shift $#
    local p; p="$(python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","cwd":sys.argv[2],"tool_input":{"command":sys.argv[1]}}))' "$c" "$cwd")"
    printf '%s' "$p" | env -i PATH="$PATH" HOME="$HOME" SPIRA_CONFIG_BIN="$TMP/cfg" SPIRA_CONCIERGE=1 "$@" bash "$HOOK" 2>&1
}

PR='gh pr create'
out="$(fence "$PR -R acme-org/widgetry --title t --body b")"; rc=$?
is "refuses a create against a registered repo by -R" 2 "$rc"
want "the refusal names the batcher's tool" "open-batch" "$out"
want "the refusal names the override" "CONCIERGE_RAW_PR_CONSIDERED" "$out"
fence "gh pr merge 20 --repo acme-org/widgetry --squash" >/dev/null; is "refuses a merge by --repo" 2 $?
fence "GH_REPO=acme-org/widgetry $PR --fill" >/dev/null; is "refuses a create targeted by GH_REPO" 2 $?
fence "$PR --fill" "$TMP/reg" >/dev/null; is "refuses a create with no -R from inside the checkout" 2 $?
fence "cd $TMP/reg && $PR --fill" >/dev/null; is "refuses a create after a cd into the checkout" 2 $?
fence "git status && echo x | $PR -R acme-org/widgetry" >/dev/null; is "refuses it inside a compound command" 2 $?

out="$(fence "CONCIERGE_RAW_PR_CONSIDERED='batch is wedged' $PR -R acme-org/widgetry --fill")"; rc=$?
is "admits with the override prefixed on the command" 0 "$rc"
is "the override admits silently" "" "$out"
fence "$PR -R acme-org/widgetry --fill" / CONCIERGE_RAW_PR_CONSIDERED=why >/dev/null; is "admits with the override in the environment" 0 $?

fence "$PR -R someone-else/elsewhere --fill" >/dev/null; is "admits an unregistered repo" 0 $?
fence "$PR --fill" "$TMP" >/dev/null; is "admits a create from outside any registered checkout" 0 $?
fence "gh pr view 20 -R acme-org/widgetry" >/dev/null; is "admits reading a PR of a registered repo" 0 $?
fence "echo 'gh pr create -R acme-org/widgetry' > /dev/null" >/dev/null; is "admits a quoted mention" 0 $?
fence "$PR -R acme-org/widgetry" / SPIRA_CONCIERGE=0 >/dev/null; is "inert outside the Concierge" 0 $?

out="$(fence "$PR -R acme-org/widgetry" / STUB_FAIL=1)"; rc=$?
is "an unreadable registry refuses" 2 "$rc"
want "and says so" "registry could not be read" "$out"

hooks_json="$(SPIRA_HOME="$HERE" bash -c 'eval "$(sed -n "/^session_settings()/,/^}/p" "$1")"; session_settings' _ "$HERE/../concierge.sh")"
want "the session settings register the fence on Bash" "concierge-raw-pr-fence.sh" "$hooks_json"
want "and keep the Stop hook" "concierge-waiter-stop.sh" "$hooks_json"

tl_summary
