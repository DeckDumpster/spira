#!/usr/bin/env bash
#
# ask — put a question, decision, insight or event in front of the operator, stored as a bead.
#
#   ask.sh add "<question>"        --default "<what I'd do>" [--why "<what is blocked>"] [--moot-when "<cmd>"] [--priority 0-4]
#   ask.sh decide "<the choice>"   --default "<what I'd do>" [--why ...]
#   ask.sh law "<slug>: <statute text>"                       [--why "<context>"]
#   ask.sh insight "<what was learned>" [--why "<why it matters>"] [--from "<who is recording>"]
#   ask.sh note "<what happened>" --kind <event.kind> [--why ...] [--target <bead>]
#   ask.sh suit "<statute-slug>"   [--why "<evidence>"] [--evidence "..."]
#   ask.sh list [needs-you|insights|events|all]
#   ask.sh rejected
#   ask.sh answered <bead-id> "<verdict>"
#   ask.sh drop <bead-id> "<reason>"
#
# WHY BEADS AND NOT A FILE
# ------------------------
# This used to write .runtime/asks.jsonl, a hand-rolled store, and it lost data: ids were a
# truncated timestamp plus a truncated random suffix, two asks filed in the same second
# collided, and because answering matched by id one reply closed both — recording a verdict
# against a question the operator never answered. Beads gives real identity, status, close reasons
# and comments, and one store rather than two. (The operator, verbatim: "the current data
# model is guaranteeing sync issues and data loss".)
#
# THE FOUR KINDS, AND THE TYPES THAT ACTUALLY EXIST
#   question  -> type `decision`, label $SPIRA_ASK_LABEL  open; the close reason IS the verdict
#   decision  -> type `decision`, label $SPIRA_ASK_LABEL  open; the close reason IS the verdict
#   insight   -> type `task`,     label insight      created CLOSED — there is nothing to do
#   note      -> type `event`,    NO labels at all   created CLOSED — an outcome, not work
#
# AN EVENT AND AN INSIGHT ARE DIFFERENT THINGS, and conflating them is what this verb exists
# to end. An EVENT is an outcome: what happened, written by the
# machinery, read and moved on from, with no thread. An INSIGHT is what was LEARNED during
# the work and might carry over — candidate law, doctrine or ruling — written by an agent.
# With only one bin, the machinery used the other one: three of the fourteen beads labelled
# `insight` were `PILGRIMAGE COMPLETE — <bead>: <title>` emitted by pilgrimage.sh (sp-94h,
# hq-5enm, sp-fv9, all now retyped). Those are outcomes wearing an insight's label, and they
# crowd out the thing the insights view is for.
#
# beads validates issue_type against a fixed set: task, bug, epic, feature, chore, decision,
# spike, rig, event. `question`, `insight`, `note` and `idea` are NOT types, whatever
# `bd create --dry-run` says — dry-run does not run the validator, so it accepted all four
# and the first real create rejected them. The taxonomy therefore lives in LABELS, with the
# type chosen from the real set.
#
# A question and a decision share the `decision` type deliberately rather than by compromise:
# the escalation policy already says an escalation is a decision request, not a problem
# report, so a question with a recommended default IS a decision awaiting a verdict.
#
# An insight exists because an answer that scrolls off screen is lost. It is neither a task
# nor a question, and before this there was nowhere to put one.
#
# `$SPIRA_ASK_LABEL` is the label on anything awaiting them — the SAME label the escalation gate and
# the mountain design already exclude, so a question can never be dispatched to a polecat as
# if it were work. No assignee is set, for the same reason: an assignee is how Gas Town routes
# work to an agent.
set -uo pipefail

# Every path comes from the harness's one configuration surface. It is two directories
# away because the cockpit ships beside the harness, not inside it.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../spira" && pwd -P)/conf.sh"
export BEADS_NO_AUTO_IMPORT=1
# THE database (the operator's call) — the same `COCKPIT_DB` every cockpit tool reads.
DB="$COCKPIT_DB"

bdt() { "${SPIRA_BD:-bd}" -C "$DB" "$@"; }
strip_warn() { grep -vE '^(warning:|  Fix:|  Or:)'; }

# _dedup_ask <ref> -> "open <id> <n>" | nothing
#
# CLIENT-SIDE FILTER ON external_ref. bd list --external-ref is silently ignored by
# bd-embedded, so every candidate must be verified via bd show. Only OPEN/in_progress
# beads are checked: a closed ask is an answered question, and a condition returning
# after the operator answered is new information, not a recurrence of the old one.
_dedup_ask() {
    local ref="$1"
    bdt list --status open,in_progress --label "$SPIRA_ASK_LABEL" --limit 0 --json 2>/dev/null \
      | python3 -c "
import sys, json, re, subprocess
target = sys.argv[1]
bd_bin = sys.argv[2]
db_dir = sys.argv[3]
try:
    for bead in json.load(sys.stdin):
        bid = bead.get('id')
        if not bid: continue
        try:
            show_out = subprocess.run([bd_bin, '-C', db_dir, 'show', bid, '--json'],
                                     capture_output=True, text=True, timeout=5)
            if show_out.returncode == 0:
                show_data = json.loads(show_out.stdout)
                if isinstance(show_data, list): show_data = show_data[0]
                if show_data.get('external_ref') == target:
                    ns = [int(m.group(1)) for lbl in (show_data.get('labels') or [])
                          for m in [re.match(r'^sp-recur-(\d+)\$', lbl)] if m]
                    print('open', bid, max(ns) if ns else 0); sys.exit(0)
        except: pass
except: pass
" "$ref" "${SPIRA_BD:-bd}" "$DB" 2>/dev/null
}

# `--default` is close to mandatory on a question: an ask without a recommendation makes the operator
# decide from scratch, which is what the escalation policy exists to prevent.
#
# AND ON AN OPERATOR ASK — one where they runs a command — put a machine check in the body:
#
#     VERIFY: <read-only shell command exiting 0 once the ask is already satisfied>
#
# `verify-asks.sh` runs those on a timer and closes the ones that pass, with the output as
# evidence. One ask requested a subscription, sat in the queue three days, and
# carried the exact proving command in its own text: *"ran those commands to verify, but i
# already did this days ago. close it out."* The check existed; nothing ran it.
# AN INSIGHT IS NOT AN ASK, AND ITS BODY MUST NOT READ LIKE ONE.
#
# (the operator, verbatim: "these insights seem more like bug reports which means they're
# implicitly asking me for feedback -- really they should be FYI only.") One `compose` wrote
# all three kinds, so every insight ever recorded opened with "**What is blocked:**" -- when
# nothing is blocked, that is the whole point of the kind -- and signed off by telling them to
# file a verdict against it. The framing they objected to was not a rendering choice; it was
# literally in the text, put there by this function.
#
# So the trailer is kind-dependent: an ask says how to answer it, a record says nothing is
# owed. `$1` is the kind, and the pane strips the old wording from the insights already
# stored, which cannot be rewritten.
compose() { # kind why default evidence from
    local kind="$1" why="$2" dflt="$3" ev="${4:-}" from="${5:-}" body=""
    if [ "$kind" = law ]; then
        # A LAW PROPOSAL LEADS WITH THE DECISION AND ITS DEFAULT. The operator reads the pane
        # top-to-bottom; a body that opens with statute text forces them to read the whole thing
        # before knowing what they are being asked. The first line states the question and the
        # default so the answer is ready before the reading begins.
        # The default is supplied by the caller (always "yes — enact it" for law proposals filed
        # without --default), so an empty dflt here is a caller bug rather than a valid state.
        body="**Enact this statute?** Default: ${dflt:-yes — enact it}."$'\n\n'
        [ -n "${STATUTE_TEXT:-}" ] && \
            body="${body}**Statute:**"$'\n\n'"${STATUTE_TEXT}"$'\n\n'
        [ -n "$why" ] && \
            body="${body}**Context:** ${why}"$'\n\n'
        [ -n "$ev" ] && \
            body="${body}**Evidence**"$'\n'"\`\`\`"$'\n'"${ev}"$'\n'"\`\`\`"$'\n\n'
        [ -n "${MOOT_WHEN:-}" ] && \
            body="${body}MOOT-WHEN: ${MOOT_WHEN}"$'\n\n'
        # THREE VERDICTS, executed by the panel against the statute book.
        body="${body}_Filed by the brain session. Verdict in the cockpit pane (⏎ to type):_"$'\n'
        body="${body}_• **enact** — writes the statute and closes this bead_"$'\n'
        body="${body}_• **amend: \<new text\>** — replaces the statute text before enacting_"$'\n'
        body="${body}_• **decline** — closes without enacting_"
        printf '%s' "$body"
        return
    fi
    [ -n "$dflt" ] && body="${body}**Default — what I would do:** ${dflt}"$'\n\n'
    if [ -n "$why" ]; then
        case "$kind" in
            # Nothing is blocked by an outcome — that is what makes it an outcome. Saying so
            # is the same defect as the insight that opened with "**What is blocked:**".
            event)   body="${body}**Detail:** ${why}"$'\n\n' ;;
            insight) body="${body}**Why it matters:** ${why}"$'\n\n' ;;
            # A lawsuit is a challenge to existing law, not a task waiting on a person.
            suit)    body="${body}**THE STATUTE ON TRIAL — \`${SUIT_SLUG:-?}\`, verbatim and in force today:**"$'\n\n'
                     body="${body}> $(printf '%s' "${SUIT_STATUTE:-(could not be read)}" | sed 's/^/> /; s/^> > /> /')"$'\n\n'
                     body="${body}**THE CHARGE — why it is wrong, over-broad or superseded:**"$'\n'"${why}"$'\n\n' ;;
            *)       body="${body}**What is blocked:** ${why}"$'\n\n' ;;
        esac
    fi
    # THE EVIDENCE TRAVELS WITH THE ASK. (the operator, verbatim: "with a failure like this,
    # i want to see the log itself in the decision pane. i don't know what's being asked of
    # me here!") An escalation naming a path they cannot open from the pane is a problem report
    # wearing a decision's clothes: they must go find the facts before they can even tell what is
    # being asked, which is the work the escalation existed to do for them.
    [ -n "$ev" ] && body="${body}**Evidence**"$'\n'"\`\`\`"$'\n'"${ev}"$'\n'"\`\`\`"$'\n\n'
    # MOOT-WHEN IS MACHINE-READABLE. moot-sweep.sh runs this command on a timer; exit 0
    # means the condition that fired this alert has cleared and the ask is resolved without
    # human involvement. Never set on events or insights — neither has a clearing condition.
    [ -n "${MOOT_WHEN:-}" ] && case "$kind" in
        event|insight) ;;
        *) body="${body}MOOT-WHEN: ${MOOT_WHEN}"$'\n\n' ;;
    esac
    if [ "$kind" = event ]; then
        # An event has no thread and nothing is owed, so it says neither how to answer it nor
        # how to dismiss it — it is read and moved on from. Saying anything else here is how
        # every insight ever recorded came to open with "**What is blocked:**".
        body="${body}_An outcome, recorded by the machinery. Nothing is owed and there is no thread._"
    elif [ "$kind" = insight ]; then
        local recorder="${from:-brain session}"
        body="${body}_Recorded by ${recorder}. Nothing is owed — this is a record, not a_"$'\n'
        body="${body}_request. Press \`d\` in the cockpit pane to dismiss it; \`h\` brings it back._"
    elif [ "$kind" = suit ]; then
        # THREE VERDICTS, unique to this kind. The panel executes the chosen one against the
        # statute book when the key is pressed; a failed rule.sh leaves this bead open.
        body="${body}_Filed by the brain session. Verdict in the cockpit pane (⏎ to type):_"$'\n'
        body="${body}_• **uphold** — closes, statute unchanged_"$'\n'
        body="${body}_• **retire** — removes the statute from the statute book_"$'\n'
        body="${body}_• **amend: \<new text\>** — replaces the statute with new text_"
    else
        body="${body}_Filed by the brain session. Answer inline in the cockpit pane, or:_"$'\n'
        body="${body}\`.claude/cockpit/ask.sh answered <id> \"<verdict>\"\`"
    fi
    printf '%s' "$body"
}

create() { # type text why default labels
    local type="$1" text="$2" why="$3" dflt="$4" labels="$5" out id kind=ask
    local prio="${PRIO:-1}"; local -a extra=()
    : "${EV:=}"
    if [ "$type" = event ]; then
        kind=event
        # P2, not P1. An event is a firehose and it is created closed, so its priority is
        # inert — but a stream of P1 rows is what every count of "urgent" reads off.
        prio=2
        # THE TAXONOMY IS THE POINT, so the kind is validated rather than trusted. Free text
        # here makes the panel's badge unreadable, and `event_kind` is varchar(32) — a longer
        # one is the database's problem to report, at write time, silently.
        case "${KIND:-}" in
            "") echo "ask: an event needs --kind (e.g. pilgrimage.complete, bead.landed, note)" >&2; return 1 ;;
            *[!a-z0-9.]*|.*|*.|*..*) echo "ask: --kind '$KIND' is not a taxonomy — use lowercase dotted segments, e.g. bead.landed" >&2; return 1 ;;
        esac
        [ "${#KIND}" -le 32 ] || { echo "ask: --kind '$KIND' is ${#KIND} chars; event_kind holds 32" >&2; return 1; }
        extra+=(--event-category "$KIND")
        [ -n "${TARGET:-}" ] && extra+=(--event-target "$TARGET")
        # A FENCE, because these labels are what every OTHER reader keys on. `overseer` is
        # what DECISIONS matches, so an event carrying it lands in the queue of things
        # awaiting the operator; `insight` puts it back in the bin this verb exists to empty;
        # and `spira`/`plan` on an event that ever reaches `bd ready` makes it claimable work
        # by an aeon. The emitters pass no labels at all, so this only ever fires on a new one.
        local bad
        # literal-ok: a deny-list of reserved names checked as DATA — it must name them
        for bad in overseer needs-ryan insight spira plan; do
            case ",$labels," in *",$bad,"*)
                echo "ask: refusing to label an event '$bad' — that label is how another reader claims or queues it" >&2
                return 1 ;;
            esac
        done
    fi
    # The kind is read off the labels rather than passed separately: the labels are already
    # what every reader — the pane, `ask.sh list`, watch-answers.sh — uses to tell an insight
    # from an ask, and a second source for the same fact is a second thing to get out of step.
    [ "$kind" = ask ] && case ",$labels," in
        *,insight,*)  kind=insight ;;
        *,ask-suit,*) kind=suit ;;
        *,ask-law,*)  kind=law ;;
    esac
    # THE ID COMES FROM `--json`, NEVER FROM A GREP OVER THE HUMAN OUTPUT.
    #
    # `bd create` prepends an advisory when a title looks like test data, and that advisory
    # ECHOES THE TITLE three times before the "Created issue:" line. `strip_warn` does not
    # match it — its lines begin `⚠`, `  Title:`, `  Recommendation:`. So the first
    # id-shaped token in the output is whatever the TITLE happens to contain, and the next
    # thing this script does with an insight's id is `bdt close "$id"`.
    #
    # Seen once, from the same grep run by hand: a title containing
    # "sp-pane-insights-fyi" yielded `sp-pane`, and the `bd delete` that followed removed the
    # sp-pane EPIC, its three labels and all seven of its dependency edges. Restored from
    # Dolt history; nothing about the command said it had addressed the wrong bead.
    out=$(bdt create --title "$text" --type "$type" -p "$prio" \
            --labels "$labels" -d "$(compose "$kind" "$why" "$dflt" "$EV" "${FROM:-}")" \
            ${extra+"${extra[@]}"} ${EXT_REF:+--external-ref "$EXT_REF"} --json 2>&1)
    id=$(python3 -c '
import json, re, sys
t = sys.stdin.read()
i = t.find("{")
if i >= 0:
    try:
        print(json.loads(t[i:])["id"]); raise SystemExit
    except (ValueError, KeyError):
        pass
# Fallback, anchored to the line that ANNOUNCES the id rather than to the whole output.
for line in t.splitlines():
    m = re.search(r"Created issue:\s+((?:hq|[a-z]{2})-[a-z0-9]+)", line)
    if m:
        print(m.group(1)); raise SystemExit
' <<<"$out")
    if [ -z "$id" ]; then
        echo "ask: create failed" >&2; printf '%s\n' "$out" >&2; return 1
    fi
    printf '%s' "$id"
}

parse_opts() { # sets WHY / DFLT / KIND / TARGET / MOOT_WHEN / FROM / REF from remaining args
    WHY=""; DFLT=""; EV=""; KIND=""; TARGET=""; MOOT_WHEN=""; FROM=""; REF=""; PRIO=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --why)     WHY="${2:-}"; shift 2 ;;
            --default) DFLT="${2:-}"; shift 2 ;;
            # A stable dedupe key for this ask. When provided, ask.sh checks for an open
            # bead with this external_ref before filing: a match bumps a recurrence count
            # instead of creating a duplicate. Titles carry measurements that change between
            # passes; the ref is the stable identity (law-a-documented-control-must-exist).
            --ref)     REF="${2:-}"; shift 2 ;;
            # An event only. --kind is the taxonomy the panel renders as the badge, and
            # --target the bead the outcome happened TO, kept in the row's own `target`
            # column rather than spelled into the title, so a reader can filter on it.
            --kind)    KIND="${2:-}"; shift 2 ;;
            --target)  TARGET="${2:-}"; shift 2 ;;
            # --evidence carries the facts themselves; --evidence-file reads them from a
            # file, which is the usual case because the facts are normally a log tail.
            --evidence) EV="${2:-}"; shift 2 ;;
            --evidence-file)
                if [ -r "${2:-}" ]; then EV="$(tail -c 2500 "$2" 2>/dev/null)"
                else EV="(evidence file unreadable: ${2:-})"; fi
                shift 2 ;;
            # A shell command exiting 0 when the condition that fired this alert has cleared.
            # moot-sweep.sh runs it on a timer and resolves the ask without human involvement.
            --moot-when) MOOT_WHEN="${2:-}"; shift 2 ;;
            # --from overrides the default "brain session" attribution in insight footers.
            # The archivist uses this to sign as "the archivist from session <name>" rather
            # than appearing to be the session it swept.
            --from)    FROM="${2:-}"; shift 2 ;;
            --priority)
                case "${2:-}" in [0-4]) PRIO="$2" ;;
                    *) echo "ask: --priority must be 0-4, got '${2:-}'" >&2; exit 2 ;; esac
                shift 2 ;;
            *) shift ;;
        esac
    done
}

# Print usage and exit 0 for -h/--help anywhere in the argument list, and for bare
# invocation. Scanning the full list means `ask.sh insight --help` cannot create a bead
# titled '--help' by treating the flag as the insight's title.
if [ $# -eq 0 ]; then
    sed -n '3,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0
fi
for _a in "$@"; do
    case "$_a" in -h|--help) sed -n '3,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;; esac
done
unset _a

# A title beginning with - is almost certainly a mistyped flag. Refuse it before any bead
# is created so the caller can correct it rather than find a junk bead in the queue.
require_title() {
    case "$1" in
        -*) printf 'ask: title looks like a flag: %s — pass --help for usage\n' "$1" >&2; exit 1 ;;
    esac
}

case "${1:-list}" in

add|ask|question)
    shift; text="${1:?usage: ask.sh add \"<question>\" --default \"<your default>\" [--why ...] [--ref <stable-key>]}"; shift || true
    require_title "$text"
    parse_opts "$@"
    # --default is mandatory: a rejection proceeds on the default, so an ask without one
    # cannot be rejected coherently — there is nothing to proceed on.
    [ -n "$DFLT" ] || { echo "ask: --default is required for add/decide — a premise-rejected ask proceeds on it, so an ask without one cannot be rejected coherently" >&2; exit 1; }
    # DEDUPE BY REF. When the caller supplies --ref, check for an open bead carrying that
    # external_ref before creating a new one. A match means the condition has not cleared:
    # bump the recurrence count and note the repeat rather than filing a duplicate.
    # No match (or no --ref): create a new bead, recording the ref for future passes.
    # A closed bead is an answered question; if the condition returns after an answer,
    # that IS new information and a fresh ask is correct.
    if [ -n "${REF:-}" ]; then
        _dedup_hit="$(_dedup_ask "$REF")"
        if [ -n "$_dedup_hit" ]; then
            _dedup_rest="${_dedup_hit#open }"
            _dedup_id="${_dedup_rest%% *}"
            _dedup_n="${_dedup_rest##* }"
            n=$((_dedup_n + 1))
            bdt label add "$_dedup_id" "sp-recur-$n" >/dev/null 2>&1
            bdt note "$_dedup_id" "Recurrence $n at $(date -u +%Y-%m-%dT%H:%M:%SZ)." >/dev/null 2>&1
            echo "recurred [$_dedup_id] ($n) $text"
            exit 0
        fi
        EXT_REF="$REF" id=$(create decision "$text" "$WHY" "$DFLT" "$SPIRA_ASK_LABEL,overseer,ask-question") || exit 1
    else
        id=$(create decision "$text" "$WHY" "$DFLT" "$SPIRA_ASK_LABEL,overseer,ask-question") || exit 1
    fi
    echo "asked [$id] $text"
    # When the ref identifies the work bead this escalation is about, write a typed edge so
    # bd graph can traverse the ask→work relationship.  An unrecognised ref format is silently
    # skipped; the ask was already created and its external_ref carries the ref for dedup.
    case "${REF:-}" in
        escalation:*:*)
            _esc_work="${REF##*:}"
            [ -n "$_esc_work" ] && bdt dep add "$id" "$_esc_work" --type tracks >/dev/null 2>&1 || true
            ;;
    esac
    ;;

decide|decision)
    shift; text="${1:?usage: ask.sh decide \"<the choice>\" --default \"<your default>\" [--why ...]}"; shift || true
    require_title "$text"
    parse_opts "$@"
    [ -n "$DFLT" ] || { echo "ask: --default is required for add/decide — a premise-rejected ask proceeds on it, so an ask without one cannot be rejected coherently" >&2; exit 1; }
    id=$(create decision "$text" "$WHY" "$DFLT" "$SPIRA_ASK_LABEL,overseer,ask-decision") || exit 1
    echo "decision [$id] $text"
    ;;

law|propose-law)
    # A law proposal whose body carries the statute text ready to enact.
    # Title format: "<slug>: <statute text>" — the panel's parse_enact splits on the first colon
    # to extract the slug and statute text it passes to rule.sh. The title keeps the full form
    # so the panel can work without parsing the body.
    # --default: if omitted, defaults to "yes — enact it". A law proposal without a recommended
    # verdict leaves the operator deciding from scratch, which is what the escalation policy
    # exists to prevent (law-escalate-decisions-not-problems).
    shift; text="${1:?usage: ask.sh law \"<slug>: <statute text>\" [--why \"<context>\"]}"; shift || true
    require_title "$text"
    parse_opts "$@"
    # Validate format: must contain a colon separating slug from statute text.
    case "$text" in
        *:*) ;;
        *) echo "ask: law proposal must be '<slug>: <statute text>' — the panel splits on the first colon" >&2; exit 1 ;;
    esac
    # Default: enact it. The rendered body must lead with the decision and its default so the
    # operator knows what they are being asked in the first line (law-escalate-decisions-not-problems).
    [ -z "$DFLT" ] && DFLT="yes — enact it"
    # Extract the statute text (everything after the first ": ") for the body. The title keeps
    # the full "<slug>: <statute>" form so the panel's parse_enact can still find both parts.
    STATUTE_TEXT="${text#*: }"
    export STATUTE_TEXT
    id=$(create decision "$text" "$WHY" "$DFLT" "$SPIRA_ASK_LABEL,overseer,ask-law") || exit 1
    echo "law [$id] $text"
    ;;

insight|learned)
    shift; text="${1:?usage: ask.sh insight \"<what was learned>\" [--why ...]}"; shift || true
    require_title "$text"
    parse_opts "$@"
    # Created then immediately closed: an insight is a record, not work. Left open it would
    # show up in `bd ready` and eventually in front of a polecat.
    id=$(create task "$text" "$WHY" "" "insight,overseer") || exit 1
    bdt close "$id" --reason "recorded" >/dev/null 2>&1
    echo "insight [$id] $text"
    ;;

note|event)
    # AN OUTCOME, NOT AN ASK AND NOT AN INSIGHT. Landings, reclaims, CI verdicts and completed
    # pilgrimages had nowhere durable to go: they flashed on the health pane's RECENT line for
    # one repaint and scrolled away, so the only bin that survived a refresh was the insights
    # queue — and the machinery used it.
    #
    # Created then immediately closed, for the same reason an insight is, and one more: an
    # OPEN event carrying the plan's labels is claimable by an aeon. It carries no labels at
    # all, so `bd ready --label spira,plan` cannot see it even before it is closed.
    shift; text="${1:?usage: ask.sh note \"<what happened>\" --kind <event.kind> [--why ...] [--target <bead>]}"; shift || true
    require_title "$text"
    parse_opts "$@"
    : "${KIND:=note}"
    id=$(create event "$text" "$WHY" "" "") || exit 1
    bdt close "$id" --reason "recorded" >/dev/null 2>&1
    echo "event [$id] $KIND: $text"
    ;;

promote)
    # AN FYI CAN BECOME A DECISION. (the operator, verbatim: "in some cases this may actually
    # result in one item turning into another type — i.e. FYI turns into a decision, etc.")
    #
    # An insight is filed as a record because at the time it was one. Then they comment on
    # it and it stops being a record: "this absolutely needs to be fixed" is a directive and
    # "not quite a law, not quite a decision, but something else" is a question. Re-filing it
    # as a new bead would strand the thread they wrote in, which is the whole reason they asked
    # for threads. So the item is promoted IN PLACE, keeping its id and its comments.
    id="${2:?usage: ask.sh promote <bead-id> [--to decision|task] [--why ...] [--default ...]}"
    shift 2 || true
    TO=decision
    for a in "$@"; do case "$prev_arg" in --to) TO="$a" ;; esac; prev_arg="$a"; done
    parse_opts "$@"
    case "$TO" in
        decision) newlabels="$SPIRA_ASK_LABEL,overseer,ask-decision"; newtype=decision ;;
        task)     newlabels="$SPIRA_ASK_LABEL,overseer,ask-task";     newtype=task ;;
        *) echo "ask: --to must be decision or task" >&2; exit 1 ;;
    esac
    # An insight is created CLOSED; a thing awaiting the operator must be open or it cannot be
    # answered, and the escalation gate defers it rather than dispatching it to a worker.
    bdt reopen "$id" >/dev/null 2>&1
    bdt update "$id" --type "$newtype" >/dev/null 2>&1
    bdt label remove "$id" insight >/dev/null 2>&1
    for l in ${newlabels//,/ }; do bdt label add "$id" "$l" >/dev/null 2>&1; done
    [ -n "${WHY:-}${DFLT:-}" ] && \
        "$(dirname "$0")/reply.sh" "$id" "Promoted from FYI to $TO.${DFLT:+ Default: $DFLT}${WHY:+ What is blocked: $WHY}" >/dev/null 2>&1
    echo "promoted [$id] FYI -> $TO (kept its id and its thread)"
    ;;

suit|lawsuit)
    # FILE A CHALLENGE TO A STATUTE IN FORCE. A suit is not an ask: the operator's verdict
    # runs rule.sh against the statute book. Three verdicts: uphold (closes, no change),
    # retire (runs rule.sh retire), amend (runs rule.sh enact with new text).
    #
    # FILING IS REFUSED when the slug is not in force, naming it. A suit against a statute
    # that does not exist would place an action on Ryan's plate whose subject cannot be
    # found and cannot be acted on. Validate first, so the bead is only created when it
    # refers to something real.
    shift; slug="${1:?usage: ask.sh suit <statute-slug> [--why \"<evidence>\"] [--evidence ...]}"
    shift || true
    require_title "lawsuit"
    parse_opts "$@"
    # Canonicalise: strip the law- prefix so rule.sh slugify adds it back cleanly.
    clean_slug="${slug#law-}"
    RULE_SH="$(dirname "$SPIRA_HOME")/rule.sh"
    # THE STATUTE TRAVELS WITH THE SUIT. This call already had to fetch it to prove it is in
    # force; discarding the text left the operator holding a verdict about a law the bead
    # never showed them. (Ryan, 2026-09-12: "i can't even interact with these lawsuit
    # decisions. they don't include the law, they don't include enough information about
    # what happened.") A decision that requires going and finding its own subject is a
    # problem report wearing a decision's clothes — the same fault the evidence block was
    # added to fix, one field over.
    if ! SUIT_STATUTE="$("$RULE_SH" show "$clean_slug" 2>/dev/null)" || [ -z "$SUIT_STATUTE" ]; then
        echo "ask: no statute 'law-${clean_slug}' is in force — check 'rule.sh list'" >&2
        exit 1
    fi
    export SUIT_STATUTE SUIT_SLUG="law-${clean_slug}"
    # statute:law-<slug> is the machine-readable label the panel reads to know which statute
    # to operate on. It avoids body parsing and keeps the panel and the shell tool in sync
    # by the same convention as enacted:law-<slug> on a promoted insight.
    id=$(create decision "lawsuit: law-${clean_slug}" "$WHY" "" \
            "${SPIRA_ASK_LABEL},overseer,ask-suit,statute:law-${clean_slug}") || exit 1
    echo "suit [$id] against law-${clean_slug}"
    ;;

answered)
    id="${2:?usage: ask.sh answered <bead-id> \"<verdict>\"}"
    verdict="${3:?a verdict is required — the close reason IS the record}"
    # `--force`, and the exit status actually read. An ask decomposed out of an epic carries
    # the epic's `blocks` edges, and `bd close` refuses a blocked issue: *"cannot close
    # blocked issue: sp-wok.5 is blocked by [sp-wok.3] (use --force to override)"*. Those
    # edges order the WORK; they do not order the operator's answer, and the pane is where
    # the person who owns the decision is the one pressing the key. Nothing downstream catches
    # the hollow close this permits — HOLLOW-CLOSE was a Gas Town check and Spira has no
    # equivalent (see close_decision in panel/src/model.rs, and sp-hollow).
    #
    # The status matters as much as the flag. Piping into `tail -2` made $? the pipeline's
    # last stage, so a refused close printed its error and was followed, unconditionally, by
    # the "does this verdict generalise" trailer — reading exactly like a recorded verdict.
    # And the verdict itself existed only as an argument to a command that failed, so it was
    # gone. Keep it on the bead before saying anything went wrong.
    if bdt close "$id" --reason "$verdict" --force 2>&1 | strip_warn | tail -2; [ "${PIPESTATUS[0]}" -ne 0 ]; then
        bdt comments add "$id" "$verdict" >/dev/null 2>&1 \
            && echo "  NOT CLOSED — your answer is kept on $id as a comment." \
            || echo "  NOT CLOSED, AND THE ANSWER WAS NOT SAVED: $verdict"
        exit 1
    fi
    echo
    echo "  Does this verdict generalise? If so it is law, not a closed bead:"
    echo "    $(dirname "$SPIRA_HOME")/rule.sh enact <slug> \"<statute + the case that produced it>\""
    ;;

drop|dismiss)
    id="${2:?usage: ask.sh drop <bead-id> \"<reason>\"}"
    # `--force` for the same reason `answered` uses it: a dependency edge orders the work,
    # not the operator's dismissal of the ask.
    bdt close "$id" --reason "dismissed: ${3:-not needed}" --force 2>&1 | strip_warn | tail -2
    ;;

list)
    # --all is required: insights are created CLOSED by design, and `bd list` hides
    # closed issues unless asked, so the insights view was permanently empty.
    bdt list --all --limit 0 --json 2>/dev/null | strip_warn | python3 -c '
import json, sys
view = sys.argv[1] if len(sys.argv) > 1 else "needs-you"
try:
    rows = json.load(sys.stdin)
except Exception:
    print("  could not read the town database"); raise SystemExit(1)
rows = rows if isinstance(rows, list) else rows.get("issues", [])
# The taxonomy is in labels; issue_type cannot express it.
def lab(r): return set(r.get("labels") or [])
def kind(r):
    l = lab(r)
    if r.get("issue_type") == "event": return r.get("event_kind") or "event"
    if "insight" in l: return "insight"
    if "ask-decision" in l: return "decision"
    if "ask-task" in l: return "task"
    if "ask-question" in l: return "question"
    return r.get("issue_type") or "?"
mine = [r for r in rows if "overseer" in lab(r)]
if view in ("events", "event"):
    # NOT filtered through `mine`: an event carries no labels at all, by design, so
    # `overseer` is exactly what it must not have. The type IS the filter here.
    sel = [r for r in rows if r.get("issue_type") == "event" and "archived" not in lab(r)]
elif view in ("insights", "insight"):
    # Archived insights have been read and dismissed; they stay in `all`, not here.
    sel = [r for r in mine if "insight" in lab(r) and "archived" not in lab(r)]
elif view == "all":
    sel = mine
else:
    sel = [r for r in mine if "insight" not in lab(r) and r.get("status") == "open"]
sel.sort(key=lambda r: r.get("created_at") or "", reverse=True)
for r in sel[:40]:
    print("  [%s] %-19s %-9s %s" % (r["id"], kind(r), r.get("status"), (r.get("title") or "")[:76]))
print()
print("  %d %s" % (len(sel), view))' "${2:-needs-you}"
    ;;

rejected)
    # Premise-rejected asks are the only training signal the escalation filter has: they
    # record what the operator considered not worth deciding, with the reason why. Listed
    # newest first so the most recent signal is the first thing read.
    bdt list --all --limit 0 --json 2>/dev/null | strip_warn | python3 -c '
import json, sys
try:
    rows = json.load(sys.stdin)
except Exception:
    print("  could not read the town database"); raise SystemExit(1)
rows = rows if isinstance(rows, list) else rows.get("issues", [])
sel = [r for r in rows if "premise-rejected" in (r.get("labels") or [])]
sel.sort(key=lambda r: r.get("closed_at") or r.get("updated_at") or "", reverse=True)
for r in sel[:40]:
    reason = (r.get("close_reason") or "").strip()
    # Strip the "premise-rejected: " prefix to show only the why.
    if reason.startswith("premise-rejected: "):
        why = reason[len("premise-rejected: "):]
    elif reason == "premise-rejected":
        why = "(no reason given)"
    else:
        why = reason or "(no reason given)"
    print("  [%s] %s" % (r["id"], (r.get("title") or "")[:72]))
    print("       %s" % why[:100])
print()
print("  %d premise-rejected" % len(sel))'
    ;;

*) sed -n '3,11p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
