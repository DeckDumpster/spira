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
#   8. When the Concierge is not running, the same alert lands in operator mail as a note
#      instead of being typed into a session nobody is reading.
#   9. The operator path accepts exactly permissions, policy and destructive: each reaches
#      operator mail as a question with its default. Any other class is refused by
#      construction and routed back to the Concierge as an alert — operator mail gets
#      nothing for it.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): case 3 first proves case 2's
# alert landed, then proves a second call for the same streak adds no second message.
#
# covers: reconciler-alert/src/**.rs reconciler-engine/src/alert.rs spira/mail/kinds/alert.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

. "$HERE/testlib.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    skip "test-reconciler-alert: cargo not found on PATH or at ~/.cargo/bin"
fi

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# --- 1. unit tests: the pure alert module, then the CLI's own arg parsing -------------------
if out="$(CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/engine-target" "$CARGO_BIN" test --manifest-path "$ROOT/reconciler-engine/Cargo.toml" alert:: 2>&1)"; then
    ok "reconciler-engine alert:: unit tests pass"
else
    bad "reconciler-engine alert:: unit tests" "$(printf '%s\n' "$out" | command grep -E 'FAILED|panicked|error' | head -5)"
fi

# reconciler-alert depends on ../reconciler-engine as a path dependency — copied as a
# sibling of the isolated build tree so the relative path still resolves (same shape as
# test-czar-pass.sh's isolated build for czar-pass).
cp -r "$ROOT/reconciler-alert" "$T/reconciler-alert-src"
cp -r "$ROOT/reconciler-engine" "$T/reconciler-engine"
if out="$(CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/alert-target" "$CARGO_BIN" test --manifest-path "$T/reconciler-alert-src/Cargo.toml" 2>&1)"; then
    ok "reconciler-alert unit tests pass"
else
    bad "reconciler-alert unit tests" "$(printf '%s\n' "$out" | command grep -E 'FAILED|panicked|error' | head -5)"
fi

CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/alert-target" "$CARGO_BIN" build --release \
    --manifest-path "$T/reconciler-alert-src/Cargo.toml" >/dev/null 2>&1
BIN="$T/alert-target/release/reconciler-alert"
[ -x "$BIN" ] || bail "reconciler-alert binary was not built at $BIN"

# --- fixtures --------------------------------------------------------------------------------
# Real mail.sh and its real conf.sh, so this suite exercises the actual lint and the actual
# alert.md kind file this bead adds — not a hand-written model of what mail.sh accepts.
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_DB="$T/db"                 # never the operator's real store (law-run-the-suite-in-a-container)
mkdir -p "$SPIRA_RUN"

# Stub concierge.sh: "status" answers from a file the scenario toggles, so the fixture never
# starts a real tmux session. SPIRA_REPO is where czar-pass/main.sh's own convention (and
# doctor.sh) expect concierge.sh to live: $SPIRA_REPO/concierge.sh.
FX_REPO="$T/fx-repo"; mkdir -p "$FX_REPO"
export SPIRA_REPO="$FX_REPO"
cat > "$FX_REPO/concierge.sh" <<'CEOF'
#!/usr/bin/env bash
[ "${1:-}" = status ] || exit 2
[ -f "$CONCIERGE_RUNNING_FLAG" ] && exit 0
exit 1
CEOF
chmod +x "$FX_REPO/concierge.sh"
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
out="$("$BIN" gap --invariant fleet-size --now 1000 --state "$STATE" \
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
out="$("$BIN" gap --invariant fleet-size --now 1060 --state "$STATE" \
    --status gap --desired "8 aeons" --observed "3 aeons" --since 940 --is-gap 2>&1)"
want "reports no alert (deduped)" "no alert" "$out"
is   "still exactly one message — no repeat" "1" "$(mailcount concierge)"

# ==========================================================================================
printf '\n%s\n' "4. once the streak closes, a new streak (new since) alerts again"
# ==========================================================================================
"$BIN" gap --invariant fleet-size --now 1100 --state "$STATE" --status satisfied >/dev/null 2>&1
out="$("$BIN" gap --invariant fleet-size --now 1200 --state "$STATE" \
    --status gap --desired "8 aeons" --observed "2 aeons" --since 1150 --is-gap 2>&1)"
want "reports sent to concierge again" "sent to concierge" "$out"
is   "a second message reaches concierge" "2" "$(mailcount concierge)"

# ==========================================================================================
printf '\n%s\n' "5. a gap inside its own grace period raises nothing"
# ==========================================================================================
out="$("$BIN" gap --invariant cockpit-dashboards --now 3000 --state "$STATE" \
    --status gap --desired "dashboards configured" --observed "none configured" --since 2990 2>&1)"
want "reports no alert" "no alert" "$out"
is   "no new message for a still-graced gap" "2" "$(mailcount concierge)"

# ==========================================================================================
printf '\n%s\n' "6. an unobservable input, past its own grace, alerts like a gap"
# ==========================================================================================
out="$("$BIN" gap --invariant queue-watch --now 4000 --state "$STATE" \
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
"$BIN" gap --invariant loop-stalled --now 5000 --state "$STATE" \
    --status gap --desired "a pass within 3000s" --observed "last pass 5100s ago" \
    --since 4900 --is-gap --remedy-failed --last-remedy "reset-failed + start spira-landing" >/dev/null 2>&1
newest="$(ls -t "$SPIRA_RUN/mail/concierge/new"/* | head -1)"
msg="$(cat "$newest")"
want "the failed remedy is named" "reset-failed + start spira-landing — did not close the gap" "$msg"

# ==========================================================================================
printf '\n%s\n' "8. concierge not running: the same alert lands in operator mail as a note"
# ==========================================================================================
rm -f "$CONCIERGE_RUNNING_FLAG"
before_concierge="$(mailcount concierge)"
out="$("$BIN" gap --invariant land-rate --now 6000 --state "$STATE" \
    --status gap --desired "land rate > 0 over 30m" --observed "0 landed in 30m, work waiting" \
    --since 4200 --is-gap 2>&1)"
want "reports concierge not running" "concierge not running" "$out"
is   "concierge mailbox unchanged" "$before_concierge" "$(mailcount concierge)"
is   "exactly one note reaches the operator" "1" "$(mailcount operator)"
msg="$(cat "$(mailfile operator)")"
want "forwarded as a note, not an alert"      "X-Spira-Kind: note" "$msg"
want "says why it was forwarded"              "Concierge is not running" "$msg"
want "the evidence is still inline"           "invariant: land-rate" "$msg"

# ==========================================================================================
printf '\n%s\n' "9. the operator path: permissions, policy, destructive; nothing else"
# ==========================================================================================
touch "$CONCIERGE_RUNNING_FLAG"
for class in permissions policy destructive; do
    before="$(mailcount operator)"
    out="$(printf 'the pve console needs a one-time grant\n' | "$BIN" escalate --class "$class" \
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
out="$(printf 'stop filing the same class of ticket every night\n' | "$BIN" escalate --class urgent \
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
