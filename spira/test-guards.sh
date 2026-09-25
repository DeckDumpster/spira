#!/usr/bin/env bash
# test-guards.sh — the two live PreToolUse guards, table-style: hooks/aeon-fence.sh and
# bd-close-unacked-guard.sh (+ its PostToolUse pair bd-unacked-comment-deliver.sh).
#
# Merges test-aeon-fence.sh, test-aeon-fence-bd-create.sh and test-bd-close-unacked-guard.sh
# (sp-qsr44, D1/D2). UC-06..13's original guards (bd-delivers-label, bd-remember,
# bd-update-inflight, focus-close, null-close-reason, mail-fence, testenv-batch-fence,
# schema-migration-guard) were retired by sp-fjsxb and are not re-created here.
#
# Section-5 seam for UC-04/05: bd-close-unacked-guard.sh and its deliver hook are driven
# through a stub $SPIRA_BD answering `show --format json` / `comments --json` from canned
# fixture files, so timestamp ordering is explicit and no sleep or real Dolt is needed.
# The same stub answers `create ... --dry-run` for UC-03's fallback arm.
#
# defect: sp-kz8ob sp-mvg44
# tier: T1
# covers: spira/hooks/aeon-fence.sh spira/bd-close-unacked-guard.sh spira/bd-unacked-comment-deliver.sh spira/aeon.sh spira/suites.sh UC-safety-fences-01 UC-safety-fences-02 UC-safety-fences-03 UC-safety-fences-04 UC-safety-fences-05 UC-safety-fences-15 UC-safety-fences-16
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-guards.sh"

FENCE="$HERE/hooks/aeon-fence.sh"
BDCLOSE_GUARD="$HERE/bd-close-unacked-guard.sh"
BDCLOSE_DELIVER="$HERE/bd-unacked-comment-deliver.sh"
[ -x "$FENCE" ]          || bail "hooks/aeon-fence.sh missing or not executable"
[ -x "$BDCLOSE_GUARD" ]  || bail "bd-close-unacked-guard.sh missing or not executable"
[ -x "$BDCLOSE_DELIVER" ] || bail "bd-unacked-comment-deliver.sh missing or not executable"

# UC-safety-fences-16 (gap 1): the allow-list itself — that aeon_settings() registers
# exactly these two guards and no others — is already pinned by
# test-aeon-settings-guard-allowlist.sh (sp-fjsxb). Cited, not re-tested (gap 16 of
# docs/test-plan/safety-fences.md is explicit that re-testing it here is duplication).

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
FAKE_RUN="$TMP/run"; FAKE_PROD="$TMP/prod"; FIXDIR="$TMP/fixtures"
mkdir -p "$FAKE_RUN" "$FAKE_PROD" "$FIXDIR"
BDCLOSE_RC=""
ACTOR="guard-test-actor"

# ---------------------------------------------------------------------------
# Payload builders
# ---------------------------------------------------------------------------
bash_payload() {  # bash_payload <cmd> -> PreToolUse JSON for the Bash tool
    printf '%s' "$1" | python3 -c \
        'import json,sys; cmd=sys.stdin.read(); print(json.dumps({"tool_name":"Bash","tool_input":{"command":cmd}}))'
}
tool_payload() {  # tool_payload <tool_name> <file_path> -> PreToolUse JSON for Write/Edit/Read
    python3 -c 'import json,sys; print(json.dumps({"tool_name":sys.argv[1],"tool_input":{"file_path":sys.argv[2]}}))' "$1" "$2"
}

# ---------------------------------------------------------------------------
# Stub bd — implements only the three calls these two guards shell out to.
# ---------------------------------------------------------------------------
STUB_BD="$TMP/bd"
cat > "$STUB_BD" <<'STUBEOF'
#!/usr/bin/env bash
set -u
FIXDIR="${STUB_FIXDIR:-}"
args=("$@")
[ "${args[0]:-}" = "-C" ] && args=("${args[@]:2}")
sub="${args[0]:-}"
case "$sub" in
    create)
        if [ -n "${STUB_TESTDATA_MARKER:-}" ]; then
            for a in "${args[@]}"; do
                case "$a" in *"$STUB_TESTDATA_MARKER"*) echo "appears to be test data"; exit 1 ;; esac
            done
        fi
        exit 0 ;;
    show)     cat "$FIXDIR/${args[1]:-}.show.json" 2>/dev/null || echo '{}'; exit 0 ;;
    comments) cat "$FIXDIR/${args[1]:-}.comments.json" 2>/dev/null || echo '[]'; exit 0 ;;
    *) exit 0 ;;
esac
STUBEOF
chmod +x "$STUB_BD"

# ---------------------------------------------------------------------------
# Guard runners
# ---------------------------------------------------------------------------
# fence_run <cmd> [ENV=val ...] -> stdout+stderr merged is never checked; the fence's
# protocol (gap 8) is: always exits 0, refusal is {"decision":"block"} on stdout alone.
fence_run() {
    local cmd="$1"; shift
    bash_payload "$cmd" | env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" 2>/dev/null
}
fence_rc() {  # fence_rc <cmd> [ENV..] -> the fence process's own exit code
    local cmd="$1"; shift
    bash_payload "$cmd" | env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" >/dev/null 2>&1
    printf '%s' "$?"
}
fence_run_tool() {  # fence_run_tool <tool_name> <file_path> [ENV..]
    local tool="$1" path="$2"; shift 2
    tool_payload "$tool" "$path" | env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" 2>/dev/null
}
fence_run_raw() {  # fence_run_raw <raw-stdin> [ENV..] -- for malformed/empty payload rows
    local raw="$1"; shift
    printf '%s' "$raw" | env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" 2>/dev/null
}

BDCLOSE_DB="$TMP/fakedb"
mkfix() {  # mkfix <bid> <started_at> <notes> <comments-json>
    python3 -c 'import json,sys; print(json.dumps({"started_at":sys.argv[1],"notes":sys.argv[2]}))' \
        "$2" "$3" > "$FIXDIR/$1.show.json"
    printf '%s' "$4" > "$FIXDIR/$1.comments.json"
}
# bdclose_run <bid> <cmd> [ENV=val ...] -> prints combined output on stdout; the function's
# own exit status IS the guard's exit status (0 allow / 2 block). Never call this through a
# command-substitution wrapper without also capturing "$?" in the SAME statement — "$(...)"
# forks a subshell, so a status stashed in a variable from inside the function would be lost
# the moment that subshell exits. BDCLOSE_RC is set by the caller, immediately after, from
# "$?" — never from inside this function.
bdclose_run() {
    local bid="$1" cmd="$2"; shift 2
    bash_payload "$cmd" | env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_AEON=1 BEAD_ID="$bid" BEADS_ACTOR="$ACTOR" \
        SPIRA_DB="$BDCLOSE_DB" SPIRA_BD="$STUB_BD" STUB_FIXDIR="$FIXDIR" \
        "$@" bash "$BDCLOSE_GUARD" 2>&1
}
close_cmd_for() { printf 'bd -C %s close %s --reason done' "$BDCLOSE_DB" "$1"; }
deliver_run() {  # deliver_run <bid> <run-tmp> -> stdout
    printf '{}' | env -i PATH="$PATH" HOME="$TMP" \
        BEAD_ID="$1" BEADS_ACTOR="$ACTOR" \
        SPIRA_DB="$BDCLOSE_DB" SPIRA_BD="$STUB_BD" STUB_FIXDIR="$FIXDIR" SPIRA_RUN="$2" \
        bash "$BDCLOSE_DELIVER" 2>/dev/null
}

# ===========================================================================
echo
echo "UC-safety-fences-01 — aeon session: queue- and landing-operating commands refused:"
# ===========================================================================
pc="$(fence_run "bash spira/batch.sh spira" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-01/positive-control-batch-sh-blocked" '"decision":"block"' "$pc"

out="$(fence_run "bash spira/batch.sh spira" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-01/batch-sh-blocked"        '"decision":"block"' "$out"
want "UC-safety-fences-01/reason-names-sp-kz8ob"   "sp-kz8ob"           "$out"

out="$(fence_run "git push origin main" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-01/git-push-blocked" '"decision":"block"' "$out"

out="$(fence_run "gh pr create --title x" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-01/gh-pr-create-blocked" '"decision":"block"' "$out"

out="$(fence_run "bash spira/queue.sh flush spira" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-01/queue-flush-blocked" '"decision":"block"' "$out"

out="$(fence_run "bash spira/queue.sh stats" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-01/queue-stats-allowed" '"decision":"block"' "$out"

out="$(fence_run "bash spira/verdict.sh push spira" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-01/verdict-sh-blocked" '"decision":"block"' "$out"

out="$(fence_run "bash spira/testenv-batch.sh --suites test-verdict.sh spira/sp-x" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-01/blocked-name-as-suite-arg-allowed" '"decision":"block"' "$out"

out="$(fence_run "git add spira/landing.sh" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-01/blocked-name-as-git-file-arg-allowed" '"decision":"block"' "$out"

out="$(fence_run "git add spira/landing.sh && bash spira/landing.sh push" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-01/compound-git-add-then-invocation-blocked" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "UC-safety-fences-02 — aeon session: writes under \$SPIRA_PROD / \$SPIRA_RUN/landstate refused:"
# ===========================================================================
out="$(fence_run "printf x > ${FAKE_RUN}/landstate/x" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-02/write-to-landstate-blocked" '"decision":"block"' "$out"

out="$(fence_run "echo x > ${FAKE_PROD}/f" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-02/redirect-write-to-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "rm -rf ${FAKE_PROD}/releases" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-02/rm-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "mv ${FAKE_PROD}/aeon.sh /tmp/x" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-02/mv-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "echo x | tee ${FAKE_PROD}/aeon.sh" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-02/tee-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "sed -i s/a/b/ ${FAKE_PROD}/aeon.sh" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-02/sed-i-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "git -C ${FAKE_PROD} commit -m x" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-02/git-C-prod-commit-blocked" '"decision":"block"' "$out"

out="$(fence_run "git -C ${FAKE_PROD} reset --hard" SPIRA_AEON=test-aeon)"
want "UC-safety-fences-02/git-C-prod-reset-blocked" '"decision":"block"' "$out"

out="$(fence_run "bash \"${FAKE_PROD}/spira/census.sh\" --with-suppressed" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-02/exec-from-prod-allowed" '"decision":"block"' "$out"

out="$(fence_run "cat ${FAKE_PROD}/spira/census.sh" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-02/cat-read-from-prod-allowed" '"decision":"block"' "$out"

for _cmd in \
    "${FAKE_PROD}/sop.sh match /tmp/sp-x.payload" \
    "${FAKE_PROD}/incident.sh list" \
    "${FAKE_PROD}/mail.sh send operator --from Ops --subject q --kind question --default x" \
    "${FAKE_PROD}/suites.sh run" \
    "${FAKE_PROD}/groomer.sh run"; do
    out="$(fence_run "$_cmd" SPIRA_AEON=test-aeon)"
    nowant "UC-safety-fences-02/rendered-brief-cmd-allowed-${_cmd##*/}" '"decision":"block"' "$out"
done

out="$(fence_run "bash ${FAKE_PROD}/census.sh --with-suppressed" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-02/census-sh-allowed" '"decision":"block"' "$out"

_dflag="--description"
_bd_cmd="$(printf "bd -C /db create title %s - <<'DESC'\nbash \"%s/census.sh\" and /verdict.sh are mentioned\nDESC" "$_dflag" "${FAKE_PROD}")"
out="$(fence_run "$_bd_cmd" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-02/prod-path-in-heredoc-allowed" '"decision":"block"' "$out"

out="$(fence_run "arm in aeon.sh see ${FAKE_PROD}/aeon.sh" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-02/prose-arm-in-not-mistaken-for-rm" '"decision":"block"' "$out"

out="$(fence_run "See ${FAKE_PROD}/aeon.sh tee operation" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-02/prose-tee-not-mistaken-for-tee" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "UC-safety-fences-03 — bd create test-data / unmapped-repo heuristic (stub bd, no testdb):"
# ===========================================================================
BDCREATE_DB="$TMP/prod-db"
FAKE_REPO_MAP="$TMP/repo-map"
printf 'spira | /dev/null | push\nbrain | /dev/null | push\n' > "$FAKE_REPO_MAP"

fence_bd() {  # fence_bd <cmd> [ENV..] -> fence stdout, using the stub bd's --dry-run fallback
    local cmd="$1"; shift
    bash_payload "$cmd" | env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_AEON=test-aeon SPIRA_DB="$BDCREATE_DB" SPIRA_BD="$STUB_BD" \
        SPIRA_REPO_MAP="$FAKE_REPO_MAP" "$@" \
        bash "$FENCE" 2>/dev/null
}

# D2: the positive control, A1 and A2 were the same command run three times — one row now.
out="$(fence_bd "bd -C $BDCREATE_DB create \"test-land-state\" -l \"plan\"")"
want "UC-safety-fences-03/test-data-title-blocked" '"decision":"block"' "$out"
want "UC-safety-fences-03/test-data-title-names-override" "SPIRA_BD_CREATE_OVERRIDE" "$out"

out="$(fence_bd "SPIRA_DB=/tmp/some-testdb bd -C $BDCREATE_DB create \"test bead\" -l \"plan,repo:fixture-repo\"")"
want "UC-safety-fences-03/inline-SPIRA_DB-does-not-bypass" '"decision":"block"' "$out"

out="$(fence_bd "bd -C $BDCREATE_DB create \"Implement config reload\" -l \"plan,repo:spira\"")"
nowant "UC-safety-fences-03/real-title-mapped-repo-allowed" '"decision":"block"' "$out"

out="$(fence_bd "bd -C /tmp/other-testdb create \"test bead\" -l \"plan\"")"
nowant "UC-safety-fences-03/non-prod-db-path-allowed" '"decision":"block"' "$out"

out="$(fence_bd "SPIRA_BD_CREATE_OVERRIDE=1 bd -C $BDCREATE_DB create \"test bead\" -l \"plan,repo:fixture-repo\"")"
nowant "UC-safety-fences-03/override-var-bypasses" '"decision":"block"' "$out"

out="$(fence_bd "bd -C $BDCREATE_DB create \"Add lane detection\" -l \"plan,repo:fixture-repo\"")"
want "UC-safety-fences-03/unmapped-repo-real-title-blocked" '"decision":"block"' "$out"
want "UC-safety-fences-03/unmapped-repo-names-override" "SPIRA_BD_CREATE_OVERRIDE" "$out"

# The fallback arm: title fails the direct test/debug/tmp/temp regex, so aeon-fence.sh asks
# bd itself via --dry-run; the stub answers "appears to be test data" for the marker word.
# Single-word title: the guard's own title parser is whitespace-based and captures only the
# first token of a quoted multi-word title (a real truncation defect, filed as sp-o5qm1 and
# not exercised here — a multi-word title would silently test the wrong string).
out="$(fence_bd "bd -C $BDCREATE_DB create \"sneakytestfixture\" -l \"plan\"" STUB_TESTDATA_MARKER=sneakytestfixture)"
want "UC-safety-fences-03/dry-run-fallback-flags-test-data" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "UC-safety-fences-04 — bd close refused while a post-claim comment is unacknowledged:"
# ===========================================================================
mkfix "bc1" "2026-01-01T00:00:00Z" "" \
  '[{"id":"11111111-1111-1111-1111-111111111111","author":"operator","created_at":"2026-01-02T00:00:00Z","text":"Amendment: use approach B, not A"}]'
out="$(bdclose_run bc1 "$(close_cmd_for bc1)")"
BDCLOSE_RC=$?
is   "UC-safety-fences-04/positive-control-blocks-rc"      2                                     "$BDCLOSE_RC"
want "UC-safety-fences-04/positive-control-blocks-message" "BLOCKED by bd-close-unacked-guard"    "$out"
want "UC-safety-fences-04/names-comment-id"                "11111111-1111-1111-1111-111111111111" "$out"
want "UC-safety-fences-04/names-override"                  "SPIRA_CLOSE_UNACKED_CONSIDERED"        "$out"

mkfix "bc2" "2026-01-01T00:00:00Z" "ACK 11111111-1111-1111-1111-111111111111: applied — will use approach B" \
  '[{"id":"11111111-1111-1111-1111-111111111111","author":"operator","created_at":"2026-01-02T00:00:00Z","text":"Amendment"}]'
bdclose_run bc2 "$(close_cmd_for bc2)" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-04/ack-note-allows" 0 "$BDCLOSE_RC"

bdclose_run bc1 "$(close_cmd_for bc1)" SPIRA_CLOSE_UNACKED_CONSIDERED="set aside for now" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-04/env-override-allows" 0 "$BDCLOSE_RC"

bdclose_run bc1 "SPIRA_CLOSE_UNACKED_CONSIDERED=deliberate bd -C $BDCLOSE_DB close bc1 --reason done" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-04/inline-override-allows" 0 "$BDCLOSE_RC"

mkfix "bc3" "2026-01-01T00:00:00Z" "" '[]'
bdclose_run bc3 "$(close_cmd_for bc3)" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-04/no-comments-allows" 0 "$BDCLOSE_RC"

mkfix "bc4" "2026-01-02T00:00:00Z" "" \
  '[{"id":"22222222-2222-2222-2222-222222222222","author":"operator","created_at":"2026-01-01T00:00:00Z","text":"pre-claim"}]'
bdclose_run bc4 "$(close_cmd_for bc4)" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-04/pre-claim-comment-allows" 0 "$BDCLOSE_RC"

mkfix "bc5" "2026-01-01T00:00:00Z" "" \
  "[{\"id\":\"33333333-3333-3333-3333-333333333333\",\"author\":\"$ACTOR\",\"created_at\":\"2026-01-02T00:00:00Z\",\"text\":\"self\"}]"
bdclose_run bc5 "$(close_cmd_for bc5)" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-04/self-comment-allows" 0 "$BDCLOSE_RC"

mkfix "bc6" "2026-01-01T00:00:00Z" "" \
  '[{"id":"44444444-4444-4444-4444-444444444444","author":"operator","created_at":"2026-01-02T00:00:00Z","text":"note"}]'
bdclose_run bc6 "bd -C $BDCLOSE_DB update bc6 --status in_progress" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-04/non-close-command-allows" 0 "$BDCLOSE_RC"

# ===========================================================================
echo
echo "UC-safety-fences-05 — unacked comment delivered once as PostToolUse additionalContext:"
# ===========================================================================
DELIVER_TMP1="$TMP/deliver-1"; mkdir -p "$DELIVER_TMP1"
out="$(deliver_run bc1 "$DELIVER_TMP1")"
want "UC-safety-fences-05/delivers-additionalContext" "additionalContext"                        "$out"
want "UC-safety-fences-05/names-comment-id"           "11111111-1111-1111-1111-111111111111"     "$out"
want "UC-safety-fences-05/names-ack-command"          "ACK 11111111-1111-1111-1111-111111111111" "$out"

out2="$(deliver_run bc1 "$DELIVER_TMP1")"
nowant "UC-safety-fences-05/idempotent-second-call-empty" "additionalContext" "$out2"

DELIVER_TMP4="$TMP/deliver-4"; mkdir -p "$DELIVER_TMP4"
out4="$(deliver_run bc4 "$DELIVER_TMP4")"
nowant "UC-safety-fences-05/pre-claim-not-delivered" "additionalContext" "$out4"

DELIVER_TMP5="$TMP/deliver-5"; mkdir -p "$DELIVER_TMP5"
out5="$(deliver_run bc5 "$DELIVER_TMP5")"
nowant "UC-safety-fences-05/self-comment-not-delivered" "additionalContext" "$out5"

# ===========================================================================
echo
echo "UC-safety-fences-15 — shared PreToolUse pass-through block (D1), applied to both guards:"
# ===========================================================================
FENCE_REP_CMD="bash spira/batch.sh spira"

out="$(fence_run "$FENCE_REP_CMD")"
nowant "UC-safety-fences-15/fence-no-SPIRA_AEON-allows" '"decision":"block"' "$out"

out="$(fence_run_tool Read /etc/hostname SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-15/fence-non-Bash-tool-allows" '"decision":"block"' "$out"

out="$(fence_run "echo '$FENCE_REP_CMD'" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-15/fence-single-quoted-prose-allows" '"decision":"block"' "$out"

heredoc_cmd="$(printf "bd -C /db create title --description - <<'DESC'\n%s\nDESC" "$FENCE_REP_CMD")"
out="$(fence_run "$heredoc_cmd" SPIRA_AEON=test-aeon)"
nowant "UC-safety-fences-15/fence-heredoc-prose-allows" '"decision":"block"' "$out"

# aeon-fence.sh has exactly one override — SPIRA_AEON_OVERRIDE, read from the hook's own
# process env, never parsed out of the command text — so there is no separate "inline"
# variant for this guard, unlike the two below.
out="$(fence_run "$FENCE_REP_CMD" SPIRA_AEON=test-aeon SPIRA_AEON_OVERRIDE=1)"
nowant "UC-safety-fences-15/fence-override-allows" '"decision":"block"' "$out"

BDCLOSE_REP="$(close_cmd_for bc1)"

out=$(printf '%s' "$(bash_payload "$BDCLOSE_REP")" | env -i PATH="$PATH" HOME="$TMP" \
    BEAD_ID="bc1" BEADS_ACTOR="$ACTOR" SPIRA_DB="$BDCLOSE_DB" SPIRA_BD="$STUB_BD" STUB_FIXDIR="$FIXDIR" \
    bash "$BDCLOSE_GUARD" 2>&1); rc=$?
is "UC-safety-fences-15/bdclose-no-SPIRA_AEON-allows" 0 "$rc"

out=$(printf '{"tool_name":"Read","tool_input":{"file_path":"/tmp/x"}}' | env -i PATH="$PATH" HOME="$TMP" \
    SPIRA_AEON=1 BEAD_ID="bc1" BEADS_ACTOR="$ACTOR" SPIRA_DB="$BDCLOSE_DB" SPIRA_BD="$STUB_BD" STUB_FIXDIR="$FIXDIR" \
    bash "$BDCLOSE_GUARD" 2>&1); rc=$?
is "UC-safety-fences-15/bdclose-non-Bash-tool-allows" 0 "$rc"

bdclose_run bc1 "echo 'bd -C $BDCLOSE_DB close bc1 --reason done'" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-15/bdclose-single-quoted-prose-allows" 0 "$BDCLOSE_RC"

bdclose_run bc1 "$(close_cmd_for bc1)" SPIRA_CLOSE_UNACKED_CONSIDERED="set aside" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-15/bdclose-env-override-allows" 0 "$BDCLOSE_RC"

bdclose_run bc1 "SPIRA_CLOSE_UNACKED_CONSIDERED=x bd -C $BDCLOSE_DB close bc1 --reason done" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-15/bdclose-inline-override-allows" 0 "$BDCLOSE_RC"

# Heredoc prose is NOT exempted for this guard: its close-detector splits the command on
# newlines (among other separators) before regex-matching "bd ... close", so a heredoc BODY
# line that merely mentions closing is indistinguishable from a real close invocation on its
# own line. This blocks a non-close command (a false positive, not a false pass — the guard
# still never lets an unacknowledged close through) and is pinned here as documented,
# undecided-not-fixed behaviour rather than an assumed pass. Filed as sp-fel4v.
heredoc_close="$(printf "bd -C %s comment bc1 note <<'EOF'\nplease run bd -C %s close bc1 --reason done\nEOF" "$BDCLOSE_DB" "$BDCLOSE_DB")"
bdclose_run bc1 "$heredoc_close" >/dev/null
BDCLOSE_RC=$?
is "UC-safety-fences-15/bdclose-heredoc-prose-currently-blocks-sp-fel4v" 2 "$BDCLOSE_RC"

# ===========================================================================
echo
echo "Gap 6 — aeon-fence.sh now covers Write/Edit against \$SPIRA_PROD, not just Bash:"
# ===========================================================================
out="$(fence_run_tool Write "$FAKE_PROD/aeon.sh" SPIRA_AEON=test-aeon)"
want "gap6/write-into-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run_tool Edit "$FAKE_PROD/aeon.sh" SPIRA_AEON=test-aeon)"
want "gap6/edit-into-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run_tool Write "$TMP/outside/notes.md" SPIRA_AEON=test-aeon)"
nowant "gap6/write-outside-prod-allowed" '"decision":"block"' "$out"

out="$(fence_run_tool Write "$FAKE_PROD/aeon.sh" SPIRA_AEON=test-aeon SPIRA_AEON_OVERRIDE=1)"
nowant "gap6/override-bypasses-write-block" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "Gap 7 — malformed/empty PreToolUse payload: fail-open, decided and pinned per guard:"
# ===========================================================================
out="$(fence_run_raw 'not json at all' SPIRA_AEON=test-aeon)"
nowant "gap7/fence-malformed-payload-fails-open" '"decision":"block"' "$out"

out="$(fence_run_raw '' SPIRA_AEON=test-aeon)"
nowant "gap7/fence-empty-payload-fails-open" '"decision":"block"' "$out"

out=$(printf 'not json at all' | env -i PATH="$PATH" HOME="$TMP" \
    SPIRA_AEON=1 BEAD_ID="bc1" BEADS_ACTOR="$ACTOR" SPIRA_DB="$BDCLOSE_DB" SPIRA_BD="$STUB_BD" STUB_FIXDIR="$FIXDIR" \
    bash "$BDCLOSE_GUARD" 2>&1); rc=$?
is "gap7/bdclose-malformed-payload-fails-open" 0 "$rc"

out=$(printf '' | env -i PATH="$PATH" HOME="$TMP" \
    SPIRA_AEON=1 BEAD_ID="bc1" BEADS_ACTOR="$ACTOR" SPIRA_DB="$BDCLOSE_DB" SPIRA_BD="$STUB_BD" STUB_FIXDIR="$FIXDIR" \
    bash "$BDCLOSE_GUARD" 2>&1); rc=$?
is "gap7/bdclose-empty-payload-fails-open" 0 "$rc"

# ===========================================================================
echo
echo "Gap 8 — two refusal protocols, asserted structurally (not just substring text):"
# ===========================================================================
rc="$(fence_rc "$FENCE_REP_CMD" SPIRA_AEON=test-aeon)"
is "gap8/fence-protocol-always-exits-0-even-when-blocking" 0 "$rc"

out="$(bdclose_run bc1 "$(close_cmd_for bc1)")"
BDCLOSE_RC=$?
is     "gap8/bdclose-protocol-exits-2-when-blocking"          2                     "$BDCLOSE_RC"
nowant "gap8/bdclose-protocol-does-not-emit-decision-json"    '"decision":"block"'  "$out"

# ===========================================================================
echo
echo "Gap 2 — aeon_settings() fails open silently (python3 error, non-executable guard):"
# ===========================================================================
AEON_SH="$HERE/aeon.sh"
extract_aeon_settings() { sed -n '/^aeon_settings() {/,/^}/p' "$AEON_SH"; }

GAP2_HOME="$TMP/gap2-home"; mkdir -p "$GAP2_HOME/hooks"
cp "$FENCE" "$GAP2_HOME/hooks/aeon-fence.sh"
cp "$HERE/hooks/aeon-mail-deliver.sh" "$GAP2_HOME/hooks/aeon-mail-deliver.sh"
cp "$BDCLOSE_GUARD" "$GAP2_HOME/bd-close-unacked-guard.sh"
chmod +x "$GAP2_HOME/hooks/aeon-fence.sh" "$GAP2_HOME/hooks/aeon-mail-deliver.sh" "$GAP2_HOME/bd-close-unacked-guard.sh"

baseline="$(SPIRA_HOME="$GAP2_HOME" bash -c "$(extract_aeon_settings); aeon_settings")"
want "gap2/baseline-wires-both-guards" "aeon-fence.sh" "$baseline"

chmod -x "$GAP2_HOME/bd-close-unacked-guard.sh"
noexec_out="$(SPIRA_HOME="$GAP2_HOME" bash -c "$(extract_aeon_settings); aeon_settings" 2>&1)"
nowant "gap2/non-executable-guard-dropped-silently"        "bd-close-unacked-guard.sh" "$noexec_out"
want   "gap2/non-executable-guard-other-hook-still-present" "aeon-fence.sh"             "$noexec_out"
chmod +x "$GAP2_HOME/bd-close-unacked-guard.sh"

mkdir -p "$TMP/nobin"
printf '#!/usr/bin/env bash\nexit 1\n' > "$TMP/nobin/python3"; chmod +x "$TMP/nobin/python3"
nopy_out="$(SPIRA_HOME="$GAP2_HOME" PATH="$TMP/nobin:$PATH" bash -c \
    "$(extract_aeon_settings); _AEON_SETTINGS=\"\$(aeon_settings)\" || _AEON_SETTINGS=''; printf '<%s>' \"\$_AEON_SETTINGS\"")"
is "gap2/python3-failure-falls-back-to-no-fences" "<>" "$nopy_out"

# ===========================================================================
echo
echo "suites.sh quarantine refuses from an aeon session (not a UC in the catalogue; ported as-is):"
# ===========================================================================
out_c0="$(SPIRA_AEON="" SPIRA_CONF=/nonexistent SPIRA_RUN="$FAKE_RUN" \
    bash "$HERE/suites.sh" quarantine nonexistent-suite.sh bead-id "reason" 2>&1 || true)"
nowant "suites-quarantine/silent-without-SPIRA_AEON" "aeons may not" "$out_c0"

out_c1="$(SPIRA_AEON=test-aeon SPIRA_CONF=/nonexistent SPIRA_RUN="$FAKE_RUN" \
    bash "$HERE/suites.sh" quarantine nonexistent-suite.sh bead-id "reason" 2>&1)"
rc_c1=$?
is   "suites-quarantine/exits-nonzero-in-aeon-session" 1               "$rc_c1"
want "suites-quarantine/names-aeons-may-not"           "aeons may not" "$out_c1"
want "suites-quarantine/names-operator-or-Ops"          "operator"      "$out_c1"

tl_summary
