#!/usr/bin/env bash
#
# sop.sh — write, match, recall and synthesise a Standard Operating Procedure.
#
#   sop.sh write <slug> [-|<file>]   write or amend sop-<slug>; text on stdin or from a file
#   sop.sh show <slug>               one SOP's full text
#   sop.sh list                      what is on the shelf
#   sop.sh match [-|<file>]          which SOPs match an incident payload
#   sop.sh applied <slug> --bead <id>|--pass <id> --check pass|fail --held yes|no|unknown [--why -|<file>]
#                                    record that a runbook was consulted, and what came of it
#   sop.sh log [--bead <id>] [--pass <id>] [--sop <slug>] [--check pass|fail] [--since <epoch>]
#                                    the applications ledger, oldest first
#   sop.sh digest                    one `<key> <hash>` line per SOP — what the shelf holds now
#   sop.sh ledger-init               create an empty applications ledger if there is none
#   sop.sh retire <slug>             remove it
#   sop.sh synth                     regenerate wiki/notes/standard-operating-procedures.md
#   sop.sh lint                      validate every sop- key against the write validator
#
# `<slug>` is written without the `sop-` prefix; it is added for you.
#
# WHY THIS IS THE SAME MECHANISM AS rule.sh
# -----------------------------------------
# Statutes are how to behave; SOPs are how to fix. They are stored the same way and read
# the same way — `bd remember` / `bd recall`, split by prefix — so Ops reads its runbooks
# exactly as every agent already reads law, rather than through a second knowledge channel
# that has to be invented, delivered and remembered.
#
# WHY AN SOP HAS A SHAPE AND THIS PROGRAM REFUSES PROSE
# -----------------------------------------------------
# A runbook written as a paragraph cannot be matched to an incident by a program and cannot
# be executed without being re-read and re-interpreted. Four fields make it mechanical:
#
#   MATCH:     an extended regex tested against the incident payload   (optional)
#   SYMPTOM:   what you are looking at                                 (required)
#   CHECK:     the command that confirms it is really this             (required)
#   FIX:       what to do about it                                     (required)
#   ESCALATE:  when this is not yours to fix                           (optional)
#   REF:       the long form, usually a wiki page                      (optional)
#
# MATCH is what makes recall deterministic. Matching an incident to a runbook by asking a
# model is the expensive tier; a regex written by whoever resolved the incident is the
# cheap one, and cheap-then-expensive is the whole shape of this harness.
#
# WHY THE WORD CAP
# ----------------
# An SOP is injected into every Ops session, exactly like a statute. 250 words is enough
# for a real procedure and small enough that thirty of them still fit; the long form goes
# behind REF, where it costs nothing until someone needs it.
#
# WHY SPIRA AND NOT THE TOWN
# --------------------------
# Statutes go to both because Gas Town agents still read them. SOPs go only to Spira: the
# Ops persona is the only thing that executes one, it reads the harness database, and putting
# runbooks in the town would charge every polecat context for a book it cannot act on.
#
# WHY AN APPLICATION IS RECORDED, AND WHY THE RECORD IS TWO PLACES
# ----------------------------------------------------------------
# The loop above told Ops to match a runbook, run its CHECK, then run its FIX — and nothing
# wrote down that any of it happened. A session that matched an SOP and ignored it left
# exactly the trace of one that executed it faithfully, so three questions nobody could
# answer about any SOP on the shelf: how often it fired, how often it was applied, and how
# often the incident came back anyway.
#
# `applied` is that record, and it goes to BOTH a ledger and the bead, because they are read
# by different readers and neither substitutes for the other. The LEDGER is for counting —
# one line, machine-shaped, appended, never rewritten. The BEAD NOTE is for the human reading
# the incident six weeks later, who has the bead in front of them and not this directory.
#
# `--held` IS THE LOAD-BEARING FIELD, and `--held yes` is a FIRST-CLASS OUTCOME. "The SOP fit,
# it held, and it taught us nothing new" is the outcome a healthy shelf produces most of the
# time, and it has to be STATED rather than inferred from silence: the check that fires on a
# session which recorded nothing cannot tell a good quiet session from an absent one, so an
# honest "it worked, nothing to add" is precisely what keeps a good session from being
# punished for it.
#
# THE LEDGER IS APPEND-ONLY AND SORTED BY CONSTRUCTION. One JSON object per line, timestamp
# first, appended in the order the records were made — so the file is in time order without
# anything ever sorting it, and a diff of it only ever grows at the tail. That is the same
# property the sorted-JSONL mirrors have and the same reason: a file that reshuffles puts its
# whole history into every commit and stops being reviewable. It is parseable with `grep` and
# readable with `cat`, which is the situation you are in when the thing that reads it is the
# thing that broke.
#
# LINES ARE SHORT ON PURPOSE. A single `printf` of one line under PIPE_BUF to a file opened
# for append is atomic against other appenders, so two Ops sessions recording at once produce
# two whole lines rather than one interleaved mess. `--why` is therefore truncated in the
# ledger and kept in full on the bead note, which has no such constraint.
#
# THE LAST RECORD FOR A (bead, sop) PAIR WINS. The brief records once between the CHECK and
# the FIX — when `held` is genuinely not known yet — and again once it is. Both lines stay,
# because the first is the evidence the CHECK ran at all and deleting it would be the exact
# erasure this exists to stop; a reader asking "did it hold" takes the last one.
#
# A REFUSAL HERE COSTS MORE THAN A THIN RECORD, so this validates the things that are typos
# and nothing else. An unknown slug, an unspellable verdict, or `--check fail --held yes` —
# a CHECK that did not confirm cannot have held — are refused, because each is a mistake at
# the keyboard that would otherwise poison the count. A missing `--why` is not refused: the
# sibling check fires on the ABSENCE of a record, so a fence that turns a session away here
# would manufacture the very silence it punishes.
#
# WHEN THE SHELF CANNOT BE READ, THE RECORD IS STILL WRITTEN, marked `shelf=unreadable`. The
# slug check exists to catch a typo; if the database is down, refusing would mean the one
# incident where the harness itself is broken is the one incident that leaves no trace.
set -uo pipefail
. "$(dirname "$0")/lib.sh"
HERE="$(cd "$(dirname "$0")" && pwd)"

# `synth` renders into the operator's wiki, which is the one thing here the harness must not
# require. SPIRA_WIKI is empty on a clone that has no wiki, and `synth` says so and stops
# rather than deriving a path that happens to resolve (rule 2 of the boundary).
#
# But SPIRA_WIKI unconditionally is wrong when the caller is already standing in a WORKTREE
# of it — an Ops aeon works in its own worktree of the wiki repo, and a worktree can commit
# only its own tree. Writing into SPIRA_WIKI itself lands the regenerated page on the shared
# checkout every other worktree hangs off, uncommitted, for the next `git add -A` anywhere in
# it to sweep up. Resolve by GIT COMMON DIR, not by path name: every worktree of one
# repository shares a common dir, and that is the one comparison a worktree at an arbitrary
# path still passes — a bare `rev-parse --show-toplevel` would only agree by coincidence.
_sop_wiki_worktree_root() {
    local cwd_top cwd_git wiki_git
    cwd_top="$(git rev-parse --show-toplevel 2>/dev/null)" || return 1
    cwd_git="$(git -C "$cwd_top" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 1
    wiki_git="$(git -C "$SPIRA_WIKI" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 1
    [ "$cwd_git" = "$wiki_git" ] || return 1
    printf '%s\n' "$cwd_top"
}
OUT="${SOP_PAGE:-}"
if [ -z "$OUT" ] && [ -n "${SPIRA_WIKI:-}" ]; then
    _sop_wiki_root="$(_sop_wiki_worktree_root || true)"
    OUT="${_sop_wiki_root:-$SPIRA_WIKI}/wiki/notes/standard-operating-procedures.md"
fi
WORD_CAP="${SOP_WORD_CAP:-250}"

# WHERE THE APPLICATIONS LEDGER LIVES. Under the runtime directory and so NOT a config key,
# by the same rule as the gate log and the yield record: the harness put it there, and a
# colleague who moves SPIRA_RUN moves this with it. The environment may still point it
# elsewhere, which is the seam a suite drives so that a fixture's planted records never land
# in the real count.
#
# NOTHING PRUNES IT. Every other record here has a retention because it is a queue of recent
# events; this one is the long-run answer to "how often did that runbook actually work", and
# a count that forgets its own history cannot answer it. One line per incident per runbook is
# a few kilobytes a year.
LEDGER="${SPIRA_SOP_LEDGER:-$SPIRA_RUN/sop/applied.jsonl}"
# How much of `--why` survives into the ledger line. The full text goes on the bead note; this
# bound is what keeps a line short enough that its append stays atomic against a second Ops
# session writing at the same moment.
WHY_CAP="${SOP_WHY_CAP:-400}"

usage() { sed -n '3,17p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }
slugify() { printf 'sop-%s' "${1#sop-}"; }
# A flag proves its value is present before taking it: `shift 2` with one argument left shifts
# nothing at all, and the parse loop then spins forever on the same token.
need() { [ "$1" -ge 2 ] || { echo "sop: $2 needs a value" >&2; exit 1; }; }

# Read from a file, from stdin on `-`, or from stdin when nothing is named. Never from an
# argument: prose in a shell argument is how backticks and $( ) become command substitution
# (law-commit-messages-via-stdin).
slurp() {
    case "${1:--}" in
        -) cat ;;
        *) [ -f "$1" ] || { echo "sop: no such file: $1" >&2; exit 1; }; cat "$1" ;;
    esac
}

# All SOPs as JSON, {key: text}. One query, so every subcommand costs the same.
shelf() {
    bdjson memories 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = {}
print(json.dumps({k: v.strip() for k, v in sorted(d.items())
                  if isinstance(v, str) and k.startswith("sop-")}))'
}

case "${1:-}" in

write)
    [ $# -ge 2 ] || usage
    key="$(slugify "$2")"
    text="$(slurp "${3:--}")"
    [ -n "${text//[[:space:]]/}" ] || { echo "sop: refusing — empty SOP" >&2; exit 1; }

    # Fail closed on shape. A malformed SOP that is stored anyway is worse than a rejected
    # one, because it reads as a runbook right up to the moment someone needs it at 3am.
    missing=""
    for field in SYMPTOM CHECK FIX; do
        grep -qE "^[[:space:]]*$field:" <<< "$text" || missing="$missing $field"
    done
    if [ -n "$missing" ]; then
        echo "sop: refusing — missing required field(s):$missing" >&2
        echo "     An SOP is SYMPTOM / CHECK / FIX at minimum, with optional MATCH," >&2
        echo "     ESCALATE and REF. A paragraph cannot be matched or executed." >&2
        exit 1
    fi

    # A MATCH line must be a regex grep can actually compile, checked here rather than
    # discovered by `sop.sh match` returning nothing during an incident.
    re="$(sed -n 's/^[[:space:]]*MATCH:[[:space:]]*//p' <<< "$text" | head -1)"
    if [ -n "$re" ]; then
        # grep exits 1 for "compiled fine, matched nothing" and 2 for "will not compile".
        # Only the second is an error, which is why this tests the status rather than
        # whether anything matched.
        printf '' | grep -E -- "$re" >/dev/null 2>&1
        if [ "$?" -ge 2 ]; then
            echo "sop: refusing — MATCH is not a valid extended regex: $re" >&2
            exit 1
        fi
    fi

    words=$(wc -w <<<"$text")
    if [ "$words" -gt "$WORD_CAP" ]; then
        echo "sop: refusing — ${words} words, cap is ${WORD_CAP}." >&2
        echo "     Every Ops session pays for every SOP. Keep the procedure here and put" >&2
        echo "     the narrative behind a REF: line pointing at a wiki page." >&2
        exit 1
    fi

    # SOPs ship in this repository and are scanned by inventory.sh on every landing.
    # A CHECK or FIX step that names an operator-specific absolute path would block every
    # branch that calls `sop.sh write`. Use $SPIRA_DB, $SPIRA_HOME, or other env vars from
    # conf.sh instead — those expand to the right paths on any clone.
    inv_hits="$(printf '%s\n' "$text" | bash "$(dirname "$0")/inventory.sh" --scan /dev/stdin 2>/dev/null)"
    if [ -n "$inv_hits" ]; then
        echo "sop: refusing — SOP text names operator infrastructure:" >&2
        printf '%s\n' "$inv_hits" | sed 's/^/     /' >&2
        echo "     Use env vars (\$SPIRA_DB, \$SPIRA_HOME, …) instead of absolute paths." >&2
        exit 1
    fi

    bdq remember --key "$key" "$text" >/dev/null || {
        echo "sop: failed to write $key to $SPIRA_DB" >&2; exit 1; }
    echo "wrote $key (${words} words)"
    [ -n "$re" ] && echo "  matches: $re" || echo "  no MATCH: line — recall falls back to key tokens"
    "$0" synth
    ;;

show)
    [ $# -eq 2 ] || usage
    bdq recall "$(slugify "$2")" 2>/dev/null || {
        echo "sop: no such SOP: $(slugify "$2")" >&2; exit 1; }
    ;;

list)
    shelf | python3 -c '
import sys, json, re
d = json.load(sys.stdin)
for k, v in d.items():
    sym = ""
    m = re.search(r"^\s*SYMPTOM:\s*(.+)$", v, re.M)
    if m: sym = m.group(1).strip()
    print(f"  {k:<40} {len(v.split()):>3}w  {sym[:60]}")
print(f"\n{len(d)} SOP(s) on the shelf")'
    ;;

match)
    # The deterministic tier. Every SOP whose MATCH regex fires against the payload, best
    # first by number of distinct lines hit; SOPs with no MATCH line fall back to their key
    # tokens, which is weak on purpose — it is a nudge to go and write a MATCH.
    payload="$(slurp "${2:--}")"
    shelf | SOP_PAYLOAD="$payload" python3 -c '
import sys, json, os, re
payload = os.environ.get("SOP_PAYLOAD", "")
d = json.load(sys.stdin)
hits = []
for k, v in d.items():
    m = re.search(r"^\s*MATCH:\s*(.+)$", v, re.M)
    sym = re.search(r"^\s*SYMPTOM:\s*(.+)$", v, re.M)
    sym = sym.group(1).strip() if sym else ""
    score, how = 0, ""
    if m:
        try:
            pat = re.compile(m.group(1).strip(), re.I | re.M)
            score = len({l for l in payload.splitlines() if pat.search(l)})
            how = "MATCH"
        except re.error:
            score, how = 0, "BAD-REGEX"
    else:
        toks = [t for t in k[len("sop-"):].split("-") if len(t) > 3]
        score = sum(1 for t in toks if re.search(re.escape(t), payload, re.I))
        how = "key-tokens"
    if score > 0:
        hits.append((score, k, how, sym))
for score, k, how, sym in sorted(hits, reverse=True):
    print(f"{k}\t{how}\t{score}\t{sym}")
' 2>/dev/null
    ;;

applied)
    # THE RECORD. Written between the CHECK and the FIX with `--held unknown`, and again once
    # the fix has been verified. See the header for why it goes to two places and why almost
    # nothing here is a refusal.
    #
    # --bead IS FOR INCIDENT BEADS; --pass IS FOR BEADLESS SWEEP PASSES. The join target
    # changes — a sweep pass has a stable pass id but no bead — so the ledger carries either
    # `"bead"` or `"pass"` as the identity key, never both. Two records from the same sweep
    # pass carry the same pass id and join on it. A record with neither is still refused: the
    # reasoning that a record nobody can join back to something counts nothing still applies.
    [ $# -ge 2 ] || usage
    key="$(slugify "$2")"; shift 2
    bead=""; passid=""; check=""; held=""; why_src=""; have_why=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --bead)  need $# --bead;  bead="$2";    shift 2 ;;
            --pass)  need $# --pass;  passid="$2";  shift 2 ;;
            --check) need $# --check; check="$2";   shift 2 ;;
            --held)  need $# --held;  held="$2";    shift 2 ;;
            --why)   need $# --why;   why_src="$2"; have_why=1; shift 2 ;;
            *) echo "sop: applied: unexpected argument: $1" >&2; usage ;;
        esac
    done

    [ -n "$bead" ] || [ -n "$passid" ] || { echo "sop: applied needs --bead <id> or --pass <id> — a record nobody can join back to an incident counts nothing" >&2; exit 1; }
    case "$check" in
        pass|fail) ;;
        *) echo "sop: applied needs --check pass|fail — did the SOP's CHECK confirm this really is that failure?" >&2; exit 1 ;;
    esac
    case "$held" in
        yes|no|unknown) ;;
        *) echo "sop: applied needs --held yes|no|unknown — did the FIX resolve it?" >&2
           echo "     yes      it fit, it held, and it taught us nothing new. Say this; it is a real outcome." >&2
           echo "     no       it fit and the fix did NOT hold. The runbook needs amending." >&2
           echo "     unknown  too early to tell, or the CHECK did not confirm so no FIX was run." >&2
           exit 1 ;;
    esac
    # THE ONE INCOHERENT COMBINATION. A CHECK that did not confirm means the SOP does not
    # apply and its FIX was never run, so it cannot have held. Refused rather than recorded,
    # because it is a slip at the keyboard and a count that contains it is worse than one
    # short by a line.
    if [ "$check" = fail ] && [ "$held" = yes ]; then
        echo "sop: refusing — --check fail --held yes. A CHECK that did not confirm means the" >&2
        echo "     SOP does not apply and its FIX was never run, so nothing of it can have held." >&2
        echo "     Record --held unknown and diagnose instead." >&2
        exit 1
    fi

    why=""
    [ "$have_why" = 1 ] && why="$(slurp "$why_src")"

    # IS THE SLUG REAL? Only answerable when the shelf can be read at all, and the difference
    # matters: `bd memories --json` prints nothing when the query fails and an object when the
    # shelf is genuinely empty, so an empty STRING is the broken case and `{}` is the honest
    # one. Conflating them would refuse every record on the day the database is down, which is
    # the day a record is worth most.
    raw="$(bdjson memories 2>/dev/null)"
    if [ -z "${raw//[[:space:]]/}" ]; then
        shelf_state="unreadable"
        echo "sop: warning — could not read the shelf from $SPIRA_DB; recording $key unverified" >&2
    else
        shelf_state="ok"
        if ! SOP_KEY="$key" python3 -c '
import sys, json, os
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)   # unparseable is the unreadable case, handled above
sys.exit(0 if os.environ["SOP_KEY"] in d else 1)' <<< "$raw"; then
            echo "sop: refusing — no such SOP: $key. \`sop.sh list\` shows the shelf." >&2
            exit 1
        fi
    fi

    # METRIC ENFORCEMENT. A SOP that declares METRIC: <KEY> <SUBCMD> verifies that the named
    # cockpit metric has reached 0 before held=yes can be recorded. Re-reading the metric via
    # the detector that triggered the incident is the only way to confirm the fix held: a CHECK
    # that implements its own scope in prose can look in the wrong place, find zero, and record
    # held=yes for an unchanged metric — which is exactly what produced 17 false held=yes
    # entries for sop-unadopted-refs-stale-db (sp-gt7k). The rule: held=yes is only recordable
    # when the detector's own number has actually changed; otherwise held=unknown.
    #
    # SOP_METRIC_COCKPIT overrides the cockpit script path, which is the seam a test drives so
    # that the probe returns a controlled value rather than walking real git repositories.
    #
    # A METRIC FIELD IS OPT-IN. SOPs that do not declare METRIC are unaffected: held=yes passes
    # through as before. The enforcement only fires when the SOP author said "verify this number."
    #
    # IF THE SHELF IS UNREADABLE, the METRIC cannot be extracted and enforcement is skipped.
    # Recording held=yes when the database is down is better than refusing to record anything,
    # because the day the harness is broken is the day a record is worth most.
    #
    # law-absence-needs-a-positive-control: a metric that cannot be read is not reporting zero.
    _metric_note=""
    if [ "$held" = yes ] && [ "$shelf_state" = ok ]; then
        _metric_spec="$(SOP_KEY="$key" python3 -c '
import sys, json, re, os
try: d = json.load(sys.stdin)
except Exception: sys.exit()
text = d.get(os.environ.get("SOP_KEY", ""), "")
m = re.search(r"^\s*METRIC:\s*(.+)$", text, re.M)
if m: print(m.group(1).strip())
' <<< "$raw" 2>/dev/null || true)"
        if [ -n "$_metric_spec" ]; then
            _metric_key="${_metric_spec%% *}"
            _metric_subcmd="${_metric_spec#* }"
            # A single-token METRIC has no subcommand — malformed; skip enforcement so the
            # lint error is what the author sees, not a silent pass.
            if [ -n "$_metric_key" ] && [ -n "$_metric_subcmd" ] && [ "$_metric_key" != "$_metric_subcmd" ]; then
                _cockpit_path="${SOP_METRIC_COCKPIT:-$HERE/cockpit.sh}"
                _metric_raw="$(timeout 30 bash "$_cockpit_path" "$_metric_subcmd" 2>/dev/null \
                    | grep "^${_metric_key}=" | sed "s/^${_metric_key}=//" | head -1)" \
                    || _metric_raw=""
                _metric_fixed="$(METRIC_V="$_metric_raw" python3 -c '
import os
v = os.environ.get("METRIC_V", "")
print("yes" if v.isdigit() and int(v) == 0 else "no")
' 2>/dev/null || echo no)"
                if [ "$_metric_fixed" = yes ]; then
                    _metric_note="METRIC ${_metric_key}=0: fix confirmed by cockpit.sh ${_metric_subcmd}."
                else
                    held=unknown
                    _raw_display="${_metric_raw:-?}"
                    _metric_note="METRIC ${_metric_key}=${_raw_display} (still nonzero or unreadable); held downgraded from yes to unknown — the fix has not cleared the metric that triggered this incident."
                fi
            fi
        fi
    fi

    # ONE CLOCK READ, not two. Separate `date` calls can straddle a second boundary and put a
    # timestamp and an epoch that disagree onto the same line, which is the sort of thing
    # nobody notices until they are reconciling two records a year later.
    read -r epoch ts <<< "$(date -u '+%s %Y-%m-%dT%H:%M:%SZ')"
    actor="${BEADS_ACTOR:-${SPIRA_AEON:+aeon-$SPIRA_AEON}}"; actor="${actor:-${USER:-unknown}}"

    # WHAT THE NOTE SAYS WHEN THE AEON SAID NOTHING. The outcome is rendered as a sentence
    # rather than left as three flags, because the reader of a bead note is a person holding
    # an incident and not this program's usage text — and because `--held yes` has to READ as
    # a complete outcome, not as a blank.
    case "$check:$held" in
        fail:*)    verdict="The CHECK did not confirm: this SOP does not apply to this incident. Its MATCH fired anyway, which is a fact about the regex." ;;
        pass:yes)  verdict="The SOP fit, it held, and it taught us nothing new." ;;
        pass:no)   verdict="The SOP fit and its FIX did NOT hold. The runbook needs amending, not the threshold." ;;
        pass:unknown) verdict="The CHECK confirmed. Whether the FIX held is not known yet." ;;
    esac

    # THE BEAD NOTE IS ONLY FOR BEAD-BACKED RECORDS. A sweep pass has no bead and nowhere
    # to put one. `note_state` is "n/a" for pass records so the ledger line is honest about
    # why it carries no note, and callers that check for "failed" can still trust it.
    if [ -n "$bead" ]; then
        note_state="ok"
        {
            printf 'SOP %s applied — CHECK %s, held=%s.\n\n%s\n' "$key" "$check" "$held" "$verdict"
            [ -n "${_metric_note:-}" ] && printf '\n%s\n' "$_metric_note"
            [ -n "${why//[[:space:]]/}" ] && printf '\n%s\n' "$why"
            printf '\nRecorded %s by %s. Ledger: %s\n' "$ts" "$actor" "$LEDGER"
        } | bdq note "$bead" --stdin >/dev/null 2>&1 || note_state="failed"
    else
        note_state="n/a"
    fi

    # THE LEDGER LINE IS WRITTEN LAST, so it can say whether the note landed. A half-written
    # record that admits which half is missing is worth more than one that does not.
    # THE IDENTITY KEY IS EITHER "bead" OR "pass" — never both, never absent. Downstream
    # readers (sop.sh log --bead, sop.sh log --pass) filter on whichever key is present.
    mkdir -p "$(dirname "$LEDGER")" || { echo "sop: cannot create ledger directory" >&2; exit 1; }
    line="$(SOP_TS="$ts" SOP_EPOCH="$epoch" SOP_SOP="$key" SOP_BEAD="$bead" SOP_PASS="$passid" \
            SOP_CHECK="$check" SOP_HELD="$held" SOP_WHY="$why" SOP_CAP="$WHY_CAP" \
            SOP_ACTOR="$actor" SOP_SHELF="$shelf_state" SOP_NOTE="$note_state" \
            python3 -c '
import os, json
w = " ".join(os.environ.get("SOP_WHY", "").split())[: int(os.environ["SOP_CAP"])]
bead = os.environ.get("SOP_BEAD", "")
passid = os.environ.get("SOP_PASS", "")
rec = {
    "ts":    os.environ["SOP_TS"],
    "epoch": int(os.environ["SOP_EPOCH"]),
    "sop":   os.environ["SOP_SOP"],
}
# Identity key: "bead" for incident-backed records, "pass" for beadless sweep passes.
if bead:
    rec["bead"] = bead
else:
    rec["pass"] = passid
rec.update({
    "check": os.environ["SOP_CHECK"],
    "held":  os.environ["SOP_HELD"],
    "actor": os.environ["SOP_ACTOR"],
    "shelf": os.environ["SOP_SHELF"],
    "note":  os.environ["SOP_NOTE"],
    "why":   w,
})
print(json.dumps(rec, separators=(",", ":"), ensure_ascii=False))')"
    [ -n "$line" ] || { echo "sop: failed to render the ledger line — nothing recorded" >&2; exit 1; }
    printf '%s\n' "$line" >> "$LEDGER" || {
        echo "sop: failed to append to $LEDGER" >&2; exit 1; }

    target="${bead:-pass=$passid}"
    echo "recorded $key on $target — check=$check held=$held"
    echo "  $verdict"
    if [ "$note_state" = failed ]; then
        echo "sop: the ledger line was written but the note on $bead was NOT — the human reading" >&2
        echo "     that incident will not see this. Add it by hand: bd -C $SPIRA_DB note $bead --stdin" >&2
        exit 1
    fi
    ;;

log)
    # THE READ SIDE, and the reason it lives here rather than in each of its callers: the
    # ledger's format is this program's business, and three consumers writing three parsers
    # is three ways to disagree about what a record means.
    #
    # THE EXIT STATUS IS THE POINT, and it has three values because absence and blindness are
    # different answers that a two-valued status would merge — the merged one reading as
    # all-clear (law-absence-needs-a-positive-control):
    #
    #   0   at least one matching record; the lines are on stdout
    #   1   the ledger was READ and holds no matching record. A true absence.
    #   2   the ledger could not be read at all. NOT an absence; do not treat it as one.
    #
    # THE FILTERS ARE HERE AND NOT IN THE CALLERS, for the reason above. `--check` and
    # `--since` exist because the closing-rule check in aeon.sh asks a narrower question than
    # "has anything ever been recorded against this bead": it asks whether THIS SESSION
    # recorded that a runbook actually fit. A caller grepping the lines for `"check":"pass"`
    # would be a second parser of this format, and it would also lose the three-valued exit —
    # which is the only thing that separates "no such record" from "cannot read the ledger".
    shift || true
    fb=""; fp=""; fs=""; fc=""; fsince=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --bead)  need $# --bead;  fb="$2";               shift 2 ;;
            --pass)  need $# --pass;  fp="$2";               shift 2 ;;
            --sop)   need $# --sop;   fs="$(slugify "$2")";  shift 2 ;;
            --check) need $# --check; fc="$2";               shift 2 ;;
            --since) need $# --since; fsince="$2";           shift 2 ;;
            *) echo "sop: log: unexpected argument: $1" >&2; usage ;;
        esac
    done
    case "$fc" in
        ""|pass|fail) ;;
        *) echo "sop: log: --check takes pass or fail, not '$fc'" >&2; exit 1 ;;
    esac
    # A NON-NUMERIC --since IS REFUSED RATHER THAN COERCED. Silently reading it as 0 would
    # widen the filter to the whole ledger, and a filter that fails open reports records the
    # caller did not ask for as though they were the ones it did.
    case "$fsince" in
        ""|*[!0-9]*) [ -z "$fsince" ] || { echo "sop: log: --since takes a unix epoch, not '$fsince'" >&2; exit 1; } ;;
    esac
    if [ ! -f "$LEDGER" ]; then
        echo "sop: no ledger at $LEDGER — nothing has ever been recorded, or it is not where this program looks." >&2
        exit 2
    fi
    SOP_FB="$fb" SOP_FP="$fp" SOP_FS="$fs" SOP_FC="$fc" SOP_FSINCE="$fsince" python3 -c '
import sys, os, json
fb, fp = os.environ.get("SOP_FB", ""), os.environ.get("SOP_FP", "")
fs = os.environ.get("SOP_FS", "")
fc, fsince = os.environ.get("SOP_FC", ""), os.environ.get("SOP_FSINCE", "")
fsince = int(fsince) if fsince else None
seen = bad = shown = 0
try:
    fh = open(sys.argv[1], encoding="utf-8", errors="replace")
except OSError as e:
    print("sop: cannot read the ledger: %s" % e, file=sys.stderr); sys.exit(2)
for ln in fh:
    ln = ln.strip()
    if not ln: continue
    seen += 1
    try: r = json.loads(ln)
    except Exception: bad += 1; continue
    if fb and r.get("bead") != fb: continue
    if fp and r.get("pass") != fp: continue
    if fs and r.get("sop")  != fs: continue
    if fc and r.get("check") != fc: continue
    # A LINE WITH NO PARSEABLE EPOCH IS OUTSIDE EVERY --since WINDOW, never inside one. A
    # record whose timestamp cannot be read says nothing about when it was made, and the
    # caller asking "was this recorded since T" is entitled to a no.
    if fsince is not None:
        try:
            if int(r.get("epoch")) < fsince: continue
        except (TypeError, ValueError): continue
    print(ln); shown += 1
# A FILE THAT HAS LINES AND NONE OF THEM PARSE IS BROKEN, NOT EMPTY. Reporting that as "no
# records" is the reading that stops anybody looking.
if seen and bad == seen:
    print("sop: %d ledger line(s) and not one parsed as JSON — this ledger is corrupt, not empty" % seen,
          file=sys.stderr)
    sys.exit(2)
sys.exit(0 if shown else 1)
' "$LEDGER"
    ;;

digest)
    # WHAT THE SHELF HOLDS RIGHT NOW, one `<key> <sha256-of-text>` line per SOP, sorted by key.
    #
    # WHY A DIGEST AND NOT A COUNT. `bd remember` upserts, so amending an existing runbook —
    # which is one of the honest ways an Ops session can end — leaves the shelf exactly the
    # size it was. A per-key hash makes a new SOP and an edited one the same observation,
    # which is what a caller comparing two of these actually wants to know.
    #
    # WHY IT IS NOT A SINGLE HASH OF EVERYTHING. A whole-shelf hash cannot tell an addition
    # from a RETIREMENT, and those are opposite facts: writing a runbook discharges the
    # closing rule and removing one does not. Line-wise, a caller checks for a line present
    # after and absent before, and a retirement simply produces no such line.
    #
    # THE EXIT STATUS CARRIES THE DIFFERENCE BETWEEN EMPTY AND BLIND, like `log` above:
    #   0  the shelf was read; its lines are on stdout, and no lines means it is genuinely bare
    #   2  the shelf could NOT be read. Not an empty shelf; do not compare two of these.
    # `bd memories --json` prints nothing when the query fails and `{}` when the shelf is
    # honestly empty, so the empty STRING is the broken case. Conflating them would make a
    # database outage look like a session that wrote nothing, which is the reading that gets
    # a good session punished (law-absence-needs-a-positive-control).
    [ $# -eq 1 ] || usage
    raw="$(bdjson memories 2>/dev/null)"
    if [ -z "${raw//[[:space:]]/}" ]; then
        echo "sop: could not read the shelf from $SPIRA_DB — this is not an empty shelf" >&2
        exit 2
    fi
    printf '%s' "$raw" | python3 -c '
import sys, json, hashlib
try: d = json.load(sys.stdin)
except Exception:
    print("sop: the shelf did not parse as JSON — this is not an empty shelf", file=sys.stderr)
    sys.exit(2)
if not isinstance(d, dict):
    print("sop: the shelf did not parse as an object — this is not an empty shelf", file=sys.stderr)
    sys.exit(2)
for k, v in sorted(d.items()):
    if not k.startswith("sop-") or not isinstance(v, str): continue
    # Hashed on the STRIPPED text, the same normalisation `shelf` applies, so a trailing
    # newline gained or lost in transit is not reported as an amendment nobody made.
    print("%s %s" % (k, hashlib.sha256(v.strip().encode("utf-8")).hexdigest()))'
    ;;

ledger-init)
    # THE INSTRUMENT, BEFORE ITS SILENCE IS BELIEVED. `log` answers 2 — unreadable — when
    # there is no ledger file at all, and it is right to: an absent file and a misconfigured
    # path are the same observation from inside this program. But a caller that must judge
    # "did this session record anything" needs `log`'s 1 to be reachable on a fresh install,
    # where nothing has ever been recorded and so nothing has ever created the file.
    #
    # So the caller creates it first, and an EMPTY ledger is a real ledger: from then on 1
    # means "read it, nothing there" and 2 means the read genuinely failed. This is the
    # positive control the closing-rule check rests on — it makes absence a thing that can be
    # observed rather than inferred from a missing file.
    #
    # `applied` still creates the ledger on its own first write, so this is never required;
    # it only removes an ambiguity for whoever is about to act on a silence.
    [ $# -eq 1 ] || usage
    if [ -e "$LEDGER" ]; then
        echo "ledger present: $LEDGER"
    else
        mkdir -p "$(dirname "$LEDGER")" || { echo "sop: cannot create ledger directory for $LEDGER" >&2; exit 1; }
        : >> "$LEDGER" || { echo "sop: cannot create the ledger at $LEDGER" >&2; exit 1; }
        echo "created empty ledger: $LEDGER"
    fi
    ;;

retire)
    [ $# -eq 2 ] || usage
    key="$(slugify "$2")"
    bdq forget "$key" >/dev/null 2>&1 || { echo "sop: no such SOP: $key" >&2; exit 1; }
    echo "retired $key"
    "$0" synth
    echo
    echo "Retire an SOP the way a statute is retired: remove it. Do not leave it standing"
    echo "with a correction attached — that is a stale runbook with a warning label."
    ;;

synth)
    # REGENERATED WHOLE, NEVER PATCHED (law-regenerate-derived-summaries). Editing the page
    # does nothing; amend the SOP. It exists for the same two reasons common-law.md does:
    # the Dolt store is gitignored and backed up nowhere, so this is the only copy of the
    # runbooks that leaves the building, and it is the only one readable without any of this
    # tooling — which is the situation you are in when something has already failed.
    if [ -z "$OUT" ]; then
        echo "sop: no wiki configured (SPIRA_WIKI) — nothing to synthesise into." >&2
        echo "sop: the SOPs themselves are in the database; \`sop.sh list\` reads them." >&2
        exit 0
    fi
    today="$(TZ="${SPIRA_TZ:-${TZ:-}}" date '+%Y-%m-%d')"
    # The shelf goes to a FILE, not down a pipe. A heredoc-fed `python3 -` owns stdin, so
    # `shelf | python3 - <<PY` silently hands the script an empty stdin and it renders an
    # empty page over a good one — a derived document that regenerates itself to nothing.
    # A path also has no ARG_MAX ceiling, which argv would.
    shelf_file="$(mktemp)"; trap 'rm -f "$shelf_file"' EXIT
    shelf > "$shelf_file"
    python3 - "$today" "$OUT" "$shelf_file" <<'PY'
import json, sys, re, textwrap
today, out = sys.argv[1], sys.argv[2]
sops = json.load(open(sys.argv[3]))
if not isinstance(sops, dict):
    sys.exit("sop-synth: refusing — the shelf did not parse; the page is left alone")

def field(v, name):
    m = re.search(rf"^\s*{name}:\s*(.*?)(?=^\s*(?:MATCH|SYMPTOM|CHECK|METRIC|FIX|ESCALATE|REF):|\Z)",
                  v, re.M | re.S)
    return m.group(1).strip() if m else ""

b = []
b += ["---", "type: note", "created: 2026-09-05", f"updated: {today}",
      "tags: [spira, ops, sop, runbook, generated]",
      "aliases: [SOPs, Standard operating procedures, The shelf]", "---", ""]
b += ["# Standard operating procedures", ""]
b += ["**Generated — do not edit.** Regenerated whole by the harness's `spira/sop.sh synth` from "
      "the Spira beads database, which is the source of truth. Editing this page has no "
      "effect; the next run overwrites it. Amend an SOP instead:", ""]
b += ["```bash", "spira/sop.sh write <slug> -   # text on stdin", "```", ""]
b += ["Statutes are how to behave; SOPs are how to fix. They share one mechanism, split by "
      "prefix — `law-` and `sop-` — so the [[spira]] Ops persona reads its runbooks exactly "
      "the way every agent already reads [[common-law]]. Ops is summoned by an incident bead "
      "filed from a failed systemd unit, matches the payload against the `MATCH:` lines "
      "below, and executes the first one that fires.", ""]
b += [f"**{len(sops)} SOP(s)** on the shelf as of {today}.", ""]
b += ["## The closing rule", ""]
b += ["**An incident resolved without an SOP must produce one.** This is "
      "`law-bake-rules-into-tools` applied to production, and it is enforced rather than "
      "asked for: writing an SOP is what regenerates this page, the regenerated page is the "
      "commit that names the incident bead, and a bead closed with no commit naming it is "
      "reopened by `aeon.sh`. An incident fixed by hand and forgotten does not close.", ""]
b += ["## The shelf", ""]
if not sops:
    b += ["*Empty.* The first incident to be resolved fills it.", ""]
for k, v in sops.items():
    b += [f"### {k[len('sop-'):].replace('-', ' ').capitalize()}", "", f"`{k}`", ""]
    for name, label in (("SYMPTOM", "Symptom"), ("CHECK", "Check"), ("METRIC", "Metric"),
                        ("FIX", "Fix"), ("ESCALATE", "Escalate"), ("REF", "Reference")):
        val = field(v, name)
        if not val:
            continue
        if "\n" in val or val.strip().startswith(("$", "sudo", "systemctl", "bd ", "git ")):
            b += [f"**{label}**", "", "```", *val.splitlines(), "```", ""]
        else:
            b += [f"**{label}** — {' '.join(textwrap.wrap(val, 10000))}", ""]
    m = re.search(r"^\s*MATCH:\s*(.+)$", v, re.M)
    b += [f"**Matches** `{m.group(1).strip()}`" if m else
          "**Matches** — no `MATCH:` line; this SOP is found by key tokens only, which is "
          "weak. Add one.", ""]
b += ["Related: [[spira]], [[common-law]], [[codified-judgement]]", ""]
open(out, "w").write("\n".join(b))
print(f"sop-synth: wrote {out} — {len(sops)} SOP(s)")
PY
    ;;

lint)
    # VALIDATES EVERY SOP ON THE SHELF AGAINST THE SAME RULES THAT `write` ENFORCES.
    # `bd remember sop-<slug>` bypasses those rules; lint is what catches what slipped through.
    # Fails closed: an unreadable shelf is not a clean shelf — a broken database is not evidence
    # that no malformed SOPs exist (law-absence-needs-a-positive-control).
    [ $# -eq 1 ] || usage
    raw="$(bdjson memories 2>/dev/null)"
    if [ -z "${raw//[[:space:]]/}" ]; then
        echo "sop: lint: could not read the shelf from $SPIRA_DB — refusing to report clean" >&2
        exit 1
    fi
    SOP_LINT_CAP="$WORD_CAP" printf '%s\n' "$raw" | python3 -c '
import sys, json, re, os

try:
    d = json.load(sys.stdin)
except Exception:
    print("sop: lint: shelf did not parse as JSON — refusing to report clean", file=sys.stderr)
    sys.exit(1)
if not isinstance(d, dict):
    print("sop: lint: shelf is not an object — refusing to report clean", file=sys.stderr)
    sys.exit(1)

sops = {k: v.strip() for k, v in d.items() if isinstance(v, str) and k.startswith("sop-")}
cap = int(os.environ.get("SOP_LINT_CAP", "250"))
failures = []

for key, text in sorted(sops.items()):
    reasons = []
    if not text.strip():
        reasons.append("empty SOP")
    else:
        for field in ("SYMPTOM", "CHECK", "FIX"):
            if not re.search(rf"^\s*{field}:", text, re.M):
                reasons.append("missing required field: " + field)
        m = re.search(r"^\s*MATCH:\s*(.+)$", text, re.M)
        if m:
            pat = m.group(1).strip()
            try:
                re.compile(pat)
            except re.error:
                reasons.append("MATCH is not a valid extended regex: " + pat)
        # METRIC is optional. If present, it must be exactly two tokens: an uppercase metric
        # key (SP_SOMETHING) and a lowercase cockpit subcommand (unsent, sops, …). A single
        # token has no subcommand and cannot be used by applied to re-read the metric; a key
        # with lowercase or a subcommand with uppercase signals the fields were swapped.
        m_metric = re.search(r"^\s*METRIC:\s*(.+)$", text, re.M)
        if m_metric:
            parts = m_metric.group(1).strip().split()
            if len(parts) != 2 \
               or not re.match(r"^[A-Z][A-Z0-9_]*$", parts[0]) \
               or not re.match(r"^[a-z][a-z0-9_-]*$", parts[1]):
                reasons.append(
                    "METRIC must be KEY SUBCMD — uppercase cockpit key then lowercase "
                    "cockpit subcommand (e.g. SP_UNADOPTED unsent); got: "
                    + m_metric.group(1).strip())
        words = len(text.split())
        if words > cap:
            reasons.append("%d words, cap is %d" % (words, cap))
    if reasons:
        for r in reasons:
            print("FAIL  %s: %s" % (key, r))
        failures.append(key)
    else:
        print("ok    %s" % key)

if failures:
    n = len(sops)
    print(
        "\n%d of %d SOP(s) failed — fix with sop.sh write or remove with sop.sh retire" % (len(failures), n),
        file=sys.stderr,
    )
    sys.exit(1)
else:
    n = len(sops)
    print("ok — %s" % ("%d SOP(s) on the shelf, all valid" % n if n else "shelf is empty"))
' || exit 1
    ;;

*) usage ;;
esac
