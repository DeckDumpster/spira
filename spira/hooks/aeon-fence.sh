#!/usr/bin/env bash
# aeon-fence.sh — PreToolUse hook: refuse queue-operating commands in aeon sessions.
#
# Registered by aeon.sh via --settings so it is bound to the aeon, not to a global
# profile (law-guard-binds-the-caller). Only fires when SPIRA_AEON is set.
#
# OVERRIDE (Ops incidents only): the operator sets SPIRA_AEON_OVERRIDE=1 before
# starting the session; an aeon cannot set it from inside a running session.
#
# EXIT: 0 always. Block by printing {"decision":"block","reason":"..."} to stdout.
set -uo pipefail
[ -n "${SPIRA_AEON:-}" ] || exit 0

[ -z "${SPIRA_AEON_OVERRIDE:-}" ] || {
    printf 'aeon-fence: SPIRA_AEON_OVERRIDE set — bypassing fence (aeon=%s)\n' "$SPIRA_AEON" >&2
    exit 0
}

payload="$(cat 2>/dev/null || true)"

tool="$(printf '%s' "$payload" | python3 -c '
import json, sys
try: d = json.load(sys.stdin); print(d.get("tool_name",""))
except Exception: print("")' 2>/dev/null)"

# Write/Edit carry no shell command to parse, but they can still land bytes in
# $SPIRA_PROD the same as a Bash redirect (sp-qsr44 gap 6: this guard used to
# inspect Bash only, so Write/Edit into prod passed through unrefused).
if [ "$tool" = "Write" ] || [ "$tool" = "Edit" ]; then
    file_path="$(printf '%s' "$payload" | python3 -c '
import json, sys
try: d = json.load(sys.stdin); print(d.get("tool_input",{}).get("file_path",""))
except Exception: print("")' 2>/dev/null)"
    tw_reason=""
    if [ -n "$file_path" ] && [ -n "${SPIRA_PROD:-}" ]; then
        case "$file_path" in
            "${SPIRA_PROD}"|"${SPIRA_PROD}"/*)
                tw_reason="aeons may not write to the production checkout \$SPIRA_PROD via $tool (sp-kz8ob: use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
        esac
    fi
    # The operator's own config (repo-map, spira.toml, ...) is not this aeon's ref either
    # (law-an-aeon-touches-only-its-own-ref): sp-iks0y, an aeon Edit landed an unlanded
    # clause in the operator's live repo-map and broke every gate reading it.
    if [ -z "$tw_reason" ] && [ -n "$file_path" ]; then
        _config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/spira"
        case "$file_path" in
            "${_config_dir}"|"${_config_dir}"/*)
                tw_reason="aeons may not write to the operator's config \$HOME/.config/spira via $tool (sp-iks0y: a config change belongs in the bead's notes as a deploy step; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
        esac
    fi
    if [ -n "$tw_reason" ]; then
        printf 'aeon-fence: BLOCKED aeon=%s bead=%s: %s\n' \
            "${SPIRA_AEON:-?}" "${BEAD_ID:-?}" "$tw_reason" >&2
        printf '{"decision":"block","reason":"%s"}\n' \
            "$(printf '%s' "$tw_reason" | sed 's/"/\\"/g')"
    fi
    exit 0
fi

[ "$tool" = "Bash" ] || exit 0

# STRUCTURAL, NOT CHARGED-AFTER-THE-FACT (sp-4o925). A headless session has no notification
# channel: backgrounding a command and ending the turn to "wait for" it orphans the process
# when the session exits, and aeon.sh's yield-headless disposition only charges an attempt
# for a mistake already made. Refusing the parameter removes the mechanism instead.
rib="$(printf '%s' "$payload" | python3 -c '
import json, sys
try: d = json.load(sys.stdin); print("1" if d.get("tool_input",{}).get("run_in_background") else "")
except Exception: print("")' 2>/dev/null)"
if [ "$rib" = "1" ]; then
    reason="aeon sessions run headless and cannot be woken by a background task notification (sp-4o925: run_in_background is refused) — run this command in the foreground instead"
    printf 'aeon-fence: BLOCKED aeon=%s bead=%s: %s\n' \
        "${SPIRA_AEON:-?}" "${BEAD_ID:-?}" "$reason" >&2
    printf '{"decision":"block","reason":"%s"}\n' \
        "$(printf '%s' "$reason" | sed 's/"/\\"/g')"
    exit 0
fi

cmd="$(printf '%s' "$payload" | python3 -c '
import json, sys
try: d = json.load(sys.stdin); print(d.get("tool_input",{}).get("command",""))
except Exception: print("")' 2>/dev/null)"

[ -n "$cmd" ] || exit 0

# Returns "1" if $1 is executed in $2 — by bare name (the release's tools are invoked by
# name on PATH, sp-gypjk) or by a path ending /$1, directly or through bash/sh/source; ""
# if it only appears as an argument (a git file operation, a cat). Splits compound commands
# on shell separators and newlines; a heredoc body is inert unless a shell reads it. An optional $3 names the one verb that stays allowed (queue stats).
# An optional $4 (comma-separated) inverts that to a deny-list: only a subcommand named in
# it counts as a hit, so e.g. "release build"/"release status" pass while "release
# activate" does not (used for the release crate's own mutating subcommands, sp-jsnbm).
_exec_ctx() {
    python3 -c '
import sys, re
script = sys.argv[1]; cmd = sys.argv[2]
allow = sys.argv[3] if len(sys.argv) > 3 else ""
deny = sys.argv[4].split(",") if len(sys.argv) > 4 and sys.argv[4] else None
EXEC = {"bash", "sh", "ksh", "zsh", "dash", "source", ".", "exec", "env", "timeout"}
def is_script(tok):
    return tok == script or tok.endswith("/" + script)
HEREDOC = re.compile(r"<<-?\s*[\"\x27]?([A-Za-z_][A-Za-z_0-9]*)")
def strip_heredocs(text):
    out = []; end = None; keep = False
    for line in text.split("\n"):
        if end is not None:
            if line.strip() == end: end = None
            elif keep: out.append(line)
            continue
        out.append(line)
        m = HEREDOC.search(line)
        if m:
            end = m.group(1)
            keep = re.search(r"\b(bash|sh|zsh|dash|ksh)\b", line[:m.start()]) is not None
    return "\n".join(out)
def hit(text, depth=0):
    for sub in re.split(r"&&|\|\||;|\||\n", strip_heredocs(text)):
        toks = sub.replace("\"", " ").replace("'"'"'", " ").split()
        while toks and "=" in toks[0] and not toks[0].startswith("="):
            toks = toks[1:]
        while toks and toks[0] in EXEC:
            toks = toks[1:]
            if toks and toks[0] == "-c" and depth < 3:
                if hit(" ".join(toks[1:]), depth + 1): return True
                toks = []
                break
            while toks and (toks[0].startswith("-") or toks[0].isdigit() or "=" in toks[0]):
                toks = toks[1:]
        if toks and is_script(toks[0]):
            if allow and len(toks) > 1 and toks[1] == allow:
                continue
            if deny is not None:
                if len(toks) > 1 and toks[1] in deny:
                    return True
                continue
            return True
    return False
if hit(cmd): print("1")
' "$@" 2>/dev/null
}

reason=""

for _script in landing.sh slay.sh world.sh deploy.sh promote.sh; do
    case "$cmd" in
        *"$_script"*)
            [ "$(_exec_ctx "$_script" "$cmd")" = "1" ] && { reason="aeons may not call $_script (sp-kz8ob: landing and batch handle forge writes; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)"; break; } ;;
    esac
done

# release's mutating subcommands (activate, install-tarball, rollback — sp-jsnbm: the
# release crate's DESIGN.md) replace activate.sh's own fence entry: deploy.sh and
# land-local are the only callers. build/verify/prune/status/stage/canary stay allowed —
# an aeon may legitimately run those against its own worktree.
if [ -z "$reason" ]; then
    case "$cmd" in
        *release*)
            [ "$(_exec_ctx release "$cmd" "" "activate,install-tarball,rollback")" = "1" ] && \
                reason="aeons may not call release activate/install-tarball/rollback (sp-kz8ob: landing and batch handle forge writes; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
    esac
fi

if [ -z "$reason" ]; then
    # queue.sh is the queue binary now (queue/DESIGN.md §7.2 row 18), invoked by name:
    # match either, bare or by path; `stats` stays the one allowed verb.
    case "$cmd" in
        *queue*)
            { [ "$(_exec_ctx "queue.sh" "$cmd" stats)" = "1" ] || [ "$(_exec_ctx "queue" "$cmd" stats)" = "1" ]; } && \
                reason="aeons may not operate the queue (sp-kz8ob: queue stats is the only read-only subcommand; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
    esac
fi

if [ -z "$reason" ]; then
    case "$cmd" in
        *"gh pr create"*|*"gh pr edit"*|*"gh pr merge"*|*"gh pr close"*|\
        *"gh release create"*|*"gh release edit"*|*"gh release delete"*|\
        *"gh workflow run"*|\
        *"gh run rerun"*|*"gh run cancel"*)
            reason="aeons carry no forge credentials (sp-kz8ob: forge writes go through landing.sh and the batcher)" ;;
    esac
fi

if [ -z "$reason" ]; then
    case "$cmd" in
        *"git push"*|*"git -C"*" push "*)
            reason="aeons carry no push credentials (sp-kz8ob: landing.sh and the batcher handle all merges and pushes)" ;;
    esac
fi

if [ -z "$reason" ] && [ -n "${SPIRA_RUN:-}" ]; then
    # Refuse write shapes only, mirroring the \$SPIRA_PROD rule below: reads, script
    # execution and mentions of the path in prose (a heredoc body, a quoted string) are
    # permitted (sp-ozym9; law-a-matcher-reads-code-not-prose). The retired landing
    # ledger is no longer fenced: the lifecycle cutover deleted it (sp-2c1n0, sp-j7l3q).
    case "$cmd" in
        *">${SPIRA_RUN}/queue"*|*"> ${SPIRA_RUN}/queue"*|\
        *">>${SPIRA_RUN}/queue"*|*">> ${SPIRA_RUN}/queue"*)
            reason="aeons may not write to \$SPIRA_RUN/queue (sp-kz8ob: use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
    esac
    if [ -z "$reason" ]; then
        _run_write="$(printf '%s' "$cmd" | python3 -c '
import sys, re
cmd = sys.stdin.read()
guarded = sys.argv[1:]
WRITE = {"rm", "mv", "cp", "truncate", "tee", "install", "ln", "chmod", "chown", "mkdir"}
for seg in re.split(r"&&|\|\||;|\||\n", cmd):
    seg = seg.strip()
    if not seg or not any(g in seg for g in guarded):
        continue
    rem = seg
    while True:
        m = re.match(r"^[A-Za-z_][A-Za-z0-9_]*=[^\s]*\s+", rem)
        if not m:
            break
        rem = rem[m.end():]
    toks = rem.split()
    if not toks:
        continue
    t0 = toks[0].split("/")[-1]
    if t0 in WRITE:
        print("1"); sys.exit(0)
    if t0 == "sed":
        for t in toks[1:]:
            if t == "-i" or (t.startswith("-") and not t.startswith("--") and "i" in t[1:]):
                print("1"); sys.exit(0)
' "${SPIRA_RUN}/queue" 2>/dev/null)"
        [ "$_run_write" = "1" ] && reason="aeons may not write to \$SPIRA_RUN/queue (sp-kz8ob: use SPIRA_AEON_OVERRIDE=1 for Ops incidents)"
    fi
fi

# The operator's config lives outside every worktree, so it is never this aeon's ref to
# write (law-an-aeon-touches-only-its-own-ref): sp-iks0y, an Edit tool call landed an
# unlanded clause in the operator's live repo-map and broke the in-force gate.
if [ -z "$reason" ]; then
    _config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/spira"
    case "$cmd" in
        *">${_config_dir}"*|*"> ${_config_dir}"*|\
        *">>${_config_dir}"*|*">> ${_config_dir}"*)
            reason="aeons may not write to the operator's config \$HOME/.config/spira (sp-iks0y: a config change belongs in the bead's notes as a deploy step; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
    esac
    if [ -z "$reason" ]; then
        _config_write="$(printf '%s' "$cmd" | python3 -c '
import sys, re
cmd = sys.stdin.read()
guarded = sys.argv[1]
WRITE = {"rm", "mv", "cp", "truncate", "tee", "install", "ln", "chmod", "chown", "mkdir"}
for seg in re.split(r"&&|\|\||;|\||\n", cmd):
    seg = seg.strip()
    if not seg or guarded not in seg:
        continue
    rem = seg
    while True:
        m = re.match(r"^[A-Za-z_][A-Za-z0-9_]*=[^\s]*\s+", rem)
        if not m:
            break
        rem = rem[m.end():]
    toks = rem.split()
    if not toks:
        continue
    t0 = toks[0].split("/")[-1]
    if t0 in WRITE:
        print("1"); sys.exit(0)
    if t0 == "sed":
        for t in toks[1:]:
            if t == "-i" or (t.startswith("-") and not t.startswith("--") and "i" in t[1:]):
                print("1"); sys.exit(0)
' "$_config_dir" 2>/dev/null)"
        [ "$_config_write" = "1" ] && reason="aeons may not write to the operator's config \$HOME/.config/spira (sp-iks0y: a config change belongs in the bead's notes as a deploy step; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)"
    fi
fi

if [ -z "$reason" ] && [ -n "${SPIRA_PROD:-}" ]; then
    # Refuse write shapes only; reads and script execution from prod are permitted.
    # Redirect syntax is matched positionally. Command names are checked only as the
    # first word of each pipeline segment so prose containing "rm " embedded in other
    # words (e.g. "arm in") cannot trip the guard (law-a-matcher-reads-code-not-prose).
    case "$cmd" in
        *">${SPIRA_PROD}"*|*"> ${SPIRA_PROD}"*|\
        *">>${SPIRA_PROD}"*|*">> ${SPIRA_PROD}"*)
            reason="aeons may not write to the production checkout \$SPIRA_PROD (sp-kz8ob: use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
    esac
    if [ -z "$reason" ]; then
        _prod_write="$(printf '%s' "$cmd" | python3 -c '
import sys, re
cmd = sys.stdin.read()
prod = sys.argv[1]
GIT_WRITE = {"commit", "reset", "checkout", "clean", "add"}
WRITE = {"rm", "mv", "tee"}
for seg in re.split(r"&&|\|\||;|\||\n", cmd):
    seg = seg.strip()
    if not seg or prod not in seg:
        continue
    rem = seg
    while True:
        m = re.match(r"^[A-Za-z_][A-Za-z0-9_]*=[^\s]*\s+", rem)
        if not m:
            break
        rem = rem[m.end():]
    toks = rem.split()
    if not toks:
        continue
    t0 = toks[0].split("/")[-1]
    if t0 in WRITE:
        print("1"); sys.exit(0)
    if t0 == "sed":
        for t in toks[1:]:
            if t == "-i" or (t.startswith("-") and not t.startswith("--") and "i" in t[1:]):
                print("1"); sys.exit(0)
        continue
    if t0 == "git":
        i = 1
        while i < len(toks):
            if toks[i] == "-C" and i + 1 < len(toks) and prod in toks[i+1]:
                if any(s in GIT_WRITE for s in toks[i+2:]):
                    print("1"); sys.exit(0)
                break
            i += 1
' "${SPIRA_PROD}" 2>/dev/null)"
        [ "$_prod_write" = "1" ] && reason="aeons may not write to the production checkout \$SPIRA_PROD (sp-kz8ob: use SPIRA_AEON_OVERRIDE=1 for Ops incidents)"
    fi
fi

# No bd rule (sp-j7l3q; Ryan 2026-10-05, superseding sp-nkr42 here): no aeon persona has bd.
# The model's PATH holds only `work` (sp-zf4q3) and every persona's bead operations are
# `work` broker verbs (sp-st0mm), so the old `bd create` heuristic guarded nothing.

[ -n "$reason" ] || exit 0

printf 'aeon-fence: BLOCKED aeon=%s bead=%s: %s\n' \
    "${SPIRA_AEON:-?}" "${BEAD_ID:-?}" "$reason" >&2

printf '{"decision":"block","reason":"%s"}\n' \
    "$(printf '%s' "$reason" | sed 's/"/\\"/g')"
exit 0
