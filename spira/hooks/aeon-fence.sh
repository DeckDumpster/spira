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

[ "$tool" = "Bash" ] || exit 0

cmd="$(printf '%s' "$payload" | python3 -c '
import json, sys
try: d = json.load(sys.stdin); print(d.get("tool_input",{}).get("command",""))
except Exception: print("")' 2>/dev/null)"

[ -n "$cmd" ] || exit 0

# Returns "1" if /$1 appears as an exec target in $2; "" if only as a git file argument.
# Splits compound commands on shell separators; sub-commands starting with "git" are
# file operations, not invocations.
_exec_ctx() {
    python3 -c '
import sys, re
script = sys.argv[1]; cmd = sys.argv[2]
pat = "/" + script
EXEC = {"bash", "sh", "ksh", "zsh", "dash", "source", "."}
for sub in re.split(r"&&|\|\||;", cmd):
    sub = sub.strip()
    if pat not in sub: continue
    toks = sub.split()
    if not toks: continue
    t = toks[0]
    if t in EXEC or t.startswith("/") or t.startswith("./") or pat in t:
        print("1"); break
' "$1" "$2" 2>/dev/null
}

reason=""

for _script in landing.sh batch.sh verdict.sh slay.sh world.sh deploy.sh activate.sh promote.sh; do
    case "$cmd" in
        *"/$_script"*)
            [ "$(_exec_ctx "$_script" "$cmd")" = "1" ] && { reason="aeons may not call $_script (sp-kz8ob: landing and batch handle forge writes; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)"; break; } ;;
    esac
done

if [ -z "$reason" ]; then
    case "$cmd" in
        *"/queue.sh"*)
            case "$cmd" in
                *"/queue.sh stats"*) ;;
                *)
                    [ "$(_exec_ctx "queue.sh" "$cmd")" = "1" ] && \
                        reason="aeons may not operate the queue (sp-kz8ob: queue.sh stats is the only read-only subcommand; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
            esac ;;
    esac
fi

if [ -z "$reason" ]; then
    case "$cmd" in
        *"gh pr create"*|*"gh pr edit"*|*"gh pr merge"*|*"gh pr close"*|\
        *"gh release create"*|*"gh release edit"*|*"gh release delete"*|\
        *"gh workflow run"*|\
        *"gh run rerun"*|*"gh run cancel"*)
            reason="aeons carry no forge credentials (sp-kz8ob: forge writes go through landing.sh and batch.sh)" ;;
    esac
fi

if [ -z "$reason" ]; then
    case "$cmd" in
        *"git push"*|*"git -C"*" push "*)
            reason="aeons carry no push credentials (sp-kz8ob: landing.sh and batch.sh handle all merges and pushes)" ;;
    esac
fi

if [ -z "$reason" ] && [ -n "${SPIRA_RUN:-}" ]; then
    case "$cmd" in
        *"${SPIRA_RUN}/landstate"*|*"${SPIRA_RUN}/queue"*)
            reason="aeons may not write to \$SPIRA_RUN/landstate or \$SPIRA_RUN/queue (sp-kz8ob)" ;;
    esac
fi

if [ -z "$reason" ] && [ -n "${SPIRA_PROD:-}" ]; then
    # Refuse write shapes only; reads and script execution from prod are permitted.
    case "$cmd" in
        *">${SPIRA_PROD}"*|*"> ${SPIRA_PROD}"*|\
        *">>${SPIRA_PROD}"*|*">> ${SPIRA_PROD}"*|\
        *"rm "*"${SPIRA_PROD}"*|\
        *"mv "*"${SPIRA_PROD}"*|\
        *"tee"*"${SPIRA_PROD}"*|\
        *"sed -i"*"${SPIRA_PROD}"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" commit"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" reset"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" checkout"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" clean"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" add"*)
            reason="aeons may not write to the production checkout \$SPIRA_PROD (sp-kz8ob: use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
    esac
fi

# Refuse bd create against the production store when bd's own test-data heuristic fires
# or when a repo: label is not in the repo-map (detect_livelocked's unmapped-repo predicate).
# Refuses, not warns — the aeon has a testdb for throwaway creates. sp-mvg44.
if [ -z "$reason" ] && [ -n "${SPIRA_DB:-}" ]; then
    case "$cmd" in *bd*create*)
        _bd_check="$(printf '%s' "$cmd" | \
            SPIRA_DB="$SPIRA_DB" \
            SPIRA_REPO_MAP="${SPIRA_REPO_MAP:-}" \
            SPIRA_BD="${SPIRA_BD:-bd}" \
            python3 -c '
import sys, os, re, subprocess

cmd = sys.stdin.read()
spira_db = os.environ.get("SPIRA_DB", "").rstrip("/")
repo_map = os.environ.get("SPIRA_REPO_MAP", "")
bd_bin = os.environ.get("SPIRA_BD", "bd")

if not spira_db:
    sys.exit(0)

valid_repos = set()
if repo_map:
    try:
        with open(repo_map) as f:
            for line in f:
                s = line.strip()
                if not s or s.startswith("#"):
                    continue
                parts = s.split("|")
                if len(parts) >= 2:
                    n = parts[0].strip()
                    if n:
                        valid_repos.add(n)
    except OSError:
        pass

for seg in re.split(r"&&|\|\||;|\n", cmd):
    seg = seg.strip()
    if not seg or "bd" not in seg:
        continue

    # Strip leading VAR=val assignments; capture SPIRA_DB override.
    rem = seg
    inline_db = None
    while True:
        m = re.match(r"^([A-Z_][A-Z0-9_]*)=(\S*)\s+", rem)
        if not m:
            break
        if m.group(1) == "SPIRA_DB":
            v = m.group(2).rstrip("/")
            if "$" not in v:
                inline_db = v
        rem = rem[m.end():]

    if not re.match(r"bd\s", rem):
        continue

    toks = rem.split()
    if not toks or toks[0] != "bd":
        continue

    db_path = None
    create_i = None
    i = 1
    while i < len(toks):
        t = toks[i]
        if t in ("-C", "--db") and i + 1 < len(toks):
            db_path = toks[i+1].rstrip("/")
            i += 2
        elif t == "create":
            create_i = i
            break
        elif t.startswith("-"):
            i += 1
        else:
            break

    if create_i is None:
        continue

    eff_db = db_path or inline_db or spira_db
    if eff_db != spira_db:
        continue

    rest = toks[create_i+1:]
    title = None
    labels = ""
    j = 0
    while j < len(rest):
        t = rest[j]
        if t in ("-l", "--label") and j + 1 < len(rest):
            labels = rest[j+1].strip("\"'"'"'")
            j += 2
        elif t.startswith("-"):
            if t in ("-a","--assignee","-d","--description","--context",
                     "--acceptance","--design","--due","--defer",
                     "-e","--estimate","--priority","--type",
                     "--append-notes","--external-ref","--body-file",
                     "--design-file","--assignee","--deps"):
                j += 2
            else:
                j += 1
        elif title is None:
            title = t.strip("\"'"'"'")
            j += 1
        else:
            j += 1

    if labels and valid_repos:
        for lbl in labels.split(","):
            lbl = lbl.strip()
            if lbl.startswith("repo:"):
                rname = lbl[5:]
                if rname and rname not in valid_repos:
                    print("unmapped-repo:" + rname)
                    sys.exit(0)

    if title is not None:
        try:
            args = [bd_bin, "-C", eff_db, "create", title]
            if labels:
                args += ["-l", labels]
            args.append("--dry-run")
            r = subprocess.run(args, capture_output=True, text=True, timeout=10)
            if "appears to be test data" in (r.stdout + r.stderr):
                print("test-data")
        except Exception:
            pass
' 2>/dev/null)"
        case "$_bd_check" in
            test-data)
                case "$cmd" in *"SPIRA_BD_CREATE_OVERRIDE=1"*) ;;
                    *) reason="aeons may not create test-data beads in the production store (bd flagged this title; use testdb_up or bd --db <tmp> for throwaway creates; set SPIRA_BD_CREATE_OVERRIDE=1 to override for a deliberate production bead)" ;;
                esac ;;
            unmapped-repo:*)
                case "$cmd" in *"SPIRA_BD_CREATE_OVERRIDE=1"*) ;;
                    *) _unmapped="${_bd_check#unmapped-repo:}"
                       reason="aeons may not create beads with unmapped repo: labels in the production store (repo '${_unmapped}' not in \$SPIRA_REPO_MAP; add it to the map before filing against it; set SPIRA_BD_CREATE_OVERRIDE=1 to override)" ;;
                esac ;;
        esac
    esac
fi

[ -n "$reason" ] || exit 0

printf 'aeon-fence: BLOCKED aeon=%s bead=%s: %s\n' \
    "${SPIRA_AEON:-?}" "${BEAD_ID:-?}" "$reason" >&2

printf '{"decision":"block","reason":"%s"}\n' \
    "$(printf '%s' "$reason" | sed 's/"/\\"/g')"
exit 0
