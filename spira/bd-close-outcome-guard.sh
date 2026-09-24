#!/usr/bin/env bash
# bd-close-outcome-guard.sh — PreToolUse hook: refuse bd close in aeon sessions
#   when the close reason does not declare a valid terminal outcome on its first
#   non-empty line as "OUTCOME: <type>".
#
# Valid outcomes:
#   landed      work is already on the base branch (typically the sentinel's record)
#   submitted   work committed on branch, handed to the landing pass
#   delivered   deliverable is not code (beads, notes, docs); names what was written
#   escalated   blocked on operator decision; names an ask bead that has this bead
#               as a dependent so the answer can be delivered back
#   blocked     blocked on another bead; names the blocking bead
#   abandoned   bead should not proceed; reason explains why
#   parked      out of lifetime; names what work remains
#
# Exit 2 BLOCKS the tool call and feeds stderr to the model. Exit 0 allows.
# Override: BEAD_OUTCOME_CONSIDERED=<explanation> in env or inline in the command.
#
# covers: spira/bd-close-outcome-guard.sh spira/aeon.sh spira/chamber/builder.md
set -uo pipefail

PAYLOAD="$(cat)"

[ -n "${SPIRA_AEON:-}" ] || exit 0

BID="${BEAD_ID:-}"; DB="${SPIRA_DB:-}"; BD="${SPIRA_BD:-bd}"
PREFIX="${SPIRA_ID_PREFIX:-sp}"

HIT="$(GUARD_PAYLOAD="$PAYLOAD" BEAD_ID="$BID" SPIRA_DB="$DB" SPIRA_BD="$BD" \
       SPIRA_ID_PREFIX="$PREFIX" python3 2>/dev/null << 'PYEOF'
import json, os, re, shlex, subprocess, sys

VALID = {"landed", "submitted", "delivered", "escalated", "blocked", "abandoned", "parked"}

payload = os.environ.get("GUARD_PAYLOAD", "")
try:
    d = json.loads(payload)
except Exception:
    sys.exit(0)

if d.get("tool_name") != "Bash":
    sys.exit(0)

cmd = (d.get("tool_input") or {}).get("command", "")
if not cmd:
    sys.exit(0)

# A heredoc body is data handed to whatever command reads it, not a command itself --
# a commit message describing this very guard says "bd close" in prose. Blank heredoc
# bodies before hunting for an actual bd-close invocation; reason extraction below still
# reads bodies from the untouched `cmd`.
def blank_heredoc_bodies(text):
    return re.sub(
        r"(<<-?\s*[\x27\"]?(?P<delim>\w+)[\x27\"]?[^\n]*)\n.*?^[ \t]*(?P=delim)[ \t]*$",
        lambda m: m.group(1),
        text, flags=re.DOTALL | re.MULTILINE,
    )

# A token like "bd-close-outcome-guard.sh" (this guard's own filename, named in a `git rm`
# or `git mv` argument list) contains "bd" and "close" as hyphen-separated substrings that
# a character-level regex cannot tell apart from the real command. Tokenize instead: "bd"
# must be the command word (after any VAR=val prefixes, optionally path-qualified) and
# "close" must be a separate argument token.
def segment_has_bd_close(seg):
    try:
        toks = shlex.split(seg, posix=True)
    except ValueError:
        toks = seg.split()
    i = 0
    while i < len(toks) and re.match(r"^\w+=", toks[i]):
        i += 1
    if i >= len(toks):
        return False
    if toks[i].rsplit("/", 1)[-1] != "bd":
        return False
    return "close" in toks[i + 1:]

segs = re.split(r"\|\||\&\&|[;|\n]", blank_heredoc_bodies(cmd))
found_close = any(segment_has_bd_close(seg) for seg in segs)
if not found_close:
    sys.exit(0)

if "BEAD_OUTCOME_CONSIDERED=" in cmd:
    sys.exit(0)

# Extract the close reason from --reason/-reason-file- or a heredoc body.
reason = ""
m = re.search(r"(?<!\w)--reason\s+(?!-?file\b)([\"'])(.*?)\1", cmd, re.DOTALL)
if m:
    reason = m.group(2)
else:
    m = re.search(r"(?<!\w)--reason\s+(?!-?file\b)(\S+)", cmd)
    if m:
        reason = m.group(1)
if not reason:
    hd = re.search(
        r"<<-?\s*[\x27\"]?(\w+)[\x27\"]?(.*?)(?:^\s*\1\s*$)",
        cmd, re.DOTALL | re.MULTILINE
    )
    if hd:
        reason = hd.group(2).strip()

if not reason:
    print("NO_REASON")
    sys.exit(0)

first_line = ""
for line in reason.splitlines():
    s = line.strip()
    if s:
        first_line = s
        break

if not first_line.upper().startswith("OUTCOME:"):
    print("NO_OUTCOME:" + first_line[:60])
    sys.exit(0)

outcome = first_line[len("OUTCOME:"):].strip().lower()
if outcome not in VALID:
    print("BAD_OUTCOME:" + outcome)
    sys.exit(0)

pfx = re.escape(os.environ.get("SPIRA_ID_PREFIX", "sp"))
ID_RE = re.compile(r"\b" + pfx + r"-[a-z0-9]+\b")
bid    = os.environ.get("BEAD_ID", "")
db     = os.environ.get("SPIRA_DB", "")
bd_bin = os.environ.get("SPIRA_BD", "bd")

if outcome == "escalated":
    ids = [i for i in ID_RE.findall(reason) if i != bid]
    if not ids:
        print("ESCALATED_NO_BID")
        sys.exit(0)
    ask = ids[0]
    # Verify the ask bead lists this bead as a dependent so the answer can be delivered.
    if bid and db:
        try:
            r = subprocess.run(
                [bd_bin, "-C", db, "dep", "list", ask, "--direction=up", "--json"],
                capture_output=True, text=True, timeout=10
            )
            if r.returncode == 0 and r.stdout.strip():
                deps = json.loads(r.stdout)
                if not isinstance(deps, list):
                    deps = [deps]
                if not any(str(x.get("id", "")) == bid for x in deps):
                    print("ESCALATED_NO_DEP:" + ask)
                    sys.exit(0)
        except Exception:
            pass  # fail open on any error

elif outcome == "blocked":
    ids = [i for i in ID_RE.findall(reason) if i != bid]
    if not ids:
        print("BLOCKED_NO_BID")
        sys.exit(0)

elif outcome == "submitted":
    # A work-type bead is not closed by the aeon that wrote it — only the landing pass
    # closes it, citing the merge commit (bead_close_on_land, lib.sh), once the commit is
    # actually on the base. Convert the close into the label it should have been and
    # refuse the close itself; a type this cannot resolve is left to close as before
    # (fail open — the same posture as the escalated/blocked lookups above).
    work_types = set(os.environ.get("SPIRA_WORK_CLOSE_TYPES", "task bug feature").split())
    label = os.environ.get("SPIRA_SUBMITTED_LABEL", "spira-submitted")
    if bid and db and work_types:
        try:
            r = subprocess.run(
                [bd_bin, "-C", db, "show", bid, "--json"],
                capture_output=True, text=True, timeout=10
            )
            itype = ""
            if r.returncode == 0 and r.stdout.strip():
                d = json.loads(r.stdout)
                d = d if isinstance(d, list) else [d]
                if d:
                    itype = d[0].get("issue_type", "") or ""
            if itype in work_types:
                subprocess.run(
                    [bd_bin, "-C", db, "label", "add", bid, label],
                    capture_output=True, text=True, timeout=10
                )
                print("CONVERTED_SUBMITTED:" + label)
                sys.exit(0)
        except Exception:
            pass  # fail open — an unreadable type does not block the close

sys.exit(0)
PYEOF
)"

[ -n "$HIT" ] || exit 0

[ -n "${BEAD_OUTCOME_CONSIDERED:-}" ] && exit 0

OUTCOME_LIST="landed, submitted, delivered, escalated, blocked, abandoned, parked"

case "$HIT" in
    NO_REASON)
        printf '\nBLOCKED by bd-close-outcome-guard: no legible reason — outcome cannot be verified.\n\n' >&2
        printf 'Use --reason-file - with a heredoc:\n\n' >&2
        printf '    bd -C "$SPIRA_DB" close "$BEAD_ID" --reason-file - <<'"'"'REASON'"'"'\n' >&2
        printf '    OUTCOME: submitted\n' >&2
        printf '    What landed and how it was verified.\n' >&2
        printf '    REASON\n' >&2
        ;;
    NO_OUTCOME:*)
        printf '\nBLOCKED by bd-close-outcome-guard: no terminal outcome declared.\n\n' >&2
        printf 'The first non-empty line of the reason must be:  OUTCOME: <type>\n\n' >&2
        printf 'Valid types: %s\n\n' "$OUTCOME_LIST" >&2
        printf 'Example:\n\n' >&2
        printf '    OUTCOME: submitted\n' >&2
        printf '    What landed and how it was verified.\n\n' >&2
        printf 'Override when declaring an outcome would misrepresent the close:\n' >&2
        printf '    BEAD_OUTCOME_CONSIDERED=<explanation>\n' >&2
        ;;
    BAD_OUTCOME:*)
        TYPE="${HIT#BAD_OUTCOME:}"
        printf '\nBLOCKED by bd-close-outcome-guard: "%s" is not a valid outcome type.\n\n' "$TYPE" >&2
        printf 'Valid types: %s\n' "$OUTCOME_LIST" >&2
        ;;
    ESCALATED_NO_BID)
        printf '\nBLOCKED by bd-close-outcome-guard: OUTCOME: escalated — reason must name the ask bead ID.\n\n' >&2
        printf 'Include the ask bead ID in the reason so the verdict can be delivered back.\n' >&2
        ;;
    ESCALATED_NO_DEP:*)
        ASK="${HIT#ESCALATED_NO_DEP:}"
        printf '\nBLOCKED by bd-close-outcome-guard: OUTCOME: escalated — ask bead %s has no dependent.\n\n' "$ASK" >&2
        printf 'The ask bead must list this bead as a dependent so the answer is delivered:\n\n' >&2
        printf '    bd -C "$SPIRA_DB" dep add %s %s\n' "$ASK" "${BID:-<this-bead>}" >&2
        ;;
    BLOCKED_NO_BID)
        printf '\nBLOCKED by bd-close-outcome-guard: OUTCOME: blocked — reason must name the blocking bead ID.\n\n' >&2
        printf 'Include the blocking bead ID in the reason.\n' >&2
        ;;
    CONVERTED_SUBMITTED:*)
        LABEL="${HIT#CONVERTED_SUBMITTED:}"
        printf '\nBLOCKED by bd-close-outcome-guard: work beads are not closed by the aeon that wrote them.\n\n' >&2
        printf '%s is now labelled %s. Do not close it — the landing pass closes it for you,\n' \
            "${BID:-this bead}" "$LABEL" >&2
        printf 'citing the merge commit, once it lands. End your turn here.\n' >&2
        ;;
esac
exit 2
