#!/usr/bin/env bash
#
# bd-close-focus-guard.sh — during a focus period, bd close must cite an sp-id
#   in the close reason, naming the bead that will surface any finding.
#
#   bash bd-close-focus-guard.sh    reads Claude Code's PreToolUse JSON on stdin
#
# Exit 2 BLOCKS the tool call and feeds stderr back to the model. Exit 0 allows.
#
# WHY THIS EXISTS
# ---------------
# A close performed under a focus period (SPIRA_FAYTHS narrowed in the operator's
# conf file) can swallow a finding. When an aeon closes a bead and writes "there is a
# defect here — refile it later" in the reason without creating a bead, the finding
# lives only in that reason text. The reason is read only by someone who already went
# looking for that bead, so a finding left there stops existing the moment the focus
# period ends. The scar (sp-7p9): a real defect was closed under a focus period with
# "refile deliberately once the focus period closes" in its reason; the period closed,
# nothing refiled it, and the suite went green only by accident three hours later.
#
# The gap is not the close policy — focus periods are deliberate. The gap is that a
# close reason alone is not a filing. This guard enforces the rule at the chokepoint:
# when SPIRA_FAYTHS is narrowed to a strict subset of the chamber's auto-summoned
# personas, the close reason must name an sp-id that will surface any finding (an
# open bead, a deferred bead, or an insight).
#
# HOW FOCUS PERIOD IS DETECTED
# ----------------------------
# SPIRA_FAYTHS in the environment OR in the operator's conf file (in the same search
# order conf.sh uses: $SPIRA_CONF, $SPIRA_REPO/spira.conf, ~/.config/spira/spira.conf,
# /etc/spira/spira.conf). A non-empty SPIRA_FAYTHS alone is not sufficient — the guard
# compares the configured roster against the chamber's auto-summoned personas (those
# without FAYTH_SUMMON=operator). SPIRA_FAYTHS == full auto roster is not a focus
# period; SPIRA_FAYTHS missing at least one auto persona IS a focus period.
# SPIRA_FAYTHS is not exported by conf.sh (law-gates-run-in-a-clean-environment), so
# the guard reads the conf file directly rather than relying on the environment.
#
# SP-ID VERIFICATION
# ------------------
# When an sp-id is found in the reason and SPIRA_DB is available, the guard calls
# `bd show` to confirm the cited bead exists and is either (a) not closed, or (b)
# an insight. Verification is best-effort: a lookup failure allows the close.
#
# WHY AEON SESSIONS ONLY
# ----------------------
# The actor that swallowed the finding had SPIRA_AEON set; brain sessions do not.
# Binding this to the brain session would bind the most disciplined caller and miss
# the offender (law-guard-binds-the-caller).
#
# A fence is a polite refusal — the override is named and honoured.
#
# WIRING: designed to be called as a Claude Code PreToolUse hook. Register via the
# operator's settings file alongside bd-update-closed-guard.sh:
#   { "matcher": "Bash", "hooks": [{ "type": "command",
#     "command": "bash /path/to/spira/bd-close-focus-guard.sh" }] }
# Source harness.sh first when the path must be derived from SPIRA_HOME:
#   "command": "bash -c '. .claude/harness.sh 2>/dev/null; bash \"$SPIRA_HOME/bd-close-focus-guard.sh\"'"
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

# Drain stdin unconditionally so the caller's pipe is not left open (which would
# cause a BrokenPipeError in the process writing the PreToolUse payload).
PAYLOAD="$(cat)"

# AEON SESSIONS ONLY. The actor that swallowed the finding had SPIRA_AEON set;
# maintenance sessions at the keyboard do not. Exiting 0 avoids binding the wrong
# caller (law-guard-binds-the-caller).
[ -n "${SPIRA_AEON:-}" ] || exit 0

# The Python block does all matching and returns one of:
#   BLOCK:<fayths>                — no sp-id in reason; fayths is the active roster
#   INVALID:<sp_id>:<fayths>      — sp-id found but bead is closed and not an insight
#   (nothing, exits 0)            — allow the close
#
# The payload is passed via GUARD_PAYLOAD rather than stdin so the heredoc can supply
# the Python source without fighting the pipeline for stdin.
HIT="$(GUARD_PAYLOAD="$PAYLOAD" \
  SPIRA_CONF="${SPIRA_CONF:-}" \
  SPIRA_HOME="${SPIRA_HOME:-$HERE}" \
  SPIRA_DB="${SPIRA_DB:-}" \
  SPIRA_BD="${SPIRA_BD:-}" \
  SPIRA_ID_PREFIX="${SPIRA_ID_PREFIX:-sp}" \
  python3 << 'PYEOF'
import json, os, re, subprocess, sys

def read_conf_key(key):
    # Environment wins; SPIRA_FAYTHS may be set explicitly in tests.
    v = os.environ.get(key, "")
    if v:
        return v
    spira_home = os.environ.get("SPIRA_HOME", "")
    spira_repo = os.path.dirname(spira_home) if spira_home else ""
    candidates = []
    explicit = os.environ.get("SPIRA_CONF", "")
    if explicit:
        candidates.append(explicit)
    if spira_repo:
        candidates.append(os.path.join(spira_repo, "spira.conf"))
    home = os.path.expanduser("~")
    xdg = os.environ.get("XDG_CONFIG_HOME", os.path.join(home, ".config"))
    candidates.append(os.path.join(xdg, "spira", "spira.conf"))
    candidates.append("/etc/spira/spira.conf")
    for path in candidates:
        try:
            with open(path) as f:
                for line in f:
                    line = line.strip()
                    if not line or line.startswith("#"):
                        continue
                    if "=" not in line:
                        continue
                    k, _, v = line.partition("=")
                    k = k.strip()
                    v = v.strip()
                    # Strip surrounding quotes, same as conf.sh.
                    if (v.startswith('"') and v.endswith('"')) or \
                       (v.startswith("'") and v.endswith("'")):
                        v = v[1:-1]
                    if k == key and v:
                        return v
        except Exception:
            continue
    return ""

def auto_fayths(spira_home):
    # Return the set of auto-summoned persona names from the chamber directory.
    # Operator personas (FAYTH_SUMMON != auto) are excluded — they are never in
    # the sentinel roster, so their absence from SPIRA_FAYTHS is not a narrowing.
    # Returns None when the chamber cannot be read (causes fail-open).
    chamber = os.path.join(spira_home, "chamber")
    if not os.path.isdir(chamber):
        return None
    result = set()
    try:
        for fname in os.listdir(chamber):
            if not fname.endswith(".fayth"):
                continue
            name = fname[:-6]
            fpath = os.path.join(chamber, fname)
            summon = "auto"
            try:
                with open(fpath) as f:
                    for line in f:
                        line = line.strip()
                        if line.startswith("FAYTH_SUMMON="):
                            summon = line[len("FAYTH_SUMMON="):].strip().strip("\"'")
                            break
            except Exception:
                pass
            if summon == "auto":
                result.add(name)
    except Exception:
        return None
    return result if result else None

def is_focus_period(spira_fayths, spira_home):
    # True when SPIRA_FAYTHS names fewer auto-summoned personas than the full chamber.
    # A full roster is not a focus period. Missing SPIRA_FAYTHS is not a focus period.
    # Unreadable chamber: fail open (return False).
    if not spira_fayths:
        return False
    configured = set(spira_fayths.split())
    all_auto = auto_fayths(spira_home)
    if all_auto is None:
        return False
    return configured < all_auto  # strict subset

payload = os.environ.get("GUARD_PAYLOAD", "")
try:
    d = json.loads(payload)
except Exception:
    sys.exit(0)

if d.get("tool_name") != "Bash":
    sys.exit(0)

c = (d.get("tool_input") or {}).get("command", "")
if not c:
    sys.exit(0)

# Strip single-quoted spans so echoed prose does not trigger the guard.
c_nosq = re.sub(r"\x27[^\x27]*\x27", " ", c)

# Detect a `bd close` invocation.
segs = re.split(r"\|\||\&\&|[;|\n]", c_nosq)
found_close = False
for seg in segs:
    # Match: [env=val ...] [path/]bd [opts] close [...]
    if re.search(r"(?:^|\s)(?:\w+=\S+\s+)*(?:[-\w./]*/)?bd\b[^\n]*\bclose\b", seg):
        found_close = True
        break

if not found_close:
    sys.exit(0)

# Check if a focus period is active.
spira_fayths = read_conf_key("SPIRA_FAYTHS")
spira_home   = os.environ.get("SPIRA_HOME", "")
if not is_focus_period(spira_fayths, spira_home):
    sys.exit(0)

# Extract the close reason from the FULL command. Two forms:
#   (a) --reason "text"  or  --reason text  (not --reason-file)
#   (b) --reason-file -  with heredoc body
reason = ""

# Form (a): --reason followed by a quoted or bare value.
m = re.search(r"(?<!\w)--reason\s+(?!-?file\b)([\"'])(.*?)\1", c, re.DOTALL)
if m:
    reason = m.group(2)
else:
    m = re.search(r"(?<!\w)--reason\s+(?!-?file\b)(\S+)", c)
    if m:
        reason = m.group(1)

# Form (b): heredoc body.
if not reason:
    hd = re.search(
        r"<<-?\s*[\x27\"]?(\w+)[\x27\"]?(.*?)(?:^\s*\1\s*$)",
        c, re.DOTALL | re.MULTILINE
    )
    if hd:
        reason = hd.group(2).strip()

# No legible reason — cannot inspect the content. Fail open.
if not reason:
    sys.exit(0)

_id_prefix = re.escape(os.environ.get("SPIRA_ID_PREFIX", "sp"))
_ID_RE = re.compile(r"\b" + _id_prefix + r"-[a-z0-9]+\b")
sp_ids = _ID_RE.findall(reason)

if sp_ids:
    # Verify the cited id exists and is not closed (unless it is an insight).
    # Best-effort: any error allows the close rather than blocking it.
    sp_id  = sp_ids[0]
    db     = os.environ.get("SPIRA_DB", "")
    bd_bin = os.environ.get("SPIRA_BD", "") or "bd"
    if db:
        try:
            proc = subprocess.run(
                [bd_bin, "-C", db, "show", sp_id, "--format", "json"],
                capture_output=True, text=True, timeout=5
            )
            if proc.returncode == 0 and proc.stdout.strip():
                raw  = json.loads(proc.stdout)
                bead = raw[0] if isinstance(raw, list) else raw
                status = bead.get("status", "")
                raw_labels = bead.get("labels") or []
                if isinstance(raw_labels, str):
                    raw_labels = raw_labels.split()
                is_open    = status not in ("closed",)
                is_insight = "insight" in raw_labels
                if is_open or is_insight:
                    sys.exit(0)
                # Closed and not an insight: invalid citation.
                print("INVALID:" + sp_id + ":" + spira_fayths)
                sys.exit(0)
        except Exception:
            pass  # Fail open on any error.
    # No database to check against or verification failed: allow.
    sys.exit(0)

# No sp-id in the reason during an active focus period.
print("BLOCK:" + spira_fayths)
PYEOF
)"

[ -n "$HIT" ] || exit 0

# OVERRIDE, named here and honoured. A session that genuinely needs to close
# during a focus period without citing a bead — e.g. a clean completion with no
# finding — may set BD_CLOSE_FOCUS_OVERRIDE=1.
[ "${BD_CLOSE_FOCUS_OVERRIDE:-0}" = "1" ] && exit 0
if printf '%s' "$PAYLOAD" | python3 -c '
import json, sys
d = json.load(sys.stdin)
c = (d.get("tool_input") or {}).get("command", "")
sys.exit(0 if "BD_CLOSE_FOCUS_OVERRIDE=1" in c else 1)
' 2>/dev/null; then exit 0; fi

FAYTHS="${HIT#*:}"
FAYTHS="${FAYTHS%%:*}"

printf '\nBLOCKED by bd-close-focus-guard: focus period active, close reason must cite a bead id.\n\n' >&2
printf 'SPIRA_FAYTHS is narrowed ("%s"): some work is being held back.\n' "$FAYTHS" >&2
printf 'A finding left only in a close reason stops existing when the focus period ends.\n\n' >&2

if printf '%s' "$HIT" | grep -q '^INVALID:'; then
    CITED="${HIT#INVALID:}"; CITED="${CITED%%:*}"
    printf 'The cited bead (%s) is already closed and is not an insight.\n' "$CITED" >&2
    printf 'Cite an open bead, a deferred bead, or an insight bead instead.\n\n' >&2
fi

printf 'Before closing, file a bead that will surface the finding:\n\n' >&2
printf '  Insight (a record, created closed — no action required):\n' >&2
printf '    cockpit/ask.sh insight "<what was found>" --why "<why it matters>"\n\n' >&2
printf '  Open or deferred bead (work to be done later):\n' >&2
printf '    bd -C "$SPIRA_DB" create --title "<finding>" ...\n\n' >&2
printf 'Then include the bead id in the close reason.\n\n' >&2
printf 'OVERRIDE for a close with no finding (work is genuinely done):\n' >&2
printf '  BD_CLOSE_FOCUS_OVERRIDE=1\n' >&2
exit 2
