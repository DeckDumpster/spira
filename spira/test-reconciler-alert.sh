#!/usr/bin/env bash
#
# test-reconciler-alert.sh — the alert path (sp-fufyb): an evidence-carrying wake to the
# Concierge, deduplicated per gap, and an operator escalation path refused for any class
# but permissions, policy or destructive.
#
# WHAT THIS SUITE CHECKS. reconciler-alert did not exist before this bead, so every case
# below is seen to fail first by construction (law-a-regression-test-must-be-seen-to-fail):
# there was no binary to produce the mail this suite inspects.
#
#   1. reconciler-engine's alert module and reconciler-alert's own #[test]s (cargo).
#   2. A fresh gap past grace wakes the Concierge, evidence-carrying: invariant, desired vs
#      observed, since-when, last remedy tried ("none attempted" when none was).
#   3. The same unresolved streak does NOT alert a second time (dedup per gap, not per pass).
#   4. Once the streak closes and a NEW streak (new since) opens, it alerts again.
#   5. A gap still inside its grace period (is_gap=false) raises nothing.
#   6. An unreadable (unobservable) input drives the same alert path as a gap, once past its
#      own grace — never silently treated as satisfied, never silently skipped either.
#   7. A remedy that did not close its gap is named as having failed in the evidence.
#   8. When the Concierge is not running, the alert is still queued in its mailbox, never the operator's
#      instead of being typed into a session nobody is reading.
#   9. The operator path accepts exactly permissions, policy and destructive: each reaches
#      operator mail as a question with its default. Any other class is refused by
#      construction and routed back to the Concierge as an alert — operator mail gets
#      nothing for it.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): case 3 first proves case 2's
# alert landed, then proves a second call for the same streak adds no second message.
#
# tier: T3
# covers: reconciler-alert/src/**.rs reconciler-engine/src/alert.rs spira/mail/kinds/alert.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

. "$HERE/testlib.sh"


T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# (this crate's own #[test]s run once per round as the workspace unit-test step, not here.)

# The tree under test's own reconciler-alert, on PATH (sp-gypjk).
command -v reconciler-alert >/dev/null 2>&1 || bail "reconciler-alert is not on PATH"

# --- fixtures --------------------------------------------------------------------------------
# Real mail and its real conf.sh, so this suite exercises the actual lint and the actual
# alert.md kind file this bead adds — not a hand-written model of what mail accepts.
export SPIRA_HOME="$HERE"
SPIRA_RUN="$T/run"; tl_config SPIRA_RUN="$SPIRA_RUN"
SPIRA_DB="$T/db"; tl_config SPIRA_DB="$SPIRA_DB"        # never the operator's real store (law-run-the-suite-in-a-container)
# sp-70ocl dropped mail's literal "needs-operator" Rust fallback: mail now refuses to file
# any question/decision ask (mail/src/cmds.rs::send) when SPIRA_ASK_LABEL does not resolve,
# rather than guessing. A real install's conf.sh always exports it; this fixture has no
# conf.sh to source (same reason it already pins SPIRA_RUN above instead of letting
# spira_config derive one — resolve_run_dir's own sp-ivfu3-2 note), so it must pin
# SPIRA_ASK_LABEL explicitly too, or every `mail send ... --kind question` call in section 9
# below silently refuses instead of reaching the operator.
tl_config SPIRA_ASK_LABEL="needs-operator"
# SPIRA_MAIL/SPIRA_MAIL_KINDS/SPIRA_MAIL_INDEX/SPIRA_MAIL_MUTE: all four now declared by the
# fixture (a fictional /fixture/userhome/... tree, mail_mute=true) instead of deriving from
# SPIRA_RUN/SPIRA_HOME — left alone, `mail send` tries to create its mailbox under that
# unwritable fixture path (permission denied) or refuses on an unknown kind, and even once
# those are fixed, a muted send lands straight in cur/ as already-seen, so every
# mailcount()/mailfile() in this suite (which only looks at new/) reads empty (one source of
# config, per Ryan 2026-10-05).
tl_config SPIRA_MAIL="$SPIRA_RUN/mail" SPIRA_MAIL_KINDS="$HERE/mail/kinds" \
    SPIRA_MAIL_INDEX="$SPIRA_RUN/mail/index" SPIRA_MAIL_MUTE=0
mkdir -p "$SPIRA_RUN"

# Stub tmux: "has-session -t =concierge" answers from a file the scenario toggles, so the
# fixture never starts a real tmux session. reconciler-alert runs tmux by bare name, so the
# stub is found on PATH ahead of the real one.
STUB_BIN="$T/stub-bin"; mkdir -p "$STUB_BIN"
cat > "$STUB_BIN/tmux" <<'CEOF'
#!/usr/bin/env bash
[ "${1:-}" = has-session ] && [ "${3:-}" = "=concierge" ] || exit 2
[ -f "$CONCIERGE_RUNNING_FLAG" ] && exit 0
exit 1
CEOF
chmod +x "$STUB_BIN/tmux"
export PATH="$STUB_BIN:$PATH"
export CONCIERGE_RUNNING_FLAG="$T/concierge-running"
touch "$CONCIERGE_RUNNING_FLAG"   # concierge running by default; case 8 removes it

STATE="$T/alerted.json"
mailfile() { # <mailbox> — path glob to the (single) message file, empty if none
    printf '%s\n' "$SPIRA_RUN/mail/$1/new"/* 2>/dev/null
}
mailcount() { find "$SPIRA_RUN/mail/$1/new" -type f 2>/dev/null | wc -l | tr -d ' '; }

# ==========================================================================================
printf '\n%s\n' "2. a fresh gap past grace wakes the concierge, evidence-carrying"
# ==========================================================================================
out="$(reconciler-alert gap --invariant fleet-size --now 1000 --state "$STATE" \
    --status gap --desired "8 aeons" --observed "3 aeons" --since 940 --is-gap 2>&1)"; rc=$?
[ "$rc" -eq 0 ] && ok "gap call exits 0" || bad "gap call exits 0" "rc=$rc: $out"
want "reports sent to concierge" "sent to concierge" "$out"
is   "exactly one message reaches concierge" "1" "$(mailcount concierge)"
msg="$(cat "$(mailfile concierge)" 2>/dev/null)"
want "message carries X-Spira-Kind: alert" "X-Spira-Kind: alert" "$msg"
want "evidence names the invariant"        "invariant: fleet-size" "$msg"
want "evidence names desired"              "desired:   8 aeons" "$msg"
want "evidence names observed"             "observed:  3 aeons" "$msg"
want "evidence names since-when"           "since:     60s ago" "$msg"
want "evidence says no remedy was tried"   "last remedy tried: none attempted" "$msg"
is "operator mailbox is empty so far" "0" "$(mailcount operator)"

# ==========================================================================================
printf '\n%s\n' "3. the same unresolved streak does not alert twice (dedup per gap)"
# ==========================================================================================
out="$(reconciler-alert gap --invariant fleet-size --now 1060 --state "$STATE" \
    --status gap --desired "8 aeons" --observed "3 aeons" --since 940 --is-gap 2>&1)"
want "reports no alert (deduped)" "no alert" "$out"
is   "still exactly one message — no repeat" "1" "$(mailcount concierge)"

# ==========================================================================================
printf '\n%s\n' "4. once the streak closes, a new streak (new since) alerts again"
# ==========================================================================================
reconciler-alert gap --invariant fleet-size --now 1100 --state "$STATE" --status satisfied >/dev/null 2>&1
out="$(reconciler-alert gap --invariant fleet-size --now 1200 --state "$STATE" \
    --status gap --desired "8 aeons" --observed "2 aeons" --since 1150 --is-gap 2>&1)"
want "reports sent to concierge again" "sent to concierge" "$out"
is   "a second message reaches concierge" "2" "$(mailcount concierge)"

# ==========================================================================================
printf '\n%s\n' "5. a gap inside its own grace period raises nothing"
# ==========================================================================================
out="$(reconciler-alert gap --invariant cockpit-dashboards --now 3000 --state "$STATE" \
    --status gap --desired "dashboards configured" --observed "none configured" --since 2990 2>&1)"
want "reports no alert" "no alert" "$out"
is   "no new message for a still-graced gap" "2" "$(mailcount concierge)"

# ==========================================================================================
printf '\n%s\n' "6. an unobservable input, past its own grace, alerts like a gap"
# ==========================================================================================
out="$(reconciler-alert gap --invariant queue-watch --now 4000 --state "$STATE" \
    --status unobservable --reason "queue-watch: no reading in 90s" --since 3900 --is-gap 2>&1)"
want "reports sent to concierge" "sent to concierge" "$out"
# Grab the newest message specifically (three now exist).
newest="$(ls -t "$SPIRA_RUN/mail/concierge/new"/* | head -1)"
msg="$(cat "$newest")"
want "unobservable is never rendered as satisfied" "desired:   (unobservable)" "$msg"
want "the reason is carried as the observed field" "observed:  queue-watch: no reading in 90s" "$msg"

# ==========================================================================================
printf '\n%s\n' "7. a remedy that did not close its gap is named as failed"
# ==========================================================================================
reconciler-alert gap --invariant loop-stalled --now 5000 --state "$STATE" \
    --status gap --desired "a pass within 3000s" --observed "last pass 5100s ago" \
    --since 4900 --is-gap --remedy-failed --last-remedy "reset-failed + start spira-landing" >/dev/null 2>&1
newest="$(ls -t "$SPIRA_RUN/mail/concierge/new"/* | head -1)"
msg="$(cat "$newest")"
want "the failed remedy is named" "reset-failed + start spira-landing — did not close the gap" "$msg"

# ==========================================================================================
printf '\n%s\n' "8. concierge not running: the alert is queued for the concierge, never the operator"
# ==========================================================================================
rm -f "$CONCIERGE_RUNNING_FLAG"
before_concierge="$(mailcount concierge)"
before_operator="$(mailcount operator)"
out="$(reconciler-alert gap --invariant land-rate --now 6000 --state "$STATE" \
    --status gap --desired "land rate > 0 over 30m" --observed "0 landed in 30m, work waiting" \
    --since 4200 --is-gap 2>&1)"
want "reports concierge not running" "concierge not running" "$out"
is   "the alert is queued in the concierge mailbox" "$((before_concierge + 1))" "$(mailcount concierge)"
is   "the operator mailbox is untouched" "$before_operator" "$(mailcount operator)"
msg="$(cat "$(ls -t "$SPIRA_RUN/mail/concierge/new"/* | head -1)")"
want "the evidence is still inline" "invariant: land-rate" "$msg"

# ==========================================================================================
printf '\n%s\n' "9. the operator path: permissions, policy, destructive; nothing else"
# ==========================================================================================
touch "$CONCIERGE_RUNNING_FLAG"
for class in permissions policy destructive; do
    before="$(mailcount operator)"
    out="$(printf 'the pve console needs a one-time grant\n' | reconciler-alert escalate --class "$class" \
        --subject "escalation-$class needs a decision" --default "wait for the operator" 2>&1)"
    want "escalate $class: reports sent to operator" "sent to operator" "$out"
    after="$(mailcount operator)"
    [ "$after" -eq $((before + 1)) ] && ok "escalate $class: one new operator message" \
        || bad "escalate $class: one new operator message" "before=$before after=$after"
    newest="$(ls -t "$SPIRA_RUN/mail/operator/new"/* | head -1)"
    msg="$(cat "$newest")"
    want "escalate $class: kind is question"    "X-Spira-Kind: question" "$msg"
    want "escalate $class: carries its default" "wait for the operator" "$msg"
done

before_op="$(mailcount operator)"
before_con="$(mailcount concierge)"
out="$(printf 'stop filing the same class of ticket every night\n' | reconciler-alert escalate --class urgent \
    --subject "an unpermitted escalation class" 2>&1)"
want "an unpermitted class is refused" "refused" "$out"
is "operator mail gets nothing for a refused class" "$before_op" "$(mailcount operator)"
[ "$(mailcount concierge)" -eq $((before_con + 1)) ] && ok "the refusal itself is routed to the concierge" \
    || bad "the refusal itself is routed to the concierge" "before=$before_con after=$(mailcount concierge)"
newest="$(ls -t "$SPIRA_RUN/mail/concierge/new"/* | head -1)"
msg="$(cat "$newest")"
want "the refusal names the disallowed class"   "'urgent' is not permissions, policy or destructive" "$msg"
want "the refusal is a Concierge alert"         "X-Spira-Kind: alert" "$msg"

tl_summary
