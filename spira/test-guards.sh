#!/usr/bin/env bash
# test-guards.sh — the live PreToolUse guard, table-style: hooks/dwarf-fence.sh.
#
# Merges test-dwarf-fence.sh, test-dwarf-fence-bd-create.sh and test-bd-close-unacked-guard.sh
# (sp-qsr44, D1/D2). UC-06..13's original guards (bd-delivers-label, bd-remember,
# bd-update-inflight, focus-close, null-close-reason, mail-fence, testenv-batch-fence,
# schema-migration-guard) were retired by sp-fjsxb and are not re-created here.
#
# defect: sp-kz8ob sp-mvg44
# tier: T1
# covers: spira/hooks/dwarf-fence.sh dwarf/src/* testenv/src/suites/* UC-safety-fences-01 UC-safety-fences-02 UC-safety-fences-03 UC-safety-fences-15
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-guards.sh"

FENCE="$HERE/hooks/dwarf-fence.sh"
[ -x "$FENCE" ]          || bail "hooks/dwarf-fence.sh missing or not executable"

# UC-safety-fences-16 RETIRED (sp-j89pd, wave 4.2): dwarf_settings (lib.sh) had zero live
# callers — dwarf::run::dwarf_settings is the Rust port, a pure function with a fixed
# two-entry PreToolUse allow-list and no python3 subprocess at all, so the python3-failure
# fallback Gap 2 used to test here is not merely covered elsewhere, it is structurally gone.
# test-dwarf-settings-guard-allowlist.sh (gap 1, sp-fjsxb) is retired with it. Both are now
# dwarf::run::tests::settings_json_matches_python_dumps; see docs/test-plan/safety-fences.md
# and the [use_case.uncovered] marker on UC-safety-fences-16.

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
FAKE_RUN="$TMP/run"; FAKE_PROD="$TMP/prod";
FAKE_CONFIG="$TMP/.config/spira"
mkdir -p "$FAKE_RUN" "$FAKE_PROD" "$FAKE_CONFIG"
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
bash_rib_payload() {  # bash_rib_payload <cmd> <0|1> -> PreToolUse JSON for Bash with run_in_background set
    python3 -c 'import json,sys
print(json.dumps({"tool_name":"Bash","tool_input":{"command":sys.argv[1],"run_in_background":sys.argv[2]=="1"}}))' "$1" "$2"
}

# ---------------------------------------------------------------------------
# Guard runners
# ---------------------------------------------------------------------------
# fence_run <cmd> [ENV=val ...] -> stdout+stderr merged is never checked; the fence's
# protocol (gap 8) is: always exits 0, refusal is {"decision":"block"} on stdout alone.
fence_run() {
    local cmd="$1"; shift
    bash_payload "$cmd" | env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" 2>/dev/null
}
fence_rc() {  # fence_rc <cmd> [ENV..] -> the fence process's own exit code
    local cmd="$1"; shift
    bash_payload "$cmd" | env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" >/dev/null 2>&1
    printf '%s' "$?"
}
fence_run_tool() {  # fence_run_tool <tool_name> <file_path> [ENV..]
    local tool="$1" path="$2"; shift 2
    tool_payload "$tool" "$path" | env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" 2>/dev/null
}
fence_run_raw() {  # fence_run_raw <raw-stdin> [ENV..] -- for malformed/empty payload rows
    local raw="$1"; shift
    printf '%s' "$raw" | env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" 2>/dev/null
}


# ===========================================================================
echo
echo "UC-safety-fences-01 — dwarf session: queue- and landing-operating commands refused:"
# ===========================================================================
pc="$(fence_run "bash spira/landing.sh spira" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-01/positive-control-landing-sh-blocked" '"decision":"block"' "$pc"

out="$(fence_run "bash spira/landing.sh spira" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-01/landing-sh-blocked"      '"decision":"block"' "$out"
want "UC-safety-fences-01/reason-names-sp-kz8ob"   "sp-kz8ob"           "$out"

out="$(fence_run "git push origin main" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-01/git-push-blocked" '"decision":"block"' "$out"

out="$(fence_run "gh pr create --title x" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-01/gh-pr-create-blocked" '"decision":"block"' "$out"

out="$(fence_run "/opt/spira/bin/queue flush spira" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-01/queue-flush-blocked" '"decision":"block"' "$out"

out="$(fence_run "/opt/spira/bin/queue stats" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-01/queue-stats-allowed" '"decision":"block"' "$out"

out="$(fence_run "queue verdict spira" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-01/queue-verdict-blocked" '"decision":"block"' "$out"

out="$(fence_run "/opt/spira/bin/testenv --suites test-verdict.sh spira/sp-x" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-01/blocked-name-as-suite-arg-allowed" '"decision":"block"' "$out"

out="$(fence_run "git add spira/landing.sh" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-01/blocked-name-as-git-file-arg-allowed" '"decision":"block"' "$out"

out="$(fence_run "git add spira/landing.sh && bash spira/landing.sh push" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-01/compound-git-add-then-invocation-blocked" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "sp-4o925 — dwarf session: Bash run_in_background refused structurally, not charged after the fact:"
# ===========================================================================
fence_rib() {  # fence_rib <cmd> <0|1> [ENV..] -> fence stdout for a Bash call with run_in_background set
    local cmd="$1" rib="$2"; shift 2
    bash_rib_payload "$cmd" "$rib" | env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" 2>/dev/null
}
fence_rib_rc() {  # fence_rib_rc <cmd> <0|1> [ENV..] -> the fence process's own exit code
    local cmd="$1" rib="$2"; shift 2
    bash_rib_payload "$cmd" "$rib" | env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$FAKE_RUN" SPIRA_PROD="$FAKE_PROD" "$@" \
        bash "$FENCE" >/dev/null 2>&1
    printf '%s' "$?"
}

pc="$(fence_rib "/opt/spira/bin/testenv --suites test-verdict.sh spira/sp-x" 1 SPIRA_DWARF=test-dwarf)"
want "sp-4o925/positive-control-run-in-background-blocked" '"decision":"block"' "$pc"

out="$(fence_rib "/opt/spira/bin/testenv --suites test-verdict.sh spira/sp-x" 1 SPIRA_DWARF=test-dwarf)"
want "sp-4o925/run-in-background-blocked"        '"decision":"block"' "$out"
want "sp-4o925/reason-names-sp-4o925"            "sp-4o925"           "$out"
want "sp-4o925/reason-says-run-in-foreground"    "foreground"         "$out"

rc="$(fence_rib_rc "/opt/spira/bin/testenv --suites test-verdict.sh spira/sp-x" 1 SPIRA_DWARF=test-dwarf)"
is "sp-4o925/blocked-protocol-still-exits-0" 0 "$rc"

out="$(fence_rib "echo ordinary foreground command" 0 SPIRA_DWARF=test-dwarf)"
nowant "sp-4o925/run-in-background-false-allowed" '"decision":"block"' "$out"

out="$(fence_run "echo ordinary foreground command" SPIRA_DWARF=test-dwarf)"
nowant "sp-4o925/run-in-background-absent-allowed" '"decision":"block"' "$out"

out="$(fence_rib "/opt/spira/bin/testenv --suites test-verdict.sh spira/sp-x" 1)"
nowant "sp-4o925/no-SPIRA_DWARF-run-in-background-allowed" '"decision":"block"' "$out"

out="$(fence_rib "/opt/spira/bin/testenv --suites test-verdict.sh spira/sp-x" 1 SPIRA_DWARF=test-dwarf SPIRA_DWARF_OVERRIDE=1)"
nowant "sp-4o925/override-bypasses-run-in-background-block" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "UC-safety-fences-02 — dwarf session: writes under \$SPIRA_PROD / \$SPIRA_RUN/queue refused:"
# ===========================================================================
out="$(fence_run "printf x > ${FAKE_RUN}/queue/x" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-02/write-to-queue-blocked" '"decision":"block"' "$out"

# sp-j7l3q: the lifecycle cutover deletes the landstate ledger (sp-2c1n0); its write fence went with it.
out="$(fence_run "printf x > ${FAKE_RUN}/landstate/x" SPIRA_DWARF=test-dwarf)"
nowant "sp-j7l3q/landstate-no-longer-fenced" '"decision":"block"' "$out"

out="$(fence_run "echo x > ${FAKE_PROD}/f" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-02/redirect-write-to-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "rm -rf ${FAKE_PROD}/releases" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-02/rm-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "mv ${FAKE_PROD}/dwarf.sh /tmp/x" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-02/mv-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "echo x | tee ${FAKE_PROD}/dwarf.sh" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-02/tee-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "sed -i s/a/b/ ${FAKE_PROD}/dwarf.sh" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-02/sed-i-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run "git -C ${FAKE_PROD} commit -m x" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-02/git-C-prod-commit-blocked" '"decision":"block"' "$out"

out="$(fence_run "git -C ${FAKE_PROD} reset --hard" SPIRA_DWARF=test-dwarf)"
want "UC-safety-fences-02/git-C-prod-reset-blocked" '"decision":"block"' "$out"

out="$(fence_run "bash \"${FAKE_PROD}/spira/census\" --with-suppressed" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-02/exec-from-prod-allowed" '"decision":"block"' "$out"

out="$(fence_run "cat ${FAKE_PROD}/spira/census" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-02/cat-read-from-prod-allowed" '"decision":"block"' "$out"

for _cmd in \
    "${FAKE_PROD}/../bin/sop match /tmp/sp-x.payload" \
    "${FAKE_PROD}/incident.sh list" \
    "${FAKE_PROD}/mail send operator --from Ops --subject q --kind question --default x" \
    "${FAKE_PROD}/../bin/testenv suites status" \
    "${FAKE_PROD}/../bin/groomer deadlocked"; do
    out="$(fence_run "$_cmd" SPIRA_DWARF=test-dwarf)"
    nowant "UC-safety-fences-02/rendered-brief-cmd-allowed-${_cmd##*/}" '"decision":"block"' "$out"
done

out="$(fence_run "bash ${FAKE_PROD}/census --with-suppressed" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-02/census-allowed" '"decision":"block"' "$out"

_dflag="--description"
_bd_cmd="$(printf "work file title %s - <<'DESC'\nbash \"%s/census\" and /verdict.sh are mentioned\nDESC" "$_dflag" "${FAKE_PROD}")"
out="$(fence_run "$_bd_cmd" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-02/prod-path-in-heredoc-allowed" '"decision":"block"' "$out"

out="$(fence_run "arm in dwarf.sh see ${FAKE_PROD}/dwarf.sh" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-02/prose-arm-in-not-mistaken-for-rm" '"decision":"block"' "$out"

out="$(fence_run "See ${FAKE_PROD}/dwarf.sh tee operation" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-02/prose-tee-not-mistaken-for-tee" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "sp-ozym9 — \$SPIRA_RUN/queue: write shapes refused, reads and prose allowed:"
# ===========================================================================
out="$(fence_run "rm ${FAKE_RUN}/queue/x" SPIRA_DWARF=test-dwarf)"
want "sp-ozym9/rm-queue-blocked" '"decision":"block"' "$out"

out="$(fence_run "mv a ${FAKE_RUN}/queue/b" SPIRA_DWARF=test-dwarf)"
want "sp-ozym9/mv-queue-blocked" '"decision":"block"' "$out"

out="$(fence_run "echo x | tee ${FAKE_RUN}/queue/x" SPIRA_DWARF=test-dwarf)"
want "sp-ozym9/tee-queue-blocked" '"decision":"block"' "$out"

out="$(fence_run "sed -i s/a/b/ ${FAKE_RUN}/queue/x" SPIRA_DWARF=test-dwarf)"
want "sp-ozym9/sed-i-queue-blocked" '"decision":"block"' "$out"

out="$(fence_run "cat ${FAKE_RUN}/queue/sp-7youp" SPIRA_DWARF=test-dwarf)"
nowant "sp-ozym9/cat-queue-allowed" '"decision":"block"' "$out"

out="$(fence_run "ls ${FAKE_RUN}/queue | wc -l" SPIRA_DWARF=test-dwarf)"
nowant "sp-ozym9/ls-queue-allowed" '"decision":"block"' "$out"

out="$(fence_run "grep -l '^LANDED' ${FAKE_RUN}/queue/*" SPIRA_DWARF=test-dwarf)"
nowant "sp-ozym9/grep-queue-allowed" '"decision":"block"' "$out"

_ldflag="--description"
_ld_cmd="$(printf "work file title %s - <<'DESC'\nsee %s/queue/sp-7youp for evidence\nDESC" "$_ldflag" "${FAKE_RUN}")"
out="$(fence_run "$_ld_cmd" SPIRA_DWARF=test-dwarf)"
nowant "sp-ozym9/queue-path-in-heredoc-allowed" '"decision":"block"' "$out"

out="$(fence_run "echo '${FAKE_RUN}/queue/x needs no write'" SPIRA_DWARF=test-dwarf)"
nowant "sp-ozym9/queue-path-in-single-quoted-prose-allowed" '"decision":"block"' "$out"

out="$(fence_run "rm ${FAKE_RUN}/queue/x" SPIRA_DWARF=test-dwarf SPIRA_DWARF_OVERRIDE=1)"
nowant "sp-ozym9/override-bypasses-queue-write-block" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "UC-safety-fences-03 — dwarf session: direct bd/bdq calls are refused, by any path (sp-dxbocs):"
# ===========================================================================
STUB_BD="$TMP/bd"
printf '#!/bin/sh\nexit 0\n' > "$STUB_BD"; chmod +x "$STUB_BD"
# Reverses the 2026-10-05 no-bd-rule ruling (sp-j7l3q) on 2026-11-18: ops sweeps reached the
# store by absolute path, past a PATH that held only `work`.
BDCREATE_DB="$TMP/prod-db"
fence_bd() {  # fence_bd <cmd> [ENV..] -> fence stdout with a production SPIRA_DB in scope
    local cmd="$1"; shift
    bash_payload "$cmd" | env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" HOME="$TMP" \
        SPIRA_DWARF=test-dwarf SPIRA_DB="$BDCREATE_DB" SPIRA_BD="$STUB_BD" "$@" \
        bash "$FENCE" 2>/dev/null
}
_bd_heredoc="$(printf 'work note-on sp-1 - <<EOF\nbd create x\nEOF')"
for rune in builder ops groomer; do
    for c in 'bd create x' '~/.local/bin/bd create x' 'BEADS_DIR=/x/.beads ~/.local/bin/bd create x' \
             'cd /x && bd list' '$SPIRA_RELEASE/bin/bdq create -t bug x' '/r/bin/bdq --help' \
             'bash -c "bd show sp-1"' 'env FOO=1 bd-embedded list' 'export BEADS_DIR=/x' \
             'sudo -u root bd list' 'timeout 5 /usr/bin/bd list' 'echo `bd create x`' 'echo $(bd list)' \
             'ls | xargs bd show' 'find . -exec bd list {} +' 'eval "bd create x"' 'bash -lc "BEADS_DIR=/x true"' \
             'declare -x BEADS_DIR=/x' 'if bd list; then true; fi' 'while bd list; do true; done' \
             'nice -n 5 bd list' 'xargs -n 1 bd show' 'time bd list' 'nohup bd list' "$(printf 'true\nbd list')"; do
        out="$(fence_bd "$c" SPIRA_RUNE="$rune")"
        want "sp-dxbocs/$rune/refused: $c" '"decision":"block"' "$out"
        want "sp-dxbocs/$rune/names-the-exit: $c" 'work file' "$out"
    done
    for c in 'work file "bd is broken" --for builder --repo spira --body-file -' 'grep -n bd foo.rs' \
             'echo "run bd"' "$_bd_heredoc" 'cargo test -p bead' 'echo BEADS_DIR=x'; do
        out="$(fence_bd "$c" SPIRA_RUNE="$rune")"
        nowant "sp-dxbocs/$rune/allowed: $c" '"decision":"block"' "$out"
    done
    out="$(fence_bd 'bd create x' SPIRA_RUNE="$rune" SPIRA_DWARF_OVERRIDE=1)"
    nowant "sp-dxbocs/$rune/override-bypasses" '"decision":"block"' "$out"
    out="$(bash_payload 'bd create x' | env -i PATH="$PATH" HOME="$TMP" SPIRA_RUNE="$rune" bash "$FENCE" 2>/dev/null)"
    nowant "sp-dxbocs/$rune/silent-outside-dwarf" '"decision":"block"' "$out"
done

# ===========================================================================
echo
echo "UC-safety-fences-15 — shared PreToolUse pass-through block (D1), applied to both guards:"
# ===========================================================================
FENCE_REP_CMD="bash spira/landing.sh spira"

out="$(fence_run "$FENCE_REP_CMD")"
nowant "UC-safety-fences-15/fence-no-SPIRA_DWARF-allows" '"decision":"block"' "$out"

out="$(fence_run_tool Read /etc/hostname SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-15/fence-non-Bash-tool-allows" '"decision":"block"' "$out"

out="$(fence_run "echo '$FENCE_REP_CMD'" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-15/fence-single-quoted-prose-allows" '"decision":"block"' "$out"

heredoc_cmd="$(printf "work file title --body-file - <<'DESC'\n%s\nDESC" "$FENCE_REP_CMD")"
out="$(fence_run "$heredoc_cmd" SPIRA_DWARF=test-dwarf)"
nowant "UC-safety-fences-15/fence-heredoc-prose-allows" '"decision":"block"' "$out"

# dwarf-fence.sh has exactly one override — SPIRA_DWARF_OVERRIDE, read from the hook's own
# process env, never parsed out of the command text — so there is no separate "inline"
# variant for this guard, unlike the two below.
out="$(fence_run "$FENCE_REP_CMD" SPIRA_DWARF=test-dwarf SPIRA_DWARF_OVERRIDE=1)"
nowant "UC-safety-fences-15/fence-override-allows" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "Gap 6 — dwarf-fence.sh now covers Write/Edit against \$SPIRA_PROD, not just Bash:"
# ===========================================================================
out="$(fence_run_tool Write "$FAKE_PROD/dwarf.sh" SPIRA_DWARF=test-dwarf)"
want "gap6/write-into-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run_tool Edit "$FAKE_PROD/dwarf.sh" SPIRA_DWARF=test-dwarf)"
want "gap6/edit-into-prod-blocked" '"decision":"block"' "$out"

out="$(fence_run_tool Write "$TMP/outside/notes.md" SPIRA_DWARF=test-dwarf)"
nowant "gap6/write-outside-prod-allowed" '"decision":"block"' "$out"

out="$(fence_run_tool Write "$FAKE_PROD/dwarf.sh" SPIRA_DWARF=test-dwarf SPIRA_DWARF_OVERRIDE=1)"
nowant "gap6/override-bypasses-write-block" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "sp-iks0y — dwarf session: writes to the operator's live \$HOME/.config/spira refused:"
# ===========================================================================
out="$(fence_run_tool Edit "$FAKE_CONFIG/repo-map" SPIRA_DWARF=test-dwarf)"
want "sp-iks0y/edit-repo-map-blocked"    '"decision":"block"' "$out"
want "sp-iks0y/reason-names-sp-iks0y"    "sp-iks0y"           "$out"

out="$(fence_run_tool Write "$FAKE_CONFIG/repo-map" SPIRA_DWARF=test-dwarf)"
want "sp-iks0y/write-repo-map-blocked" '"decision":"block"' "$out"

out="$(fence_run_tool Write "$TMP/outside/notes.md" SPIRA_DWARF=test-dwarf)"
nowant "sp-iks0y/write-outside-config-allowed" '"decision":"block"' "$out"

out="$(fence_run_tool Edit "$FAKE_RUN/worktree/sp-x/notes.md" SPIRA_DWARF=test-dwarf)"
nowant "sp-iks0y/edit-inside-worktree-allowed" '"decision":"block"' "$out"

out="$(fence_run_tool Edit "$FAKE_CONFIG/repo-map" SPIRA_DWARF=test-dwarf SPIRA_DWARF_OVERRIDE=1)"
nowant "sp-iks0y/override-bypasses-config-write-block" '"decision":"block"' "$out"

out="$(fence_run "echo x > ${FAKE_CONFIG}/repo-map" SPIRA_DWARF=test-dwarf)"
want "sp-iks0y/bash-redirect-into-config-blocked" '"decision":"block"' "$out"

out="$(fence_run "printf x >> ${FAKE_CONFIG}/repo-map" SPIRA_DWARF=test-dwarf)"
want "sp-iks0y/bash-append-into-config-blocked" '"decision":"block"' "$out"

out="$(fence_run "rm ${FAKE_CONFIG}/repo-map" SPIRA_DWARF=test-dwarf)"
want "sp-iks0y/bash-rm-config-blocked" '"decision":"block"' "$out"

out="$(fence_run "sed -i s/a/b/ ${FAKE_CONFIG}/repo-map" SPIRA_DWARF=test-dwarf)"
want "sp-iks0y/bash-sed-i-config-blocked" '"decision":"block"' "$out"

out="$(fence_run "echo x | tee ${FAKE_CONFIG}/repo-map" SPIRA_DWARF=test-dwarf)"
want "sp-iks0y/bash-tee-config-blocked" '"decision":"block"' "$out"

out="$(fence_run "cat ${FAKE_CONFIG}/repo-map" SPIRA_DWARF=test-dwarf)"
nowant "sp-iks0y/bash-cat-config-allowed" '"decision":"block"' "$out"

out="$(fence_run "rm ${FAKE_CONFIG}/repo-map" SPIRA_DWARF=test-dwarf SPIRA_DWARF_OVERRIDE=1)"
nowant "sp-iks0y/override-bypasses-bash-config-block" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "Gap 7 — malformed/empty PreToolUse payload: fail-open, decided and pinned per guard:"
# ===========================================================================
out="$(fence_run_raw 'not json at all' SPIRA_DWARF=test-dwarf)"
nowant "gap7/fence-malformed-payload-fails-open" '"decision":"block"' "$out"

out="$(fence_run_raw '' SPIRA_DWARF=test-dwarf)"
nowant "gap7/fence-empty-payload-fails-open" '"decision":"block"' "$out"


# ===========================================================================
echo
echo "Gap 8 — the fence refuses on stdout and exits 0:"
# ===========================================================================
rc="$(fence_rc "$FENCE_REP_CMD" SPIRA_DWARF=test-dwarf)"
is "gap8/fence-protocol-always-exits-0-even-when-blocking" 0 "$rc"

# ===========================================================================
echo
echo "pattern kills are refused in a dwarf session; PID kills and unit stops pass:"
# ===========================================================================
for c in 'pkill -f "it-"' 'pkill cargo' 'killall cargo' 'pgrep -f it- | xargs kill' \
         'FOO=1 sudo pkill -9 -f x' 'cd /tmp && pkill -f it-' 'bash -c "pkill -f it-"' \
         'kill $(pgrep -f it-)' 'sh -lc "pkill x"' 'pkill -f "it-' 'pgrep x | xargs -r kill' 'sudo -u root pkill -f x' 'env -u FOO pkill -f x' \
         'kill $(ps aux | grep it- | awk "{print \$2}")' 'kill "$(pgrep -f it-)"' \
         'pgrep -f it- | while read p; do kill $p; done' 'ps aux | grep it- | awk "{print \$2}" | xargs kill -9' 'timeout 5 /usr/bin/pkill -f it-'; do
    out="$(fence_run "$c" SPIRA_DWARF=test-dwarf)"
    want "pattern-kill/refused: $c" '"decision":"block"' "$out"
    want "pattern-kill/names-the-exit: $c" 'systemctl --user stop' "$out"
done
for c in 'ps aux | grep kill' 'ps aux | grep -v kill | wc -l' 'kill 1234' 'kill -9 1234 5678' 'systemctl --user stop spira-x.service' \
         'grep pkill notes.md' 'echo "do not pkill -f it-"' 'cat /proc/1234/cmdline | xargs -0 echo' \
         'git commit -m "no pkill here"'; do
    out="$(fence_run "$c" SPIRA_DWARF=test-dwarf)"
    nowant "pattern-kill/allowed: $c" '"decision":"block"' "$out"
done
out="$(fence_run 'pkill -f "it-"' SPIRA_DWARF=test-dwarf SPIRA_DWARF_OVERRIDE=1)"
nowant "pattern-kill/override-bypasses" '"decision":"block"' "$out"
out="$(fence_run 'pkill -f "it-"')"
nowant "pattern-kill/silent-outside-dwarf" '"decision":"block"' "$out"


tl_summary
