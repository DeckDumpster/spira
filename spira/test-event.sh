#!/usr/bin/env bash
#
# test-event.sh — the outcome stream: that it records, that it does not flood, and that
# every kind the harness emits is one the emitter will accept.
#
#   ./test-event.sh
#
# Spira's outcomes used to exist only as text. The health pane scraped three log files for
# landings, reopenings, poisonings, reclaims and claims, sorted them and kept four — so
# everything below the fourth line was not aged out, it was never stored, and "how many times
# did a bead reopen" was unanswerable without grepping a log that rotates.
#
# The half of that fix with no second reader is the RATE LIMIT. An emitter that floods is not
# a milder failure than one that is silent: a stream nobody can read is the log line it
# replaced, and a hot retry loop can produce dozens of reclaims per hour. So the negative
# cases here — a steady state emits nothing, a storm is one row — carry more weight than
# the positive ones, and both must hold at once: a suppressor with no counter would pass the
# storm case by recording nothing at all.
#
# NO DATABASE. The emitter's contract is what it hands to `mail.sh send`, and what
# `mail.sh` then delivers is test-mail-delivery.sh's question. Splitting
# them keeps this suite fast enough to be the one that always runs.
# defect: sp-gvm
# covers: spira/*.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
SH="$TMP/spira"; RUN="$TMP/run"; mkdir -p "$SH" "$RUN"
# conf.sh travels with lib.sh, which refuses to run without it.
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"

# THE EMITTER IS A RECORDER, NOT A SWALLOWER. A stub that exits 0 without a trace would pass
# every positive case here whether or not it was ever called.
cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
printf '%s\n' "$*" >> "$EMITTED"
cat >> "$EMITTED"
exit "${MAIL_RC:-0}"
MAIL
chmod +x "$SH/mail.sh"

# In a subshell, never sourced here: lib.sh overwrites PATH outright, as it must to run under
# systemd, and a suite that inherited that would be testing the harness's PATH as well.
cat > "$TMP/emit" <<'E'
#!/usr/bin/env bash
set -uo pipefail
. "$SPIRA_HOME/lib.sh"
spira_event "$@"
E
chmod +x "$TMP/emit"

# SPIRA_CONF=/nonexistent: this suite asserts what the code does, and the operator's own
# configuration is not part of that (law-gates-run-in-a-clean-environment).
emit() {   # emit <kind> <target|-> <title> [detail]
    SPIRA_CONF=/nonexistent SPIRA_HOME="${HOME_OVERRIDE:-$SH}" SPIRA_REPO="$TMP" SPIRA_RUN="$RUN" \
    SPIRA_DB=/nonexistent-spira-db \
    SPIRA_EVENT_COOLDOWN="${COOLDOWN:-3600}" EMITTED="$TMP/emitted" MAIL_RC="${MAIL_RC:-0}" \
        bash "$TMP/emit" "$@" 2>&1
}
emitted()  { cat "$TMP/emitted" 2>/dev/null; }
# `grep -c` prints 0 AND exits 1 on no match, so an `|| echo 0` fallback prints TWO zeros
# and every count comparison fails against a number that looks right in the message.
# Count lines that start with "send " — one per mail.sh invocation.
rows()     { grep -c '^send ' "$TMP/emitted" 2>/dev/null || true; }
fresh()    { rm -rf "$RUN/events"; : > "$TMP/emitted"; }

echo "test-event.sh"

# --------------------------------------------------------------------------------------
echo
echo "the positive control — an outcome reaches the emitter whole"
# --------------------------------------------------------------------------------------
fresh
out="$(emit bead.landed sp-x "landed spira/sp-x on brain's main" "merged as abc1234")"
is   "a first outcome is recorded"        "1"            "$(rows)"
want "with the kind the panel badges"     "kind: bead.landed" "$(emitted)"
want "the bead it happened to, in its own column" "target: sp-x" "$(emitted)"
want "the title as written"               "landed spira/sp-x on brain's main" "$(emitted)"
want "and the detail beside it"           "merged as abc1234" "$(emitted)"
want "through --kind note, not insight"   "--kind note"  "$(emitted)"
nowant "and never as an insight"          "insight"      "$(emitted)"

# An outcome about the plan rather than a bead carries no target field at all, rather than a
# literal "-" that would filter as if it were a bead id.
fresh
emit spira.note - "the plan moved" >/dev/null
nowant "a plan-level outcome carries no target" "target:" "$(emitted)"

# --------------------------------------------------------------------------------------
echo
echo "the rate limit — a steady state is not news"
# --------------------------------------------------------------------------------------
fresh
emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim 1" >/dev/null
is "the first reclaim is recorded" "1" "$(rows)"
emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim 2" >/dev/null
is "a repeat inside the window is not" "1" "$(rows)"

# THE STORM: 26 attempts in three hours was the real case that shaped this.
fresh
for n in $(seq 1 26); do emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim $n" >/dev/null; done
is "a 26-reclaim storm is one row, not 26" "1" "$(rows)"

# ...AND THE CHECK COULD HAVE COUNTED HIGHER. A suppressor that recorded nothing at all would
# pass the case above; the same loop over distinct beads must produce a row each, because the
# window is per (kind, bead) and a real fleet of reclaims is real news.
fresh
for n in $(seq 1 6); do emit branch.reclaimed "sp-storm$n" "reclaimed sp-storm$n" >/dev/null; done
is "six different beads reclaiming is six rows" "6" "$(rows)"

# The pair is (kind, target), so a bead that lands and is then reopened says both things.
fresh
emit bead.landed   sp-y "landed sp-y"   >/dev/null
emit bead.reopened sp-y "reopened sp-y" >/dev/null
is "a second KIND on the same bead is not a repeat" "2" "$(rows)"

# --------------------------------------------------------------------------------------
echo
echo "suppressed is not dropped — the count rides out on the next one"
# --------------------------------------------------------------------------------------
# A window that expires with a run of suppressions behind it must SAY so. A panel that
# renders a storm as one quiet row is a check reporting all-clear on the thing it exists to
# show (law-alerts-must-be-actionable).
fresh
COOLDOWN=1 emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim 1" >/dev/null
for n in 2 3 4; do COOLDOWN=999 emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim $n" >/dev/null; done
is "three repeats are held" "1" "$(rows)"
sleep 2
COOLDOWN=1 emit branch.reclaimed sp-b0c "reclaimed sp-b0c — reclaim 5" >/dev/null
is   "the expired window emits again"     "2"            "$(rows)"
want "carrying what it held back"         "+3 more since" "$(emitted)"

# And the window belongs to the FIRST emission, not the last suppression: refreshing it on
# every repeat is how a loop faster than the window goes permanently silent.
fresh
COOLDOWN=2 emit branch.reclaimed sp-hot "reclaimed sp-hot — 1" >/dev/null
for n in 2 3 4 5; do sleep 1; COOLDOWN=2 emit branch.reclaimed sp-hot "reclaimed sp-hot — $n" >/dev/null; done
[ "$(rows)" -ge 2 ] && ok "a loop faster than the window still surfaces" \
                    || bad "a loop faster than the window still surfaces" "got $(rows) rows"

# --------------------------------------------------------------------------------------
echo
echo "a broken delivery path is named, never silent"
# --------------------------------------------------------------------------------------
# An emitter that returns quietly when there is nowhere to send is how a converted site
# records nothing for a month (law-absence-needs-a-positive-control).
fresh
# A home that has lib.sh + conf.sh but no mail.sh — spira_event can load but has nowhere to send.
mkdir -p "$TMP/no-mail-home"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$TMP/no-mail-home/"
out="$(HOME_OVERRIDE="$TMP/no-mail-home" emit bead.landed sp-x "landed sp-x")"; rc=$?
is   "a missing emitter fails loudly"  "1"                "$rc"
want "and names what is missing"       "mail.sh not found" "$out"
want "and says which outcome was lost" "bead.landed"      "$out"

fresh
out="$(MAIL_RC=1 emit bead.landed sp-x "landed sp-x")"; rc=$?
is   "an emitter that refuses is reported"   "1"           "$rc"
want "naming the outcome it could not record" "bead.landed" "$out"

fresh
out="$(emit "" sp-x "no kind")"; rc=$?
is "an outcome with no kind is refused" "1" "$rc"
is "and nothing is written"             "0" "$(rows)"
out="$(emit bead.landed sp-x "")"; rc=$?
is "an outcome with no title is refused" "1" "$rc"
is "and nothing is written for it either" "0" "$(rows)"

# --------------------------------------------------------------------------------------
echo
echo "the taxonomy — every kind the harness emits is one the emitter accepts"
# --------------------------------------------------------------------------------------
# A typo'd kind is refused at send time. Catching it at the gate is the difference between
# a taxonomy and free text, so the vocabulary is declared here and every call site checked
# against it — adding a kind is then a deliberate act, which is what a taxonomy is.
KINDS="aeon.claimed bead.landed bead.poisoned bead.reopened branch.reclaimed ci.failed"
KINDS="$KINDS pilgrimage.complete note"

sites="$(grep -rhE '[^#]spira_event [a-z0-9.]+' "$HERE"/*.sh 2>/dev/null \
         | grep -v '^\s*#' \
         | grep -oE 'spira_event [a-z0-9.]+' \
         | sed -E 's/.* //' | sort -u)"
[ -n "$sites" ] && ok "the call sites can be found at all" \
                || bad "the call sites can be found at all" "the grep matched nothing — it is measuring itself, not the harness"

for k in $sites; do
    case " $KINDS " in
        *" $k "*) ;;
        *) bad "kind '$k' is in the declared taxonomy" \
               "an emitted kind nobody declared — add it to KINDS here, or fix the call site" ;;
    esac
done
case "$sites" in *bead.landed*) ok "the emitted kinds are all declared" ;; esac

# Each kind must be lowercase dotted segments, no longer than 32 characters (the db column).
for k in $KINDS; do
    case "$k" in
        *[!a-z0-9.]*|.*|*.|*..*) bad "kind '$k' is valid" "not lowercase dotted segments" ;;
        *) [ "${#k}" -le 32 ] || bad "kind '$k' fits 32-char limit" "${#k} chars" ;;
    esac
done
ok "every declared kind is lowercase dotted and fits the column"

# --------------------------------------------------------------------------------------
echo
echo "the call sites — every outcome the harness has is wired to one"
# --------------------------------------------------------------------------------------
# aeon.claimed cannot be driven by a suite: aeon.sh launches a Claude session, so every
# suite stubs it. The other five are exercised through their own scripts in test-landing.sh,
# test-poison.sh and test-ci-park.sh. What is checkable here is that the claim is still
# wired, and wired AFTER the ledger — the ledger is what aeon_count and the born/awake
# positive control read, and it must not come to depend on a database being reachable.
claim="$(grep -n -A14 '^ledger "awake \$FAYTH \$BEAD_ID"' "$HERE/aeon.sh" 2>/dev/null)"
want "a claim is emitted"          "spira_event aeon.claimed" "$claim"
want "and only after the ledger"   "ledger \"awake"           "$claim"

for site in \
    "landing.sh:bead.landed"    "landing.sh:bead.reopened" \
    "sentinel.sh:bead.poisoned" "gate-check.sh:ci.failed" \
    "strand.sh:branch.reclaimed"; do
    f="${site%%:*}"; k="${site##*:}"
    grep -q "spira_event $k " "$HERE/$f" \
        && ok "$f emits $k" || bad "$f emits $k" "no call site"
done

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
