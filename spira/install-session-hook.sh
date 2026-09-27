#!/usr/bin/env bash
#
# install-session-hook.sh — register the session hook in the coding agent's own settings file.
#
#   install-session-hook.sh install            add or refresh Spira's entries
#   install-session-hook.sh status             what is registered on the session events
#   install-session-hook.sh uninstall          remove Spira's entries
#   install-session-hook.sh prune <substring>  remove every entry whose command contains it
#
# WHAT IT WIRES, AND WHY ONLY THAT ONE EVENT
# -------------------------------------------
# `SessionStart` with NO MATCHER.
#
# A matcher is a regular expression tested against the event's match query, and an absent or
# `*` matcher matches everything. For `SessionStart` the match query is its `source`, which
# the client's own hook-input schema restricts to `startup`, `resume`, `clear`, `compact` and
# `fork`. Naming a subset of those is how a hook comes to be missing from exactly the case it
# was written for: a three-way `clear|startup|resume` — a shape that is easy to arrive at —
# silently omits `compact` and `fork`, both of which open a context window with no Monitor
# attached, which is the only condition this hook is about. An absent matcher also survives
# the client adding a sixth source, where a list would go quietly missing on it.
#
# `PostCompact` is NOT ALSO REGISTERED. A compaction already raises `SessionStart` with
# `source=compact`, so registering both ran the hook twice on every compaction — once for
# each event the client fired. `install` strips any `PostCompact` registration of this hook it
# finds rather than leaving it as a second, silent trigger for a box that registered it before
# this was known.
#
# `SessionEnd` is NOT registered. Under systemd there are no processes for a departing session
# to guarantee, and its output goes into a context that is being discarded. The hook handles
# the event so that registering it costs nothing, but a hook whose only effect is a process
# spawn is noise with a cost.
#
# WHY A PROGRAM AND NOT AN INSTRUCTION
# ------------------------------------
# This file belongs to a program the harness does not ship, it lives outside every checkout,
# and nothing that lands in this repository can change it. A registration done by hand is done
# once and then rots invisibly — the failure being fixed here is a hook still bound to a path
# whose harness had been decommissioned, printing its banner into every session on the box.
# `status` is the check, and `doctor.sh` runs it.
#
# IT MANAGES ITS OWN ENTRIES AND NOBODY ELSE'S. `install` replaces Spira's and reports any
# other command registered on the same events rather than removing it — deleting somebody
# else's hook is not a decision this may take silently. `prune` is how that removal is asked
# for, by name, one substring at a time.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

HOOK="$SPIRA_HOME/hooks/session.sh"
SETTINGS="$SPIRA_CLIENT_SETTINGS"

# THE EVENTS AND THE TIMEOUT IN ONE PLACE. The timeout is generous next to what the hook costs
# — one `systemctl is-active` for the whole manifest and a few file reads — because the
# penalty for being slow is a warning and the penalty for being killed is a session that
# starts with no idea what is watching it.
EVENTS="SessionStart"

# A RETIRED EVENT, stripped on install but never (re-)added. `PostCompact` used to carry this
# hook too, before `SessionStart`'s own `source=compact` was known to fire on every compaction
# — a box that installed under the old EVENTS still has that entry, and `install` is the repair
# path that reaches every box.
RETIRE_EVENTS="PostCompact"
TIMEOUT=10

usage() {
    echo "usage: install-session-hook.sh install|status|uninstall|prune <substring>" >&2
    exit 2
}

# edit <mode> [substring] — rewrite the settings file, or describe it. One program for all
# four verbs so there is one implementation of the file's shape.
#
# WRITTEN THROUGH A TEMPORARY FILE AND A BACKUP. This is the operator's live client
# configuration; a partial write of it is a client that will not start, and the whole file may
# hold settings nothing here knows about, which is why it is parsed and re-serialised rather
# than templated.
edit() {
    python3 - "$SETTINGS" "$HOOK" "$1" "${2-}" "$TIMEOUT" "$EVENTS" "$RETIRE_EVENTS" <<'PY'
import json, os, sys, tempfile

settings, hook, mode, needle, timeout, events, retire_events = sys.argv[1:8]
events = events.split()
retire_events = retire_events.split()

try:
    with open(settings) as fh:
        doc = json.load(fh)
except FileNotFoundError:
    doc = {}
except (OSError, ValueError) as exc:
    print("install-session-hook: cannot read %s: %s" % (settings, exc), file=sys.stderr)
    raise SystemExit(1)
if not isinstance(doc, dict):
    print("install-session-hook: %s is not a JSON object" % settings, file=sys.stderr)
    raise SystemExit(1)

hooks = doc.get("hooks")
if not isinstance(hooks, dict):
    hooks = {}

def commands(entry):
    """Every command string in one matcher entry, whatever shape it is in."""
    out = []
    hs = entry.get("hooks") if isinstance(entry, dict) else None
    if isinstance(hs, dict):
        hs = [hs]
    for h in hs or []:
        if isinstance(h, dict) and isinstance(h.get("command"), str):
            out.append(h["command"])
    return out

def strip(entries, match):
    """Drop every hook whose command matches, and any entry left holding none."""
    kept, dropped = [], []
    for entry in entries:
        if not isinstance(entry, dict):
            kept.append(entry)
            continue
        hs = entry.get("hooks")
        if isinstance(hs, dict):
            hs = [hs]
        if not isinstance(hs, list):
            kept.append(entry)
            continue
        keep = []
        for h in hs:
            cmd = h.get("command") if isinstance(h, dict) else None
            if isinstance(cmd, str) and match(cmd):
                dropped.append(cmd)
            else:
                keep.append(h)
        # AN ENTRY EMPTIED OF HOOKS IS REMOVED, not left as `{"hooks": []}`. The client
        # validates a matcher entry by its hooks being non-empty, so leaving the husk turns a
        # clean uninstall into a settings file that is reported as malformed.
        if keep:
            entry = dict(entry)
            entry["hooks"] = keep
            kept.append(entry)
    return kept, dropped

if mode == "status":
    rc = 0
    for ev in events:
        entries = hooks.get(ev) or []
        mine = [c for e in entries for c in commands(e) if c == hook]
        others = [c for e in entries for c in commands(e) if c != hook]
        if mine:
            print("  ok      %-14s %s" % (ev, hook))
        else:
            print("  MISSING %-14s the session hook is not registered" % ev)
            rc = 1
        for c in others:
            print("  other   %-14s %s" % (ev, c))
    for ev in retire_events:
        entries = hooks.get(ev) or []
        if [c for e in entries for c in commands(e) if c == hook]:
            print("  STALE   %-14s the session hook is still registered here; run install to remove it" % ev)
            rc = 1
    raise SystemExit(rc)

changed = []
if mode == "install":
    # A COMMAND THAT DOES NOT EXIST IS A HOOK ERROR IN EVERY SESSION, reported to the user at
    # every start, and it is the shape this fails into: the registration is repaired on a timer
    # while the file it names arrives with a branch, so for a while the two disagree. Refusing
    # is what makes the repair inert until there is something to register.
    if not os.access(hook, os.X_OK):
        print("install-session-hook: %s is not executable — nothing registered" % hook,
              file=sys.stderr)
        raise SystemExit(1)
    for ev in events:
        entries = [e for e in (hooks.get(ev) or [])]
        entries, _ = strip(entries, lambda c: c == hook)
        # NO "matcher" KEY AT ALL. An absent matcher matches every source, which is the point;
        # writing "*" would work identically and reads as a pattern somebody chose.
        entries.append({"hooks": [{"type": "command", "command": hook,
                                   "timeout": int(timeout)}]})
        hooks[ev] = entries
        changed.append(ev)
    # A RETIRED EVENT IS CLEANED UP, NOT REGISTERED. A box that installed under the old EVENTS
    # still fires this hook twice on every compaction until its stale entry is stripped; install
    # is the repair path that reaches every box, so it removes without re-adding.
    for ev in retire_events:
        entries, dropped = strip(hooks.get(ev) or [], lambda c: c == hook)
        if dropped:
            changed.append("%s (retired)" % ev)
        if entries:
            hooks[ev] = entries
        else:
            hooks.pop(ev, None)
elif mode == "uninstall":
    for ev in list(hooks):
        entries, dropped = strip(hooks.get(ev) or [], lambda c: c == hook)
        if dropped:
            changed.append("%s (%d)" % (ev, len(dropped)))
        if entries:
            hooks[ev] = entries
        else:
            hooks.pop(ev, None)
elif mode == "prune":
    if not needle:
        print("install-session-hook: prune needs a substring", file=sys.stderr)
        raise SystemExit(2)
    for ev in list(hooks):
        entries, dropped = strip(hooks.get(ev) or [], lambda c: needle in c)
        for c in dropped:
            changed.append("%s: %s" % (ev, c))
        if entries:
            hooks[ev] = entries
        else:
            hooks.pop(ev, None)
else:
    print("install-session-hook: unknown mode %r" % mode, file=sys.stderr)
    raise SystemExit(2)

if hooks:
    doc["hooks"] = hooks
else:
    doc.pop("hooks", None)

# WRITTEN ONLY WHEN THE FILE WOULD ACTUALLY DIFFER, because `install` is a REPAIR run from a
# timer as well as by hand. A version that rewrote on every pass would churn the operator's
# live client settings and its backup once a minute, and would report a change on every one of
# them — which is the same defect as an alert that always fires.
try:
    with open(settings) as fh:
        prior = fh.read()
except OSError:
    prior = None
rendered = json.dumps(doc, indent=2) + "\n"
if prior == rendered:
    raise SystemExit(0)
if not changed and prior is not None:
    print("nothing to change in %s" % settings)
    raise SystemExit(0)

os.makedirs(os.path.dirname(settings) or ".", exist_ok=True)
if prior is not None:
    with open(settings + ".spira.bak", "w") as fh:
        fh.write(prior)
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(settings) or ".", prefix=".settings.")
with os.fdopen(fd, "w") as fh:
    fh.write(rendered)
os.replace(tmp, settings)
for c in changed:
    print("%s %s" % (mode, c))
PY
}

case "${1:-}" in
    install)   edit install ;;
    uninstall) edit uninstall ;;
    prune)     [ -n "${2:-}" ] || usage; edit prune "$2" ;;
    status)    echo "session hook: $HOOK"
               echo "settings:     $SETTINGS"
               [ -x "$HOOK" ] || echo "  MISSING        the hook is not executable at that path"
               edit status ;;
    *)         usage ;;
esac
