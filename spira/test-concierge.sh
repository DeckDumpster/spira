#!/usr/bin/env bash
#
# test-concierge.sh — the Concierge persona: brief composition, resume/HEADLESS handling,
#   and the ways the operator's way in has broken before. No systemd, no tmux, no live
#   statute book — see test-concierge-acceptance.sh for the host-only remainder (cgroup
#   survival, the launcher under a real systemd oneshot, the cockpit layout pane) and
#   test-concierge-roster.sh for the roster, resume-id and session-hook seams.
#
# UC-operator-channel-41 — DEMOTED TO T1 (coverage map row 41): the brief-composition
# section used to skip outright when the live statute book was empty, so a container run
# and a full run reported the same green for a different number of assertions. It now
# drives compose_brief through the SPIRA_MEMORIES_CMD seam against a fixture persona and a
# fixture chamber, always: the mechanism under test (no leftover {{, an executable mail
# and bead.sh, the statute book present, a typo'd core set refusing rather than composing
# silently) does not depend on what is currently enacted in the real book.
#
# defect: sp-u4x sp-epe0m
# covers: spira/lib.sh concierge.sh spira/chamber/concierge.fayth spira/chamber/concierge.md UC-operator-channel-40 UC-operator-channel-41
# hermetic-ok: fixture chamber and a private tmux socket; no systemd and no database
# requires: claude
# host-reason: concierge.sh refuses to run any subcommand without claude and tmux on PATH (spira_require); the `here` section also needs a real tmux binary and skips on its own if one is not on PATH
# scar: the concierge persona lacked FAYTH_SUMMON=operator and could be claimed by the sentinel as an ordinary worker.
# tier: T2
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$(cd "$HERE/.." && pwd)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# EVERY `concierge.sh start` BELOW MUST NEVER TOUCH THE OPERATOR'S REAL CLIENT SETTINGS.
# `start` now ensures the SessionStart hook is registered (`release session-hook install`),
# which defaults SPIRA_CLIENT_SETTINGS to $HOME/.claude/settings.json — and none of the
# fixtures below override HOME. Exported once here rather than on every invocation below.
export SPIRA_CLIENT_SETTINGS="$TMP/settings.json"
tl_config SPIRA_CLIENT_SETTINGS="$SPIRA_CLIENT_SETTINGS"

echo "the escalation list — three classes, not the old five (sp-3dggv)"

# SEEN TO FAIL FIRST: before sp-3dggv, concierge.md escalated on five-plus classes, including
# "anything that will page him" and "a product decision about what a feature IS or what a
# number MEANS" — prose the operator's verdict retired. Pin the persona text to the amended
# law-escalate-decisions-not-problems statute so the two cannot drift apart silently.
CM="$(cat "$HERE/chamber/concierge.md")"
want   "names PERMISSIONS"                                          "PERMISSIONS" "$CM"
want   "names POLICY"                                                "POLICY"      "$CM"
want   "names DESTRUCTIVE"                                           "DESTRUCTIVE" "$CM"
nowant "no longer escalates on 'anything that will page'"            "anything that will page" "$CM"
nowant "no longer escalates on the retired product-decision class"   "product decision about what a feature IS" "$CM"
nowant "no longer escalates on work outside an approved design's intent" "approved design" "$CM"
nowant "no longer escalates on a choice between defensible options"  "choice between defensible options" "$CM"

echo
echo "the brief — argument handling"

# BRIEF REJECTS EXTRA ARGUMENTS: 'brief --resume' exits 2 and names the working composition.
# An operator who types it expects an error, not silent success with the flag dropped.
out="$(bash "$HARNESS/concierge.sh" brief --resume 2>&1)"; rc=$?
is   "brief --resume exits 2"              2 "$rc"
want "and names the working composition"   "append-system-prompt" "$out"
# POSITIVE CONTROL: without extra arguments the exit code is not 2, confirming the check
# fires on the flag and not on something unrelated.
rc_plain=0; bash "$HARNESS/concierge.sh" brief 2>/dev/null >/dev/null || rc_plain=$?
if [ "$rc_plain" -ne 2 ]; then
    ok "brief without args does not exit 2"
else
    bad "brief without args does not exit 2" "positive control broken"
fi

echo
echo "the brief — composed against a fixture chamber and a fixture statute cache"

# A FIXTURE PERSONA, NOT THE SHIPPED ONE — editing chamber/concierge.fayth to drive this
# would leave the suite one failed assertion away from having corrupted the thing it tests.
# mail and bead.sh are SYMLINKED IN rather than reimplemented, so "names an executable
# tool" is checking the real tools under a fixture chamber, not a fixture's stand-ins.
FX="$TMP/fx"; mkdir -p "$FX/chamber"
# round 2 fix: SPIRA_HOME IS the home now (locate_home no longer searches) and every
# binary reads <home>/conf.d to resolve its config schema at all — give this stub
# chamber the real registry.
ln -s "$HERE/conf.d" "$FX/conf.d"
cp "$HERE/chamber/concierge.md" "$FX/chamber/fx.md"
ln -sf "$(command -v mail)" "$FX/mail"   # the real compiled binary, found on the suite's own PATH (mail is gone, sp-ooh1k)
ln -sf "$HERE/bead.sh" "$FX/bead.sh"
cat > "$FX/chamber/fx.fayth" <<'EOF'
FAYTH_NAME=fx
FAYTH_STATUTE_CORE="law-rm-alpha"
EOF

FIX_JSON='{"law-rm-alpha":"Alpha fixture statute body.","law-rm-beta":"Beta fixture statute body."}'
fixture_cmd() { printf 'printf %s' "$(printf '%q' "$FIX_JSON")"; }

brief_fx() { # brief_fx [persona] -> compose the brief for a fixture persona
    # SPIRA_MEMORIES_CACHE is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG):
    # declare via tl_config, not the env prefix below, which no process reads it from any more.
    # round 2 fix (pattern 6): SPIRA_CHAMBER no longer derives from SPIRA_HOME either.
    # SPIRA_RUN is the same story (pattern 2): it no longer derives from SPIRA_HOME=$FX —
    # it is a flat declared value in the complete fixture (/fixture/userhome/spira/run), a path
    # this suite's own fixture tree neither owns nor can write to. concierge.sh writes the
    # rendered brief at $SPIRA_RUN/concierge-brief.md with no fallback, so give it this
    # suite's own run dir.
    tl_config SPIRA_MEMORIES_CACHE="" SPIRA_CHAMBER="$FX/chamber" SPIRA_RUN="$FX/run"
    SPIRA_HOME="$FX" CONCIERGE_FAYTH="${1:-fx}" \
        SPIRA_MEMORIES_CMD="$(fixture_cmd)" bash "$HARNESS/concierge.sh" brief
}

BRIEF="$(brief_fx 2>"$TMP/err")"
if [ -n "$BRIEF" ] && [ -f "$BRIEF" ]; then
    ok "concierge.sh brief renders a file"
    B="$(cat "$BRIEF")"
    # EVERY PLACEHOLDER, because an unsubstituted one is a command line the session will try
    # to run. The failure arrives hours later as "the concierge does not escalate anything".
    nowant "no placeholder survives rendering"  "{{"          "$B"
    want   "the brief names the mail path"      "mail send operator"  "$B"
    # mail is a compiled binary now, invoked bare (sp-ooh1k) — the brief names no path to
    # extract any more; the only thing left to prove is that "mail" itself resolves.
    if command -v mail >/dev/null 2>&1; then
        ok "mail path in brief resolves on PATH: mail"
    else
        bad "mail path in brief resolves on PATH" "[not found on PATH]"
    fi
    want   "and the bead contract"              "bead.sh file" "$B"
    # THE FILE, NOT THE STRING. The string check above is the positive control: the path
    # must be named for the grep below to find it. The assertion with teeth is this one —
    # a path that is named but absent passes the string check and fails here.
    bead_path="$(printf '%s\n' "$B" | grep -oE '[^ `(]*bead\.sh' | head -1)"
    if [ -n "$bead_path" ] && command -v "$bead_path" >/dev/null 2>&1; then
        ok "bead tool in brief resolves on PATH: $bead_path"
    else
        bad "bead tool in brief resolves on PATH" "[${bead_path:-<not found>}]"
    fi
    want "and carries the statute book"       "# Memories in force" "$B"
    want "the declared core statute renders in full" "## law-rm-alpha" "$B"
    want "and its body is present"                   "Alpha fixture statute body" "$B"
else
    bad "concierge.sh brief produced nothing" "$(cat "$TMP/err")"
fi

# A MISSING BRIEF IS A REFUSAL, NOT A DEGRADED START — the assertion that the refusal exists.
# Pointed at a persona with no markdown, it must fail loudly rather than launch a session whose
# only difference from a working one is that it was never told anything.
out="$(brief_fx no-such-persona 2>&1)"; rc=$?
is   "a missing brief exits non-zero"     1 "$rc"
want "and says which file was missing"    "no-such-persona.md" "$out"

# A CORE SET THAT RENDERS NOTHING IN FULL IS A TYPO, AND IT IS THE SILENT ONE. render_memories
# matches core slugs EXACTLY and demotes anything it does not recognise to the index tier
# without a word, so a mistyped or retired slug costs that statute its full text and says
# nothing at all. The brief still looks complete — right size, every placeholder filled, the
# law apparently present — which is why this needs an assertion rather than a reader.
cat > "$FX/chamber/typo.md" <<EOF
$(cat "$HERE/chamber/concierge.md")
EOF
cat > "$FX/chamber/typo.fayth" <<'EOF'
FAYTH_NAME=typo
FAYTH_STATUTE_CORE="law-slug-that-does-not-exist"
EOF
out="$(brief_fx typo 2>&1)"; rc=$?
is   "an all-typo core set exits non-zero"  1 "$rc"
want "and says the slugs were demoted"      "no statute rendered in full" "$out"

echo
echo "the brief — no-wiki install (SPIRA_WIKI unset)"

# SPIRA_WIKI UNSET: every path the brief names must be a file that exists on this host.
# THE POSITIVE CONTROL: the brief must still name the bead tool; absence of a wiki-relative
# path alone would pass just as well against a brief that named nothing at all.
# SPIRA_WIKI is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG); the empty
# value here is the real "no wiki installed" state under test, not a default fallback —
# declare it via tl_config rather than the env prefix, which no process reads any more.
tl_config SPIRA_WIKI=""
BRIEF_NW="$(brief_fx 2>"$TMP/err_nw")"
if [ -n "$BRIEF_NW" ] && [ -f "$BRIEF_NW" ]; then
    ok "no-wiki brief renders"
    BNW="$(cat "$BRIEF_NW")"
    nowant "no-wiki brief does not name a wiki-relative tool"  ".claude/bead.sh" "$BNW"
    want   "no-wiki brief still names the harness bead tool"   "bead.sh file"    "$BNW"
    bead_path="$(printf '%s\n' "$BNW" | grep -oE '[^ `(]*bead\.sh' | head -1)"
    if [ -n "$bead_path" ] && command -v "$bead_path" >/dev/null 2>&1; then
        ok "bead tool in brief resolves on PATH: $bead_path"
    else
        bad "bead tool in brief resolves on PATH" "[${bead_path:-<not found>}]"
    fi
else
    bad "no-wiki brief renders" "$(cat "$TMP/err_nw")"
fi

echo
echo "resume — launcher carries SPIRA_CONCIERGE=1, and the retry mechanism is in the script"

want "the launcher source contains SPIRA_CONCIERGE export" \
    "SPIRA_CONCIERGE=1" "$(cat "$HARNESS/concierge.sh")"

echo
echo "the clipboard shim (sp-gg587) — load-buffer gains -w only under SPIRA_CONCIERGE=1"

# THE MECHANISM. Claude Code copies a mouse selection with `tmux load-buffer -` (no -w),
# which fills only the buffer of the tmux server it runs against — the concierge's own
# nested socket, short of the operator's terminal. `-w` additionally relays the buffer via
# OSC 52 (see cockpit/layout.sh apply_clipboard_mode for the other half). The shim must add
# -w to exactly that command, only when SPIRA_CONCIERGE=1, and leave everything else
# untouched — a fake `tmux` that just echoes its own argv stands in for the real one, so
# nothing here needs an actual tmux server.
SHIM_TMP="$(mktemp -d)"; trap 'rm -rf "$SHIM_TMP" "$TMP"' EXIT
SHIM_REAL="$SHIM_TMP/realbin"; mkdir -p "$SHIM_REAL"
printf '#!/bin/sh\nprintf "REAL:%%s\\n" "$*"\n' > "$SHIM_REAL/tmux"; chmod +x "$SHIM_REAL/tmux"

# A PATH prefix: conf.sh keeps the caller's PATH first and only appends its tail (sp-gypjk),
# so write_tmux_shim's own `command -v tmux` finds the fake.
# SPIRA_RUN is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare via
# tl_config, not the env prefix below, which no process reads it from any more.
tl_config SPIRA_RUN="$SHIM_TMP/run"
shim_dir="$(PATH="$SHIM_REAL:$PATH" bash "$HARNESS/concierge.sh" _write-tmux-shim)"
want "the shim dir is under SPIRA_RUN" "$SHIM_TMP/run/concierge-tmux-shim" "$shim_dir"
[ -x "$shim_dir/tmux" ] && ok "the shim script is executable" \
    || bad "the shim script is executable" "missing at $shim_dir/tmux"

out_on="$(SPIRA_CONCIERGE=1 PATH="$shim_dir:$SHIM_REAL:$PATH" tmux load-buffer -)"
want "SPIRA_CONCIERGE=1: load-buffer gains -w" "REAL:load-buffer -w -" "$out_on"

out_off="$(PATH="$shim_dir:$SHIM_REAL:$PATH" tmux load-buffer -)"
want "without SPIRA_CONCIERGE=1: load-buffer is untouched (positive control)" \
    "REAL:load-buffer -" "$out_off"

out_other="$(SPIRA_CONCIERGE=1 PATH="$shim_dir:$SHIM_REAL:$PATH" tmux attach -t foo)"
want "SPIRA_CONCIERGE=1: a non-load-buffer command passes through untouched" \
    "REAL:attach -t foo" "$out_other"

rm -rf "$SHIM_TMP"; trap 'rm -rf "$TMP"' EXIT

want "the launcher composes PATH from the tmux shim dir before exec'ing claude" \
    'export PATH=%q:$PATH' "$(cat "$HARNESS/concierge.sh")"

echo
echo "here — convergence: attach when session exists; start-then-attach otherwise"

# SEEN TO FAIL FIRST: old `here` composed a brief and launched a standalone claude in every
# path. New `here` attaches when the session exists (no brief composed), and calls start
# when it does not. The discriminating fact: compose_brief outputs "N statutes in full" to
# stderr; a path that skips it produces no such line. Uses a real tmux socket (no systemd
# needed for either branch of `here`).
if ! command -v tmux >/dev/null 2>&1; then
    printf '1..0 # SKIP no tmux on PATH — here/attach convergence requires it\n'
    exit 77
fi
HERE_SOCK="test-here-conv-$$"
tmux -L "$HERE_SOCK" kill-server 2>/dev/null || true

# POSITIVE CONTROL: with no session, here calls start → compose_brief → fails (no brief for
# a fixture persona with no chamber here). The failure names the missing brief, confirming
# the code path reaches start.
#
# SPIRA_HOME MUST EXIST, even with no chamber under it: conf.sh derives SPIRA_REPO from
# SPIRA_HOME when `git -C` finds no checkout there, by `cd`-ing to its parent (sp-eekjm/
# sp-ubcgo's `spira-config resolve --sh-all` now hard-refuses with no SPIRA_REPO at all,
# where the bash-only conf.sh used to tolerate it) — and `cd nonexistent/..` fails, same as
# any other missing directory. A SPIRA_HOME that was never mkdir'd broke that derivation
# before compose_brief ever ran, surfacing as "spira_require: command not found" instead of
# this test's own "no brief at" (sp-wm2a3). The sibling fixture below (CONV_EMPTY) already
# gets this right via `mktemp -d`.
mkdir -p "$TMP/empty-chamber"
ln -s "$HERE/conf.d" "$TMP/empty-chamber/conf.d"
# SPIRA_RUN/SPIRA_WIKI are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG):
# declare via tl_config, not the env prefixes below, which no process reads them from any
# more. Same values for both calls, so one declaration covers them.
tl_config SPIRA_RUN="$TMP" SPIRA_WIKI="$TMP/fakebrain"
here_nostart="$(SPIRA_HOME="$TMP/empty-chamber" CONCIERGE_FAYTH=concierge \
    SPIRA_CONF="$TMP/no.conf" \
    CONCIERGE_SOCKET="$HERE_SOCK" CONCIERGE_SESSION="$HERE_SOCK" \
    bash "$HARNESS/concierge.sh" here 2>&1)" || true
want "here calls start when no session (brief-composition error visible)" "no brief at" "$here_nostart"

# THE PROPERTY: when the session exists, here exec-attaches — no brief is composed.
tmux -L "$HERE_SOCK" new-session -d -s "$HERE_SOCK" 2>/dev/null
here_out="$(SPIRA_CONF="$TMP/no.conf" \
    CONCIERGE_SOCKET="$HERE_SOCK" CONCIERGE_SESSION="$HERE_SOCK" \
    bash "$HARNESS/concierge.sh" here 2>&1)" || true
nowant "here does not compose brief when session exists (no second client)" \
    "no brief at" "$here_out"

tmux -L "$HERE_SOCK" kill-server 2>/dev/null || true

echo
echo "start — the launcher's --model comes from persona.<fayth>.model, not FAYTH_MODEL"

# THE PROPERTY UNDER TEST (sp-zs04v.4). concierge.sh's MODEL resolution used to read
# FAYTH_MODEL through fayth_get; it now calls persona_model, which reads
# persona.<fayth>.model out of spira.toml first. A fixture persona whose fayth still
# declares its own (now unused) FAYTH_MODEL proves spira.toml wins over a real
# declaration, not merely over an absent one.
if ! systemctl --user status >/dev/null 2>&1 || ! command -v systemd-run >/dev/null 2>&1; then
    printf '  skip  (no systemd user session — launcher model test requires it)\n'
else
    MT_TMP="$TMP/model-test"; mkdir -p "$MT_TMP/chamber"
    ln -s "$HERE/conf.d" "$MT_TMP/conf.d"
    cp "$HARNESS/spira/chamber/concierge.md" "$MT_TMP/chamber/modeltest.md"
    sed -e 's|^FAYTH_NAME=.*|FAYTH_NAME=modeltest|' \
        -e 's|^FAYTH_MODEL=.*|FAYTH_MODEL=fayth-declared-should-not-be-used|' \
        -e 's|^FAYTH_STATUTE_CORE=.*|FAYTH_STATUTE_CORE="law-rm-alpha"|' \
        "$HARNESS/spira/chamber/concierge.fayth" > "$MT_TMP/chamber/modeltest.fayth"

    MT_TOML="$MT_TMP/spira.toml"
    # SPIRA_RUN/SPIRA_WIKI/SPIRA_REPO_MAP/SPIRA_CHAMBER/SPIRA_MEMORIES_CACHE/
    # SPIRA_CLIENT_SETTINGS are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG).
    # This case's own subject IS config loading (persona.model out of spira.toml), so per
    # that rule it keeps its own file — written here, not via tl_config — layered under the
    # complete fixture below. client_settings must be declared here too: dropping the
    # suite's own override layer (this toml is base:$MT_TOML, not base:$_TL_CONF_OVERRIDE)
    # also drops the top-of-suite SPIRA_CLIENT_SETTINGS override, so without this `start`'s
    # session-hook install falls through to the complete fixture's own literal
    # /fixture/userhome/.claude/settings.json and refuses (permission denied) instead of
    # writing under this case's own $MT_TMP.
    # chamber = "$MT_TMP/chamber", not chamber-empty: spira-config's chamber_dir() (the
    # fayth_get/persona_model path) now prefers the cfg()-resolved SPIRA_CHAMBER over
    # <home>/chamber unconditionally (one source of config) — pointing it at the empty dir
    # made fayth_get read nothing and silently return "" for FAYTH_STATUTE_CORE (every core
    # slug demoted, "no statute rendered in full"). The old worry this dodged —
    # spira_toml_resolve's auto-convert-from-fayth re-seeding this toml from modeltest.fayth's
    # FAYTH_MODEL — is retired along with every other derivation (conf.sh: "the write
    # spira_toml_resolve's auto-convert USED TO EXIST to survive"); nothing reads a fayth to
    # produce a spira.toml any more, so there is nothing left to defeat.
    cat > "$MT_TOML" <<EOF
[persona.modeltest]
model = "concierge-toml-model"

[spira]
run = "$MT_TMP/run"
wiki = "$MT_TMP"
repo_map = "/nonexistent"
chamber = "$MT_TMP/chamber"
memories_cache = ""
client_settings = "$MT_TMP/settings.json"
EOF

    SOCK_MT="test-concierge-model-$$"
    tmux -L "$SOCK_MT" kill-server 2>/dev/null || true
    # SPIRA_CHAMBER points at an EMPTY dir, not $MT_TMP/chamber: spira_toml_resolve's
    # auto-convert-from-fayth watches SPIRA_CHAMBER for a fayth newer than the cached
    # toml and would otherwise re-seed $MT_TOML from modeltest.fayth's own FAYTH_MODEL,
    # defeating the property under test. compose_brief/fayth_get still find
    # modeltest.{md,fayth} through SPIRA_HOME directly, which this does not affect.
    # THE STATUTE BOOK COMES FROM THE SAME FIXTURE SEAM brief_fx uses (SPIRA_MEMORIES_CMD).
    # Without it compose_brief reads the box's real statute database, so this case passed
    # only on a host that had one and refused ("cannot reach the statute book database")
    # everywhere else — including every testenv container.
    # SPIRA_RELEASE and PATH: concierge.sh start refuses without the first (sp-31gtu), and a
    # transient unit gets the user manager's environment, not a launcher's — so it is handed
    # the suite's own release and the PATH the suite's launcher built from it.
    ENVARGS=(SPIRA_RELEASE="$SPIRA_RELEASE" PATH="$PATH" SPIRA_HOME="$MT_TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="${SPIRA_TOML%%:*}:$MT_TOML" CONCIERGE_FAYTH=modeltest \
        CONCIERGE_SOCKET="$SOCK_MT" CONCIERGE_SESSION="$SOCK_MT" \
        SPIRA_MEMORIES_CMD="$(fixture_cmd)")
    env "${ENVARGS[@]}" \
        systemd-run --user --wait --collect --quiet --pipe -- \
        env "${ENVARGS[@]}" \
        bash "$HARNESS/concierge.sh" start >"$MT_TMP/start.err" 2>&1 </dev/null || true
    tmux -L "$SOCK_MT" kill-server 2>/dev/null || true
    if [ -f "$MT_TMP/run/concierge-launch.sh" ]; then
        lnch_mt="$(cat "$MT_TMP/run/concierge-launch.sh")"
        want   "launcher's --model came from persona.modeltest.model" "concierge-toml-model" "$lnch_mt"
        nowant "launcher did not use the fayth's own FAYTH_MODEL" \
            "fayth-declared-should-not-be-used" "$lnch_mt"
    else
        bad "launcher's --model came from persona.modeltest.model" \
            "start did not write launcher: $(tail -5 "$MT_TMP/start.err" 2>/dev/null)"
    fi
fi

echo
echo "the ensure unit leaves the tmux server it starts alive"
svc="$(sed -n '/^\[Service\]/,/^\[/p' "$HARNESS/systemd/concierge.service" | grep -v '^\s*#')"
want "the unit is a oneshot, whose cgroup is reaped when start returns" "Type=oneshot" "$svc"
# THE MECHANISM IS IN THE SCRIPT, not the unit: concierge.sh start wraps tmux new-session
# with systemd-run --remain-after-exit, putting the server in its own transient cgroup that
# outlives the oneshot. This assert confirms the mechanism is visible in the script where
# maintainers look; the behavioural version (does the session actually survive) is host-only
# and lives in test-concierge-acceptance.sh.
want "start escapes the oneshot cgroup via systemd-run --remain-after-exit" \
    "remain-after-exit" "$(cat "$HARNESS/concierge.sh")"

echo
echo "duplicate client detection"

# concierge_live_pid finds a process by the TRANSCRIPT FILE it has open, not by argv
# (sp-epe0m): a bare `claude --resume` typed into a terminal with a picker selection never
# puts the session id in argv at all, so a holder resumed that way was invisible to the old
# argv scan and a second client got launched onto the same session. Identity is now the
# transcript path at $SPIRA_TOKEN_PROJECTS/*/<sid>.jsonl held open on a live fd.
LP_PROJ="$TMP/lp-projects/proj"; mkdir -p "$LP_PROJ"
LP_HOLD_PY="$TMP/hold-open.py"
cat > "$LP_HOLD_PY" <<'PY'
import os, sys, time
time.sleep(float(os.environ.get('LP_HOLD_DELAY', '0')))
f = open(sys.argv[1])
if len(sys.argv) > 3:
    import os
    with open(sys.argv[3], 'w') as pf:
        pf.write(str(os.getpid()))
time.sleep(float(sys.argv[2]))
PY

lp() {  # SPIRA_RUN/SPIRA_WIKI/SPIRA_TOKEN_PROJECTS are registered keys (per Ryan
        # 2026-10-05, ONE SOURCE OF CONFIG): declare via tl_config, not the env prefix,
        # which no process reads them from any more.
        tl_config SPIRA_RUN="$TMP" SPIRA_WIKI="$TMP/fakebrain" SPIRA_TOKEN_PROJECTS="$TMP/lp-projects"
        SPIRA_CONF="$TMP/no.conf" \
        bash "$HARNESS/concierge.sh" _live-pid "$1" 2>/dev/null; }

# POSITIVE CONTROL: an id with no transcript file anywhere must return nothing.
is "no pid for an id with no transcript" "" "$(lp "definitely-no-transcript-$$")"

# THE PROPERTY UNDER TEST: a claude-argv0 process holding the transcript open on an fd is
# found, with no `--resume` anywhere in its argv — the exact shape a bare, picker-resumed
# session has.
LP_SID="live-pid-test-$(date +%s)"
LP_TRANSCRIPT="$LP_PROJ/$LP_SID.jsonl"; : > "$LP_TRANSCRIPT"
bash -c "exec -a claude python3 '$LP_HOLD_PY' '$LP_TRANSCRIPT' 10" &
LP_PID=$!
LP_OPENED=0
for _ in $(seq 50); do
    for l in /proc/$LP_PID/fd/*; do
        [ "$(readlink "$l" 2>/dev/null)" = "$LP_TRANSCRIPT" ] && { LP_OPENED=1; break 2; }
    done
    sleep 0.1
done
trap 'kill "$LP_PID" 2>/dev/null; rm -rf "$TMP"' EXIT

is "fixture holder opened the transcript" 1 "$LP_OPENED"
found="$(lp "$LP_SID")" || found=""
is "live pid is found by its open transcript, with no --resume in argv" "$LP_PID" "$found"

# ARGV ALONE IS NO LONGER ENOUGH. A process that names the id in argv but never opens the
# transcript is not a holder — the shape the old scan wrongly trusted.
LP_MENTION_SID="mention-only-$(date +%s)-$$"
bash -c "exec -a claude python3 -c 'import time; time.sleep(10)' --resume $LP_MENTION_SID" &
LP_MPID=$!; sleep 0.3
is "naming the id in argv without the transcript open is not a holder" "" "$(lp "$LP_MENTION_SID")"
kill "$LP_MPID" 2>/dev/null; wait "$LP_MPID" 2>/dev/null || true

# IDENTITY, NOT A MENTION (2026-09-25): a non-claude process holding the transcript open —
# a `tail -F` of the file — is not a holder either; argv0 still has to be claude.
LP_TAIL_SID="tail-only-$(date +%s)-$$"
LP_TAIL_TRANSCRIPT="$LP_PROJ/$LP_TAIL_SID.jsonl"; : > "$LP_TAIL_TRANSCRIPT"
bash -c "exec -a tail python3 '$LP_HOLD_PY' '$LP_TAIL_TRANSCRIPT' 10" &
LP_TPID=$!; sleep 0.3
is "a non-claude process holding the transcript open is not a holder" "" "$(lp "$LP_TAIL_SID")"
kill "$LP_TPID" 2>/dev/null; wait "$LP_TPID" 2>/dev/null || true

# PROCESS GONE: pid is no longer returned after the process exits.
kill "$LP_PID" 2>/dev/null; wait "$LP_PID" 2>/dev/null || true
is "pid is gone after the process exits" "" "$(lp "$LP_SID")"
trap 'rm -rf "$TMP"' EXIT  # restore trap without the kill

echo
echo "convergence — a live holder with no tty is HEADLESS, not convergence (D10: was two tests)"

# SEEN TO FAIL FIRST: the old behaviour was exit 0 (treating the headless case as
# convergence). The correct behaviour is exit 3 (HEADLESS): has-session already failed, so a
# live holder with no controlling terminal means the tmux server that used to hold it is gone,
# and no second client is spawned. `claude` is stubbed on PATH so "no client was launched" is
# checked directly rather than inferred. The holder proves itself by an open transcript fd, as
# concierge_live_pid now requires, and `setsid` detaches it from any controlling terminal —
# exactly what a claude client actually orphaned by its dead tmux server looks like — so this
# needs neither systemd nor tmux to run.
CONV_PROJ="$(mktemp -d)"
CONV_DIR="$(mktemp -d)"; mkdir -p "$CONV_DIR/bin"
ln -s "$HERE/conf.d" "$CONV_DIR/conf.d"
printf '#!/bin/sh\necho STUB_CLAUDE_RAN\n' > "$CONV_DIR/bin/claude"; chmod +x "$CONV_DIR/bin/claude"
CONV_BRAIN="$(bash -c ". '$HERE/conf.sh' >/dev/null 2>&1; printf %s \"\${SPIRA_WIKI:-\$SPIRA_REPO}\"")"
CONV_SID="conv-headless-$(date +%s)"
printf '%s\n%s\n' "$CONV_SID" "$CONV_BRAIN" > "$CONV_DIR/concierge-session"
CONV_TRANSCRIPT="$CONV_PROJ/$CONV_SID.jsonl"; : > "$CONV_TRANSCRIPT"
CONV_PIDFILE="$CONV_DIR/holder.pid"
setsid bash -c "exec -a claude python3 '$LP_HOLD_PY' '$CONV_TRANSCRIPT' 60 '$CONV_PIDFILE'" \
    </dev/null >/dev/null 2>&1 &
CONV_WRAP=$!
for _i in 1 2 3 4 5 6 7 8 9 10; do [ -s "$CONV_PIDFILE" ] && break; sleep 0.2; done
CONV_PID="$(cat "$CONV_PIDFILE" 2>/dev/null)"
trap 'kill "$CONV_PID" "$CONV_WRAP" 2>/dev/null; rm -rf "$CONV_DIR" "$CONV_PROJ" "$TMP"' EXIT

# POSITIVE CONTROL FOR THE FIXTURE ITSELF: the holder must actually have no controlling
# terminal, or the HEADLESS branch below is not the branch this exercises.
want "the headless fixture holder has no tty (positive control)" "?" "$(ps -o tty= -p "$CONV_PID" 2>/dev/null | tr -d '[:space:]')"

CONV_SOCK="conv-no-second-$$"
# SPIRA_RUN/SPIRA_TOKEN_PROJECTS are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefix below, which no process reads any more.
tl_config SPIRA_RUN="$CONV_DIR" SPIRA_TOKEN_PROJECTS="$CONV_PROJ"
conv_out="$(PATH="$CONV_DIR/bin:$PATH" \
    CONCIERGE_SOCKET="$CONV_SOCK" CONCIERGE_SESSION="$CONV_SOCK" \
    bash "$HARNESS/concierge.sh" start 2>&1)"; conv_rc=$?
kill "$CONV_PID" "$CONV_WRAP" 2>/dev/null; wait "$CONV_WRAP" 2>/dev/null
trap 'rm -rf "$TMP"' EXIT

# POSITIVE CONTROL: if the fixture holder was never found, none of the assertions below
# prove anything.
want "the fixture holder is detected"          "$CONV_SID"       "$conv_out"
if [ "$conv_rc" -ne 0 ]; then ok "a holder with no tmux session does not exit 0"; else bad "a holder with no tmux session does not exit 0" "exited 0"; fi
is   "start exits 3 (HEADLESS)"                              3 "$conv_rc"
want "it names the condition"                                "HEADLESS"        "$conv_out"
nowant "no claude stub was launched"                         "STUB_CLAUDE_RAN" "$conv_out"
nowant "it does not advise an attach that cannot work"       "attach:  tmux"   "$conv_out"

# POSITIVE CONTROL FOR THE GUARD ITSELF: without a live pid the convergence check is skipped
# and start proceeds to compose_brief. Pointed at an empty chamber, compose_brief fails with
# "no brief at..." — proving this is a fallthrough and not a second accidental short-circuit.
CONV_EMPTY="$(mktemp -d)"
ln -s "$HERE/conf.d" "$CONV_EMPTY/conf.d"
# SPIRA_RUN/SPIRA_TOKEN_PROJECTS unchanged from the tl_config declared above.
conv_no_out="$(SPIRA_HOME="$CONV_EMPTY" \
    CONCIERGE_SOCKET="$CONV_SOCK" CONCIERGE_SESSION="$CONV_SOCK" \
    bash "$HARNESS/concierge.sh" start 2>&1)"; conv_no_rc=$?
rm -rf "$CONV_EMPTY"
is   "start does not short-circuit without a live pid (proceeds to compose_brief)" 1 "$conv_no_rc"
want "and the failure is compose_brief's, not the convergence guard's" "no brief at" "$conv_no_out"

echo
echo "convergence — a live holder WITH a tty is LIVE ELSEWHERE, and is never killed (sp-epe0m)"

# THE BUG THIS BEAD FIXES. A bare `claude --resume` typed into an SSH terminal with a picker
# selection has the exact same signature the HEADLESS case above has — no tmux session on
# this socket — but it is a person mid-conversation, not an orphan. Treating it as HEADLESS
# would have `start` (and cockpit-ensure, which runs it with CONCIERGE_RECOVER_HEADLESS=1)
# kill a live session out from under them. A real controlling terminal is what tells the two
# apart: python's pty module allocates one deterministically regardless of whether this test
# itself is run under a tty, so the fixture does not depend on how the suite is invoked.
if python3 -c 'import os,pty
c,_m=pty.fork()
if c==0: os._exit(0)
os.waitpid(c,0)' 2>/dev/null; then
LIVE_PROJ="$(mktemp -d)"
LIVE_DIR="$(mktemp -d)"; mkdir -p "$LIVE_DIR/bin"
ln -s "$HERE/conf.d" "$LIVE_DIR/conf.d"
printf '#!/bin/sh\necho STUB_CLAUDE_RAN\n' > "$LIVE_DIR/bin/claude"; chmod +x "$LIVE_DIR/bin/claude"
LIVE_SID="conv-live-elsewhere-$(date +%s)"
printf '%s\n%s\n' "$LIVE_SID" "$CONV_BRAIN" > "$LIVE_DIR/concierge-session"
LIVE_TRANSCRIPT="$LIVE_PROJ/$LIVE_SID.jsonl"; : > "$LIVE_TRANSCRIPT"
LIVE_HOLD_PY="$TMP/hold-open-tty.py"
cat > "$LIVE_HOLD_PY" <<'PY'
import os, pty, sys, time
transcript, secs, pidfile = sys.argv[1], float(sys.argv[2]), sys.argv[3]
child, _master = pty.fork()
if child == 0:
    with open(pidfile, 'w') as pf:
        pf.write(str(os.getpid()))
    f = open(transcript)
    time.sleep(secs)
    os._exit(0)
os.waitpid(child, 0)
PY
LIVE_PIDFILE="$LIVE_DIR/holder.pid"
bash -c "exec -a claude python3 '$LIVE_HOLD_PY' '$LIVE_TRANSCRIPT' 30 '$LIVE_PIDFILE'" &
LIVE_WRAP=$!
for _i in 1 2 3 4 5 6 7 8 9 10; do [ -s "$LIVE_PIDFILE" ] && break; sleep 0.2; done
LIVE_PID="$(cat "$LIVE_PIDFILE" 2>/dev/null)"
trap 'kill "$LIVE_PID" "$LIVE_WRAP" 2>/dev/null; rm -rf "$LIVE_DIR" "$LIVE_PROJ" "$TMP"' EXIT

# POSITIVE CONTROL FOR THE FIXTURE ITSELF: the holder must actually have a real tty, or the
# LIVE ELSEWHERE branch below is not the branch this exercises.
_live_tty_check="$(ps -o tty= -p "$LIVE_PID" 2>/dev/null | tr -d '[:space:]')"
if [ -n "$_live_tty_check" ] && [ "$_live_tty_check" != '?' ]; then
    ok "the live-elsewhere fixture holder has a real tty (positive control)"
else
    bad "the live-elsewhere fixture holder has a real tty (positive control)" "tty=[$_live_tty_check]"
fi

LIVE_SOCK="live-no-second-$$"
# CONCIERGE_RECOVER_HEADLESS=1 IS THE EXACT ENVIRONMENT cockpit-ensure RUNS start IN. Proving
# the holder survives even with recovery enabled is the point: this is the flag that turns the
# HEADLESS branch's "kill and restart" from advice into an action, and a live holder must never
# reach that action.
# SPIRA_RUN/SPIRA_TOKEN_PROJECTS are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefix below, which no process reads any more.
tl_config SPIRA_RUN="$LIVE_DIR" SPIRA_TOKEN_PROJECTS="$LIVE_PROJ"
live_out="$(PATH="$LIVE_DIR/bin:$PATH" \
    CONCIERGE_SOCKET="$LIVE_SOCK" CONCIERGE_SESSION="$LIVE_SOCK" CONCIERGE_RECOVER_HEADLESS=1 \
    bash "$HARNESS/concierge.sh" start 2>&1)"; live_rc=$?

want   "the fixture holder is detected"                    "$LIVE_SID"       "$live_out"
is     "start exits 4 (refusing, like the stray-holder case)" 4 "$live_rc"
want   "it says the session is live elsewhere"              "live elsewhere" "$live_out"
nowant "it does not call the live holder HEADLESS"           "HEADLESS"      "$live_out"
nowant "no claude stub was launched"                         "STUB_CLAUDE_RAN" "$live_out"
if kill -0 "$LIVE_PID" 2>/dev/null; then
    ok "the live holder was not killed, even with recovery enabled"
else
    bad "the live holder was not killed, even with recovery enabled" "pid $LIVE_PID is gone"
fi
kill "$LIVE_PID" "$LIVE_WRAP" 2>/dev/null; wait "$LIVE_WRAP" 2>/dev/null
trap 'rm -rf "$TMP"' EXIT
else
    # A runner with no pseudo-terminal cannot build the fixture: a declared, counted skip.
    _tl_init; _tl_counts_load; _TL_SKIP=$((_TL_SKIP + 1)); _tl_counts_flush
    printf 'skip - live-elsewhere section: no pseudo-terminal available on this runner\n'
fi

echo
echo "singleton — a bare, hand-started holder is detected by name, not by resume id (sp-rig42)"

# THE GAP concierge_live_pid DOESN'T COVER. That check finds a process holding the RECORDED
# resume id. A bare `claude --remote-control <name>` typed into a dead cockpit pane holds no
# resume id at all — it was never resumed — so it is invisible to that check even though it
# answers to the same Remote Control name and the same phone session. concierge_stray_holders
# scans for the NAME instead, via the internal _stray-holders subcommand.
SH_SESS="stray-test-$$"
SH_TMP="$TMP/stray"; mkdir -p "$SH_TMP"
ln -s "$HERE/conf.d" "$SH_TMP/conf.d"
SH_FAKE="$SH_TMP/fakeclaude"
printf '#!/bin/sh\nsleep 30\n' > "$SH_FAKE"; chmod +x "$SH_FAKE"

# SPIRA_RUN/SPIRA_WIKI are registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG):
# declare via tl_config, not the env prefixes below (sh_holders and out_start share this
# value), which no process reads them from any more.
tl_config SPIRA_RUN="$SH_TMP" SPIRA_WIKI="$SH_TMP"
sh_holders() { SPIRA_CONF="$TMP/no.conf" \
    CONCIERGE_SOCKET="$SH_SESS" CONCIERGE_SESSION="$SH_SESS" \
    bash "$HARNESS/concierge.sh" _stray-holders 2>/dev/null; }

# POSITIVE CONTROL: before any process registers under this made-up session name, none found.
is "no stray holders before one exists" "" "$(sh_holders)"

bash -c "exec -a claude-strayx '$SH_FAKE' --remote-control '$SH_SESS'" &
SH_PID=$!
trap 'kill "$SH_PID" 2>/dev/null; rm -rf "$TMP"' EXIT
sleep 0.3

is "the bare holder is found by name, with no resume id involved" "$SH_PID" "$(sh_holders)"

# start REFUSES rather than launching a third session on top of the confusion, and it does
# so before compose_brief — the statute book need not be present for this check to fire.
out_start="$(SPIRA_CONF="$TMP/no.conf" \
    CONCIERGE_SOCKET="$SH_SESS" CONCIERGE_SESSION="$SH_SESS" \
    bash "$HARNESS/concierge.sh" start 2>&1)"; rc_start=$?
is   "start refuses (exit 4) when a stray holder is live" 4 "$rc_start"
want "and names the holding pid"                           "$SH_PID" "$out_start"

kill "$SH_PID" 2>/dev/null; wait "$SH_PID" 2>/dev/null || true
trap 'rm -rf "$TMP"' EXIT
is "no stray holders once the process exits" "" "$(sh_holders)"

echo
echo "wake — refuses rather than typing into a dead pane (sp-rig42)"

# has-session proves the tmux session exists; it says nothing about whether the process
# inside the pane is still alive. remain-on-exit keeps a dead pane around instead of tmux
# tearing the whole session down with it, which is what lets this be tested directly.
WK_SOCK="test-wake-dead-$$"
tmux -L "$WK_SOCK" kill-server 2>/dev/null || true
tmux -L "$WK_SOCK" new-session -d -s "$WK_SOCK" -x 80 -y 24
tmux -L "$WK_SOCK" set-option -t "$WK_SOCK" remain-on-exit on
tmux -L "$WK_SOCK" send-keys -t "$WK_SOCK" -l -- "exit" && tmux -L "$WK_SOCK" send-keys -t "$WK_SOCK" Enter
sleep 0.5

out_wake="$(CONCIERGE_SOCKET="$WK_SOCK" CONCIERGE_SESSION="$WK_SOCK" \
    bash "$HARNESS/concierge.sh" wake "hi" 2>&1)"; rc_wake=$?
is   "wake refuses on a dead pane"  1                      "$rc_wake"
want "and says why"                 "pane process has exited" "$out_wake"
tmux -L "$WK_SOCK" kill-server 2>/dev/null || true

# POSITIVE CONTROL: against a live pane, wake still succeeds — the refusal fires on
# deadness, not on every call.
WK_LIVE="test-wake-live-$$"
tmux -L "$WK_LIVE" kill-server 2>/dev/null || true
tmux -L "$WK_LIVE" new-session -d -s "$WK_LIVE" "sleep 30"
rc_wake_live=0
CONCIERGE_SOCKET="$WK_LIVE" CONCIERGE_SESSION="$WK_LIVE" \
    bash "$HARNESS/concierge.sh" wake "hi" >/dev/null 2>&1 || rc_wake_live=$?
is "wake succeeds against a live pane (positive control)" 0 "$rc_wake_live"
tmux -L "$WK_LIVE" kill-server 2>/dev/null || true

echo
echo "wake — never types into a non-empty input line"

# A PANE WITH THE CLIENT'S OWN INPUT-LINE MARKER, `❯ `, HOLDING TEXT. bash --norc prints
# the marker itself (no real Claude Code needed) and then blocks on `read`, so send-keys
# after it echoes onto the same screen line exactly as a person mid-keystroke would leave
# it — the shape `_wake_input_busy` in concierge.sh looks for.
WK_HOLD="test-wake-hold-$$"
tmux -L "$WK_HOLD" kill-server 2>/dev/null || true
tmux -L "$WK_HOLD" new-session -d -s "$WK_HOLD" -x 80 -y 24 \
    "bash --norc -c 'printf \"❯ \"; read -r _line'"
# remain-on-exit: the fixture shell's `read` returns and the pane's process exits the
# instant the wake delivers text + Enter — without this the session (and its screen
# content) would vanish before the final assertion below can capture it.
tmux -L "$WK_HOLD" set-option -t "$WK_HOLD" remain-on-exit on
sleep 0.3
tmux -L "$WK_HOLD" send-keys -t "$WK_HOLD" -l -- "half-written"
sleep 0.3
want "the fixture pane shows the half-written input line (positive control)" \
    "❯ half-written" "$(tmux -L "$WK_HOLD" capture-pane -p -t "$WK_HOLD")"

WK_HOLD_RUN="$TMP/wake-hold-run"; mkdir -p "$WK_HOLD_RUN"
# SPIRA_RUN is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare via
# tl_config, not the env prefix below, which no process reads it from any more.
tl_config SPIRA_RUN="$WK_HOLD_RUN"
(
    CONCIERGE_WAKE_WARN_SECS=0 CONCIERGE_SOCKET="$WK_HOLD" CONCIERGE_SESSION="$WK_HOLD" \
        bash "$HARNESS/concierge.sh" wake "the woken text" >"$TMP/wake-hold.out" 2>&1
) &
WK_WAKE_PID=$!

# WHILE THE LINE STAYS BUSY, THE WAKE MUST NOT HAVE DELIVERED ANYTHING.
sleep 1.5
want "still busy: the pane still shows only the half-written text" \
    "❯ half-written" "$(tmux -L "$WK_HOLD" capture-pane -p -t "$WK_HOLD")"
nowant "still busy: the wake has not delivered its text yet" \
    "the woken text" "$(tmux -L "$WK_HOLD" capture-pane -p -t "$WK_HOLD")"
want "a held wake says so, so a stale input line cannot stall delivery silently" \
    "wake held" "$(cat "$TMP/wake-hold.out")"
is "the wake call has not returned while the line is busy" 1 \
    "$(kill -0 "$WK_WAKE_PID" 2>/dev/null && echo 1 || echo 0)"

# EMPTY THE LINE, NEVER SUBMITTING IT — the wake is what may act, not a person's Enter.
# "half-written" is 12 characters; one extra BSpace is a harmless no-op on an empty line.
tmux -L "$WK_HOLD" send-keys -t "$WK_HOLD" BSpace BSpace BSpace BSpace BSpace \
    BSpace BSpace BSpace BSpace BSpace BSpace BSpace BSpace
sleep 0.3
nowant "the input line is now empty" "half-written" "$(tmux -L "$WK_HOLD" capture-pane -p -t "$WK_HOLD")"

# THE WAKE DELIVERS ONCE THE LINE IS EMPTY, AND RETURNS. Its own loop polls every 2s with
# a 1s settle, so this allows a full margin over that cadence.
for _i in $(seq 1 20); do
    kill -0 "$WK_WAKE_PID" 2>/dev/null || break
    sleep 0.5
done
is "the wake call returns once the line clears" 0 \
    "$(kill -0 "$WK_WAKE_PID" 2>/dev/null && echo 1 || echo 0)"
tmux -L "$WK_HOLD" kill-server 2>/dev/null || true

echo
echo "wake — delivery is proved only after the line empties, with no wall-clock sleep (sp-a9uuu)"

# THE GAP round 102 LEFT: the pane-capture assertion that proved the woken text landed
# passed once, then flipped at load average ~24 against the same code, and was deleted
# (law-a-test-that-flips-is-deleted). "the wake call returns" above proves only that the
# process exited, not that it typed anything — and a faster wall-clock sleep would flip the
# same way. This replaces it with a lock-step: concierge.sh's wake adopts
# _wake_input_busy/_wake_deliver from the environment when they are already functions
# (declare -F guards each), so a write to the control FIFO cannot return until wake's own
# loop is on the other end asking for the next state. "Still busy" and "delivered" become
# facts about ordering, not about how long something waited.
DET_SOCK="test-wake-det-$$"
tmux -L "$DET_SOCK" kill-server 2>/dev/null || true
tmux -L "$DET_SOCK" new-session -d -s "$DET_SOCK" "sleep 30"

DET_CTL="$TMP/wake-det-ctl"; mkfifo "$DET_CTL"
DET_LOG="$TMP/wake-det-deliver.log"; : > "$DET_LOG"
DET_RUN="$TMP/wake-det-run"; mkdir -p "$DET_RUN"
# EXPORTED, NOT JUST THE FUNCTIONS: concierge.sh runs under set -u, so a hook that closes
# over an unexported variable dies the instant it is called there, in the child process,
# with "DET_CTL: unbound variable" — the paths, not only the functions that read them, have
# to cross into that environment.
export DET_CTL DET_LOG

# THE HOOK, adopted by concierge.sh's own declare -F guard: busy is "the test has not yet
# said empty", read one state per call so each call blocks until the test feeds it.
_wake_input_busy() {
    local state
    read -r state < "$DET_CTL" || return 1
    [ "$state" = "busy" ]
}
# THE OBSERVATION: what would have been a real tmux send-keys is recorded instead, so
# delivery is read from a file rather than inferred from a pane capture.
_wake_deliver() { printf '%s\n' "$1" >> "$DET_LOG"; }
export -f _wake_input_busy _wake_deliver

# SPIRA_RUN is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare via
# tl_config, not the env prefix below, which no process reads it from any more.
tl_config SPIRA_RUN="$DET_RUN"
(
    CONCIERGE_WAKE_POLL_SECS=0 CONCIERGE_WAKE_SETTLE_SECS=0 \
        CONCIERGE_SOCKET="$DET_SOCK" CONCIERGE_SESSION="$DET_SOCK" \
        timeout 30 bash "$HARNESS/concierge.sh" wake "the woken text" >"$TMP/wake-det.out" 2>&1 # batch-job: outer bound on a wake fed by a fake state feeder; a short one turns load into a false red
) &
DET_PID=$!
trap 'kill "$DET_PID" 2>/dev/null; tmux -L "$DET_SOCK" kill-server 2>/dev/null; rm -rf "$TMP"' EXIT

# feed <state> -> hand wake's hook its next state, or fail fast (never hang the suite) if
# nothing is on the other end asking for one — the 30s outer timeout above is what turns a
# wake that delivered early into a bounded, readable failure here instead of the pane-capture
# assertion's old failure mode: a flip discovered only much later, under load.
det_fed=1
feed() {
    if ! timeout 5 bash -c 'printf "%s\n" "$1" > "$2"' _ "$1" "$DET_CTL"; then
        bad "wake asked for the next poll state ('$1')" "timed out — it must have returned already"
        det_fed=0
    fi
}

# TWO BUSY POLLS. Each write blocks until wake's loop has opened the FIFO asking for the
# next state — proof, by the write returning at all, that the loop ran and is asking again
# rather than having fallen through to delivery.
feed busy
feed busy

# THE PROPERTY: at this exact point wake cannot have progressed past the busy loop — it is
# blocked reading the FIFO for a third state — so this is not a race against a moving
# target, it is a fact about where the process is stuck.
is "still busy: nothing has been delivered yet"      "" "$(cat "$DET_LOG")"
is "still busy: the wake call has not returned"      1  "$(kill -0 "$DET_PID" 2>/dev/null && echo 1 || echo 0)"

# EMPTY THE LINE — fed twice: once for the first busy loop's exit check, once more for the
# settle re-check's own call to the same hook.
feed empty
feed empty

if [ "$det_fed" -eq 1 ]; then
    wait "$DET_PID"; det_rc=$?
    is   "the wake call exits 0 once the line is empty" 0 "$det_rc"
    want "and the delivered text is exactly what was recorded, not inferred from a pane" \
        "the woken text" "$(cat "$DET_LOG")"
else
    kill "$DET_PID" 2>/dev/null; wait "$DET_PID" 2>/dev/null
    bad "the wake call exits 0 once the line is empty" "skipped — a feed above timed out"
fi
trap 'tmux -L "$DET_SOCK" kill-server 2>/dev/null; rm -rf "$TMP"' EXIT
tmux -L "$DET_SOCK" kill-server 2>/dev/null || true
unset -f _wake_input_busy _wake_deliver

echo
echo "the way in — /proc scan hygiene and the dangling-resume retry, as source shape"

_src="$(cat "$HARNESS/concierge.sh")"
want "/proc read is guarded before it is attempted"            '[ -r "$f" ] || continue' "$_src"
want "redirect is grouped so the shell's own error is covered" '{ tr' "$_src"

want  "a resume with no transcript left retries fresh"       "starting fresh"      "$_src"
# The launcher is one line; grep -v deletes it entirely. Verify regeneration is used instead.
nowant "the retry regenerates rather than filtering the launcher" 'LAUNCHER.noresume' "$_src"

echo
echo "sp-aaew9: an intact transcript is never discarded for a resume that merely exited"

# THE DEFECT THIS FIXES, as source shape (the live behaviour is exercised in
# test-concierge-acceptance.sh, which needs a real tmux/systemd session to run the launcher).
# The old code deleted the session file unconditionally the moment --resume failed to stay
# up for 3s; a fast exit for ANY reason — another holder, an eviction — was indistinguishable
# from "the id cannot be resumed" and threw the real conversation away.
nowant "no unconditional rm -f of the session file on a resume failure" \
    'rm -f "$SPIRA_RUN/concierge-session"' "$_src"
want "the fresh-vs-refuse fork checks whether the transcript itself still exists" \
    "concierge_transcript_path" "$_src"
want "an intact transcript refuses rather than retrying" \
    "refusing to discard it" "$_src"
want "the refusal names any process holding the transcript open" \
    "concierge_fd_holders" "$_src"
want "and captures the dead pane's last output" \
    "capture-pane" "$_src"

tl_summary
