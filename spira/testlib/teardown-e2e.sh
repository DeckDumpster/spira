# Shared by the test-aeon-teardown-e2e*.sh parts: the claude shim and the ledger/requeue
# readers. Needs HERE and a built fa_setup fixture.
MAINREPO="$FA_REPO"; export MAINREPO
shim() {   # shim <commit:0|1> <close:0|1> [move-the-base:0|1] [claude-rc:0|1]
    printf '%s' "$1" > "$FA_TMP/docommit"; printf '%s' "$2" > "$FA_TMP/doclose"
    printf '%s' "${3:-0}" > "$FA_TMP/domove"; printf '%s' "${4:-0}" > "$FA_TMP/doclauderc"
    cat > "$FA_BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
if [ "$(cat "$TMP/docommit")" = 1 ]; then
    printf 'the aeon wrote this %s\n' "$(date +%s%N)" > f
    git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
fi
# THE BASE MOVES WHILE THE SESSION IS RUNNING, which is the live shape and the only one that
# produces the defect: a base that had already moved before the aeon started would simply be
# branched from, and there would be nothing to rebase.
if [ "$(cat "$TMP/domove")" = 1 ]; then
    git -C "$MAINREPO" checkout -q main
    printf 'someone else landed this %s\n' "$(date +%s%N)" > "$MAINREPO/f"
    git -C "$MAINREPO" -c user.email=b@b -c user.name=other commit -qam "another bead — a conflicting change"
    git -C "$MAINREPO" push -q origin main 2>/dev/null
fi
[ "$(cat "$TMP/doclose")" = 1 ] && bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit "$(cat "$TMP/doclauderc")"
SHIM
    chmod +x "$FA_BIN/claude"
}
field() { fa_field "$1" "$2"; }
lib() { bash -c ". \"$HERE/lib.sh\"; $1" 2>/dev/null; }
count_of()   { local c; c="$(lib "attempts_of $1")"; printf '%s' "${c:-0}"; }
requeue_of() { local c; c="$(lib "requeues_of $1")"; printf '%s' "${c:-0}"; }
