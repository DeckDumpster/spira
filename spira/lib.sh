# lib.sh — shared helpers for the Spira harness. Sourced, never executed.
#
# Spira runs personas as AEONS: summoned from a FAYTH (a persona definition), they claim
# one bead with a lease, work it, close or fail it, and exit. Nothing is long-lived except
# the systemd timers, because a session that dies takes its state with it — the scar behind
# cockpit-ensure.timer — while a lease is recovered by whoever runs `bd reclaim` next.

# EVERY PATH COMES FROM conf.sh AND NOTHING IS HARDCODED HERE. It also sets PATH, because
# `bd`, `claude` and `git` live on the LOGIN shell's PATH and everything here is invoked
# from systemd, where that PATH does not exist; bootstrapping in one place is the
# difference between working and failing silently to a log nobody reads.
#
# Sourced by ABSOLUTE path derived from this file, not from the caller's $0: lib.sh is
# sourced by scripts two directories away.
_spira_lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# NAME IT WHEN IT IS ABSENT. Several suites copy lib.sh into a scratch directory to run it
# out of its own tree; one that forgets conf.sh would otherwise die with bash's own "No
# such file or directory" naming a path nobody wrote.
[ -f "$_spira_lib_dir/conf.sh" ] || {
    printf 'spira: conf.sh is missing beside lib.sh at %s — the harness cannot resolve any path without it\n' \
        "$_spira_lib_dir" >&2
    return 1 2>/dev/null || exit 1
}
. "$_spira_lib_dir/conf.sh"
. "$_spira_lib_dir/suite-covers.sh"
# FAYTH SHIMS NOW EXEC spira-config (wave 4.22, sp-r5zd2: fayth_get and friends below are
# one-line shims onto `spira-config fayth ...`). A `.fayth` file is sourced inside THAT
# binary's own subprocess, which inherits only the real process environment — not this
# shell's unexported variables, the way the old in-shell `( subshell )` did. conf.sh
# deliberately leaves most of its ~264 keys unexported (its own comment above the export
# list); these are the ones every shipped `.fayth`'s FAYTH_LABELS line actually references
# (grepped: spira/chamber/*.fayth) that are not already on that list. Exported here, once,
# the same narrow way spira/bead.sh:49 already does for the bead binary — not a blanket
# export, which would leak the per-copy facts conf.sh's own fence exists to keep out of
# every child process (SPIRA_HOME, SPIRA_REPO, the maps, SPIRA_FAYTHS, SPIRA_MAX_AEONS —
# law-gates-run-in-a-clean-environment). A custom operator fayth referencing some OTHER
# unexported key is the same trap bead.sh's own narrow list already carries; widen this
# list (and bead.sh's) together if one shows up.
export SPIRA_CZAR_LABEL SPIRA_GROOMER_LABEL SPIRA_MAECHEN_LABEL SPIRA_BATCH_JUDGEMENT_LABEL SPIRA_HOME_REPO
# CERTIFICATION ONTO EVENTS is `spira-lc certify` / `spira-lc resubmit` (sp-arpjt retired
# lifecycle-cert.sh into spira-lc's caller verbs). The lc_certify/lc_resubmit wrapper
# functions that once stood in for those two calls are retired too (sp-uwhx0): their only
# caller was batch.sh's stale-certification sweep, itself retired with batch.sh — nothing
# else called them (grepped). Call `spira-lc certify`/`spira-lc resubmit` directly.
unset _spira_lib_dir
export BEADS_NO_AUTO_IMPORT=1
mkdir -p "$SPIRA_RUN"

# Always name the database. This repo has no .beads, so an implicit bd reads whatever store
# the working directory resolves to, or nothing — never the database meant.
# SPIRA_BD is the seam a suite uses for the ONE thing a real bd cannot be asked to do on
# demand — a probe that fails. It is not a place to put a model of bd: the suites run the
# real binary against a throwaway database (`testdb.sh`), because a partial model drifts
# silently and its gaps surface as failures in correct code. It exists as an env var rather
# than a PATH entry because lib.sh overwrites PATH outright, as it must to run under
# systemd, so a directory prepended by a test would be thrown away by the export above.
#
# SPIRA_BDJSON_FIXTURE is the one sanctioned exception: a read-only, per-probe test that
# needs a pure classification result, not bd's own filtering behaviour, points it at a
# canned-JSON file and bdsim.py answers `list`/`show`/`memories` from that file instead of a
# live store. Query shapes bdsim.py does not simulate stay on real bd, in
# test-cockpit-bd-contract.sh.
bdq() {
    # Refuse a repo: label at create time if it has no repo-map entry, naming valid keys.
    # A bad label is refused here, before bd is called, so no bead is created and no summon
    # is wasted reaching the summon-time unmapped-repo fence (law-bake-rules-into-tools).
    [ "${1:-}" = create ] && { _bdq_check_repo_label "$@" || return 1; }
    # Refuse a bead whose title or description contains vocabulary that halts the harness,
    # unless needs-ryan is already on it — which is the label that makes such a bead correct.
    [ "${1:-}" = create ] && { _bdq_check_destructive "$@" || return 1; }
    # Refuse DELETE FROM schema_migrations regardless of needs-ryan. This SQL was recommended
    # by escalation beads (which carry needs-ryan) three times; needs-ryan means "Ryan will
    # review" — it does not mean the SQL is correct. (sp-1khst)
    [ "${1:-}" = create ] && { _bdq_check_schema_delete "$@" || return 1; }
    if [ "${SPIRA_FAYTH:-}" = czar ] && [ -n "${SPIRA_CZAR_CLASS:-}" ]; then
        case "${1:-}" in
            reopen)
                czar-fence.sh "${SPIRA_CZAR_CLASS}" || return 1 ;;
            update|close)
                if [ "${2:-}" != "${SPIRA_CZAR_TRIGGER_BEAD:-__none__}" ]; then
                    czar-fence.sh "${SPIRA_CZAR_CLASS}" || return 1
                fi ;;
        esac
    fi
    if [ -n "${SPIRA_BDJSON_FIXTURE:-}" ]; then
        bdsim.py "$SPIRA_BDJSON_FIXTURE" "$@"
        return $?
    fi
    # Refuse rather than fall through to bd's own auto-discovery: bd -C "" does not
    # error, it walks up from $PWD to find a store, which for anything run from
    # inside an operator's harness checkout resolves to that operator's real,
    # production database. A silent SPIRA_DB propagation loss must become a
    # loud failure here, not a write to the wrong database (sp-agdzk / sp-25b7s).
    if [ -z "${SPIRA_DB:-}" ]; then
        echo "bdq: refusing - SPIRA_DB is empty/unset (would fall through to bd auto-discovery)" >&2
        return 1
    fi
    # A pooled Dolt connection the server already dropped surfaces on the next query as
    # "invalid connection" — the Go driver detects it dead before sending anything, so the
    # query never ran and retrying it is exactly as safe as the first attempt. install.sh
    # already retries `bd init` once on this identical string; every other bd call goes
    # through here, so this is the one place that covers all of them (sp-ydog2).
    #
    # SOP_APPLIED_TRACE=1 logs wall-clock start/end around the whole retry loop below, which
    # every bd subprocess call in the codebase goes through (including the three `sop
    # applied` makes: bdjson memories, and the bead-note write). Off by default — a `date`
    # call and an append are cheap, but this runs on every bd invocation in the harness, so
    # it stays gated rather than always-on. Follow-up to sp-ohnz7 (the bash sop.sh's applied()
    # ledger/wiki-regen contention): the hangs it reproduced only appear under real
    # concurrent dolt load and could not safely be forced (law-probe-a-fixture-not-
    # production), so this turns "time it under load" into "read the trace from the next
    # hang that happens naturally" (sp-h54i5).
    local _sop_trace_file _sop_t0
    if [ "${SOP_APPLIED_TRACE:-}" = 1 ]; then
        _sop_trace_file="${SOP_APPLIED_TRACE_FILE:-${SPIRA_RUN:-/tmp}/sop/trace.log}"
        mkdir -p "$(dirname "$_sop_trace_file")" 2>/dev/null || true
        _sop_t0="$(date -u '+%Y-%m-%dT%H:%M:%S.%NZ')"
    fi
    local _bdq_try=1 _bdq_tries="${SPIRA_BDQ_CONN_RETRIES:-2}" _bdq_rc _bdq_err
    _bdq_err="$(mktemp)"
    while :; do
        timeout "${BD_TIMEOUT:-180}" "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@" 2>"$_bdq_err"
        _bdq_rc=$?
        if [ "$_bdq_rc" -eq 0 ] || [ "$_bdq_try" -ge "$_bdq_tries" ] \
                || ! grep -q "invalid connection" "$_bdq_err"; then
            break
        fi
        _bdq_try=$((_bdq_try + 1))
    done
    cat "$_bdq_err" >&2
    rm -f "$_bdq_err"
    if [ -n "${_sop_trace_file:-}" ]; then
        printf '%s start=%s end=%s rc=%s tries=%s argv=%s\n' "$$" "$_sop_t0" \
            "$(date -u '+%Y-%m-%dT%H:%M:%S.%NZ')" "$_bdq_rc" "$_bdq_try" "$*" \
            >> "$_sop_trace_file" 2>/dev/null || true
    fi
    return "$_bdq_rc"
}

_bdq_check_repo_label() {   # _bdq_check_repo_label <create-args> -> 0 or refuse
    local arg next_is_labels=0 labels="" repo_val valid
    for arg in "$@"; do
        if [ "$next_is_labels" = 1 ]; then
            labels="$arg"; next_is_labels=0; continue
        fi
        case "$arg" in
            --labels|-l) next_is_labels=1 ;;
            --labels=*)  labels="${arg#--labels=}" ;;
        esac
    done
    [ -z "$labels" ] && return 0
    repo_val="$(printf '%s\n' "$labels" | tr ',' '\n' | grep '^repo:' | head -1 | cut -c6-)"
    [ -z "$repo_val" ] && return 0
    # The home repo is always a valid target. repo_names reads the repo-map file,
    # which lists satellite repos only — the home repo is handled by spira_home_repo()
    # and is never in that file.
    [ "$repo_val" = "$(spira_home_repo)" ] && return 0
    valid="$(repo_names 2>/dev/null | sort | tr '\n' ' ' | sed 's/ $//')"
    if ! repo_names 2>/dev/null | grep -qxF "$repo_val"; then
        printf 'spira: repo:%s is not in the repo map; valid keys: %s\n' \
            "$repo_val" "${valid:-<map not found>}" >&2
        return 1
    fi
    return 0
}

_bdq_check_destructive() {  # _bdq_check_destructive <create-args> -> 0 or refuse
    # Refuse a bead whose title or description names a procedure that halts the harness —
    # world.sh stop, spira-world down, systemd/install.sh, daemon-reload, systemctl
    # stop/restart of a spira-* unit, schema migrations, or the phrase "world stopped" —
    # unless needs-ryan is already on the bead, which is what makes such a bead correct.
    #
    # THE SCAR THIS CLOSES. sp-6ylz had "needs the world stopped" in its own title and was
    # dispatchable anyway. An aeon ran world.sh stop from step 2 and killed the sentinel
    # timer, the ops timer, both watchers, and three live aeons including itself. The filer
    # had written the danger into the title and still filed it dispatchable; a rule that
    # requires remembering at file time is a resolution, not a mechanism. (sp-6hdi)
    local arg next="" labels="" title="" desc="" saw_create=0 positioned=0
    for arg in "$@"; do
        if [ -n "$next" ]; then
            case "$next" in
                labels)      labels="$arg" ;;
                title)       title="$arg"; positioned=1 ;;
                description) desc="$arg" ;;
            esac
            next=""; continue
        fi
        case "$arg" in
            --labels|-l)       next=labels ;;
            --labels=*)        labels="${arg#--labels=}" ;;
            --title)           next=title ;;
            --title=*)         title="${arg#--title=}"; positioned=1 ;;
            -d|--description)  next=description ;;
            --description=*)   desc="${arg#--description=}" ;;
            -*)                ;;
            *)
                if [ "$saw_create" = 0 ]; then saw_create=1  # skip "create"
                elif [ "$positioned" = 0 ]; then title="$arg"; positioned=1
                fi ;;
        esac
    done

    # needs-ryan is the label that makes a halting bead correct — if it is already there,
    # the filer has already acknowledged the danger.
    # NO LITERAL, AND NO FALLBACK. This line and the one in ask_already_open below read the
    # same name and disagreed about it: this one hardcoded a value while that one read
    # ${SPIRA_ASK_LABEL:-...}. Since the code default differs from the configured value, a
    # default install left this fence with no bypass at all and refused every legitimate
    # halting bead. A fallback here would only move the disagreement one step; conf.sh
    # guarantees the variable, so an unset one is a broken environment and must say so
    # rather than silently match nothing — matching nothing fails OPEN.
    local _ask="${SPIRA_ASK_LABEL:?SPIRA_ASK_LABEL is unset — source conf.sh}"
    printf '%s\n' "$labels" | tr ',' '\n' | grep -qxF "$_ask" && return 0

    local text="$title $desc"
    [ -z "${text# }" ] && return 0

    # Each pattern is a case-insensitive ERE covering one class of halting procedure.
    local patterns=(
        'world\.sh +stop'
        'spira-world +down'
        'systemd/install\.sh'
        '\bdaemon-reload\b'
        'systemctl +(stop|restart) +spira-'
        'schema +migrat'
        'world +stopped'
    )
    local matched="" p
    for p in "${patterns[@]}"; do
        matched="$(printf '%s\n' "$text" | grep -ioE "$p" | head -1)" && [ -n "$matched" ] && break
        matched=""
    done
    [ -z "$matched" ] && return 0

    # literal-ok: operator-facing message text; it names the label to a human, it does not compare against it
    printf 'spira: bead contains "%s" — procedures that halt the harness require needs-ryan.\nAdd needs-ryan to --labels, or reword to remove the destructive step.\n' \
        "$matched" >&2
    return 1
}

_bdq_check_schema_delete() {  # _bdq_check_schema_delete <create-args> -> 0 or refuse
    # Refuse any bead whose title or description contains DELETE FROM schema_migrations,
    # regardless of needs-ryan. Unlike the general destructive-vocabulary check, needs-ryan
    # does not bypass this one: three escalation beads carried needs-ryan and still recommended
    # this SQL, and the third was approved. needs-ryan records that Ryan will decide; it does
    # not assert that the recommended action is correct. (sp-1khst)
    #
    # The SQL removes migration rows from the Dolt database backing the beads store. Once
    # committed through DOLT_COMMIT this is not recoverable without a backup restore.
    # Rebuilding bd to match the database cursor is always the correct response to a real
    # schema mismatch; see bd-pin.sh. A false mismatch — the common case — resolves with
    # `bd migrate schema`, which reports the actual state rather than grepping error strings.
    local arg next="" title="" desc="" saw_create=0 positioned=0
    for arg in "$@"; do
        if [ -n "$next" ]; then
            case "$next" in
                title)       title="$arg"; positioned=1 ;;
                description) desc="$arg" ;;
            esac
            next=""; continue
        fi
        case "$arg" in
            --title)           next=title ;;
            --title=*)         title="${arg#--title=}"; positioned=1 ;;
            -d|--description)  next=description ;;
            --description=*)   desc="${arg#--description=}" ;;
            -*)                ;;
            *)
                if [ "$saw_create" = 0 ]; then saw_create=1
                elif [ "$positioned" = 0 ]; then title="$arg"; positioned=1
                fi ;;
        esac
    done

    local text="$title $desc"
    [ -z "${text# }" ] && return 0

    printf '%s\n' "$text" | grep -iqE 'DELETE[[:space:]]+FROM[[:space:]]+schema_migrations' || return 0

    printf 'spira: bead contains "DELETE FROM schema_migrations" — this SQL is refused\n' >&2
    # literal-ok: operator-facing message text, not a comparison
    printf 'even with needs-ryan because it was escalated and approved three times while wrong.\n' >&2
    printf 'Run `bd migrate schema` and include its output in the escalation instead.\n' >&2
    printf 'The correct response to a real mismatch is rebuilding bd (see bd-pin.sh),\n' >&2
    printf 'not deleting migration rows from the database.\n' >&2
    return 1
}

# `gh` gets the same treatment and for the same reason. The pull-request landing path is the
# part of this harness that reaches OUTSIDE the box, so it is the part that most needs a
# fixture — and, like bd, a stub cannot be put in front of it by prepending to PATH, because
# the export above throws that away.
ghq() { timeout "${GH_TIMEOUT:-120}" "${SPIRA_GH:-gh}" "$@"; }

log() { printf '%s spira: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() { log "FATAL $*" >&2; exit 1; }

# getconf ignores cgroup quota; nproc returns fewer than physical cores when called from
# a unit with CPUQuota set.
host_cores() {
    local n
    n="$(getconf _NPROCESSORS_ONLN 2>/dev/null)"
    { [ -n "$n" ] && [ "$n" -gt 0 ]; } 2>/dev/null && { printf '%s' "$n"; return; }
    n="$(ls -d /sys/devices/system/cpu/cpu[0-9]* 2>/dev/null | wc -l)"
    [ "${n:-0}" -gt 0 ] 2>/dev/null && { printf '%s' "$n"; return; }
    printf '1'
}

# `bd --json` can print warnings on stdout before the payload, so never pipe it straight
# into a parser. This strips anything before the first JSON token.
json_only() { sed -n '/^[[{]/,$p'; }

bdjson() { bdq "$@" --json 2>/dev/null | json_only; }

# ask_already_open <subject> -> 0 when an OPEN operator ask already carries that subject.
#
# THE STRONGEST DEDUPE IS "IS IT ALREADY IN FRONT OF HIM", not a clock and not a stamp file.
# A clock re-asks a question already on his screen — land_escalate was rate limited to once
# an hour, which over one day put NINE identical "Spira is landing nothing" decisions in the
# operator's pane; he closed eight and the ninth arrived anyway. A stamp file is better but
# still answers a question about this box's memory rather than about his queue, and it is
# lost whenever $SPIRA_RUN is cleared.
#
# The database is the queue, so ask the database. An ask he has ALREADY CLOSED does not
# suppress a new one: a closed ask is an answered question, and the condition recurring after
# an answer is new information (law-alerts-must-be-actionable).
ask_already_open() {     # ask_already_open <subject>
    local subject="$1" hits
    [ -n "$subject" ] || return 1
    hits="$(bdjson list --status open --label "${SPIRA_ASK_LABEL:?SPIRA_ASK_LABEL is unset — source conf.sh}" --limit 0 2>/dev/null \
        | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
rows = d if isinstance(d, list) else [d]
want = sys.argv[1]
print(sum(1 for i in rows if want in (i.get("title") or "")))' "$subject" 2>/dev/null)"
    [ "${hits:-0}" -gt 0 ] 2>/dev/null
}

# ask_closed_subject <subject> -> prints the id of a CLOSED operator ask carrying that
# subject, or nothing.
#
# NOT EVERY ASK'S ANSWER IS "NEW INFORMATION" ON RECURRENCE. ask_already_open's own
# comment is right for most callers — an alert whose condition returns after being
# closed is telling him something changed. gh_issue_ask_unlanded's condition ("this
# bead's commit is not yet on the base") does not change just because he closed the
# ask; closing it IS the answer, and a caller whose only dedupe is "no ask is open"
# re-files the identical ask every pass forever. This finds that already-answered ask
# so the caller can write a durable marker instead of re-asking.
ask_closed_subject() {   # ask_closed_subject <subject>
    local subject="$1"
    [ -n "$subject" ] || return 1
    bdjson list --status closed --label "${SPIRA_ASK_LABEL:?SPIRA_ASK_LABEL is unset — source conf.sh}" --limit 0 2>/dev/null \
        | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
rows = d if isinstance(d, list) else [d]
want = sys.argv[1]
for i in rows:
    if want in (i.get("title") or ""):
        print(i.get("id") or "")
        break' "$subject" 2>/dev/null
}

# spira_ask_machinery — escalate a judgement that repeatedly could not be made.
#
# THE CASE THIS EXISTS FOR. A gate that withholds its verdict is correct to let the branch
# keep its turn, and the pass is telling the truth every time it says "the next pass takes
# it". Said eleven times in a row it is also the exact sound of a livelock, and on
# 2026-09-07 nothing anywhere turned that repetition into a signal: origin/main sat still for
# fifty minutes while every log line individually read as normal operation.
#
# So the escalation is on the REPETITION, not on the occurrence (law-alerts-must-be-actionable
# — a first lock-timeout is not actionable and paging on it would teach the operator to
# ignore the channel). It is a decision request, not a problem report: it names the machinery
# fault, what it is costing, and what to do (law-escalate-decisions-not-problems).
#
# Deduped through ask_already_open on the branch name, because the strongest dedupe is "is it
# already in front of him" rather than a clock — a rate-limited version of this same alert
# put nine identical decisions in his pane in one day.
spira_ask_machinery() {  # <bead> <branch> <repo> <outcome> <reason> <count> <gate output>
    local id="$1" br="$2" repo="$3" outcome="$4" reason="$5" n="$6" out="$7"
    ask_already_open "$br cannot be judged" && return 0
    local _subj="$br cannot be judged: $outcome x$n in a row ($reason)"
    local _dflt="raise the budget or clear the contention this reason names, then let the next pass take it; if it is not obvious, run \`gate.sh $br $repo\` by hand and read the whole output"
    local _why="$outcome means the machinery could not reach a verdict — the branch has NOT been judged and has NOT been charged, and $id is not at fault. It has now failed to be judged $n times, so this is no longer a queue clearing itself. Nothing on $br can land until a verdict is reached, and every other branch of $repo is behind the same fault."
    local _ev; _ev="$(printf '%s' "$out" | tail -20)"
    mail send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_subj

## Default
$_dflt

$_why

$_ev
MAILEOF
}

# spira_ask_machinery_class — escalate a machinery fault that belongs to no single branch.
#
# THE CASE THIS EXISTS FOR. A dead container is not any one branch's problem — every branch
# gated against it fails the same way — so counting and escalating it per branch produced
# eight separate asks for one fault, each advising a fix that could not help because the
# reason string itself was wrong. Escalated by class (repo+reason) instead, this fires once
# and names every branch the fault touched.
spira_ask_machinery_class() {  # <repo> <reason> <branches-csv> <outcome> <count> <gate output>
    local repo="$1" reason="$2" branches="$3" outcome="$4" n="$5" out="$6"
    ask_already_open "$repo cannot be judged: $outcome ($reason)" && return 0
    local _subj="$repo cannot be judged: $outcome x$n in a day ($reason) — $branches"
    local _dflt="this is one machinery fault behind every branch named above, not one per branch; fix the cause this reason names, then let the next pass take all of them"
    local _why="$outcome/$reason means the machinery could not reach a verdict for any of these branches — none of them is at fault and none has been charged. It has recurred $n times across $repo within a day, so this is escalated once for the class rather than once per branch."
    local _ev; _ev="$(printf '%s' "$out" | tail -20)"
    mail send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_subj

## Default
$_dflt

$_why

Affected branches: $branches

$_ev
MAILEOF
}

# spira_land_noverdict — record one NO_VERDICT occurrence for a branch and escalate when it
# recurs (law-alerts-must-be-actionable at the machinery level, same as spira_ask_machinery
# above).
#
# A HARNESS-FAULT REASON IS COUNTED AND ESCALATED BY CLASS (repo+reason), not by branch. A
# dead container makes every branch's gate fail identically, so the count that decides
# whether this has become a pattern belongs to the fault, and the ask that follows names
# every branch it has touched instead of filing one ask per branch. Every other NO_VERDICT
# reason (a lock wait, a missing base ref) is still genuinely per-branch and keeps the old
# per-branch key.
#
# THE CLASS WINDOW RESETS. An .asked marker older than SPIRA_NOVERDICT_CLASS_WINDOW (default
# a day) is cleared along with its count, so a fault that went away and came back on a later
# day escalates again rather than being silenced forever by yesterday's ask.
spira_land_noverdict() {  # <bead> <branch> <repo-name> <reason> <outcome> <gate output>
    local id="$1" br="$2" name="$3" reason="${4:-unspecified}" outcome="$5" out="$6"
    local nv_key nv_file nv_n

    if [ "$reason" = harness-fault ]; then
        nv_key="$(printf '%s' "$name-$reason" | tr -c 'A-Za-z0-9._-' '-')"
        nv_file="$SPIRA_RUN/noverdict/$nv_key"
        mkdir -p "$SPIRA_RUN/noverdict"
        if [ -e "$nv_file.asked" ]; then
            local _age=$(( $(date +%s) - $(date -r "$nv_file.asked" +%s 2>/dev/null || echo 0) ))
            if [ "$_age" -ge "${SPIRA_NOVERDICT_CLASS_WINDOW:-86400}" ]; then
                rm -f "$nv_file" "$nv_file.asked" "$nv_file.branches"
            fi
        fi
        nv_n=$(( $(cat "$nv_file" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$nv_n" > "$nv_file"
        grep -qxF "$br" "$nv_file.branches" 2>/dev/null || printf '%s\n' "$br" >> "$nv_file.branches"
        if [ "$nv_n" -ge "${SPIRA_NOVERDICT_MAX:-3}" ] && [ ! -e "$nv_file.asked" ]; then
            : > "$nv_file.asked"
            local _branches; _branches="$(paste -sd, "$nv_file.branches" 2>/dev/null)"
            spira_ask_machinery_class "$name" "$reason" "${_branches:-$br}" "$outcome" "$nv_n" "$out"
            progress "escalated $name — $outcome x$nv_n in a day ($reason) across ${_branches:-$br}"
        fi
        return 0
    fi

    nv_key="$(printf '%s' "$br-$reason" | tr -c 'A-Za-z0-9._-' '-')"
    nv_file="$SPIRA_RUN/noverdict/$nv_key"
    mkdir -p "$SPIRA_RUN/noverdict"
    nv_n=$(( $(cat "$nv_file" 2>/dev/null || echo 0) + 1 ))
    printf '%s\n' "$nv_n" > "$nv_file"
    if [ "$nv_n" -ge "${SPIRA_NOVERDICT_MAX:-3}" ] && [ ! -e "$nv_file.asked" ]; then
        : > "$nv_file.asked"
        spira_ask_machinery "$id" "$br" "$name" "$outcome" "$reason" "$nv_n" "$out"
        progress "escalated $id — $outcome x$nv_n on $br"
    fi
}

# spira_is_generated_file <path> -> 0 if path names a file this harness regenerates whole
# rather than hand-merges (law-regenerate-derived-summaries). A rebase conflict on one of
# these is resolved by rerunning its generator, never by reconciling the two hunks by hand.
# The declared list lives in SPIRA_REBASE_GENERATED_FILES (conf.sh) — a path substring
# match, since a generated file is named the same regardless of which directory it sits in.
spira_is_generated_file() {
    local path="$1" pat
    for pat in ${SPIRA_REBASE_GENERATED_FILES:-}; do
        case "$path" in
            *"$pat"*) return 0 ;;
        esac
    done
    return 1
}

# spira_ask_rebase_loop — tell the Concierge a bead's rebase keeps failing.
#
# Seven reopens on sp-dvlq, each one handing the next aeon "resolve the conflict" against a
# branch whose correct resolution was "drop it". The repetition is the signal: a bead that
# cannot rebase N times in a row is not learning from the reopen, and repeating it is
# machinery cycling on itself (law-alerts-must-be-actionable at the machinery level).
#
# NEVER RYAN'S DECISION (law-a-rebase-loop-is-sequenced-not-split) — every one of these was
# resolved by the Concierge with rebase guidance, never by him. So this sends a machine event
# to the Concierge's mailbox (real time, law-machine-events-wake-in-real-time) — never a
# --kind question/decision, which would file the same needs-ryan ask under another name.
spira_ask_rebase_loop() {  # <bead> <branch> <repo-name> <requeue-count> <conflicts> <other-beads> [<repo-dir> <base>]
    local id="$1" br="$2" name="$3" n="$4" conflicts="$5" others="$6"
    local repo_dir="${7:-}" base_ref="${8:-}"
    # Fetch bead title and status so the event names the work and its current state.
    local bead_title bead_status
    bead_title="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print(""); sys.exit()
d = d if isinstance(d, list) else [d]
print(d[0].get("title", "") if d else "")' 2>/dev/null)"
    bead_status="$(spira_bead_status "$id")"
    # Commits-ahead and branch tip when repo coordinates are available.
    local tip_short="" ahead=""
    if [ -n "$repo_dir" ] && [ -n "$base_ref" ]; then
        tip_short="$(git -C "$repo_dir" rev-parse --short "$br" 2>/dev/null || true)"
        ahead="$(git -C "$repo_dir" rev-list --count "$base_ref..$br" 2>/dev/null || echo '?')"
    fi
    local ctx=""
    [ -n "$others" ] && ctx=" The conflicted files were also changed on the base by $others."
    # File count over the branch's own diff (not just the conflicted files) — a bead whose
    # scope spans several hot files cannot win a rebase race it re-enters every few hours;
    # past SPIRA_REBASE_DECOMPOSE_FILES the answer is decomposition, not another hand rebase.
    local nfiles=0 decompose_ctx=""
    if [ -n "$repo_dir" ] && [ -n "$base_ref" ]; then
        nfiles="$(git -C "$repo_dir" diff --name-only "${base_ref}...${br}" 2>/dev/null | grep -c .)"
    fi
    if [ "${nfiles:-0}" -ge "${SPIRA_REBASE_DECOMPOSE_FILES:-4}" ]; then
        decompose_ctx=" $br touches $nfiles files — a bead this wide re-enters the rebase race every landing; consider splitting it into smaller beads instead of hand-rebasing the whole thing again."
    fi
    # Subject: title first so the Concierge knows what the work is (law-escalations-lead-with-the-bead).
    local _subj
    if [ -n "$bead_title" ]; then
        _subj="${bead_title}: $br rebase loop x$n in $name"
    else
        _subj="$br rebase loop x$n in $name"
    fi
    # Per-file listing, one line per conflicted file, flagging any that are GENERATED
    # (regenerate, don't hand-merge) instead of leaving that judgement to the reader.
    local _file _files_note="" _gen_note=""
    for _file in $conflicts; do
        if spira_is_generated_file "$_file"; then
            _files_note="${_files_note}${_files_note:+$'\n'}  - $_file (GENERATED — regenerate it, do not merge it by hand)"
            _gen_note=1
        else
            _files_note="${_files_note}${_files_note:+$'\n'}  - $_file"
        fi
    done
    [ -n "$_files_note" ] || _files_note="  - ${conflicts:-unknown}"
    # Suggested action: no empty slots — omit the duplicate clause when others is empty.
    local _sugg
    if [ -n "$_gen_note" ]; then
        _sugg="regenerate the GENERATED file(s) named above via their own generator and rebase again — do not hand-merge them"
    elif [ "${nfiles:-0}" -ge "${SPIRA_REBASE_DECOMPOSE_FILES:-4}" ]; then
        _sugg="split $br into smaller beads by file/deliverable and land those independently, rather than rebasing the whole thing by hand again"
    elif [ -n "$others" ]; then
        _sugg="check whether $br is a duplicate of $others and close it if so; if the work is genuinely new, rebase by hand and push"
    else
        _sugg="rebase $br by hand and push, or close it if the work is already landed"
    fi
    # Extra lines for the body: status and branch info.
    local _extra=""
    [ -n "$bead_status" ] && _extra="Status: ${bead_status}."
    if [ -n "$tip_short" ] && [ -n "$ahead" ]; then
        _extra="${_extra:+$_extra$'\n'}Branch: ${tip_short} (${ahead} commit(s) ahead of ${base_ref})."
    fi
    mail send concierge \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind note <<MAILEOF >/dev/null 2>&1
## Note

$_subj

$id has been reopened for a rebase conflict $n times and the loop is not converging. This is
machinery cycling on itself, not a decision for Ryan (law-a-rebase-loop-is-sequenced-not-split)
— rebase with explicit guidance and fast-track the bead into a round the moment it certifies.

Conflicting file(s):
$_files_note
$ctx$decompose_ctx

Suggested action: $_sugg

$_extra
MAILEOF
}

# spira_ask_red_recurring — escalate a bead that has gone RED twice with the same reason class.
#
# The second RED with the same reason class means the aeon's work did not fix the root cause.
# Each reopen costs a full session; repeating it charges work that hits the same wall.
# Deduped on "$br red recurring $reason_class" so one open ask suppresses re-escalation.
spira_ask_red_recurring() {  # <bead> <branch> <repo-name> <reason-class> <first-red-epoch>
    local id="$1" br="$2" name="$3" reason_class="$4" first_epoch="${5:-0}"
    ask_already_open "$br red recurring $reason_class" && return 0
    local elapsed_h=0
    [ "${first_epoch:-0}" -gt 0 ] && \
        elapsed_h=$(( ( $(date +%s) - first_epoch ) / 3600 ))
    local _subj="$br red recurring: $reason_class twice on $id in $name"
    local _dflt="investigate why $br cannot land ($reason_class); close the bead if the work is superseded, or rebase by hand if the root cause is external"
    mail send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_subj

## Default
$_dflt

$id has gone RED twice with the same reason class ($reason_class) on $br in $name. The shas changed between marks, so each reopen charged a session to work that hit the same wall. Elapsed since first RED: ${elapsed_h}h.
MAILEOF
}

# spira_ask_rebase_refused — one deduplicated ask per closed bead the harness cannot rebase.
# A refusal is an infrastructure fault, not the work's fault — the bead stays closed.
spira_ask_rebase_refused() {  # <bead> <branch> <repo-name> <reason>
    local id="$1" br="$2" name="$3" reason="$4"
    ask_already_open "$br rebase refused" && return 0
    local _subj="$br rebase refused in $name: $reason"
    local _dflt="fix the infrastructure; $id stays closed and its branch will land on the next pass"
    mail send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_subj

## Default
$_dflt

$id is closed; its branch $br cannot be rebased onto the base in $name.
The failure is not a merge conflict — the work is not being reopened.
Reason: $reason.
MAILEOF
}

# spira_ask_budget_deferred — branch deferred by budget exhaustion N consecutive passes.
spira_ask_budget_deferred() {  # <branch> <repo> <count>
    local br="$1" name="$2" n="$3"
    ask_already_open "$br budget-deferred" && return 0
    local _subj="$br budget-deferred: $n consecutive passes in $name"
    mail send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind alert <<MAILEOF >/dev/null 2>&1
## Alert
$_subj

Branch $br has been deferred by budget exhaustion $n consecutive landing passes in $name.
The pass runs out of gate budget before reaching this branch.
MAILEOF
}

# spira_ask_refresh_loop — escalate a pr-mode branch that will not merge despite being
# repeatedly refreshed onto the base.
#
# A branch that has been rebased N times and its pull request still has not merged is not
# a slow landing — it is a stuck one. The obstacle is not staleness; the loop keeps
# removing that and the PR stays open. An aeon must own the investigation; the bead
# belongs back on the board at high priority so the next aeon finds it immediately rather
# than after whatever the queue was already doing.
#
# Deduped on the bead id (via ask_already_open) so a stuck branch sends one alert per cap,
# not one per pass: a monitor that fires every two minutes trains the operator to mute it,
# which is the failure law-alerts-must-be-actionable names.
spira_ask_refresh_loop() {  # <repo> <repo-name> <branch> <bead> <base> <n>
    local repo="$1" name="$2" br="$3" id="$4" base="$5" n="$6" behind
    ask_already_open "$id refresh cap" && return 0
    behind="$(git -C "$repo" rev-list --count "$br..$base" 2>/dev/null)" || behind="?"
    local _subj="Spira: $id's pull request has been rebased $n time(s) and still has not merged"
    local _dflt="reopen $id at P0 so an aeon owns the pull request's own failure, and leave the branch alone until it does"
    local _ev; _ev="$(printf 'BRANCH    %s in %s\nBASE      %s, %s commit(s) ahead of the branch\nREFRESHED %s time(s); the cap is %s\n\n%s\n' \
         "$br" "$name" "$base" "$behind" "$n" "${SPIRA_PR_REFRESH_MAX:-5}" "$(bead_context "$id")")"
    mail send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_subj

## Default
$_dflt

the bead is closed and its aeon is gone, so nothing is watching this pull request. Spira has been dragging $br back onto $base every time the base moved, and $n rebases have not got it merged — which means the obstacle is not staleness.

WHAT THIS BEAD IS FOR:
$_ev
MAILEOF
}

# land_escalate — ask the operator once when the landing leg is broken.
#
# The escalation is rate limited because a dead landing leg stays dead until someone fixes
# it, and a check that says so every two minutes is a check the operator learns to scroll past
# (law-alerts-must-be-actionable).
land_escalate() {        # land_escalate <subject-tail> <evidence>
    local why="$1" ev="$2" cd="$SPIRA_RUN/landing.escalated" now last
    # ALREADY ON HIS SCREEN? THEN DO NOT ASK AGAIN. The clock below is a floor, not the
    # answer: a dead landing leg stays dead until somebody fixes it, so an hourly re-ask put
    # NINE identical "Spira is landing nothing" decisions in the operator's pane in one day.
    # He closed eight of them and the ninth arrived anyway — "why do i keep getting this."
    # The queue is the database, so ask the database rather than this box's memory of it.
    ask_already_open "Spira is landing nothing" && return 0
    now="$(date +%s)"; last=0
    [ -f "$cd" ] && last="$(cat "$cd" 2>/dev/null || echo 0)"
    [ $(( now - last )) -lt "${SPIRA_LAND_ESCALATE_EVERY:-3600}" ] && return 0
    echo "$now" > "$cd"
    local _subj="Spira is landing nothing — $why"
    local _dflt="run \`landing-pass land\` by hand to see the failure, then file the fix as a bead"
    mail send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_subj

## Default
$_dflt

every finished branch in every repository is standing unlanded until this is fixed; aeons go on working and closing beads, so the board will read as healthy while nothing reaches a base branch

$ev
MAILEOF
    # An escalation is a write, never a movement. Counting a report of paralysis as progress
    # would mute the one check that notices paralysis.
    act "escalated: the landing leg is not running"
}

# How many rows a `bd --json` payload carries. Never `| wc -l` and never a grep: the payload
# is one line, and a warning printed before it would be counted as a row.
json_count() {           # stdin: JSON; stdout: an integer, 0 on anything unparseable
    python3 -c 'import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))' 2>/dev/null || echo 0
}

# --------------------------------------------------------------------------------------
# Liveness. NEVER pgrep -f: the pattern is a substring of any command line that mentions
# it, including the caller's own, so a `pgrep -f 'aeon.sh builder'` inside a script named
# in that pattern reports itself alive. pgrep may nominate; /proc decides, on the actual
# argv of the recorded pid.
# --------------------------------------------------------------------------------------
aeon_alive() {           # aeon_alive <pidfile> -> 0 if the recorded pid is a live aeon
    local pf="$1" pid
    [ -f "$pf" ] || return 1
    pid="$(cat "$pf" 2>/dev/null)"
    [ -n "${pid:-}" ] || return 1
    [ -d "/proc/$pid" ] || return 1
    # argv[0..] must actually be our runner, not a recycled pid. Capture, THEN match:
    # `tr ... | grep -q` under pipefail returns 141 when grep closes the pipe on the first
    # match, so the live case is exactly the one that could read as dead — and a liveness
    # test that false-negatives lets the reaper rob an aeon that is still working
    # (law-no-grep-q-under-pipefail).
    local cmd; cmd="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)"
    grep -qE '(^|/)aeon( |$)|aeon\.sh' <<< "$cmd" || return 1
    return 0
}

# aeon_count <fayth> [exclude-unit] -> how many aeons of that fayth are genuinely running.
#
# THE UNIT LIST, NOT THE PID FILE, for the same reason aeons_live_total reads units: aeon.sh
# writes its pidfile only after it claims a bead, so a fast re-summon landing in that gap
# counted the slot as free a second time (sp-0y2av). ${SPIRA_SYSTEMCTL:-systemctl}, not bare
# systemctl, so a suite can stub the fleet without a real user session.
#
# EXCLUDE-UNIT IS THE CALLER'S OWN UNIT, when the caller is itself a live aeon of this
# fayth. systemd-run's transient unit exists before aeon.sh's capacity check ever runs, so a
# sweep or a claim counting units of its own fayth was counting itself — "1/1 at capacity"
# on the very first aeon, respawning forever without ever seeing a free slot (sp-0hnm6).
# Callers outside an aeon's own unit (the sentinel's CHECK 7, watchtower's fleet total) pass
# nothing and get the old, unfiltered count.
#
# THE PID FALLBACK IS FOR SUITES, not for production, exactly as aeons_live_total's own. It
# never sees this bug: aeon.sh writes its own pidfile only after this check has already run.
aeon_count() {
    local fayth="$1" exclude="${2:-}" n=0 pf
    if [ "${SPIRA_SUMMON:-systemd-run}" = systemd-run ]; then
        n="$("${SPIRA_SYSTEMCTL:-systemctl}" --user list-units "spira-aeon-${fayth}-*" --no-legend 2>/dev/null \
            | awk -v ex="$exclude" '$1 != ex' | wc -l)"
        printf '%d' "${n:-0}"
        return
    fi
    for pf in "$SPIRA_RUN"/aeon-"$fayth"-*.pid; do
        [ -e "$pf" ] || continue
        if aeon_alive "$pf"; then n=$((n+1)); else rm -f "$pf"; fi
    done
    printf '%d' "$n"
}

# aeons_live_total -> how many aeons exist right now, across every persona and every lane.
#
# THE UNIT LIST, NOT THE PID FILES, and that difference is the whole point of this function.
# aeon.sh writes its pidfile only after it has claimed a bead (aeon.sh:509), while
# systemd-run returns the moment the transient unit exists — so within a single sentinel
# pass an aeon summoned one second ago is invisible to any pid-file count. A ceiling built
# on that count does not clamp the second summon of the same pass, which is precisely the
# lag that let a pool of one run a builder and an ops aeon in the same second on
# 2026-09-09 21:46:47. The unit is authoritative the instant it is asked for.
#
# THE PID FALLBACK IS FOR SUITES, not for production: a test overrides SPIRA_SUMMON with a
# stub, no unit is ever created, and a systemd count would be 0 forever — a ceiling that
# never binds and never says so. Counting pid files there keeps the ceiling testable, and
# the lag does not apply because a stub does not race.
aeons_live_total() {
    local n=0 pf
    if [ "${SPIRA_SUMMON:-systemd-run}" = systemd-run ]; then
        # `| wc -l` and never `grep -c`: grep exits 1 on no matches, which under pipefail
        # turns an idle fleet into a failed read (law-no-grep-q-under-pipefail, same shape).
        n="$(systemctl --user list-units 'spira-aeon-*' --no-legend 2>/dev/null | wc -l)"
        printf '%d' "${n:-0}"
        return
    fi
    for pf in "$SPIRA_RUN"/aeon-*.pid; do
        [ -e "$pf" ] || continue
        if aeon_alive "$pf"; then n=$((n+1)); else rm -f "$pf"; fi
    done
    printf '%d' "$n"
}

# aeons_live_lanes -> how many lane aeons exist right now, across all lane fayths.
aeons_live_lanes() {
    local n=0 f fn
    if [ "${SPIRA_SUMMON:-systemd-run}" = systemd-run ]; then
        for f in $(spira_lane_fayths); do
            fn="$(fayth_get "$f" FAYTH_NAME "$f")"
            n=$(( n + $(systemctl --user list-units "spira-aeon-${fn}-*" --no-legend 2>/dev/null | wc -l) ))
        done
        printf '%d' "${n:-0}"
        return
    fi
    for f in $(spira_lane_fayths); do
        fn="$(fayth_get "$f" FAYTH_NAME "$f")"
        local pf
        for pf in "$SPIRA_RUN"/aeon-"$fn"-*.pid; do
            [ -e "$pf" ] || continue
            if aeon_alive "$pf"; then n=$((n+1)); else rm -f "$pf"; fi
        done
    done
    printf '%d' "$n"
}

# --------------------------------------------------------------------------------------
# THE CHAMBER. A fayth carries FAYTH_LABELS / FAYTH_EXCLUDE_LABELS precisely so that its
# partition of the graph is ITS OWN, and every question the harness asks about a persona
# must be asked through that persona's own predicate. Asking one predicate on behalf of all
# of them is not a rounding error, it is an unreachable persona: sentinel.sh CHECK 7 gated
# every summon on a single `--label spira,plan` count, so Ops could only ever wake when the
# BUILDER had work — exactly backwards for an on-call role, and ops.fayth shipped complete
# and inert, with a correct predicate nothing ever evaluated.
#
# The list of personas is likewise DISCOVERED, never hardcoded. A default of `builder`
# meant a persona that landed was not a persona that ran, and nothing said so; enumerating
# the chamber makes installing a fayth the whole of installing a persona.
# --------------------------------------------------------------------------------------
# fayth_names, spira_fayths and its task/lane splits, fayth_get, fayth_partitions,
# fayths_for_labels and persona_model (below) are SHIMS onto `spira-config fayth ...`
# (wave 4.22, sp-r5zd2: the chamber registry's home is spira-config now — the chamber IS
# config). The logic, including the leading-space quirk `spira_fayths` can carry when
# there are no fixed fayths, lives in spira-config/src/chamber.rs; this file keeps the
# names so the ~50 bash sourcers that call them need no change.
fayth_names() {          # every persona defined in the chamber, one per line
    spira-config fayth names
}

spira_fayths() {         # the personas this harness runs, space separated, IN PRIORITY ORDER
    spira-config fayth roster
}

# spira_task_fayths -> the personas the sentinel's pool summons: everything that is not a
# party member and not a lane fayth.
#
# EVERY OTHER USE OF THE ROSTER KEEPS THEM. A party member's beads must still be reaped when
# its lease dies, its partition still swept for stalled work, and its closed beads still
# checked for having landed — those were each written against one hardcoded partition once
# and the fix was to ask every persona's own predicate. Narrowing THAT would restore the bug
# by another door. This narrows only who the pool summons.
#
# FAYTH_LANE is the declared form; FAYTH_ROLE=party is preserved as a backward-compatible
# alias so an operator's custom fayth still works after upgrading. Both say the same thing:
# this persona is not drawn from SPIRA_MAX_AEONS.
spira_task_fayths() {
    spira-config fayth task
}

# spira_lane_fayths -> the personas that belong to a declared lane (FAYTH_LANE set).
#
# A lane fayth draws from its own FAYTH_MAX_CONCURRENT rather than from SPIRA_MAX_AEONS,
# so the pool can be fully occupied by builders and a lane fayth still has room. The
# sentinel's CHECK 7 handles lane fayths in a separate loop after the task pool, calling
# summon_fayth without a pool argument so the pool never clamps a lane fayth's capacity.
#
# THE NAME MUST APPEAR IN SPIRA_LANES for the lane to be declared, but that is a
# documentation and validation concern — a fayth that names an undeclared lane still
# functions, because the mechanism (FAYTH_LANE present → not a task fayth) does not
# require the name to be on the list.
spira_lane_fayths() {
    spira-config fayth lane
}

# persona_model <fayth> [default] -> the model this persona launches under.
#
# THE ONE RESOLVER (sp-z134z, sp-zs04v.4): every launch path calls this rather than each
# reading a model out of its own copy of the fayth. Reads persona.<fayth>.model out of
# spira.toml through spira-config — the operator's override surface, changeable without a
# change to main — falling back to a built-in default. NOT a fallback to the fayth's own
# FAYTH_MODEL: that was the file-scraping this resolver replaces, so a persona with no
# [persona.<name>] table launches under the bare built-in default, not its fayth's
# declaration (conf.sh's spira_toml_resolve seeds that table from the fayth on first read,
# so the declared value is not lost — see its own comment for the production-safety case).
#
# CALLS spira_toml_resolve() FRESH, not $SPIRA_TOML_FILE cached at conf.sh-sourcing time —
# the same staleness fix repo_field/repo_names carry (sp-zs04v.3): a long-lived process
# (cockpit, a sweep) must see a fayth edited after it started, and spira_toml_resolve's own
# mtime check keeps a call that finds nothing stale cheap.
persona_model() {
    spira-config fayth model "$1" "${2:-}"
}

fayth_get() {            # fayth_get <fayth> <VAR> [default] -> one field of a fayth
    spira-config fayth get "$1" "$2" "${3:-}"
}

# READY_ARGS — the ONE definition of "a bead an aeon can take". Everything that counts
# candidates, lists them or claims one reads this array, so the count the sentinel summons
# on and the query the aeon claims through cannot ask different questions. Copies of a
# predicate agree only until somebody edits one of them, and the disagreement presents as a
# healthy queue.
#
# `--limit 0` is not optional. `bd ready` pages at 100 and silently drops the rest, and an
# installation that imported a predecessor's beads sorts thousands of them above every native
# plan bead — the plan read as having no workable step at all until this was found.
#
# `--exclude-type epic` is not optional. An epic with no blockers reads as ready and would be
# claimed and "implemented", which is not a thing an epic means.
#
# `-u` is not optional, and it is the hardest of the three to see. `bd ready` counts a bead
# by status and blockers; `bd ready --claim` refuses one already carrying another actor's
# assignee. So a bead orphaned by a dead aeon is counted forever and taken never. Measured
# 2026-09-06: 13 plan beads were open, unleased and assigned to aeons that no longer existed,
# and CHECK 7 summoned an aeon every two minutes to report idle within one second. Both
# programs were right and they were answering different questions; `-u` makes it one
# question (law-absence-needs-a-positive-control — a "ready" that cannot be claimed is worse
# than a zero, because it reads as a healthy queue).
#
# claim.pools is unset on this installation, so nothing is legitimately pre-assigned to an
# alias an aeon could still claim. If that ever changes, this is the line that must learn
# about it: `-u` would then hide pool work that `--claim` would happily take.
#
# `--label SPIRA_SCOPE_LABEL` is included when the key is non-empty, keeping beads from
# other repositories out of every count, claim and strand report. An empty key means the
# operator has explicitly disabled scope restriction; the ready set is then unrestricted,
# which is the correct behaviour for a fleet with no scope boundary. The same convention
# appears in fayth_scope_check (lib.sh) and orphan_claims.
#
# detect_unclaimable_ready intentionally does NOT use READY_ARGS; it reads the full ready
# set (no scope filter) so that beads missing the scope label are seen and reported as
# UNCLAIMABLE. They are excluded from claims, counts and strand reports via READY_ARGS, but
# the detector's job is to name the condition — exclusion is not a reason to stay silent.
READY_ARGS=(ready --limit 0 --exclude-type epic,event -u)
[[ -n "${SPIRA_SCOPE_LABEL:-}" ]] && READY_ARGS+=(--label "$SPIRA_SCOPE_LABEL")
[[ -n "${SPIRA_NO_LOOP_LABEL:-}" ]] && READY_ARGS+=(--exclude-label "$SPIRA_NO_LOOP_LABEL")

# ready_raw_args -> READY_ARGS without the SPIRA_SCOPE_LABEL restriction, one argv token a
# line. This is the broadest "is this bead claimable by ANY persona" query — detect_unclaimable_
# ready's own reason for existing is seeing a bead that is MISSING the scope label, so it
# cannot ask through READY_ARGS. It is also the one shape sentinel.sh's full pass fetches
# ONCE into SPIRA_READY_SNAPSHOT (sp-bo67y): every scope-restricted consumer (bulk_ready_by_
# fayth via each fayth's own FAYTH_LABELS, mark_queue_waiters via an explicit filter) narrows
# the cached superset in-process rather than asking bd again with a narrower --label.
ready_raw_args() {
    local args=(ready --limit 0 --exclude-type epic,event -u)
    [[ -n "${SPIRA_NO_LOOP_LABEL:-}" ]] && args+=(--exclude-label "$SPIRA_NO_LOOP_LABEL")
    printf '%s\n' "${args[@]}"
}

# ready_count <labels> <exclude-labels> -> how many beads that predicate can claim.
#
# A FAILED QUERY IS NOT A ZERO. bd's own circuit breaker or a Dolt lock can refuse the read
# outright; treating that refusal the same as "the predicate matched nothing" is what made a
# transient store failure read as an empty queue (sp-3ntca). This still prints '0' on stdout
# on failure, so a caller that only reads the count (sentinel.sh's plan_ready) is unaffected,
# but now returns 1 and puts the bd error on its OWN stderr — the fayth_ready subshell
# captures exactly that line.
ready_count() {
    local out rc _errtmp
    _errtmp="$(mktemp)"
    out="$(bdq "${READY_ARGS[@]}" --label "$1" --exclude-label "$2" --json 2>"$_errtmp")"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'ready_count: query failed: %s\n' "$(head -1 "$_errtmp" 2>/dev/null)" >&2
        rm -f "$_errtmp"
        printf '0'
        return 1
    fi
    rm -f "$_errtmp"
    printf '%s' "$out" | json_only | json_count
}

# epic_parent_lookup <ready-beads-json> -> {"prio": {epic_id: priority, ...}, "started": [epic_id, ...]}
#
# THE EPIC-FIRST CLAIM ORDER'S OWN BATCHED LOOKUP (sp-ns46j). Every ready bead already
# carries its own "parent" field (bd list/ready return it inline), so which epic a bead
# belongs to costs nothing further to learn; what is missing is the EPIC's own priority and
# whether it has started. Both are fetched here — once per distinct epic referenced, never
# once per bead, however many beads reference the same epic.
#
# Priority: one `bd list --id a,b,c` for every distinct epic in the set.
# Started (any child closed, in progress, or carrying the submitted label): `bd children`
# takes one parent at a time, so this is one query per DISTINCT epic — still bounded by the
# number of epics in flight, never by the number of ready beads.
#
# A NONZERO RETURN IS A FAILED LOOKUP, NOT AN EMPTY ONE (law-payloads-go-on-stdin): the
# caller must treat it as a claim-error, the same as a failed bd query, never as "nothing
# ready".
epic_parent_lookup() {
    local ready_json="$1" parents prio_json="[]" started_csv=""
    parents="$(printf '%s' "$ready_json" | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d if isinstance(d, list) else [d]
ids = sorted({r["parent"] for r in d if r.get("parent")})
print("\n".join(ids))
' 2>/dev/null)"
    if [ -n "$parents" ]; then
        local csv; csv="$(printf '%s' "$parents" | paste -sd, -)"
        prio_json="$(bdjson list --id "$csv" --status all --limit 0 2>/dev/null)"
        [ -n "$prio_json" ] || prio_json="[]"
        local started_ids=() p kids
        while IFS= read -r p; do
            [ -n "$p" ] || continue
            kids="$(bdjson children "$p" 2>/dev/null)"
            [ -n "$kids" ] || continue
            if printf '%s' "$kids" | SPIRA_SUBMITTED_LABEL="${SPIRA_SUBMITTED_LABEL:-spira-submitted}" python3 -c '
import sys, json, os
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
subl = os.environ.get("SPIRA_SUBMITTED_LABEL", "spira-submitted")
for c in d:
    if c.get("status") in ("in_progress", "closed"):
        sys.exit(0)
    if c.get("status") == "open" and subl in (c.get("labels") or []):
        sys.exit(0)
sys.exit(1)
' 2>/dev/null; then
                started_ids+=("$p")
            fi
        done <<< "$parents"
        started_csv="$(IFS=,; printf '%s' "${started_ids[*]:-}")"
    fi
    # prio_json ON STDIN, NEVER ARGV (law-payloads-go-on-stdin): it scales with the number of
    # DISTINCT EPICS referenced, unbounded by the same fact that made ready_json unbounded
    # (sp-o4trx). started_csv is a short id list, safe as an argv token.
    printf '%s' "$prio_json" | python3 -c '
import json, sys
prio_rows = json.loads(sys.stdin.read() or "[]")
prio_rows = prio_rows if isinstance(prio_rows, list) else [prio_rows]
prio = {r["id"]: r.get("priority", 99) for r in prio_rows}
started = [x for x in sys.argv[1].split(",") if x]
print(json.dumps({"prio": prio, "started": started}))
' "$started_csv"
}

# epic_rank_rows <ready-beads-json> <epic-lookup-json> [<resumable-csv>] -> TSV, best first:
#   epic_priority  epic_started(0|1)  bead_priority  resumable(0|1)  created_at  id  epic_id
#
# A NONZERO RETURN IS A FAILED RANK, NOT AN EMPTY ONE (law-payloads-go-on-stdin): the caller
# must treat it as a claim-error, the same as a failed bd query, never as "nothing ready".
#
# THE RANK, in the order the rule states it (sp-ns46j): the parent epic's priority; then,
# among epics of equal priority, a started epic before an unstarted one; then the bead's own
# priority within the epic; then resumable work, then oldest. A bead with no epic (or whose
# epic the lookup found nothing for) ranks as its own epic, at its own priority, unstarted —
# epic_id is then the bead's own id, so a round-cutter grouping on that column still gets one
# group per bead rather than merging every unaffiliated bead into one.
#
# A REWORKED OR EJECTED BEAD KEEPS ITS EPIC'S RANK for free: nothing here reads the bead's
# own history, only its current parent and priority, so a bead sent back to ready re-enters
# at exactly the rank its epic already holds.
epic_rank_rows() {
    local ready_json="$1" lookup_json="$2" resume_csv="${3:-}"
    # READY_JSON ON STDIN, LOOKUP_JSON IN A TEMP FILE — NEVER ARGV (law-payloads-go-on-stdin).
    # At 142 ready beads this argument alone was already past MAX_ARG_STRLEN and every exec
    # here died E2BIG, silently, as an empty ranked list read as "nothing ready" (sp-o4trx).
    local _lkf; _lkf="$(mktemp)" || return 1
    printf '%s' "$lookup_json" > "$_lkf"
    printf '%s' "$ready_json" | LOOKUP_FILE="$_lkf" python3 -c '
import json, os, sys

ready = json.loads(sys.stdin.read() or "[]")
ready = ready if isinstance(ready, list) else [ready]
with open(os.environ["LOOKUP_FILE"]) as f:
    lookup = json.loads(f.read() or "{}")
prio = lookup.get("prio", {})
started = set(lookup.get("started", []))
resume = set(x for x in sys.argv[1].split(",") if x)

def rank(r):
    pid = r.get("parent") or ""
    epic_id = pid or r["id"]
    eprio = prio.get(pid, r.get("priority", 99)) if pid else r.get("priority", 99)
    estarted = 0 if pid in started else 1
    bprio = r.get("priority", 99)
    resumable = 0 if r["id"] in resume else 1
    age = r.get("created_at") or r.get("updated_at") or ""
    return (eprio, estarted, bprio, resumable, age, r["id"], epic_id)

for r in sorted(ready, key=rank):
    k = rank(r)
    print("%s\t%s\t%s\t%s\t%s\t%s\t%s" % k)
' "$resume_csv"
    local _rc=$?
    rm -f "$_lkf"
    return $_rc
}

# claim_retry <bdq claim args...> -> stdout: bd's JSON result (already through json_only).
# Empty stdout with rc 0 is a REAL empty result — bd ran the query and it matched nothing.
# rc 1 means every retry failed to complete at all; the first line of bd's own stderr from
# the last attempt is written to THIS function's stderr, one line, prefixed — never to a
# global variable, because every caller here reads claim_retry through a command
# substitution, and a command substitution is a subshell: an assignment made inside it is
# gone the instant the substitution completes. A caller that wants the message captures
# this function's stderr directly (a `{ claim_retry ...; } 2>"$errfile"` around the call,
# not a plain variable read afterward).
#
# CONCURRENT CLAIMS ARE EXPECTED CONTENTION, NOT AN EMPTY QUEUE. Aeons are summoned seconds
# apart and read the same store; a lock or commit collision at that instant is a different
# fact from a query that ran cleanly and found zero rows, and collapsing the two is what let
# a transient bd failure report as "nothing ready to claim" while ~90 beads were ready
# (sp-3ntca). A short retry absorbs the ordinary case — another aeon's claim landing between
# this one's read and write — before the failure is trusted at all.
claim_retry() {
    local out rc _errtmp _attempt=1 _tries="${SPIRA_CLAIM_RETRIES:-3}" _delay="${SPIRA_CLAIM_RETRY_DELAY_S:-1}"
    _errtmp="$(mktemp)"
    while [ "$_attempt" -le "$_tries" ]; do
        out="$(bdq "$@" --json 2>"$_errtmp")"
        rc=$?
        if [ "$rc" -eq 0 ]; then
            rm -f "$_errtmp"
            printf '%s' "$out" | json_only
            return 0
        fi
        [ "$_attempt" -lt "$_tries" ] && sleep "$_delay"
        _attempt=$((_attempt + 1))
    done
    printf 'claim_retry: query failed after %s attempt(s): %s\n' \
        "$_tries" "$(head -1 "$_errtmp" 2>/dev/null)" >&2
    rm -f "$_errtmp"
    return 1
}

# fayth_exclude <fayth> -> the persona's own exclusions, plus every OTHER persona's claim.
#
# THE ENCOUNTER CHOOSES THE PARTY (the operator, 2026-09-07: "Spira is the world. there are
# many parties within it — with different compositions — and hence many concurrent
# encounters... having beads declare the personas they prefer is a nice touch").
#
# Until now the arrow pointed the other way: each persona carried a predicate and trawled the
# whole graph for beads it liked, so a bead had no say in who worked it and two personas
# whose partitions overlapped raced for the same work. A bead may now carry `fayth:<name>`
# and that is a claim on WHO: the named persona sees it, every other persona does not.
#
# A BEAD THAT NAMES NOBODY BEHAVES EXACTLY AS BEFORE, which is what makes this safe to land
# on a live graph — the 89 beads out there today declare no preference and every one of them
# stays claimable by whoever the partition already allowed.
#
# IT NARROWS, IT NEVER WIDENS. `fayth:ops` on a bead outside Ops's partition does not hand it
# to Ops; the partition still decides WHETHER the work is yours, and this decides only that
# it is not somebody else's. A preference that could also grant would be a way to route work
# past a persona's own predicate, which is the one thing FAYTH_LABELS exists to guarantee.
#
# `--exclude-label` is OR (verified against bd: adding an unused label to the list does not
# change the count), so appending is exactly the semantics wanted here.
#
# ready_shared_exclude -> the labels every "is this claimable" predicate excludes regardless
# of caller: SPIRA_QUEUE_WAIT_LABEL, SPIRA_SUBMITTED_LABEL and SPIRA_OPEN_CHILDREN_LABEL,
# none of which marks a bead an aeon can take. fayth_exclude and strand.sh's classify_one
# both call this rather than each carrying its own copy, because a copy is how one of them
# drifts (sp-wnsks: strand.sh's counted a spira-submitted bead as ready and reported
# starvation on a partition the sentinel correctly saw as empty).
ready_shared_exclude() {
    local out=""
    [ -n "${SPIRA_QUEUE_WAIT_LABEL:-}" ] && out="${out:+$out,}${SPIRA_QUEUE_WAIT_LABEL}"
    [ -n "${SPIRA_SUBMITTED_LABEL:-}" ] && out="${out:+$out,}${SPIRA_SUBMITTED_LABEL}"
    [ -n "${SPIRA_OPEN_CHILDREN_LABEL:-}" ] && out="${out:+$out,}${SPIRA_OPEN_CHILDREN_LABEL}"
    printf '%s' "$out"
}

fayth_exclude() {        # fayth_exclude <fayth> -> comma-separated exclusions
    local me="$1" own="${2:-}" f out shared
    out="$own"
    shared="$(ready_shared_exclude)"
    [ -n "$shared" ] && out="${out:+$out,}${shared}"
    for f in $(spira_fayths 2>/dev/null); do
        [ "$f" = "$me" ] && continue
        out="${out:+$out,}fayth:$f"
    done
    printf '%s' "$out"
}

# fayth_ready <fayth> -> claimable beads under ITS OWN predicate, on stdout.
#
# EXIT CODE NAMES WHICH OF TWO DIFFERENT THINGS WENT WRONG, because "no fayth in the
# chamber" and "the ready query itself failed" used to collapse into the same caller branch
# and the same log line — so a transient bd failure was reported to the operator as a
# missing persona file, the one description that cannot be true while the persona is
# actively summoning (sp-3ntca). 2: no such fayth file. 1: the file exists but ready_count
# could not complete. 0: a real count, zero included.
#
# THE REASON GOES TO THIS FUNCTION'S OWN STDERR, NEVER A GLOBAL. Every caller reads
# fayth_ready through a command substitution (`r="$(fayth_ready "$f")"`), which is a
# subshell — an assignment made inside fayth_ready during that call cannot reach the
# caller's shell at all, so a global here would silently read as whatever it held before
# (this is the same trap claim_retry documents). A caller that wants the reason redirects
# this function's stderr to a file around the call, same as claim_retry's callers do.
fayth_ready() {
    local f="$1" F="$SPIRA_HOME/chamber/$1.fayth" out rc _errtmp
    if [ ! -f "$F" ]; then
        printf '0'
        printf 'fayth_ready: no fayth in the chamber: %s\n' "$F" >&2
        return 2
    fi
    # SPIRA_READY_CACHE: sentinel.sh --summon-only precomputes every fayth's ready count
    # with ONE bd call (bulk_ready_by_fayth) rather than paying this function's own call
    # once per partition, to hit its ~1s target (sp-0y2av). Unset in the full pass, which
    # still pays its own per-fayth query below exactly as before.
    if [ -n "${SPIRA_READY_CACHE:-}" ] && [ -f "$SPIRA_READY_CACHE" ]; then
        awk -v f="$f" '$1==f{print $2; found=1} END{if(!found) print 0}' "$SPIRA_READY_CACHE"
        return 0
    fi
    _errtmp="$(mktemp)"
    # shellcheck disable=SC1090
    out="$( ( . "$F" 2>/dev/null
              ready_count "${FAYTH_LABELS:-}" "$(fayth_exclude "$f" "${FAYTH_EXCLUDE_LABELS:-}")" \
      ) 2>"$_errtmp" )"
    rc=$?
    [ "$rc" -ne 0 ] && cat "$_errtmp" >&2
    rm -f "$_errtmp"
    printf '%s' "$out"
    return "$rc"
}

# bulk_ready_by_fayth -> "<fayth> <count>" lines, one per active fayth, from ONE bd query.
#
# fayth_ready pays one `bd ready` call per partition; sentinel.sh --summon-only cannot
# afford N of those and still land near 1s, so this fetches the whole ready set once and
# buckets it in-process with ready-bucket.py, applying the identical predicate fayth_ready
# would (FAYTH_LABELS, FAYTH_EXCLUDE_LABELS, and fayth: preference — mirrored from
# unclaimable.py's own claimers loop, not reimplemented a third time).
#
# SPIRA_READY_SNAPSHOT, WHEN SET, REPLACES THE QUERY — sentinel.sh's full pass fetches the
# ready_raw_args superset once (sp-bo67y) and every fayth here still gets the identical
# count: FAYTH_LABELS already carries SPIRA_SCOPE_LABEL itself (every chamber file sets
# `FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}..."`), so the `inc <= L` test in
# ready-bucket.py rejects an out-of-scope bead exactly as READY_ARGS's own --label would
# have, and reading the broader snapshot changes no fayth's count.
bulk_ready_by_fayth() {
    local raw parts f inc exc
    if [ -n "${SPIRA_READY_SNAPSHOT:-}" ] && [ -r "$SPIRA_READY_SNAPSHOT" ]; then
        raw="$(cat "$SPIRA_READY_SNAPSHOT")"
    else
        raw="$(bdjson "${READY_ARGS[@]}" 2>/dev/null)"
    fi
    [ -n "$raw" ] || return 0
    parts=""
    for f in $(spira_fayths); do
        inc="$(fayth_get "$f" FAYTH_LABELS)"
        [ -n "$inc" ] || continue
        exc="$(fayth_get "$f" FAYTH_EXCLUDE_LABELS)"
        parts="${parts}${f}|${inc}|${exc}"$'\n'
    done
    [ -n "$parts" ] || return 0
    printf '%s' "$raw" | json_only \
        | PARTS="$parts" SPIRA_QUEUE_WAIT_LABEL="${SPIRA_QUEUE_WAIT_LABEL:-}" \
          SPIRA_SUBMITTED_LABEL="${SPIRA_SUBMITTED_LABEL:-}" ready-bucket.py
}

# express_ready_in_task_pool <task-fayths> <express-label>
# Returns 0 when an express bead is ready in any task partition, 1 otherwise.
# Composes with FAYTH_LABELS rather than bypassing them — the partition stays intact.
express_ready_in_task_pool() {
    local task_fayths="$1" express_label="$2" f ff ec
    for f in $task_fayths; do
        ff="$SPIRA_HOME/chamber/$f.fayth"
        [ -f "$ff" ] || continue
        # shellcheck disable=SC1090
        ec="$( ( . "$ff" 2>/dev/null
                 [ -n "${FAYTH_LABELS:-}" ] || exit 1
                 ready_count "${FAYTH_LABELS},${express_label}" \
                     "$(fayth_exclude "$f" "${FAYTH_EXCLUDE_LABELS:-}")" ) 2>/dev/null)" || true
        [ "${ec:-0}" -gt 0 ] 2>/dev/null && return 0
    done
    return 1
}

# check7_pool_decision <throttled:0|1> <free> <express-ready:0|1> -> the task pool CHECK 7
# grants for this pass.
#
# An express grant sets the pool to EXACTLY 1, never to whatever free happened to hold — the
# old inline form only ever raised a throttled pool toward 1 (`[ pool -lt 1 ] && pool=1`), so
# a free value already at or above 1 passed through untouched and the throttle did nothing
# the one pass an express bead was ready (sp-zcvh1). One express grant claims one express
# bead; a second cannot be summoned until the next pass re-evaluates readiness.
check7_pool_decision() {
    local throttled="$1" free="$2" express_ready="$3"
    if [ "$throttled" = 1 ]; then
        if [ "$express_ready" = 1 ]; then printf '1'; else printf '0'; fi
    else
        printf '%s' "$free"
    fi
}

# lane_rotate <last> <lanes...> -> the lane evaluation order for this pass, on stdout.
# <last> is the lane fayth summoned on the previous pass (empty on the first pass, or when
# it no longer appears in <lanes>). Rotates it and everything before it to the end, so a
# lane that won last pass draws last this pass and the collective cap cannot be monopolised
# by whichever lane happens to sort first (G1: this used to be reimplemented inside
# test-lane-ceiling.sh rather than exercised, so a broken rotation here would still pass).
lane_rotate() {
    local last="$1"; shift
    local lanes="$*" lf before="" after="" found=0
    if [ -z "$last" ] || [ -z "$lanes" ]; then
        printf '%s' "$lanes"
        return 0
    fi
    for lf in $lanes; do
        if [ "$found" = 1 ]; then after="$after $lf"
        elif [ "$lf" = "$last" ]; then before="$before $lf"; found=1
        else before="$before $lf"
        fi
    done
    after="${after# }"; before="${before# }"
    # <last> absent from <lanes> leaves $found=0, so every lane fell into $before and
    # $after is empty — the branch below then prints $before verbatim, i.e. $lanes
    # unchanged, without a special case for "not found".
    if [ -n "$after" ] && [ -n "$before" ]; then
        printf '%s %s' "$after" "$before"
    else
        printf '%s' "${after}${before}"
    fi
}

# ck7_pool <max-aeons> <task-live> -> the task pool CHECK 7 starts a pass with, before any
# throttle is applied: <max-aeons> (SPIRA_MAX_AEONS) minus the task fayths already live,
# floored at 0. Empty when <max-aeons> is empty — no pool configured means every persona's
# own concurrency cap applies unchanged, exactly as before this pool existed.
ck7_pool() {
    local max_aeons="$1" task_live="${2:-0}"
    [ -n "$max_aeons" ] || return 0
    printf '%s' "$(( max_aeons > task_live ? max_aeons - task_live : 0 ))"
}

# ck7_throttled <stamp-exists:0|1> <override> -> 1 when the admission throttle stamp
# holds the task pool at 0 this pass, 0 otherwise. SPIRA_QUEUE_THROTTLE_OVERRIDE=off pins
# the pass unthrottled regardless of the stamp (G2).
ck7_throttled() {
    local stamp_exists="$1" override="${2:-}"
    if [ "$stamp_exists" = 1 ] && [ "$override" != off ]; then printf 1; else printf 0; fi
}

# ck7_fill_cap <fill-count-this-persona> <pool> -> "stop" once the pool (if bounded) is
# spent or <fill-count-this-persona> has reached SPIRA_MAX_LIVE_AEONS (default 4), else
# "continue". The cap is what stops an unbounded summon loop for one persona on a host
# that sets no pool at all — summon_fayth's own concurrency check is the only other guard,
# and it is per-persona configuration, not a pass-wide backstop (G3).
ck7_fill_cap() {
    local fill="$1" pool="${2:-}"
    if [ -n "$pool" ] && [ "$pool" -le 0 ]; then printf stop; return 0; fi
    if [ "$fill" -ge "${SPIRA_MAX_LIVE_AEONS:-4}" ]; then printf stop; return 0; fi
    printf continue
}

# mark_queue_waiters — apply/remove SPIRA_QUEUE_WAIT_LABEL on beads whose closed blocker
# is in the queue pipeline (CERTIFIED or BATCHED) and has not yet reached LANDED.
#
# bd considers a dep resolved once the blocker is closed, so the dependent appears in
# `bd ready`. In queue mode, CLOSED ≠ LANDED — the work is not yet on base. This label
# keeps fayth_ready from counting those beads until the blocker's landstate reaches LANDED.
#
# Same-repo work-bead blockers release earlier (at CERTIFIED) under the stacked-dependents
# rule (stack_max_depth) — see wiki/projects/spira/designs/stacked-dependents-2026-09-28.md.
# This label still governs cross-repo and non-work blockers, which wait for LANDED.
#
# A closed blocker with tip="none" (design, diagnosis, superseded) has no commit and will
# never reach LANDED via the queue path; it counts as satisfied regardless of landstate.
#
# All label decisions are made in a single pass over the union of labeled and ready beads
# using consistent dep data, preventing the clear-then-apply flip-flop that occurs when
# the release and apply steps disagree on which deps are visible.
mark_queue_waiters() {
    local label="${SPIRA_QUEUE_WAIT_LABEL:-}"
    [ -n "$label" ] || return 0
    local landstate_dir="$SPIRA_RUN/landstate"
    local id state tip qblockers=""

    # Active queue blockers: CERTIFIED or BATCHED beads with a real commit tip.
    # tip="none" means no commit was recorded (design, diagnosis, superseded bead);
    # such a bead will never reach LANDED and is treated as already satisfied.
    #
    # ONE AWK FOR THE WHOLE DIRECTORY, not a per-file loop forking two awks apiece — 546
    # landstate files forked ~1,092 processes to read two fields each (sp-bo67y). A single
    # invocation over every filename argument resets FNR at each file boundary, so one call
    # still prints exactly one "<path> <state> <tip>" line per file.
    if [ -d "$landstate_dir" ]; then
        local -a _mq_all=("$landstate_dir"/*) _mq_files=() _mq_sf
        for _mq_sf in "${_mq_all[@]}"; do [ -f "$_mq_sf" ] && _mq_files+=("$_mq_sf"); done
        if [ "${#_mq_files[@]}" -gt 0 ]; then
            while IFS=' ' read -r sf state tip; do
                [ -n "$sf" ] || continue
                id="${sf##*/}"
                case "$state" in
                    CERTIFIED|BATCHED)
                        [ -n "$tip" ] && [ "$tip" != "none" ] \
                            && qblockers="${qblockers}${id} "
                        ;;
                esac
            done < <(awk 'FNR==1{print FILENAME, $1, $2}' "${_mq_files[@]}" 2>/dev/null)
        fi
    fi

    # Collect labeled beads and ready beads, then decide each bead's label state
    # once — no separate release and apply passes that can clear for one blocker
    # and re-apply for another in the same run.
    local labeled_json ready_json
    labeled_json="$(bdjson list --status open --label "$label" --limit 0 2>/dev/null)" \
        || labeled_json=""
    ready_json=""
    if [ -n "$qblockers" ]; then
        # SPIRA_READY_SNAPSHOT, WHEN SET, IS THE BROADER ready_raw_args SUPERSET (no
        # SPIRA_SCOPE_LABEL filter) that sentinel.sh's full pass fetches once (sp-bo67y).
        # READY_ARGS itself narrows to scope, so the python below re-applies that one
        # restriction rather than asking bd again for an identical, narrower query.
        if [ -n "${SPIRA_READY_SNAPSHOT:-}" ] && [ -r "$SPIRA_READY_SNAPSHOT" ]; then
            ready_json="$(SPIRA_SCOPE_LABEL="${SPIRA_SCOPE_LABEL:-}" python3 -c '
import json, os, sys
scope = os.environ.get("SPIRA_SCOPE_LABEL", "")
try: d = json.load(sys.stdin)
except Exception: d = []
d = d if isinstance(d, list) else [d]
if scope:
    d = [b for b in d if scope in (b.get("labels") or [])]
print(json.dumps(d))
' < "$SPIRA_READY_SNAPSHOT" 2>/dev/null)" || ready_json=""
        else
            ready_json="$(bdjson "${READY_ARGS[@]}" 2>/dev/null)" || ready_json=""
        fi
    fi

    # labeled_json goes in a temp file, ready_json on stdin — neither through the
    # environment or argv (law-payloads-go-on-stdin): both scale with queue size, unbounded.
    local _lqf; _lqf="$(mktemp)" || return 1
    printf '%s' "${labeled_json:-[]}" > "$_lqf"
    QUEUE_BLOCKERS="$qblockers" QUEUE_LABEL="$label" \
    LABELED_FILE="$_lqf" python3 -c '
import json, sys, os

active = set(os.environ.get("QUEUE_BLOCKERS", "").split())
label  = os.environ["QUEUE_LABEL"]

def parse(s):
    if not s: return []
    try: d = json.loads(s); return d if isinstance(d, list) else [d]
    except Exception: return []

with open(os.environ["LABELED_FILE"]) as f:
    labeled = {b["id"]: b for b in parse(f.read()) if b.get("id")}
ready   = {b["id"]: b for b in parse(sys.stdin.read()) if b.get("id")}

def blocks_active(bead):
    return any(
        (dep.get("dependency_type") or dep.get("type")) == "blocks"
        and dep.get("depends_on_id") in active
        for dep in (bead.get("dependencies") or [])
    )

# ready dep data is authoritative; labeled-only beads (in_progress, extra blockers)
# get no dep check — removing the label is safe since the bead is not claimable.
for bid, bead in ready.items():
    currently = bid in labeled
    want = bool(active) and blocks_active(bead)
    if want and not currently: print("add", bid)
    elif not want and currently: print("remove", bid)

for bid in labeled:
    if bid not in ready:
        print("remove", bid)
' <<< "${ready_json:-[]}" 2>/dev/null \
    | while IFS=' ' read -r action id; do
        [ -n "$id" ] || continue
        case "$action" in
            add)
                bdq label add "$id" "$label" >/dev/null 2>&1 || true
                # Dual-written, not a replace (sp-ki12s precedent): fayth_ready still reads
                # the label, not this hold, until CHECK 3b's reader is cut over in the round.
                spira-lc hold "$id" wait "blocker certified or batched, not yet landed" sentinel || true
                log "mark_queue_waiters: $id — queue-wait applied"
                ;;
            remove)
                bdq label remove "$id" "$label" >/dev/null 2>&1 || true
                spira-lc unhold "$id" wait sentinel || true
                log "mark_queue_waiters: $id — blocker landed, cleared"
                ;;
        esac
    done
    rm -f "$_lqf"
}

# close_landed_queue_waiters — close any open bead carrying SPIRA_QUEUE_WAIT_LABEL whose
# landstate file records LANDED. These beads never leave the label on their own because the
# normal close path runs when the branch lands; a bead with no branch or an empty branch has
# nothing to land and is never visited by that path.
close_landed_queue_waiters() {
    local label="${SPIRA_QUEUE_WAIT_LABEL:-}"
    [ -n "$label" ] || return 0
    local landstate_dir="$SPIRA_RUN/landstate"
    local labeled_json _id _state _ls
    labeled_json="$(bdjson list --status open --label "$label" --limit 0 2>/dev/null)" \
        || labeled_json=""
    [ -n "$labeled_json" ] || return 0
    while IFS= read -r _id; do
        [ -n "$_id" ] || continue
        _ls="$landstate_dir/$_id"
        [ -f "$_ls" ] || continue
        _state=""
        { read -r _state _ < "$_ls"; } 2>/dev/null || continue
        if [ "$_state" = "LANDED" ]; then
            bdq label remove "$_id" "$label" >/dev/null 2>&1 || true
            spira-lc unhold "$_id" wait sentinel || true
            bdq close "$_id" \
                --reason "Content already on main (landstate=LANDED); no branch remained to land." \
                >/dev/null 2>&1 || true
            log "close_landed_queue_waiters: $_id — closed (LANDED, no branch)"
        fi
    done < <(printf '%s\n' "$labeled_json" | python3 -c '
import sys, json
try:
    rows = json.loads(sys.stdin.read())
    if not isinstance(rows, list): rows = [rows]
    for r in rows:
        bid = r.get("id", "")
        if bid: print(bid)
except Exception:
    pass
' 2>/dev/null)
}

# mark_open_children is CHECK 3c in the sentinel binary (sentinel/src/open_children.rs,
# sp-du8bv): it decides from the pass's one store snapshot instead of one `bd children` per
# candidate, which cost 302 s a pass. `sentinel --open-children` runs it alone.

# bead_reopen <id> <cause> [note] [suites] — hand a bead back to the graph so the NEXT aeon can claim it.
#
# <suites>, when given, is a comma-separated list of suites this withdrawal is known to have
# reddened. Written to $LANDSTATE/<id>.ejected — the sidecar gate.sh already reads
# unconditionally — so the next certification forces them via SPIRA_GATE_EJECTED_SUITES
# instead of running fences-only and rediscovering the same red (law-a-retry-must-change-an-input).
#
# REOPENING IS NOT ENOUGH. `bd reopen` keeps the assignee, and `bd ready --claim` skips any
# bead that has one even though `bd ready` lists it — so a bead reopened by the landing
# pass (a rebase conflict, a red gate) or by the aeon's own closed-without-commit check went
# back into the graph wearing a dead aeon's name and was never claimed again. Seven sat that
# way for four to eight hours at P0 while aeons took P1 work around them, and every one of
# the 23 reopens the landing log holds had the same defect. Clearing the assignee is what
# makes a reopen a reopen; it is done here so no site can forget it.
#
# RETURNS NON-ZERO IF ANY SUB-OPERATION FAILED, so a caller that needs to know — verdict.sh's
# _attr_eject, which must not report an ejection that never reopened the bead (sp-vjfv6) —
# can branch on it. Every other call site fires this under `set -e` and does not check the
# return, so each is suffixed `|| true`: a bd refusal must not abort the caller partway,
# leaving a bead reopened with no record of WHY. bd's refusal is reported on stderr where the
# harness log keeps it either way.
#
# <cause> is a stable slug (gate-red, rebase-conflict, closed-without-commit, …) written
# as a harness event row so census can break sp-reopen into classified subclasses.
# It is written AFTER bd's own `reopened` event, under event_type='reopen', so the two
# rows are distinct and the census never double-counts a harness reopen.
bead_reopen() {
    local id="$1" cause="${2:-unrecorded}" note="${3:-}" suites="${4:-}" rc=0
    # A CERTIFIED bead reopened here must stop being admissible: the batch builder
    # selects on landstate alone, and WITHDRAWN is a state it never admits. Every
    # reopen goes through this one function, so this is the one place that can't
    # be skipped by a caller that forgot.
    #
    # EXCEPT THE SUBMITTED CONVERSION (sp-qsona). aeon.sh's teardown "reopens" a work bead
    # its own session closed only to carry SPIRA_SUBMITTED_LABEL until the landing pass
    # closes it — the work is done and certification proceeds from submitted, so a
    # CERTIFIED record written moments earlier (the session's own queue.sh submit, or
    # aeon.sh's self-certify, sp-u9f82) must stay admissible. Withdrawing it here would
    # strand every converted bead: open, submitted, and never batched. This is the only
    # cause admission-exempt in _census_deliberate_reopen_causes (below): eject is also
    # deliberate for census, but a CERTIFIED-unbatched eject still needs to withdraw.
    local _wd_st _wd_tip
    read -r _wd_st _wd_tip _ <<< "$(land_state "$id" 2>/dev/null)"
    if [ "${_wd_st:-}" = CERTIFIED ] && ! _census_reopen_admission_exempt "$cause"; then
        local _wd_reason="$cause"
        [ -n "$suites" ] && _wd_reason="$cause suites=$suites"
        land_mark "$id" WITHDRAWN "${_wd_tip:-none}" "$_wd_reason"
    fi
    if [ -n "$suites" ]; then
        printf '%s' "$suites" > "$LANDSTATE/$id.ejected.$$" 2>/dev/null \
            && mv -f "$LANDSTATE/$id.ejected.$$" "$LANDSTATE/$id.ejected" 2>/dev/null || true
    fi
    bdq reopen "$id" >/dev/null 2>&1 || rc=1
    # A REOPEN MEANS REWORK, so a submitted bead stops being submitted. SPIRA_SUBMITTED_LABEL
    # is excluded from every claim (fayth_exclude), so a bead the landing pass reopens for a
    # conflict or a red gate while it still carries the label is open, unclaimable and never
    # landed — stranded. The submitted conversion itself (work-close-converted) is the one
    # reopen that ADDS the label, right after this call, and is left alone.
    if ! _census_reopen_admission_exempt "$cause"; then
        bdq label remove "$id" "${SPIRA_SUBMITTED_LABEL:-spira-submitted}" >/dev/null 2>&1 || true
    fi
    release_claim "$id" || rc=1
    _bump_write_event "$id" reopen "$cause" || rc=1
    [ -n "$note" ] && { bdq note "$id" "$note" >/dev/null 2>&1 || rc=1; }
    [ "$rc" = 0 ] || printf 'bead_reopen: %s — bd refused the reopen, the release or the note\n' "$id" >&2
    return "$rc"
}

# --------------------------------------------------------------------------------------
# ENDING A CLAIM. An assignee is written when a bead is claimed, and it is the ONLY thing
# standing between the next aeon and the work, because `bd ready --claim` refuses a bead
# carrying another actor's name. So every path that ends a claim without the work being
# done has to unwrite it.
#
# `bd reopen` sets the status and clears closed_at; it does not touch the assignee. `bd
# reclaim` does not reach these either — it reverts stale-lease IN_PROGRESS issues, and an
# orphan is OPEN with a null lease, outside its predicate by construction, because
# something already reset the status without touching the name.
#
# WHY `bd assign <id> ""` AND NOT `bd unclaim --force`. assign refuses to overwrite another
# actor's LIVE in_progress claim unless forced; unclaim --force by definition does not. That
# refusal is the safety property, because these run from a timer against a database aeons
# are claiming out of concurrently: the primitive that loses a race harmlessly is the
# correct one, and --force is how a sweep robs a live worker.
# --------------------------------------------------------------------------------------
release_claim() {        # release_claim <id> -> 0 if the assignee is now clear
    bdq assign "$1" "" >/dev/null 2>&1
}

# ---- lifecycle: the bead machine, via spira-lc (design §3.1.1, §3.5) -----------------
# aeon.sh is the trusted driver, not the sandboxed model, so it calls spira-lc directly —
# never through `work`, whose whole point is binding one bead to one aeon's restricted
# unit. These three functions are the only place aeon.sh's own claim/release/holder-dead
# reach the machine.

# lc_bead_row <id> -> "STATE<TAB>VERSION<TAB>HOLDER<TAB>LEASE_UNTIL", or a non-zero return
# when spira-lc could not be reached or the bead carries no row yet (not migrated). The
# caller must fail closed on that — never assume READY for a row it cannot read.
lc_bead_row() {
    local id="$1" out
    out="$(spira-lc show "$id" 2>/dev/null)" || return 1
    printf '%s' "$out" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
b = d.get("bead")
if not b: sys.exit(1)
print("%s\t%s\t%s\t%s" % (b.get("state") or "", b.get("version") if b.get("version") is not None else "",
                           b.get("holder") or "", b.get("lease_until") if b.get("lease_until") is not None else ""))' \
        2>/dev/null
}

# lc_event_bead <id> <expect-state> <version> <actor> <kind-json> — one CAS event against
# the bead machine, spira-lc's own exit codes passed straight through: 0 applied, 3
# refused, 2 cannot tell (the machine was unreachable — never treated as a refusal, so a
# caller that retries a "cannot tell" is safe and one that retries a real refusal is not).
lc_event_bead() {
    spira-lc event bead "$1" --expect "$2" --version "$3" --actor "$4" --kind "$5" >/dev/null 2>&1
}

# lc_claim_bead <id> <holder> <lease-until-epoch> [<stack-json> <stack-depth>
# <stack-max-depth>] -> 0 applied, 3 refused (the sp-zw9ot fixture: a bead already
# IN_DELIVERY, or genuinely held by a live holder; also DepthExceeded when stack-depth
# exceeds stack-max-depth), 2 cannot tell. The trailing three args are the caller's own
# already-computed stack proposal (aeon.sh's `stack_proposal`, design stacked-dependents-
# 2026-09-28 §1) — this function only forwards them, exactly like `lease_until`; omitted,
# they default to `{}`/0/0, which is today's unstacked claim.
#
# HOLDERDEAD BEFORE CLAIM. A row this aeon can see is WORKING only because a prior holder
# died without releasing — the fayth predicate already excludes any bead bd itself shows
# as claimed, so a live holder never reaches here. A CAS HolderDead(WORKING->READY) clears
# it; Claim is illegal from WORKING (lifecycle/src/bead.rs), so this is the only path back.
lc_claim_bead() {
    local id="$1" holder="$2" lease_until="$3" stack="${4:-{\}}" stack_depth="${5:-0}" stack_max_depth="${6:-0}" row state version rc
    row="$(lc_bead_row "$id")" || return 2
    IFS=$'\t' read -r state version _ _ <<< "$row"
    [ -n "$state" ] || return 2
    if [ "$state" = WORKING ]; then
        lc_event_bead "$id" WORKING "$version" "$holder" '"HolderDead"'
        rc=$?
        [ "$rc" -eq 0 ] || return "$rc"
        row="$(lc_bead_row "$id")" || return 2
        IFS=$'\t' read -r state version _ _ <<< "$row"
    fi
    lc_event_bead "$id" "$state" "$version" "$holder" \
        "{\"Claim\":{\"holder\":\"$holder\",\"lease_until\":$lease_until,\"stack\":$stack,\"stack_depth\":$stack_depth,\"stack_max_depth\":$stack_max_depth}}"
}

# lc_release_bead <id> <actor> — best-effort Release. Called on every release_own_claim, so
# it fires from states where Release is illegal (SUBMITTED, DONE, ...) as often as from
# WORKING; those refusals are expected, not errors, and are never surfaced to the caller —
# the row is already exactly where it should be.
lc_release_bead() {
    local id="$1" actor="$2" row state version
    row="$(lc_bead_row "$id")" || return 0
    IFS=$'\t' read -r state version _ _ <<< "$row"
    [ -n "$state" ] || return 0
    lc_event_bead "$id" "$state" "$version" "$actor" '"Release"'
    return 0
}

# lc_bead_verified <id> -> 0 when the bead machine's row already carries a definitive,
# non-failing outcome (SUBMITTED and beyond, or DONE). bd's own status never changes for a
# work-verb-driven session (design §3.4: bd status is inert for work beads) — this is
# aeon.sh's replacement for reading "closed" off bd to decide a session actually finished.
lc_bead_verified() {
    local row state
    row="$(lc_bead_row "$1" 2>/dev/null)" || return 1
    state="${row%%$'\t'*}"
    case "$state" in
        SUBMITTED|CERTIFIED|IN_DELIVERY|LANDED|DONE) return 0 ;;
        *) return 1 ;;
    esac
}

# release_own_claim <id> — an aeon hands back a bead it is still holding.
#
# Sets status back to open and clears the assignee in one update call. `bd assign <id> ""`
# refuses to overwrite another actor's LIVE in_progress claim, so if a supervisor reclaimed
# the bead and handed it to another aeon between our fence check and this call, the assign
# step fails safely and the bead is left with the new holder.
#
# THE NAME IS THE AEON'S, NOT THE FAYTH'S. aeon.sh claims under BEADS_ACTOR="aeon-$AEON",
# the per-instance name — `aeon-mindy`, not `aeon-builder`. Release sites that derived the
# actor a second time as "aeon-$FAYTH" compared against a string no bead has ever carried:
# bd answered "assignee mismatch", exited 1 into >/dev/null, and changed nothing, so every
# aeon that ended without closing left its name standing and the next `bd ready --claim`
# skipped that bead forever. Deriving the actor twice is what let the two disagree, which is
# why there is one function here and no copies of it anywhere.
release_own_claim() {
    local id="$1" me="${BEADS_ACTOR:-aeon-${SPIRA_AEON:-}}"
    [ -n "$me" ] && [ "$me" != "aeon-" ] || return 1
    lc_release_bead "$id" "$me"
    bdq update "$id" --status open --assignee "" >/dev/null 2>&1
}

# park_unmapped <id> <repo-name> — repo-map has no checkout for the bead's repo:<repo-name>.
#
# PARK, NOT RELEASE. release_own_claim alone puts the bead back on the ready queue, where
# the sentinel re-summons an aeon within two minutes — an infinite loop burning the pool.
# Adding the ask label first makes every fayth's --exclude-label filter skip it, so the
# bead sits open but unclaimed until a human corrects the label or the repo-map. Scar:
# sp-nlhy accumulated four identical notes, one per summon, before a keyboard session
# fixed the label by hand. (sp-4l0d)
# overseer keeps the bead visible in the decisions pane; without it the bead is excluded
# from every fayth predicate and invisible to the operator.
park_unmapped() {
    local id="$1" repo_name="$2"
    bdq label add "$id" "$SPIRA_ASK_LABEL" >/dev/null 2>&1 || true
    bdq label add "$id" "overseer"          >/dev/null 2>&1 || true
    # Dual-written, not a replace (sp-ki12s precedent) — the label is still what every
    # fayth's dispatch exclusion reads until that reader is cut over in the same round.
    spira-lc hold "$id" ask "repo:$repo_name has no repo-map entry" aeon.sh || true
    bdq note "$id" "Parked by aeon.sh: this bead carries repo:$repo_name, and $SPIRA_REPO_MAP has no entry for it (or its path is not a git checkout). Labeled $SPIRA_ASK_LABEL and overseer — no aeon will claim it again until a human corrects the label or adds the repo to the map and removes that label. Refusing to work it in the home repo — a fix landed in the wrong repository passes every check downstream." >/dev/null 2>&1
    release_own_claim "$id"
}

# fayth_free <fayth> [pool-remaining] [exclude-unit] -> free concurrency slots, never negative.
# exclude-unit is passed straight through to aeon_count: a live aeon asking its own capacity
# question must not count its own unit (sp-0hnm6).
#
# THE POOL IS A BATTLE PARTY (the operator, 2026-09-07: "i have a tank, a healer, and then as
# much DPS as i can"). SPIRA_MAX_AEONS is the party size, and every persona is one of two
# kinds:
#
# THE DISTINCTION IS YUNA AND IFRIT (the operator, 2026-09-07: "as i travel around Spira with
# my party, i have Yuna the summoner always around, but Ifrit the Aeon is only around during
# combat when i NEED Ifrit"). A party member travels with you; an aeon is called for the
# fight and dismissed after it. Ops is Yuna — persistent, always present, not summoned for a
# task. A builder is Ifrit — summoned onto one bead, and gone when it is done.
#
#   PARTY MEMBERS (FAYTH_ROLE=party) — the tank and the healer. They are NOT drawn from this
#   pool, because they are not task-specific work: they are persistent roles that are always
#   present, with their own summoner (Ops has spira-ops.timer). Ops is the healer. Keeping it
#   out of the pool is what actually guarantees it a place — a reserved slot inside a shared
#   pool is still a slot somebody has to release, and builders hold theirs for ~10 minutes (p50 9.4 min, p90 18.6 min measured over 28 runs).
#
#   TASK FAYTHS (the default) — summoned for one bead and gone. This pool is theirs alone.
#   FAYTH_ELASTIC means "no number of your own, take what is left": builders are the DPS, and
#   "as much as I can" is exactly the right cap for them.
#
# WHERE THE METAPHOR DIVERGES, deliberately: in the game one fayth yields one aeon, so a
# party of three builders would need three statues in the chamber. Here a fayth is the CLASS
# — a persona definition — and an aeon one summoned instance of it, which is what lets
# FAYTH_MAX_CONCURRENT exist at all. Lore-exact would buy nothing but three near-identical
# .fayth files to keep in step.
#
# A PARTY MEMBER SHOULD ALWAYS BE PRESENT, which is the part that is not in this file: Ops
# is only summoned when an incident bead is waiting, so a quiet hour means the healer is not
# in the party at all and the role exists only on paper. The watchtower binary is what keeps it
# seated — it hands Ops the pipeline's vital signs on a timer whether or not anything has
# crashed, so the persistent role has something to be persistent about. Until now it was a documented, validated
# configuration key that NOTHING READ — no default, no enforcement, unset on this host — so
# there was no pool at all: every persona had its own private cap and nothing coordinated
# them. Ops could take one and the builder three whether or not the box could carry four,
# and an on-call persona had no more claim on a slot than a feature worker.
#
# The order in SPIRA_FAYTHS IS the priority. Each persona takes up to its own
# FAYTH_MAX_CONCURRENT from what the ones before it left, so the first one named can never be
# crowded out by work that is merely plentiful — which is the whole point of putting Ops
# there. A persona declaring FAYTH_ELASTIC=1 ignores its own cap and takes the remainder,
# which is what "builders scale to fill" means; it belongs last.
#
# WHY THERE IS NO RESERVATION MECHANISM. The first version of this gave Ops a reserved slot
# inside the shared pool, because ordering alone only stops a builder taking a slot AHEAD of
# Ops within one pass and does nothing about builders already inside ~10-minute sessions.
# Taking party members out of the pool entirely is the simpler answer to the same problem and
# has no arithmetic to get wrong: a role that never competes cannot be starved.
# THE POOL IS A CEILING, NOT A FLOOR. It only ever lowers what a persona may start, so a
# host that sets nothing behaves exactly as before.
fayth_free() {           # fayth_free <fayth> [pool-remaining] [exclude-unit]
    local f="$1" pool="${2:-}" exclude="${3:-}" max have free
    max="$(fayth_get "$f" FAYTH_MAX_CONCURRENT 1)"; max="${max:-1}"
    # ELASTIC: the remainder of the pool, not this persona's own number. With no pool given
    # there is no remainder to take, so it falls back to its declared cap rather than to
    # unbounded — an elastic persona on a host that never enforced a pool must not become
    # the one that discovers the box's limits.
    local is_remainder=0
    if [ "$(fayth_get "$f" FAYTH_ELASTIC 0)" = 1 ] && [ -n "$pool" ]; then
        max="$pool"
        is_remainder=1
    fi
    # A REMAINDER IS NOT A CAP. A number that already nets out what is running (the pool
    # from sentinel.sh) is how many MORE may start. Subtracting the running count from it
    # again withholds more the more is running, so the system saturates at half its ceiling
    # and reports itself at its limit.
    have="$(aeon_count "$f" "$exclude")"
    if [ "$is_remainder" = 1 ]; then
        free="$max"
    else
        free=$(( max > have ? max - have : 0 ))
    fi
    # AND THE POOL CLAMPS LAST, after this persona's cap, because it is the outermost of
    # the two and the only one the personas share.
    [ -n "$pool" ] && [ "$pool" -lt "$free" ] 2>/dev/null && free="$pool"
    printf '%d' "$free"
}

# fayth_partitions -> every partition this host watches, one "<labels>\t<exclude-labels>" a line.
#
# THE ROSTER ANSWERS "WHOSE WORK IS THERE" FOR EVERY CHECK, not only for summoning. Reaping
# a dead lease, reporting stalled work and verifying that a closed bead actually landed were
# each written against one hardcoded partition — the builder's — which is CHECK 7's defect
# arriving by three more doors. An aeon of any other persona that died left its bead
# in_progress with no reaper looking at it, its stall was never reported as stalled, and its
# bead could close without landing and pass the sweep that exists to catch exactly that. The
# fix is the same one fayth_ready made: ask each persona's OWN predicate.
#
# DEDUPLICATED, because two personas may legitimately share a partition and a sweep run twice
# over the same labels does the same work twice and counts it twice.
#
# EMPTY WHEN THE CHAMBER IS EMPTY, and callers must say so rather than fall back to a
# partition name: a fallback would restore the hardcoded constant by another route, and a
# sweep that silently watches nothing is indistinguishable from one that found nothing
# (law-absence-needs-a-positive-control).
fayth_partitions() {
    spira-config fayth partitions
    return 0
}

fayths_for_labels() {    # fayths_for_labels <labels> -> personas whose partition IS <labels>
    spira-config fayth for-labels "$1"
    return 0
}

# summon_fayth <fayth> [pool-remaining] [require-label] [reuse-ready] -> 0 if an aeon was
# started, 1 otherwise.
#
# require-label is passed to the aeon as SPIRA_REQUIRE_LABEL, which it adds to its own
# FAYTH_LABELS before claiming (aeon.sh). Set it only when the slot itself is restricted —
# an express grant, say — so the aeon summoned under it cannot claim a bead outside that
# restriction. A normal summon leaves it unset and claims under the fayth's own predicate
# exactly as before.
#
# reuse-ready=1 skips the fayth_ready bd round trip and uses SUMMON_FAYTH_CACHED_READY
# (set by the previous call, in this same shell, for the SAME fayth) instead — a caller
# looping repeated summons for one fayth in one pass already knows the count from its last
# call, decremented by exactly the summon it just made; asking bd again for the same
# partition, once per attempt, was the cost CHECK7's fill loop paid for nothing (sp-994y9).
# Every other caller omits it, defaulting to 0: always a live query, exactly today's
# behaviour.
#
# THE STATUS IS THE ANSWER, not a word on stdout. A caller that captured the output to look
# for "summoned" would swallow the log lines below with it, and the sentinel's stdout IS the
# sentinel log — so the one pass that did something would be the one that explained itself
# least.
# world_gate <fayth> <log-prefix> -> 0 if summons are permitted, 1 if halted or draining.
# Extracted so a second caller can share the exact check rather than a second copy of it.
# aeon --escape calls this too (sp-uyw4n, sp-2w2wu; escape.sh before it, retired sp-zpaq0):
# it reaches around a broken scheduler, not around a halt or drain the operator asked for.
#
# HALTED — world.sh stop writes this stamp; only world.sh start removes it. Checked before
# drain: halt is indefinite and requires explicit operator action.
#
# DRAINING — the operator asked for an empty pool and is waiting on it. Checked ahead of
# capacity and readiness, because it is the only condition here a person is actively
# blocked on: a rollout that must not kill work in flight needs the pool to reach zero, and
# it never does while summons continue.
#
# A DRAIN EXPIRES, AND THIS IS WHAT EXPIRES IT. A drain is a held breath: right for the
# minutes an operation needs, never for an hour. Whoever sets one can die before lifting it
# — 2026-09-09, an Ops sweep drained at 18:02:03, finished its SOP at 18:04:29, exited
# without resuming, and the world sat gated for 59 minutes with 25 beads ready and no aeons,
# every pass logging "pass complete" with nothing summoned, because a drain is a MODE and
# nothing treated the mode as a fault.
#
# LIFTING IT HERE IS LOUD, NEVER SILENT — a quiet lift would hide the forgotten resume,
# which is the defect worth seeing. A STAMP WITH NO `expires` LINE expires at stamp-mtime +
# TTL, so a drain written by the older world.sh cannot wedge the loop forever either.
world_gate() {
    local f="$1" prefix="$2" _dstamp _dexp _dmt
    if [ -f "${SPIRA_RUN:-}/world.halted" ]; then
        log "$prefix $f: halted — not summoning (world.sh start to lift)"
        return 1
    fi
    _dstamp="${SPIRA_RUN:-}/world.draining"
    if [ -f "$_dstamp" ]; then
        _dexp="$(sed -n 's/^expires \([0-9][0-9]*\)$/\1/p' "$_dstamp" 2>/dev/null | head -1)"
        if [ -z "$_dexp" ]; then
            _dmt="$(stat -c %Y "$_dstamp" 2>/dev/null || echo 0)"
            _dexp=$(( _dmt + ${SPIRA_DRAIN_TTL:-1800} ))
        fi
        if [ "$(date +%s)" -ge "$_dexp" ]; then
            rm -f "$_dstamp"
            log "$prefix $f: DRAIN EXPIRED — lifting a drain nobody resumed (deadline $(date -d "@$_dexp" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || echo '?'), TTL ${SPIRA_DRAIN_TTL:-1800}s). Whoever drained did not resume; summons are live again."
        else
            log "$prefix $f: draining — not summoning (world.sh resume to lift)"
            return 1
        fi
    fi
    return 0
}

# summon_refill_argv -> the ExecStopPost property that refills this slot the instant the
# aeon it is attached to exits, rather than waiting for the next timer tick (sp-0y2av).
#
# A NESTED systemd-run, NOT A BACKGROUNDED CHILD OF THIS UNIT. ExecStopPost runs inside the
# exiting aeon's own unit, and that unit's cgroup is torn down the moment ExecStopPost's own
# process exits (KillMode=control-group, the same fact summon_fayth's own comment explains) —
# a `nohup ... &` here would be reaped mid-flight by that teardown. A second systemd-run puts
# the fast pass in ITS OWN unit and cgroup, unaffected by the first one's exit, and returns in
# milliseconds (the job is submitted, not awaited) so the aeon's own exit is never delayed.
#
# THE RESOLVED PATH, NOT THE BARE NAME: systemd validates an ExecStopPost command line itself
# (it is not shelled out through the caller's PATH), and refuses a bare "systemd-run" as "not
# an absolute path" — silently, so the refill would never fire and nothing would say why.
# `command -v` also passes an already-absolute path straight through, which is what every
# test's SPIRA_SUMMON stub is, so the same line works under a fixture unchanged.
summon_refill_argv() {
    local bin; bin="$(command -v "${SPIRA_SUMMON:-systemd-run}" 2>/dev/null || printf '%s' "${SPIRA_SUMMON:-systemd-run}")"
    printf -- '--property=ExecStopPost=%s --user --collect --quiet %s --summon-only' \
        "$bin" "$(command -v sentinel || printf sentinel)"
}

# summon_argv <fayth> -> systemd-run --property/--setenv flags shared by every summon path
# (summon_fayth, aeon --escape — escape.sh before it, retired sp-zpaq0), one argv token per
# line. The caller supplies its own --unit name and the aeon invocation that follows.
summon_argv() {
    local f="$1"
    printf '%s\n' \
        --property=TimeoutStartSec="$(fayth_get "$f" FAYTH_TIMEOUT_SECONDS 3600)" \
        "$(summon_refill_argv)" \
        --setenv=SPIRA_RELEASE="${SPIRA_RELEASE:-}" \
        --setenv=PATH="${SPIRA_RELEASE:-}/bin:${SPIRA_RELEASE:-}/spira:/usr/local/bin:/usr/bin:/bin" \
        --setenv=HOME="$HOME"
}

summon_fayth() {         # summon_fayth <fayth> [pool-remaining] [require-label] [reuse-ready]
    local f="$1" pool="${2:-}" require_label="${3:-}" reuse_ready="${4:-0}" r free
    world_gate "$f" CHECK7 || return 1
    # THE ACCOUNT BEFORE THE QUEUE. A summon during a capacity outage cannot succeed, and it
    # does not fail for free: the aeon it starts claims a bead, is refused by the API, and
    # the bead pays an attempt to discover a fact the harness already knew. Asked first, and
    # before fayth_ready, because the cheapest question is the one that skips the others.
    if capacity_paused; then
        log "CHECK7 $f: the account is out of capacity for another ${SPIRA_CAPACITY_LEFT}s — not summoning"
        return 1
    fi
    # THE FLEET CEILING, ABOVE EVERY PER-PERSONA CAP AND ABOVE THE POOL.
    #
    # SPIRA_MAX_AEONS is the TASK pool, and a lane fayth is deliberately outside it — that is
    # what "ops cannot be starved by builders" buys. The cost is that neither number is the
    # answer to "how many aeons may run at once": the real ceiling is the pool PLUS one per
    # declared lane, so a host set to a pool of 1 summoned a builder and an ops aeon in the
    # same second (2026-09-09 21:46:47) while its own log read `pool: 1 slot(s)`.
    #
    # That arithmetic is right when the binding constraint is this box's cores, because a
    # lane aeon is work the box agreed to make room for. It is wrong when the binding
    # constraint is ONE SHARED ACCOUNT, because every aeon draws on the same five-hour
    # window regardless of which partition scheduled it — and that window is shared with the
    # operator's own sessions and the concierge, so overspending it locks a person out.
    #
    # Asked here, at the one chokepoint every summon path goes through, and before
    # fayth_ready because a filesystem-free count is cheaper than a graph query.
    #
    # UNSET MEANS NO CEILING AND TODAY'S BEHAVIOUR EXACTLY, so a host that never wanted this
    # cannot acquire it by upgrading, and every existing suite passes unchanged.
    if [ -n "${SPIRA_MAX_LIVE_AEONS:-}" ]; then
        local live_all; live_all="$(aeons_live_total)"
        if [ "${live_all:-0}" -ge "$SPIRA_MAX_LIVE_AEONS" ] 2>/dev/null; then
            log "CHECK7 $f: $live_all/$SPIRA_MAX_LIVE_AEONS aeon(s) live across the whole fleet — not summoning"
            return 1
        fi
        # ELASTIC LAST-SLOT RESERVATION. An elastic persona may not consume the last fleet
        # slot while any non-elastic task persona has ready work. The ordering rule in CHECK 7
        # handles the common case — lanes are evaluated before the pool, and fixed personas
        # before elastic ones in the pool — but ordering only prevents the elastic persona
        # from racing with work that is already ready. This rule holds regardless of when
        # work becomes ready: an elastic persona evaluated first takes the slot and the
        # non-elastic persona then waits for a builder exit rather than being refused entry.
        #
        # SCOPE. Binds only the last slot (slots_free == 1). With two or more free slots the
        # elastic persona is unaffected. Non-elastic personas are never refused by this rule.
        # SPIRA_MAX_LIVE_AEONS unset means no ceiling and today's behaviour exactly.
        #
        # COST. One fayth_ready per non-elastic task persona, but only when slots_free == 1
        # and the persona being evaluated is elastic. The common case — fleet well below its
        # ceiling — pays a subtraction and a comparison and nothing else. The query count does
        # not grow per summon attempt overall; it grows per non-elastic persona only at the
        # moment the last slot is being contested.
        if [ "$(fayth_get "$f" FAYTH_ELASTIC 0)" = 1 ]; then
            local slots_free
            slots_free=$(( SPIRA_MAX_LIVE_AEONS - ${live_all:-0} ))
            if [ "${slots_free:-0}" -eq 1 ] 2>/dev/null; then
                local nef nef_r
                for nef in $(spira_task_fayths); do
                    [ "$(fayth_get "$nef" FAYTH_ELASTIC 0)" = 1 ] && continue
                    nef_r="$(fayth_ready "$nef" 2>/dev/null)" || continue
                    if [ "${nef_r:-0}" -gt 0 ] 2>/dev/null; then
                        log "CHECK7 $f: 1 fleet slot remaining, held back — $nef has $nef_r ready bead(s)"
                        return 1
                    fi
                done
            fi
        fi
        # COLLECTIVE LANE CAP. A lane fayth may not summon while lanes collectively are at
        # SPIRA_LANES_MAX_LIVE. Unset means no collective cap.
        if [ -n "${SPIRA_LANES_MAX_LIVE:-}" ] && [ -n "$(fayth_get "$f" FAYTH_LANE "")" ]; then
            local live_lanes; live_lanes="$(aeons_live_lanes)"
            if [ "${live_lanes:-0}" -ge "$SPIRA_LANES_MAX_LIVE" ] 2>/dev/null; then
                log "CHECK7 $f: $live_lanes/$SPIRA_LANES_MAX_LIVE lane slot(s) in use — not summoning"
                return 1
            fi
        fi
        # INVERTED LAST-SLOT RESERVATION. A task fayth may not take the last fleet slot
        # while any lane has ready work IT CAN ACTUALLY TAKE and lanes are below their
        # collective cap. This inverts the old lane-last-slot rule: lanes are preferred
        # when a slot is scarce, so ops and groomer are not starved by builders under a
        # steady task backlog. A lane already at its own FAYTH_MAX_CONCURRENT cannot use
        # the slot regardless, so holding it back for that lane leaves it empty instead.
        # Requires SPIRA_LANES_MAX_LIVE; without it, no preference applies.
        if [ -n "${SPIRA_LANES_MAX_LIVE:-}" ] && [ -z "$(fayth_get "$f" FAYTH_LANE "")" ]; then
            local inv_slots_free; inv_slots_free=$(( SPIRA_MAX_LIVE_AEONS - ${live_all:-0} ))
            if [ "${inv_slots_free:-0}" -eq 1 ] 2>/dev/null; then
                local live_lanes; live_lanes="$(aeons_live_lanes)"
                if [ "${live_lanes:-0}" -lt "$SPIRA_LANES_MAX_LIVE" ] 2>/dev/null; then
                    local lf lf_r lf_free
                    for lf in $(spira_lane_fayths); do
                        lf_r="$(fayth_ready "$lf" 2>/dev/null)" || continue
                        if [ "${lf_r:-0}" -gt 0 ] 2>/dev/null; then
                            lf_free="$(fayth_free "$lf")"
                            if [ "${lf_free:-0}" -gt 0 ] 2>/dev/null; then
                                log "CHECK7 $f: 1 fleet slot remaining, held back — $lf has $lf_r ready lane work"
                                return 1
                            fi
                        fi
                    done
                fi
            fi
        fi
    fi
    if [ "$reuse_ready" = 1 ] && [ -n "${SUMMON_FAYTH_CACHED_READY:-}" ]; then
        r="$SUMMON_FAYTH_CACHED_READY"
    else
        local _fr_err; _fr_err="$(mktemp)"
        r="$(fayth_ready "$f" 2>"$_fr_err")"; local _fr_rc=$? _fr_errmsg
        _fr_errmsg="$(cat "$_fr_err" 2>/dev/null)"
        rm -f "$_fr_err"
        if [ "$_fr_rc" -eq 2 ]; then
            log "CHECK7 $f: no fayth in the chamber — skipped"
            return 1
        elif [ "$_fr_rc" -ne 0 ]; then
            log "CHECK7 $f: ready query failed: ${_fr_errmsg:-bd gave no reason} — skipped, not counted as zero ready"
            return 1
        fi
    fi
    if [ "${r:-0}" -eq 0 ]; then
        log "CHECK7 $f: nothing ready in its partition"
        SUMMON_FAYTH_CACHED_READY=0
        return 1
    fi
    free="$(fayth_free "$f" "$pool")"
    if [ "${free:-0}" -eq 0 ]; then
        log "CHECK7 $f: $r ready, at concurrency cap"
        SUMMON_FAYTH_CACHED_READY="$r"
        return 1
    fi

    # A TRANSIENT UNIT, not a background child. This service is Type=oneshot with the
    # default KillMode=control-group, so systemd tears down the whole cgroup the moment the
    # pass finishes — which killed the first aeon it summoned within the same second, after
    # 1.6s of CPU, leaving an empty log and a sentinel that cheerfully reported "summoned"
    # every two minutes. systemd-run puts the aeon in its own cgroup, quota and journal.
    log "CHECK7 $f: $r ready, $free free — summoning${require_label:+, restricted to '$require_label'}"
    # systemd-run is handed the PATH-resolved aeon (a transient unit has no launcher PATH).
    local _aeon; _aeon="$(command -v aeon)" || { log "CHECK7 $f: aeon not found on PATH — not summoning"; return 1; }
    local _sargv; mapfile -t _sargv < <(summon_argv "$f")
    "${SPIRA_SUMMON:-systemd-run}" --user --collect --quiet \
        --unit="spira-aeon-$f-$(date +%s)" \
        "${_sargv[@]}" \
        ${require_label:+--setenv=SPIRA_REQUIRE_LABEL="$require_label"} \
        "$_aeon" --home "$SPIRA_HOME" "$f" 2>/dev/null
    local _rc=$?
    [ "$_rc" -eq 0 ] && SUMMON_FAYTH_CACHED_READY=$(( r > 0 ? r - 1 : 0 ))
    return "$_rc"
}

# _ck7_summon_body -> CHECK 7's lane-then-pool summon loop, unlocked. Never call this
# directly outside ck7_summon_pass (below) and its own unlocked-race test row: two of these
# running at once each read "N live, 1 free" before either's summon lands and both summon,
# which is exactly the double-summon ck7_summon_pass's flock exists to prevent.
_ck7_summon_body() {
    local TASK_FAYTHS LANE_FAYTHS pool f
    TASK_FAYTHS="$(spira_task_fayths)"
    if [ -n "${SPIRA_MAX_AEONS:-}" ]; then
        local task_live=0
        for f in $TASK_FAYTHS; do task_live=$((task_live + $(aeon_count "$f"))); done
        pool="$(ck7_pool "$SPIRA_MAX_AEONS" "$task_live")"
        log "CHECK7 pool: ${SPIRA_MAX_AEONS} slot(s), $task_live live, $pool free — order: $TASK_FAYTHS"
    else
        pool=""
    fi

    # LANE FAYTHS DRAW FIRST — a lane is a partition no builder will ever take, and a
    # builder loop run first would take every free slot before a starved lane is asked.
    # THE FLEET CEILING STILL BINDS THEM via SPIRA_MAX_LIVE_AEONS inside summon_fayth; no
    # pool argument is passed here because SPIRA_MAX_AEONS is the task pool alone.
    LANE_FAYTHS="$(spira_lane_fayths)"
    [ -n "$LANE_FAYTHS" ] && log "CHECK7 lanes (${SPIRA_LANES:-none} declared): $LANE_FAYTHS"
    local _lane_rr="$SPIRA_RUN/lane-round-robin" _lane_last
    _lane_last="$(cat "$_lane_rr" 2>/dev/null)"
    LANE_FAYTHS="$(lane_rotate "$_lane_last" $LANE_FAYTHS)"

    # WHEN INDIVIDUAL BD CALLS ARE SLOW, a partition this pass did not reach must log as
    # "not evaluated" rather than silently reading identical to "nothing ready" to strand.sh.
    local _ck7_start _ck7_budget
    _ck7_start="$(date +%s)"
    _ck7_budget="${SPIRA_SENTINEL_PASS_BUDGET_SECS:-90}"
    for f in $LANE_FAYTHS; do
        if [ $(( $(date +%s) - _ck7_start )) -ge "$_ck7_budget" ]; then
            log "CHECK7 $f: not evaluated (pass budget exhausted)"
            continue
        fi
        if summon_fayth "$f"; then
            act "summoned a $f lane aeon"
            printf '%s' "$f" > "$_lane_rr"
        fi
    done

    # THE POOL DRAWS ON WHAT IS LEFT, recomputed after the lanes above consumed some of it.
    #
    # ADMISSION THROTTLE: if --throttle-check engaged the stamp, hold the task pool at 0
    # (lanes are unaffected — the stamp says the queue is over capacity, not lane work).
    local _tc_stamp_ck7 _ck7_express_label="" _ck7_stamp_exists=0
    _tc_stamp_ck7="${SPIRA_THROTTLE_STAMP:-$SPIRA_RUN/queue-throttled}"
    [ -f "$_tc_stamp_ck7" ] && _ck7_stamp_exists=1
    if [ "$(ck7_throttled "$_ck7_stamp_exists" "${SPIRA_QUEUE_THROTTLE_OVERRIDE:-}")" = 1 ]; then
        local _ck7_express_ready=0
        express_ready_in_task_pool "$TASK_FAYTHS" "${SPIRA_EXPRESS_LABEL:-express}" && _ck7_express_ready=1
        pool="$(check7_pool_decision 1 "${pool:-0}" "$_ck7_express_ready")"
        if [ "$_ck7_express_ready" = 1 ]; then
            _ck7_express_label="${SPIRA_EXPRESS_LABEL:-express}"
            log "CHECK7 pool: throttle active — express bead ready, granting pool=$pool (restricted to '$_ck7_express_label')"
        else
            log "CHECK7 pool: throttle active ($(head -1 "$_tc_stamp_ck7" 2>/dev/null)) — task pool held at $pool"
        fi
    fi
    for f in $TASK_FAYTHS; do
        if [ $(( $(date +%s) - _ck7_start )) -ge "$_ck7_budget" ]; then
            log "CHECK7 $f: not evaluated (pass budget exhausted)"
            continue
        fi
        local _fill=0
        # ONE fayth_ready PER PERSONA PER PASS, NOT ONE PER SUMMON. A pool of N ready beads
        # used to cost N bd round trips here — the fill loop re-asked "anything ready?"
        # before every summon even though nothing between one attempt and the next could
        # have changed the answer except the summon itself, and that decrement is known
        # locally (sp-994y9).
        unset SUMMON_FAYTH_CACHED_READY
        local _ck7_reuse=0
        while summon_fayth "$f" "$pool" "$_ck7_express_label" "$_ck7_reuse"; do
            _ck7_reuse=1
            act "summoned a $f aeon"
            [ -n "$pool" ] && pool=$(( pool > 0 ? pool - 1 : 0 ))
            _fill=$(( _fill + 1 ))
            [ "$(ck7_fill_cap "$_fill" "$pool")" = stop ] && break
            [ $(( $(date +%s) - _ck7_start )) -ge "$_ck7_budget" ] && break
        done
    done
}

# ck7_summon_pass -> _ck7_summon_body, serialized against every other caller of this
# function by one flock on $SPIRA_RUN/summon.lock.
#
# ONE LOCK, TWO ENTRY POINTS. sentinel.sh's full pass and sentinel.sh --summon-only both
# reach the fleet through this one function, so a slot a fast pass just filled cannot be
# filled AGAIN by a full pass that read "free" a moment earlier, or by two fast passes
# firing back to back off ExecStopPost and the 15s timer (sp-0y2av). The lock lives here,
# not around each caller, so there is exactly one place either path can get this wrong.
#
# A FIXED FD, NOT A SUBSHELL. `( flock ...; _ck7_summon_body )` would run the body in a
# subshell, and `act`'s counter increments would not survive past it — the pass summary
# would report 0 actions on a pass that plainly summoned something.
ck7_summon_pass() {
    local _lockfile="${SPIRA_RUN:-/tmp}/summon.lock"
    mkdir -p "${SPIRA_RUN:-/tmp}" 2>/dev/null
    exec 9>"$_lockfile" || { log "CHECK7: cannot open $_lockfile"; return 1; }
    if ! flock -w "${SPIRA_SUMMON_LOCK_WAIT:-30}" 9; then
        log "CHECK7: another summon pass holds summon.lock — skipping this pass"
        exec 9>&-
        return 1
    fi
    _ck7_summon_body
    exec 9>&-
}

# named_unit_stop <systemd --user unit glob> — stop every live unit matching it, by NAME.
#
# THE ALTERNATIVE THIS REPLACES: reading a process's PPid from /proc and killing that. For
# an orphan, the PPid is the user manager itself, and SIGTERM to it activates exit.target —
# the whole session stops, not the leftover process (sp-kb0k5). A systemd-run transient
# unit's default KillMode=control-group means stopping it BY NAME tears down its whole
# cgroup, including any child that was reparented to pid 1 within it: reparenting changes a
# process's PPid, never its cgroup, so an orphan is still caught.
#
# No match is not a failure — the run may already be finished. A stop that fails is.
named_unit_stop() {
    local glob="$1" u rc=0 any=0
    for u in $("${SPIRA_SYSTEMCTL:-systemctl}" --user list-units "$glob" --all --no-legend 2>/dev/null | awk '{print $1}'); do
        any=1
        if "${SPIRA_SYSTEMCTL:-systemctl}" --user stop "$u" 2>/dev/null; then
            printf 'stopped %s\n' "$u"
        else
            printf 'could not stop %s\n' "$u" >&2
            rc=1
        fi
    done
    [ "$any" = 1 ] || printf 'no unit matches %s\n' "$glob"
    return "$rc"
}

# ======================================================================================
# API CAPACITY — the account's own five-hour window, and the second unrelated thing in this
# harness called "capacity".
#
# The other one: FAYTH_MAX_CONCURRENT is how many aeons may run at once. It has nothing to do
# with this one, which is whether the API will answer at all. The name collision is why the
# condition went unhandled for so long — `grep capacity` returned confident, irrelevant hits.
#
# WHAT GOES WRONG WITHOUT THIS. aeon.sh takes the session's exit code and any non-zero
# becomes a failed attempt, so a session the API refused to serve is recorded as work that
# could not be done. That is not merely a miscount: attempts poison at a threshold, so an
# outage does not just stop the queue, it DESTROYS it — every bead claimed while the window
# is spent burns an attempt for a condition that has nothing to do with its work, and beads
# leave circulation permanently for a fault that heals itself in minutes. A transient
# condition must not be able to write permanent state.
#
# THE DISCRIMINATING FIELD IS `status`, AND IT IS NOT `overageStatus`. Every session on this
# account emits `"overageStatus":"rejected","overageDisabledReason":"org_level_disabled"` on
# EVERY rate_limit_event, including at 7% utilization, because overage is disabled at the
# organisation level as a standing configuration. Keying on it — which the shape of the
# payload invites — would pause the harness permanently and for ever, at full health. Of 460
# rate_limit_events captured across 40 session logs here, 443 read `status: allowed`, 16
# `allowed_warning`, and exactly ONE `status: rejected` — at `utilization: 1`, ending in a
# synthetic assistant turn reading "You've hit your session limit · resets 12pm (UTC)".
# That one event is the positive
# control this detector is tested against (law-absence-needs-a-positive-control).
#
# AND `429`/`503` ARE NOT IN THESE LOGS AT ALL. A bare grep for them matches four and five
# digit token counts — `"cache_read_input_tokens":142902` contains `429` — which is how one
# log was read as holding "24x 429 and 12x 503" when it holds neither. The stream-json trace
# never carries a bare HTTP status; the refusal arrives as the rate_limit_event above and as
# `is_error` on the terminal `result` record. Match the structure, never the substring.
# ======================================================================================
SPIRA_CAPACITY_PAUSE="${SPIRA_CAPACITY_PAUSE:-$SPIRA_RUN/capacity-pause}"
# Used only when the account refused us without saying when it would stop. resetsAt has been
# present on every rejection observed, so this is the branch that should never run — which is
# exactly why it must not be a long sleep taken on faith. 15 minutes re-asks cheaply.
SPIRA_CAPACITY_BACKOFF="${SPIRA_CAPACITY_BACKOFF:-900}"

# capacity_reset_at <session-log> -> prints the epoch the window reopens; rc 0 if the
# session was ended by the account running out of capacity, rc 1 for anything else.
#
# rc 1 covers "the log does not exist", "the log is unparseable" and "the session failed for
# its own reasons" ALIKE, and that is deliberate: the false direction of this check must be
# the one that preserves today's behaviour. Reading a genuine failure as an outage would stop
# a bead ever being poisoned, which is the one property CHECK 4 exists to hold.
capacity_reset_at() {
    local logf="${1:-}"
    [ -n "$logf" ] && [ -s "$logf" ] || return 1
    # THE LAST ATTEMPT ONLY. The log carries every attempt this bead has had, and a refusal
    # is sticky evidence: attempt 1 dying to a spent window would otherwise make attempt 3
    # look refused too, so a bead that genuinely failed would be handed its attempt back and
    # the harness would pause summoning against a `resetsAt` that has already passed. No cap
    # — this runs once at teardown, and the two records that decide the verdict sit at
    # opposite ends of a session.
    # THE PROGRAM ARRIVES ON FD 3, NOT ON STDIN, because stdin is the trace. `python3 -
    # <<PY` looks right and silently reads the HEREDOC as the data too: the redirect wins,
    # the pipe is discarded unread, and the detector then says "not a refusal" about every
    # log ever handed to it — with a BrokenPipeError from the writer as the only tell.
    attempt_trace "$logf" | python3 /dev/fd/3 3<<'PY'
import json, sys

# The session limit shows up twice in one trace and either alone is enough. The
# rate_limit_event is preferred because it carries resetsAt as an epoch; the terminal
# `result` record is the fallback for a refusal that arrives without one.
LIMIT_TEXT = ("hit your session limit", "usage limit", "rate limit")
reset, hit = 0, False
for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        d = json.loads(line)
    except ValueError:
        continue          # a partial last line is normal on a killed session
    if not isinstance(d, dict):
        continue
    if d.get("type") == "rate_limit_event":
        info = d.get("rate_limit_info") or {}
        # `status`, never `overageStatus` — see the header. A value we have never seen
        # is not treated as a refusal: an unknown string must not be able to halt the
        # harness, and a real refusal also lands on the `result` record below.
        if info.get("status") == "rejected":
            hit = True
            try:
                reset = max(reset, int(info.get("resetsAt") or 0))
            except (TypeError, ValueError):
                pass
    elif d.get("type") == "result" and d.get("is_error"):
        # `subtype` is "success" on this record even though is_error is true, so subtype
        # cannot be the test. The text is what distinguishes an account refusal from a
        # session that failed at its own work.
        text = str(d.get("result") or "").lower()
        if any(t in text for t in LIMIT_TEXT):
            hit = True
if not hit:
    raise SystemExit(1)
print(reset)
PY
}

# capacity_pause_set <epoch> <reason> — record that the account is out until <epoch>.
#
# ANNOUNCED HERE AND ONLY HERE. The bead asked for it to be said "once in the ledger rather
# than every pass"; the write is the once. An existing pause is only ever EXTENDED, never
# shortened, so a second aeon dying into the same outage cannot pull the reopening forward
# to its own — older — reading of resetsAt.
capacity_pause_set() {
    local at="${1:-0}" why="${2:-unknown}" now cur
    now="$(date +%s)"
    [ "${at:-0}" -gt "$now" ] 2>/dev/null || at=$(( now + SPIRA_CAPACITY_BACKOFF ))
    cur="$(capacity_pause_until)"
    [ "${cur:-0}" -ge "$at" ] 2>/dev/null && return 0
    mkdir -p "$(dirname "$SPIRA_CAPACITY_PAUSE")" 2>/dev/null
    printf '%s %s %s\n' "$at" "$(date -u -d "@$at" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" "$why" \
        > "$SPIRA_CAPACITY_PAUSE"
    log "CAPACITY: the account is out until $(date -u -d "@$at" +%H:%M 2>/dev/null)Z ($(( at - now ))s) — summoning is paused, $why returned unchanged"
    printf '%s CAPACITY paused until %s %s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$(date -u -d "@$at" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" "$why" >> "$SPIRA_RUN/aeon-ledger.log"
}

capacity_pause_until() {  # -> the epoch a pause runs to, or 0 if none is recorded
    local at
    [ -f "$SPIRA_CAPACITY_PAUSE" ] || { printf '0'; return; }
    at="$(awk 'NR==1{print $1}' "$SPIRA_CAPACITY_PAUSE" 2>/dev/null)"
    case "${at:-}" in ''|*[!0-9]*) printf '0' ;; *) printf '%s' "$at" ;; esac
}

# capacity_paused -> rc 0 while the window is still shut, and sets $SPIRA_CAPACITY_LEFT to
# the seconds remaining.
#
# THE ANSWER IS A GLOBAL, NOT STDOUT, because this function also announces the reopening —
# and a caller reading it as `left="$(capacity_paused)"` would capture that announcement into
# a variable it then discards, so the one line saying the harness is moving again would be
# swallowed by the check that resumed it (law-absence-needs-a-positive-control, in the
# direction nobody looks: the all-clear that never printed).
#
# A pause that has run out is REMOVED here rather than merely ignored, so the file itself is
# the answer to "is the harness paused" for anything reading it without this library.
#
# PROBE WHEN THE HORIZON IS FAR OUT. A single refusal can write a pause that runs for
# days; the account may re-open hours earlier than the refusal message claimed. When the
# remaining time exceeds SPIRA_CAPACITY_PROBE_WINDOW, capacity_probe_maybe is called:
# a probe that receives a response lifts the pause early, a refused probe keeps it.
# This is the only place that lifts a pause via a probe, and it only runs here, where
# every summon check passes through — so the probe fires exactly when the queue is
# blocked and no more often than SPIRA_CAPACITY_PROBE_INTERVAL allows.
SPIRA_CAPACITY_LEFT=0
capacity_paused() {
    local at now
    at="$(capacity_pause_until)"; now="$(date +%s)"
    if [ "$at" -gt "$now" ] 2>/dev/null; then
        SPIRA_CAPACITY_LEFT=$(( at - now ))
        if [ "$SPIRA_CAPACITY_LEFT" -gt "${SPIRA_CAPACITY_PROBE_WINDOW:-18000}" ] 2>/dev/null \
           && capacity_probe_maybe; then
            rm -f "$SPIRA_CAPACITY_PAUSE"
            log "CAPACITY: probe served — pause lifted early (horizon was ${SPIRA_CAPACITY_LEFT}s out)"
            SPIRA_CAPACITY_LEFT=0
            return 1
        fi
        return 0
    fi
    SPIRA_CAPACITY_LEFT=0
    if [ -f "$SPIRA_CAPACITY_PAUSE" ]; then
        rm -f "$SPIRA_CAPACITY_PAUSE"
        log "CAPACITY: the window has reopened — summoning resumes"
    fi
    return 1
}

# Path for the probe-last timestamp. Lives beside the other capacity state files.
SPIRA_CAPACITY_PROBE_LAST="${SPIRA_CAPACITY_PROBE_LAST:-$SPIRA_RUN/capacity-probe-last}"

# capacity_probe_maybe -> rc 0 if the probe ran and the account responded, rc 1 otherwise.
#
# Returns 1 without probing when the interval has not elapsed since the last attempt. The
# timestamp is written BEFORE the probe runs, not after: if the process is killed during a
# hung probe the next call still respects the interval instead of looping immediately
# (law-bound-the-rare-path).
capacity_probe_maybe() {
    local last now interval
    interval="${SPIRA_CAPACITY_PROBE_INTERVAL:-3600}"
    now="$(date +%s)"
    last="$(awk 'NR==1{print $1}' "$SPIRA_CAPACITY_PROBE_LAST" 2>/dev/null)"
    case "${last:-}" in ''|*[!0-9]*) last=0 ;; esac
    [ "$(( now - last ))" -lt "$interval" ] 2>/dev/null && return 1
    mkdir -p "$(dirname "$SPIRA_CAPACITY_PROBE_LAST")" 2>/dev/null
    printf '%s %s\n' "$now" "$(date -u -d "@$now" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" \
        > "$SPIRA_CAPACITY_PROBE_LAST"
    log "CAPACITY: probing the account (pause horizon ${SPIRA_CAPACITY_LEFT}s, interval ${interval}s)"
    if capacity_probe; then
        log "CAPACITY: probe served — account is open"
        return 0
    else
        log "CAPACITY: probe refused — pause continues"
        return 1
    fi
}

# capacity_probe -> rc 0 if the account serves a minimal request, rc 1 otherwise.
#
# Uses SPIRA_AGENT (the configured agent CLI, default: claude) so tests drive it through
# the same seam that aeon.sh uses for its own injections. Uses SPIRA_CAPACITY_PROBE_MODEL
# so an operator whose pool runs a different model can match the probe to it; unset, it
# DEFAULTS TO THE BUILDER'S OWN RESOLVED MODEL — a probe the builder's model cannot answer
# is evidence the account is genuinely out for builders, so the two must not drift apart.
# Timeouts are treated as refusals — an API that does not answer in
# SPIRA_CAPACITY_PROBE_TIMEOUT seconds is not evidence the account is open, and the
# conservative direction is to keep the pause.
capacity_probe() {
    printf 'ok' | timeout "${SPIRA_CAPACITY_PROBE_TIMEOUT:-30}" \
        "${SPIRA_AGENT:-claude}" -p --model "${SPIRA_CAPACITY_PROBE_MODEL:-$(persona_model builder)}" \
        >/dev/null 2>&1
}

capacity_pause_why() {   # -> what was being worked when the account ran out
    [ -f "$SPIRA_CAPACITY_PAUSE" ] || return 1
    awk 'NR==1{$1="";$2="";sub(/^  */,"");print}' "$SPIRA_CAPACITY_PAUSE" 2>/dev/null
}

# --------------------------------------------------------------------------------------
# THE WITHDRAWAL LEDGER — which refusal has already been paid back.
#
# Giving an attempt back is driven by evidence that stays on disk: a session log ending in
# an account refusal. Evidence that stays is evidence that can be read twice, so a cleanup
# with no memory of itself withdraws a second attempt from the same log on its second run,
# a third on its third, and attempt counts walk to zero — after which nothing can ever
# poison, however genuinely it keeps failing. Nothing calls that cleanup automatically,
# which is not a defence: a hand-run command invites being run again.
#
# THE MARK IS KEYED ON THE LOG'S CONTENT, not on the bead and not on a date. The horizon is
# one attempt deep by construction — aeon.sh truncates `$SPIRA_RUN/<id>.log` on every
# attempt — so "has this refusal already been paid back" is exactly "is this the same log I
# paid back last time". A NEW refusal rewrites the file, the fingerprint moves, and the next
# withdrawal is made. A bead keyed mark would refuse the second outage; a dated one would
# turn the erosion back on after however long it waited.
#
# Losing the ledger costs one duplicate withdrawal per refused log and nothing worse, which
# is why it lives under SPIRA_RUN beside the logs it describes rather than in the database.
# It needs no config key for the same reason the traces do not: the harness put it there.
# --------------------------------------------------------------------------------------
SPIRA_CAPACITY_WITHDRAWN="${SPIRA_CAPACITY_WITHDRAWN:-$SPIRA_RUN/capacity-withdrawn}"

# capacity_log_fingerprint <log> -> a string that moves when the log's content does; rc 1
# if there is no readable content to fingerprint.
#
# Content and not `stat`: size and mtime make an unchanged log look new whenever anything
# copies, restores or re-syncs the runtime directory, and every one of those false readings
# spends an attempt that was never charged.
capacity_log_fingerprint() {
    local f="${1:-}" h
    [ -n "$f" ] && [ -s "$f" ] || return 1
    if command -v sha256sum >/dev/null 2>&1; then
        h="$(sha256sum < "$f" 2>/dev/null | awk '{print $1}')"
    else
        # cksum is POSIX and always there. It is weaker, and it does not need to be strong:
        # this distinguishes one session trace from the next, not from an adversary's.
        h="$(cksum < "$f" 2>/dev/null | tr -s ' ' -)"
    fi
    [ -n "$h" ] || return 1
    printf '%s' "$h"
}

capacity_withdrawn_fp() {   # capacity_withdrawn_fp <id> -> the fingerprint already paid back, or nothing
    local id="${1:-}"
    [ -n "$id" ] || return 1
    awk 'NR==1{print $1}' "$SPIRA_CAPACITY_WITHDRAWN/$id" 2>/dev/null
}

# capacity_withdrawn_mark <id> <fingerprint> <attempt> — record that this exact log has been
# paid back. Written whole rather than appended: one line per bead is the entire question,
# and a file that only ever grows is one more thing to prune.
capacity_withdrawn_mark() {
    local id="${1:-}" fp="${2:-}" att="${3:-0}"
    [ -n "$id" ] && [ -n "$fp" ] || return 1
    mkdir -p "$SPIRA_CAPACITY_WITHDRAWN" 2>/dev/null || return 1
    printf '%s %s %s\n' "$fp" "$att" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        > "$SPIRA_CAPACITY_WITHDRAWN/$id"
}

# --------------------------------------------------------------------------------------
# THREE COUNTERS, BECAUSE THERE ARE THREE FAILURES AND THEY WANT DIFFERENT ANSWERS.
#
#   sp-attempt-N   the WORK was tried and did not land. Feeds the poison threshold.
#   sp-reclaim-N   the WORKER died holding the bead. Diagnostic; feeds nothing that stops
#                  a bead being worked.
#   sp-requeue-N   the HARNESS put finished work back. The session committed and closed the
#                  bead, and a rebase onto a base that had moved underneath it no longer
#                  replayed — so the bead was reopened and the next aeon inherits the
#                  conflict. Diagnostic; feeds nothing.
#
# The third exists because the first two cannot express it and the first one was taking it.
# A session that finishes, closes, and is reopened over a rebase looks from the counter's
# side exactly like a session that ran to its own end leaving the bead open — `unlanded`,
# which charges. One bead was charged all eight of its attempts that way over work that
# later landed unchanged, and another was poisoned nineteen hours after the session that
# finished it, with a branch that merged cleanly the whole time. Poisoning is permanent and
# a poisoned bead stays OPEN while the landing pass lands only CLOSED beads, so that is a
# deadlock arrived at by counting, with nothing wrong with the work.
#
# It is a COUNTER and not merely an exemption because a bead that has cycled eight times is
# a fact worth seeing: the queue is manufacturing conflicts faster than the work can absorb
# them, and without a number nobody would know (law-take-the-simple-fix-with-a-meter).
#
# They were one counter, and one aeon dying cost a bead TWO of its three attempts: the
# teardown bumped on the way out and strand.sh bumped again when it reclaimed the same bead
# after the lease expired. Beads were poisoned without their work ever having been tried —
# the sessions were refused by the API seconds in. Poison then fires hardest during
# infrastructure flapping, which is exactly when the queue can least afford to lose work.
# "This bead cannot be worked" and "this host keeps killing aeons" are different claims and
# neither is evidence for the other.
#
# Kept as labels rather than metadata because a label is visible in every listing and
# filterable by the same --exclude-label surface claiming uses, so the poison threshold is
# enforced at SELECTION time rather than after a wasted claim.
#
# AND EACH RUNG CARRIES ITS CAUSE, because "three attempts" is only a reason to stop if all
# three were the work failing. The label is `sp-attempt-2-unlanded`, not `sp-attempt-2`: a
# bare number records that something happened without recording what, so a poison nobody can
# audit takes a bead out of circulation for reasons that have already scrolled away. The
# counter is still the leading `<prefix>-<n>`, so every reader of the number is unchanged.
# --------------------------------------------------------------------------------------
# ATTEMPTS ARE COMPUTED FROM THE EVENTS TRAIL, NOT STORED AS LABELS.
#
# An aeon claiming a bead writes event_type='claimed'. A hand-driven transition through
# `bd update --status in_progress` writes event_type='status_changed' with new_value
# containing 'in_progress'. BOTH are attempts and the count is the union of the two.
#
# THE ORIGINAL PREDICATE COUNTED ONLY status_changed AND THEREFORE RETURNED 0 FOR EVERY
# BEAD AN AEON HAD EVER WORKED. Measured 2026-09-12 across four real beads: shipped=0 for
# all of them while claimed=1 for each of the three an aeon had taken. Since poisoning is
# `attempts >= 3`, nothing could ever be poisoned and a looping bead would loop for ever —
# a silent failure of a safety mechanism, which is worse than a loud one.
#
# The error came from the brief, not the implementation: the predicate was verified against
# `bd update --status in_progress` and generalised to "how an aeon claims", which is a
# different code path writing a different event_type. Verify the path the system actually
# takes, not one that resembles it.
#
# WHY THE EVENTS TABLE, NOT LABELS. Counter labels (sp-attempt-N, sp-reclaim-N, etc.)
# produced 195 of 660 distinct labels in the old store — sp-reclaim alone had 96 forms
# because it encoded a cause suffix. Nothing stored means nothing that can disagree with
# the events trail, and a wrong value refused at write time is better than a truthful
# empty result at read time.
#
# Query path: bd sql (server mode only).
#
# THREE CONSTRAINTS, VERIFIED IN sp-lzt AND RECORDED HERE SO NO ONE REDISCOVERS THEM:
#   1. bd query cannot express it — no events field. bd sql is the right tool.
#   2. json_extract on new_value returns empty in Dolt even with a cast. LIKE works.
#   3. Filter on event_type, or a label_added row whose comment mentions 'in_progress'
#      would be counted. The filter is a whitelist of the two event types that mean an
#      attempt, never a bare LIKE over every row.
# --------------------------------------------------------------------------------------
_attempts_sql_query() {   # _attempts_sql_query <id> -> the SQL that counts attempts
    # AN ATTEMPT IS A CLAIM THAT DID NOT SUCCEED — claims minus successful closes.
    #
    # Counting raw claims makes a HARNESS REQUEUE indistinguishable from an aeon failure,
    # and the poison threshold then fires on work that succeeded. sp-7tj was claimed three
    # times and CLOSED SUCCESSFULLY three times: CHECK 5 reopened it on each pass because its
    # work reached the base by content before the Sending applied `content-landed`, and the
    # bead was poisoned for it. Three completed groom passes, one poisoned bead, nothing
    # wrong with the work.
    #
    # The old label counters exempted thrash requeues; sp-lzt deleted them; the exemption is
    # restored here: requeued/thrash events are subtracted so a thrash claim is net-zero.
    # A worker that died before judging the bead writes requeued/unjudged-<cause> and is
    # net-zero too: aeon.sh tells the bead no attempt was charged, and this is where that
    # promise is kept (sp-8fgmw).
    #
    # GREATEST(...,0) because a bead can carry more closes than claims — an operator closing
    # a bead by hand adds one with no claim behind it.
    #
    # THE created_at FLOOR. A cleared poison must stay cleared: `spira-claim unpoison` (or
    # `groomer.sh deadlocked`) records a poison.cleared event and this excludes everything at
    # or before it, so a bead an operator judged worth retrying starts that retry at 0 rather
    # than at the count that poisoned it.
    # Without the floor, clearing the label alone changes nothing this query reads, and CHECK 4
    # reads the same old count against a bare label on its very next pass (sp-qd2ul). COALESCE
    # to the epoch when no poison.cleared event exists, so an uncleared bead's count is
    # untouched.
    # THE FLOOR IS A SCALAR SUBQUERY, NOT A CORRELATED ONE. `pc.issue_id=events.issue_id`
    # against an outer query already filtered to `issue_id='%s'` still asks Dolt to
    # re-evaluate the inner subquery once per matched row rather than once per query — cheap
    # for one bead's own handful of events, but the same shape that made the bulk form
    # (_check4_bulk_sql) unusable at more than a few ids (sp-rp4g4). Filtering the inner
    # subquery on the same literal id removes the correlation outright: it is now
    # independent of the outer row and evaluated once.
    #
    # EACH sum() IS COALESCEd BEFORE THE ARITHMETIC. The outer WHERE can leave zero rows —
    # every event at or before a just-written poison.cleared floor, the exact state right
    # after a clear — and sum() over zero rows is NULL, not 0. NULL minus NULL is NULL, and
    # greatest(NULL,0) is NULL too: the query printed "<nil>" instead of "0", and
    # attempts_of's own fail-closed check (sp-418h5) correctly refused to parse it, reading
    # as a query failure a caller right after unpoison.sh could not tell from a real one.
    printf "select greatest(coalesce(sum(case when event_type='claimed' or (event_type='status_changed' and new_value like '%%in_progress%%') then 1 else 0 end),0) - coalesce(sum(case when event_type='closed' then 1 else 0 end),0) - coalesce(sum(case when event_type='requeued' and (new_value='thrash' or new_value like 'unjudged%%') then 1 else 0 end),0), 0) from events where issue_id='%s' and created_at > coalesce((select max(created_at) from events where issue_id='%s' and event_type='poison.cleared'), '1970-01-01')" "$1" "$1"
}

# attempts_of <id> -> count of in_progress status-change events
# FAIL CLOSED, NOT OPEN. A query failure used to fall through to `printf '0'` with a 0 exit —
# indistinguishable from a bead that genuinely never failed, so a poisoned bead's per-bead
# re-check (sentinel.sh's stale-poison-clear scan) read a false zero as "below threshold" and
# cleared it, only for the next pass's bulk query to see the true count and poison it right
# back (law-a-control-that-cannot-check-must-refuse). Prints nothing and returns 1 on error;
# callers must treat that as "cannot tell", never default it to 0.
attempts_of() {
    local id="$1" q result=""
    q="$(_attempts_sql_query "$id")"
    if result="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" 2>/dev/null | sed -n '3p' \
                  | tr -d ' ')" && [ -n "$result" ] \
       && printf '%d' "$result" >/dev/null 2>&1; then
        printf '%d' "$result"; return 0
    fi
    return 1
}

# BUMP FUNCTIONS. bump_requeue writes a typed event row so that census can aggregate
# failure classes across the whole store (sp-2lk); bump_lapsed and bump_poison_cleared
# (below) write through the same helper for their own event types.
#
# Failure is silent — a missed counter is acceptable; a crash in a caller is not.
#
# _bump_write_event_try — the real write, reporting whether bd sql accepted it.
# _bump_write_event must keep returning 0: aeon.sh runs under `set -e` and calls
# bump_requeue bare, so a failing return would kill a live aeon mid-requeue.
_bump_write_event_try() {
    local id="${1:-}" etype="${2:-}" cause="${3:-unrecorded}"
    [ -n "$id" ] && [ -n "$etype" ] || return 0
    local actor="${BEADS_ACTOR:-harness}"
    local uuid q
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))' 2>/dev/null)" || return 1
    q="INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$etype', '$actor', '$cause', UTC_TIMESTAMP())"
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" >/dev/null 2>&1 && return 0
    return 1
}

_bump_write_event() { _bump_write_event_try "$@" >/dev/null 2>&1; return 0; }
bump_requeue() { _bump_write_event "${1:-}" requeued  "${2:-unrecorded}"; }
bump_lapsed()  { _bump_write_event "${1:-}" lapsed    "${2:-unrecorded}"; }

write_lapse_record() {  # write_lapse_record <bead> <quiet_s> <last_action> <tip> -> $SPIRA_RUN/lapsed/<bead>-<ts>
    local bead="$1" quiet="$2" last="$3" tip="$4"
    mkdir -p "$SPIRA_RUN/lapsed" 2>/dev/null || return 0
    printf 'bead: %s\nquiet: %ss\nlast: %s\nbranch: spira/%s\ntip: %s\n' \
        "$bead" "$quiet" "$last" "$bead" "$tip" \
        > "$SPIRA_RUN/lapsed/$bead-$(date -u +%Y%m%dT%H%M%SZ)"
}

# bump_poison_cleared <id> <cause> — the event _attempts_sql_query/_check4_bulk_sql floor on
# (sp-qd2ul). Written by `spira-claim unpoison` (or `groomer.sh deadlocked`'s call into
# `spira-claim deadlocked`), never by a bare label removal: a clear that leaves no trace here
# is indistinguishable from one that was never judged, and the next CHECK 4 pass reads the
# unchanged count against a bare label and re-poisons within minutes.
bump_poison_cleared() { _bump_write_event "${1:-}" 'poison.cleared' "${2:-unrecorded}"; }

# bead_metadata <id> <key> -> the value bd update --set-metadata wrote, or empty when unset.
# --long is required: bd show --json omits metadata by default (sp-4rzlw).
bead_metadata() {
    local id="${1:-}" key="${2:-}"
    [ -n "$id" ] && [ -n "$key" ] || { printf ''; return 0; }
    bdjson show "$id" --long 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
d = d if isinstance(d, list) else [d]
m = (d[0].get("metadata") or {}) if d else {}
print(m.get(sys.argv[1], ""))' "$key" 2>/dev/null
}

# THRASH STREAK — how many consecutive thrash requeues have landed on this bead with its
# branch tip unchanged. Stored as bead metadata (thrash_tip, thrash_streak, thrash_last)
# because it must survive between aeon summons — unlike $SPIRA_RUN, which is this session's
# scratch, and unlike spira-poison, this is never read at selection time so it does not need
# a label's visibility.
#
# A thrash requeue is exempted from the poison count on the theory that the aeon was killed
# for stalling, not judged on its work (aeon.sh, THRASH IS NOT FAILURE). That theory holds
# for one requeue; it does not hold for a bead requeued again and again at the SAME commit,
# which is a bead nothing is moving rather than one aeon that got unlucky — sp-gs24i got five
# summons across seven hours this way and none of them charged, because "no new commit" was
# never checked.
#
# thrash_streak_bump <id> <tip> <note> -> the streak AFTER this bump (an integer, printed).
# Same tip as last time bumps the streak; any other tip (including the first thrash ever, or
# one that moved) resets it to 1. The caller decides what a streak at or over the configured
# cap means — see aeon.sh's .thrash handler.
thrash_streak_bump() {
    local id="${1:-}" tip="${2:-?}" note="${3:-}" prev_tip prev_streak streak
    [ -n "$id" ] || { printf '0'; return 0; }
    prev_tip="$(bead_metadata "$id" thrash_tip)"
    prev_streak="$(bead_metadata "$id" thrash_streak)"
    if [ -n "$tip" ] && [ "$tip" != "?" ] && [ "$tip" = "$prev_tip" ]; then
        streak=$(( ${prev_streak:-0} + 1 ))
    else
        streak=1
    fi
    # printf, not a herestring: <<< appends its own trailing newline, which tr would turn
    # into a trailing space that survives the cut below (command substitution only strips
    # trailing NEWLINES, not spaces).
    note="$(printf '%s' "$note" | tr '\n\r' '  ' | cut -c1-300)"
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" update "$id" \
        --set-metadata "thrash_tip=$tip" \
        --set-metadata "thrash_streak=$streak" \
        --set-metadata "thrash_last=$note" \
        >/dev/null 2>&1
    printf '%d' "$streak"
}

# DIAGNOSTIC ACCESSOR — the requeue counter is read from the events table via bd sql
# (sp-2lk).
# --------------------------------------------------------------------------------------
# _counter_events_sql <id> <event_type> — SQL that returns a single integer count.
_counter_events_sql() {
    printf "SELECT COUNT(*) FROM events WHERE issue_id='%s' AND event_type='%s'" "$1" "$2"
}
_counter_events_query() {   # _counter_events_query <id> <event_type> -> count, or '?' if bd sql fails
    # '?' ON FAILURE, NOT '0'. A query that cannot reach the events table and one that
    # reached it and found nothing print the same digit if both return '0' — the reader
    # cannot tell "no activity" from "the driver is down" (law-absence-needs-a-positive-
    # control). recurs_of's caller (incident.sh's Sin decision, before recurs_of moved to
    # the incident crate) once folded that silence into zero recurrences and stayed
    # silent through an outage forever (sp-39yd3).
    local id="$1" etype="$2" q result=""
    q="$(_counter_events_sql "$id" "$etype")"
    if result="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" 2>/dev/null | sed -n '3p' \
                  | tr -d ' ')" && [ -n "$result" ] \
       && printf '%d' "$result" >/dev/null 2>&1; then
        printf '%d' "$result"; return 0
    fi
    printf '?'
    return 1
}
requeues_of() { _counter_events_query "${1:-}" requeued;  }

# CENSUS SQL — the query and runner used by census to aggregate failure classes.
# Kept in lib.sh so that tests can call it directly without parsing census.
# --------------------------------------------------------------------------------------
_census_events_sql() {   # _census_events_sql [since_epoch_s]
    # An optional Unix epoch lower bound adds "AND created_at > FROM_UNIXTIME(ts)" so
    # callers can distinguish events since a watermark from all-time totals. Zero or absent
    # means all-time.
    local since_clause=""
    if [ -n "${1:-}" ] && [ "${1:-0}" -gt 0 ] 2>/dev/null; then
        since_clause=" AND created_at > '$(date -u -d "@${1}" '+%Y-%m-%d %H:%M:%S')'"
    fi
    # Single-source predicates for each folded event pair. A guard block that calls both
    # bead_reopen and sets REQUEUE_CAUSE to the same string writes two event streams for
    # one firing; the fold merges them so census does not double-count and a covers: label
    # on either name suppresses the whole pair (law-bake-rules-into-tools).
    local conflict_fold="(event_type = 'requeued' AND new_value = 'merge-conflict')"
    local rebase_aeon_fold="(event_type = 'requeued' AND new_value = 'rebase-conflict')"
    local eviction_fold="(event_type IN ('reopen', 'requeued') AND new_value = 'eviction-race')"
    local prod_dirty_fold="(event_type IN ('reopen', 'requeued') AND new_value = 'prod-dirty')"
    local unfinished_fold="(event_type IN ('reopen', 'requeued') AND new_value = 'unfinished-reason')"
    # REOPEN-TIMING EXCLUSION, not a fold: _reopen_cause classifies HOW RECENTLY a bead was
    # closed when the same failure fingerprint recurred ('closed-while-live' vs
    # 'recurrence'), it is not itself a cause. One re-filing always writes both this event
    # and a 'recurred' event carrying the true cause (incident.sh's file_one), so ranking
    # this one too double-counts every re-filing under two class names. Unlike the folds
    # above, no covers: entry retires it (_census_class_fold_map) because nothing should
    # ever suppress it by name — it never appears.
    local reopen_timing_exclude="(event_type = 'reopen' AND new_value IN ('closed-while-live', 'recurrence'))"
    # DELIBERATE-CAUSE FOLD. A reopen whose cause is in _census_deliberate_reopen_causes
    # (adjacent to _census_class_fold_map below) is the system working, not a fault
    # (law-a-deliberate-state-is-not-a-fault): excluded from the ranked class list here,
    # the same way an operator's hand-written row is excluded by actor_filter below —
    # still counted, by _census_deliberate_sql, just never selectable (sp-eiatd).
    local deliberate_fold="(event_type = 'reopen' AND new_value IN ($(_census_deliberate_causes_sql_list)))"
    # ACTOR PREDICATE. Ranks only events a harness component wrote (harness or an
    # aeon-* session); an operator's hand-written events-table row — e.g. zeroing an
    # attempt-ledger offset with a fabricated requeued/unjudged-<cause> row — is real
    # state, not a failure class no code path can stop emitting, and is listed
    # separately by _census_handwritten_sql instead (law-absence-needs-a-positive-
    # control, law-a-deliberate-state-is-not-a-fault).
    local actor_filter="(actor = 'harness' OR actor LIKE 'aeon-%')"
    printf "SELECT event_type, COALESCE(new_value, ''), COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type IN ('requeued', 'reclaimed', 'recurred', 'lapsed', 'reopen') AND NOT %s AND NOT (event_type = 'reopen' AND new_value = 'rebase-conflict') AND NOT %s AND NOT %s AND NOT %s AND NOT %s AND NOT %s AND NOT %s AND %s%s GROUP BY event_type, new_value UNION ALL SELECT 'reopen', 'rebase-conflict', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE ((event_type = 'reopen' AND new_value = 'rebase-conflict') OR %s OR %s) AND %s%s HAVING COUNT(DISTINCT issue_id) > 0 UNION ALL SELECT 'reopen', 'eviction-race', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE %s AND %s%s HAVING COUNT(DISTINCT issue_id) > 0 UNION ALL SELECT 'reopen', 'prod-dirty', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE %s AND %s%s HAVING COUNT(DISTINCT issue_id) > 0 UNION ALL SELECT 'reopen', 'unfinished-reason', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE %s AND %s%s HAVING COUNT(DISTINCT issue_id) > 0 UNION ALL SELECT 'reopened', 'unrecorded', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE event_type = 'reopened' AND %s%s AND issue_id NOT IN (SELECT issue_id FROM events WHERE (event_type = 'reopen' OR %s)%s) HAVING COUNT(DISTINCT issue_id) > 0 ORDER BY 3 DESC" "$conflict_fold" "$rebase_aeon_fold" "$eviction_fold" "$prod_dirty_fold" "$unfinished_fold" "$reopen_timing_exclude" "$deliberate_fold" "$actor_filter" "$since_clause" "$conflict_fold" "$rebase_aeon_fold" "$actor_filter" "$since_clause" "$eviction_fold" "$actor_filter" "$since_clause" "$prod_dirty_fold" "$actor_filter" "$since_clause" "$unfinished_fold" "$actor_filter" "$since_clause" "$actor_filter" "$since_clause" "$conflict_fold" "$since_clause"
}
# _census_handwritten_sql — events excluded from the ranked query above by the actor
# predicate, grouped so the excluded actor stays visible. Not time-windowed: this is
# a standing ledger-correction listing, not a since-watermark ranking.
_census_handwritten_sql() {
    printf "SELECT event_type, COALESCE(new_value, ''), actor, COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type IN ('requeued', 'reclaimed', 'recurred', 'lapsed', 'reopen', 'reopened') AND NOT (actor = 'harness' OR actor LIKE 'aeon-%%') GROUP BY event_type, new_value, actor ORDER BY 4 DESC"
}
census_handwritten_run_sql() {   # -> tabular output of actor-excluded events, empty when none
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$(_census_handwritten_sql)" 2>/dev/null
}
_census_class_fold_map() {
    # <folded-away-class> <canonical-class>. A covers: label naming a folded-away class
    # suppresses the class it was folded into. Keep this adjacent to the SQL fold in
    # _census_events_sql: a rename of one must carry the other.
    printf 'sp-requeue-merge-conflict sp-reopen-rebase-conflict\n'
    printf 'sp-requeue-rebase-conflict sp-reopen-rebase-conflict\n'
    printf 'sp-requeue-eviction-race sp-reopen-eviction-race\n'
    printf 'sp-requeue-prod-dirty sp-reopen-prod-dirty\n'
    printf 'sp-requeue-unfinished-reason sp-reopen-unfinished-reason\n'
    printf 'sp-requeue-workflow-run-missing sp-reopen-workflow-run-missing\n'
    printf 'sp-requeue-workflow-run-wrong-branch sp-reopen-workflow-run-wrong-branch\n'
    printf 'sp-requeue-workflow-run-stale-sha sp-reopen-workflow-run-stale-sha\n'
    printf 'sp-requeue-workflow-run-wrong-file sp-reopen-workflow-run-wrong-file\n'
    printf 'sp-requeue-workflow-run-unverifiable sp-reopen-workflow-run-unverifiable\n'
}

# _census_deliberate_reopen_causes -> "<cause> <admission-exempt:0|1>" pairs, one per
# line — the single declared list of reopen causes that are the system working, not a
# fault (law-a-deliberate-state-is-not-a-fault): firing queue.sh eject, or converting a
# closed work bead to submitted, is a correct outcome, so census must count these but
# never rank them for a Maechen remedy (sp-eiatd). Kept adjacent to _census_class_fold_map
# above, the same way that fold is kept adjacent to the SQL that uses it — both
# bead_reopen (below) and _census_events_sql/_census_deliberate_sql read this one list.
#
# <admission-exempt> marks the one behavior specific to work-close-converted: whether the
# CERTIFIED landstate and the submitted label survive the reopen. work-close-converted is
# bookkeeping (aeon.sh's teardown carrying a finished bead to the landing pass), not
# rework, so it alone stays admissible. Every other deliberate cause — eject withdraws a
# CERTIFIED-but-unbatched bead that DOES need rework — still needs WITHDRAWN written and
# the label stripped, or batch.sh's second line of defence (its own comment: "every eject
# strips the label") would re-admit a bead the operator just pulled from the queue.
_census_deliberate_reopen_causes() {
    printf 'work-close-converted 1\n'
    printf 'eject 0\n'
}

# _census_reopen_admission_exempt <cause> -> 0 (stays CERTIFIED/submitted) or 1 (does not)
_census_reopen_admission_exempt() {
    local _c _exempt
    while read -r _c _exempt; do
        [ "$_c" = "$1" ] && [ "$_exempt" = 1 ] && return 0
    done <<< "$(_census_deliberate_reopen_causes)"
    return 1
}

# _census_deliberate_causes_sql_list -> a quoted, comma-separated SQL IN-list of every
# deliberate cause's name, built from _census_deliberate_reopen_causes so the ranking
# exclusion and the visibility query below can never name a different set.
_census_deliberate_causes_sql_list() {
    local _dc _dexempt _list=""
    while read -r _dc _dexempt; do
        [ -n "$_dc" ] || continue
        _list="${_list:+$_list, }'$(printf '%s' "$_dc" | sed "s/'/''/g")'"
    done <<< "$(_census_deliberate_reopen_causes)"
    printf '%s' "${_list:-''}"
}

# _census_deliberate_sql [since_epoch_s] -> the deliberate-cause reopen counts excluded
# from _census_events_sql's ranked list above. Not ranked, never selectable, but still a
# real count (law-absence-needs-a-positive-control) — census shows it under
# --with-suppressed the same way _census_handwritten_sql's actor-excluded rows are shown.
_census_deliberate_sql() {
    local since_clause=""
    if [ -n "${1:-}" ] && [ "${1:-0}" -gt 0 ] 2>/dev/null; then
        since_clause=" AND created_at > '$(date -u -d "@${1}" '+%Y-%m-%d %H:%M:%S')'"
    fi
    printf "SELECT event_type, COALESCE(new_value, ''), COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type = 'reopen' AND new_value IN (%s)%s GROUP BY event_type, new_value ORDER BY 3 DESC" \
        "$(_census_deliberate_causes_sql_list)" "$since_clause"
}
census_deliberate_run_sql() {   # census_deliberate_run_sql [since_epoch_s] -> tabular output of deliberate-cause reopen counts, empty when none
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$(_census_deliberate_sql "${1:-}")" 2>/dev/null
}
census_events_run_sql() {   # census_events_run_sql [since_epoch_s] -> tabular output; exits non-zero when unreachable
    local q
    q="$(_census_events_sql "${1:-}")"
    local out bd_rc _errtmp _delay _attempt
    _delay="${CENSUS_RETRY_DELAY_S:-2}"
    _errtmp="$(mktemp)"
    for _attempt in 1 2 3; do
        bd_rc=0
        out="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" 2>"$_errtmp")" || bd_rc=$?
        if [ "$bd_rc" -eq 0 ]; then
            rm -f "$_errtmp"
            printf '%s\n' "$out"
            return 0
        fi
        [ "$_attempt" -lt 3 ] && sleep "$_delay" && _delay=$((_delay * 2))
    done
    printf 'census_events_run_sql: query failed after 3 attempts: %s\n' "$(cat "$_errtmp")" >&2
    rm -f "$_errtmp"
    return 1
}

# counter_label -> the historical sp-attempt-N / sp-reclaim-N / sp-requeue-N bd label a
# bead still carries, if any, read-only. bump_counter stopped writing these at sp-lzt
# (spira-claim/DESIGN.md §9's `audit` is the going-forward tool, reading the events table
# instead); this remains only to render whatever a caller finds on an old bead.
counter_label() {
    local all hit
    all="$(bdq label list "$1" 2>/dev/null | sed -n 's/^ *- //p')" || all=""
    hit="$(grep -xE "$2-$3(-.*)?" <<<"$all")" || return 1
    printf '%s' "$(sed -n 1p <<<"$hit")"
}

# --------------------------------------------------------------------------------------
# THE POISON ASK'S SUPPRESSION, AND WHY IT IS NOT THE POISON LABEL.
#
# The valve filed its escalation whenever a bead was over the threshold and did not CURRENTLY
# carry `spira-poison`, so the label was both the dispatch valve and the ask's only
# suppression — and the ask's own recommended remedy is "change the approach, then clear the
# label". Doing what it asks therefore deleted the one thing stopping it being asked again,
# and the next pass asked again. One bead reached the operator three times in forty minutes
# about work that had already landed, and he had to say so twice.
#
# So suppression keys on something the remedy does NOT move: the attempt count that produced
# the ask. At most one ask per (bead, count), ever. Clearing the poison still allows the retry
# it exists to allow — and only a genuinely new failure at count+1 may ask again. Same shape
# as watchd's backlog fingerprint, and for the same reason.
#
# THE MARK IS WRITTEN ONLY AFTER THE ASK WAS ACCEPTED, so an escalation path that is down does
# not silently consume the one notification this count will ever produce.
# --------------------------------------------------------------------------------------
SPIRA_POISON_ASKED="${SPIRA_POISON_ASKED:-$SPIRA_RUN/poison-asked}"

# poison_asked_clear <id> — drop this bead's ask history. A poison.cleared event floors
# attempts_of back to zero (sp-qd2ul), so a genuinely new run of failures can reach the same
# raw count (e.g. 3) the pre-clear history already has a "3" entry for, and poison_asked would
# read that stale entry as "already asked" and suppress the new ask. The count it dedups on
# was just reset; its history must reset with it.
poison_asked_clear() {
    rm -f "${SPIRA_POISON_ASKED:?}/$1" 2>/dev/null || true
}

# --------------------------------------------------------------------------------------
# Landing verification. CLOSED is not landed: a bead is only done when its work is in the
# commit graph. Every aeon is required to name its bead id in the commit subject, which is
# what makes this checkable by a program instead of by reading a diff.
# --------------------------------------------------------------------------------------
# --------------------------------------------------------------------------------------
# THE CONTEXT AN ESCALATION MUST CARRY. (the operator, verbatim: "i don't know what this bead
# is, i need a description of the actual goal/problem/bead when you give me a decision to
# make about it." — said after a first fix that added a log tail but not the bead itself.)
#
# A log tail answers "what went wrong". It does not answer "what was this trying to do",
# which is the question you must answer FIRST to decide anything. So: title, status, age,
# labels, and the whole description — the problem statement in the bead's own words.
# --------------------------------------------------------------------------------------
# --------------------------------------------------------------------------------------
# AN AEON HAS A NAME. Every instance used to be `aeon-builder`, so two of them were
# indistinguishable in the pane, in `bd` history and in the commit graph — you could see
# that AN aeon closed a bead and never which one, which made "what is it doing" an
# unanswerable question (the operator, verbatim: "i really want to know what the aeon is
# doing (it should have an identity)").
#
# Named for the aeons of Spira, which is the whole reason the system carries that name.
# The prefix stays `aeon-` so every existing count that greps for it still works.
# --------------------------------------------------------------------------------------
SPIRA_AEON_NAMES="valefor ifrit ixion shiva bahamut yojimbo anima cindy sandy mindy"

aeon_name_take() {       # aeon_name_take <fayth> -> a name not currently in use
    local f="$1" n live
    live=" $(for pf in "$SPIRA_RUN"/aeon-*.name; do [ -e "$pf" ] || continue
                 p="${pf%.name}"; [ -f "$p.pid" ] && aeon_alive "$p.pid" && cat "$pf"; done | tr '\n' ' ') "
    # DO NOT REUSE THE NAME THE LAST AEON HAD. Picking the first free name meant two
    # consecutive sessions were both "valefor", so the pane looked like one agent switching
    # beads when it was one dying and another starting — which hid the fact that a bead had
    # been dropped with work in flight. A cursor makes consecutive aeons distinguishable.
    local last cursor=0
    last="$(cat "$SPIRA_RUN/.aeon-name-cursor" 2>/dev/null || echo 0)"
    case "$last" in ''|*[!0-9]*) last=0 ;; esac
    local total=0; for n in $SPIRA_AEON_NAMES; do total=$((total+1)); done
    local tries=0
    while [ "$tries" -lt "$total" ]; do
        cursor=$(( (last + 1 + tries) % total ))
        local idx=0
        for n in $SPIRA_AEON_NAMES; do
            if [ "$idx" -eq "$cursor" ]; then
                case "$live" in *" $n "*) ;; *)
                    printf '%s' "$cursor" > "$SPIRA_RUN/.aeon-name-cursor"
                    printf '%s' "$n"; return 0 ;;
                esac
            fi
            idx=$((idx+1))
        done
        tries=$((tries+1))
    done
    # More concurrent aeons than names is not an error, just unusual; fall back to a
    # numbered one rather than reusing a name and making two of them indistinguishable.
    printf 'aeon%s' "$(date +%s | tail -c 4)"
}

aeon_named() {           # aeon_named <pidfile> -> the name held by that aeon, if any
    local pf="$1"; [ -f "${pf%.pid}.name" ] && cat "${pf%.pid}.name" 2>/dev/null || printf '?'
}

# --------------------------------------------------------------------------------------
# ONE SESSION LOG PER BEAD, APPENDED TO, WITH ONE SEGMENT PER ATTEMPT.
#
# aeon.sh used to open the log with `>`, so an attempt erased its predecessor and only the
# last one of a bead had a trace at all. On a day when a capacity outage killed 121 sessions
# in three to seven seconds each, three traces survived; every other one had been overwritten
# by the next attempt on the same bead, and with them the only record of why the session
# died. The operator, verbatim: "I don't want to miss any insights from here on."
#
# Appending rather than one file per attempt is deliberate. The heartbeat decides whether a
# session is alive by watching `stat -c %s` on this path grow, and that check reads a fixed
# name — a new filename per attempt leaves it watching a file nobody is writing, which is
# indistinguishable from a wedged session and costs the bead its lease. Appending keeps the
# growth signal exactly as it was.
#
# What appending DOES change is that the file now holds events from sessions that are over,
# so every reader that asks "what is happening" must read the LAST segment and not the whole
# file: a `result` record from attempt 1 taken for attempt 3's would pause the harness for a
# capacity outage that ended hours ago, or report a finished session's last tool call as a
# live one's. attempt_trace is that boundary, and it is the only place the mark is parsed.
#
# The mark is a constant rather than a configuration key because it is a FORMAT, not a path:
# an operator who changed it would make every log already on disk unreadable by the code that
# writes the next line of it. It is defined once here and written by aeon.sh through
# spira_trace_mark, so the writer and the readers cannot drift.
#
# It is not JSON and does not start with `{`, which is what makes it inert: every consumer of
# this trace already skips any line that is not a JSON object, so the mark passes through
# trace_last, trace_tail, capacity_reset_at and tokens.sh without special handling.
# --------------------------------------------------------------------------------------
SPIRA_TRACE_MARK='=== spira attempt'

# attempt_trace <logfile> [cap] -> the LAST attempt's segment, at most <cap> trailing bytes
# (0 or absent means all of it). A log with no mark in it is emitted whole, because that is
# what every log written before this change looks like and one attempt is all it ever held.
attempt_trace() {
    local f="${1:-}" cap="${2:-0}"
    [ -r "$f" ] || return 0
    python3 - "$f" "$cap" "$SPIRA_TRACE_MARK" <<'PY'
import os, sys

path, cap, mark = sys.argv[1], int(sys.argv[2]), sys.argv[3].encode()
try:
    fh = open(path, "rb")
except OSError:
    raise SystemExit(0)
with fh:
    size = os.fstat(fh.fileno()).st_size
    # BACKWARDS IN CHUNKS, never a read of the whole file. This runs on every heartbeat of
    # every live aeon, and the file it reads is the one thing here that grows without bound;
    # a forward scan would make the cost of watching a session rise with how long the bead
    # has been worked, which is the wrong way round.
    CH, keep = 1 << 16, len(mark) + 1
    start, pos, carry = 0, size, b""
    while pos > 0:
        step = min(CH, pos)
        pos -= step
        fh.seek(pos)
        buf = fh.read(step) + carry
        i = buf.rfind(b"\n" + mark)
        if i >= 0:
            start = pos + i + 1
            break
        if pos == 0 and buf.startswith(mark):
            start = 0
            break
        # A mark straddling a chunk boundary belongs to neither half alone.
        carry = buf[:keep]
    fh.seek(max(start, size - cap) if cap > 0 else start)
    sys.stdout.buffer.write(fh.read())
PY
}

# wiki_write_paths <logfile> <wiki-dir> -> paths under <wiki-dir>, one per line, named by
# Edit/Write/NotebookEdit tool calls in the CURRENT attempt of <logfile>. Relative to
# <wiki-dir>, matching `git status` output there.
#
# THE TRANSCRIPT, NOT A DIRTY SNAPSHOT. A snapshot taken at session start cannot tell this
# session's own write from a concurrent actor's — both are just "dirty now, clean at the
# watermark" (sp-4fl2e). The transcript names only what this session's own tool calls touched.
wiki_write_paths() {
    local f="${1:-}" wiki="${2:-}"
    [ -n "$wiki" ] && [ -r "$f" ] || return 0
    attempt_trace "$f" 0 2>/dev/null | python3 -c '
import sys, json, os

wiki = os.path.realpath(sys.argv[1])
EDITS = ("Edit", "Write", "NotebookEdit")
seen = set()
for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        e = json.loads(line)
    except Exception:
        continue
    if e.get("type") != "assistant":
        continue
    for c in (e.get("message") or {}).get("content") or []:
        if c.get("type") != "tool_use" or c.get("name") not in EDITS:
            continue
        inp = c.get("input") or {}
        fp = inp.get("file_path") or inp.get("notebook_path")
        if not fp:
            continue
        rp = os.path.realpath(fp)
        if rp != wiki and rp.startswith(wiki + os.sep):
            seen.add(os.path.relpath(rp, wiki))
for p in sorted(seen):
    print(p)
' "$wiki" 2>/dev/null
}

# wiki_commit_paths <write-paths> <dirty-paths> -> the subset of <write-paths> (newline-
# separated, from wiki_write_paths) that this session may commit: also present in
# <dirty-paths> (newline-separated, from a `git status` read taken AFTER the session — a
# path the session wrote and then reverted, or that another writer already committed, has
# nothing left to stage) and never wiki/tasks.md, the generated view regenerated by the
# brain session's own SessionStart hook and never authored by an aeon.
wiki_commit_paths() {
    local writes="${1:-}" dirty="${2:-}" wp out=""
    while IFS= read -r wp; do
        [ -n "$wp" ] || continue
        [ "$wp" = "wiki/tasks.md" ] && continue
        grep -qxF -- "$wp" <<< "$dirty" 2>/dev/null || continue
        out="${out:+$out$'\n'}$wp"
    done <<< "$writes"
    printf '%s' "$out"
}

# --------------------------------------------------------------------------------------
# trace_last <logfile> -> the last thing the session actually did, one line.
# --------------------------------------------------------------------------------------
trace_last() {
    local f="$1"
    [ -r "$f" ] || { printf ''; return 0; }
    # THE LAST ATTEMPT'S SEGMENT, not the file's tail. The log is appended to across
    # attempts, so a fresh attempt that has not yet written an event would otherwise report
    # the PREVIOUS session's last tool call as what this one is doing — and the heartbeat
    # grants a stalled aeon a reprieve on exactly that answer.
    attempt_trace "$f" 100000 2>/dev/null | python3 -c '
import sys, json
last = ""
for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"): continue
    try: e = json.loads(line)
    except Exception: continue
    if e.get("type") != "assistant": continue
    for c in (e.get("message", {}) or {}).get("content", []) or []:
        if c.get("type") == "tool_use":
            inp = c.get("input", {}) or {}
            last = "%s %s" % (c.get("name", "?"), str(inp.get("command") or inp.get("file_path") or "")[:200])
        elif c.get("type") == "text" and c.get("text", "").strip():
            last = c["text"].strip().replace("\n", " ")[:200]
# SAFE FOR A KEY=value FILE, AT THE SOURCE. This is arbitrary text from an agent — a shell
# command, a code fragment — and the cockpit snapshot is sourced by the pane. A newline in
# it injects extra lines and an "=" makes a bogus key; the pane rendered rustfmt help text
# where the ops summary belongs before this was clamped. An allowlist, not a blocklist:
# guessing which characters are dangerous is how the blocklist misses one.
import re
sys.stdout.write(re.sub(r"\s+", " ", re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", last)).strip()[:96])
' 2>/dev/null
}

# --------------------------------------------------------------------------------------
# trace_stats <logfile> -> KEY=value lines describing the session that is writing it:
#
#   TURNS  distinct assistant message ids
#   CTX    the LAST assistant usage: input + cache_creation + cache_read
#   TOOLS  tool_use blocks, total
#   FILES  distinct file_path across Edit/Write/NotebookEdit
#   QUIET  seconds since the trace last grew
#   ACT    the last thing it did, as trace_last reports it
#   SAID   the last non-empty assistant TEXT block
#
# WHY THESE AND NOT THE PROCESS TABLE. "Is an aeon healthy" was answerable only as "it holds
# a bead and here is the last command it ran", which says nothing about whether the session
# is making progress, near its context ceiling, or has quietly stopped. Every figure above
# comes from the one artifact that knows: the stream-json trace.
#
# THE WHOLE SEGMENT, NOT A TAIL. `attempt_trace $f 0` — a turn count taken from the last
# hundred kilobytes is not a turn count, it is a turn count minus however much was cut, and
# nothing on the pane would say which. The segment is bounded by the attempt, not by the
# file, so this stays proportional to the session being described rather than to every
# session that has ever worked the bead.
#
# ONE READ, ONE PASS, ONE FORK. The collector calls this once per aeon per pass and the pane
# only reads what it wrote; anything that walked the trace per figure would multiply the one
# genuinely unbounded read here by the number of figures.
#
# THREE STATES, NOT TWO. A trace that cannot be read renders `?`, a trace with no assistant
# event yet renders `-`, and a real reading renders a number. Collapsing the first two into
# 0 is the failure the whole panel is built against: a broken read that looks like an idle
# session displaces the suspicion that would have prompted a look
# (law-absence-needs-a-positive-control).
#
# APOSTROPHES ARE FORBIDDEN IN THE PYTHON BELOW. It lives inside python3 -c '...' — the same
# constraint as every other analyser here, and for the same reason: one would close the quote
# and leave the file syntactically invalid.
# --------------------------------------------------------------------------------------
trace_stats() {
    local f="${1:-}" m
    m="$(stat -c %Y "$f" 2>/dev/null)"
    if [ ! -r "$f" ] || [ -z "$m" ]; then
        printf 'TURNS=?\nCTX=?\nTOOLS=?\nFILES=?\nQUIET=?\nACT=?\nSAID=?\nMODEL=?\n'
        return 0
    fi
    # MTIME, NOT AN EVENT TIMESTAMP. stream-json events carry no wall clock of their own, and
    # the heartbeat in aeon.sh already treats trace growth as the liveness signal — so this is
    # the same measure the stall detector acts on rather than a second opinion about it.
    printf 'QUIET=%d\n' $(( $(date +%s) - m ))
    attempt_trace "$f" 0 2>/dev/null | python3 -c '
import sys, json, re

ALLOW = re.compile(r"[^ A-Za-z0-9._/:,()#+-]")
def clean(s, n=96):
    # SAFE FOR A KEY=value FILE, AT THE SOURCE, by the same allowlist trace_last uses. These
    # are arbitrary strings from an agent — a shell command, a code fragment, a sentence. A
    # newline injects extra lines into the snapshot and an "=" makes a bogus key; the pane
    # rendered a tools help page where the ops summary belongs before this was clamped.
    return re.sub(r"\s+", " ", ALLOW.sub(" ", s)).strip()[:n]

seen, ids, files = False, set(), set()
tools, ctx, act, said = 0, None, "", ""
# THE MODEL THE AEON WAS ACTUALLY SUMMONED WITH, read from its own trace rather than from
# the fayth file. Those two disagree exactly when it matters: a fayth edited while an aeon
# is mid-flight leaves the running session on the model it started with
# (law-long-lived-processes-pin-their-config), and a pane that read the file would relabel
# live work the moment the config changed, which is the one moment somebody is looking.
model = None
EDITS = ("Edit", "Write", "NotebookEdit")

for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        e = json.loads(line)
    except Exception:
        continue
    # The init event carries it once, at the top of the attempt. Both shapes are accepted
    # because the client has emitted it at the top level and under `message`, and a reader
    # that knew only one would report `-` for a model that is plainly there.
    if e.get("type") == "system" and e.get("subtype") == "init":
        model = e.get("model") or (e.get("message") or {}).get("model") or model
    if e.get("type") != "assistant":
        continue
    seen = True
    m = e.get("message") or {}
    # DEDUPED BY message.id, because --include-partial-messages writes one row per content
    # BLOCK and the rows of a single message share its id. Counting rows would report a turn
    # per block, which is several per turn and climbs with how chatty the turn was. The
    # blocks themselves are not duplicated across those rows, so tools and files are counted
    # from every row and only the turn count is a set.
    if m.get("id"):
        ids.add(m["id"])
    u = m.get("usage")
    if isinstance(u, dict):
        # THE CLIENTS OWN DEFINITION of total_input_tokens, so this and the status lines
        # ctx meter are the same measurement rather than two similar ones. LAST wins: a
        # context window is a level, not a total, and summing usages would report the sum of
        # every prompt ever sent as the size of the current one.
        ctx = ((u.get("input_tokens") or 0)
               + (u.get("cache_creation_input_tokens") or 0)
               + (u.get("cache_read_input_tokens") or 0))
    for c in m.get("content") or []:
        t = c.get("type")
        if t == "tool_use":
            tools += 1
            inp = c.get("input") or {}
            if c.get("name") in EDITS:
                fp = inp.get("file_path") or inp.get("notebook_path")
                if fp:
                    files.add(str(fp))
            act = "%s %s" % (c.get("name") or "?",
                             str(inp.get("command") or inp.get("file_path") or "")[:200])
        elif t == "text" and (c.get("text") or "").strip():
            said = c["text"].strip().replace("\n", " ")[:200]
            act = said

sys.stdout.write("MODEL=%s\n" % (clean(model, 32) if model else "-"))
if not seen:
    for k in ("TURNS", "CTX", "TOOLS", "FILES", "ACT", "SAID"):
        sys.stdout.write("%s=-\n" % k)
else:
    sys.stdout.write("TURNS=%d\n" % len(ids))
    sys.stdout.write("CTX=%s\n" % ("-" if ctx is None else ctx))
    sys.stdout.write("TOOLS=%d\n" % tools)
    sys.stdout.write("FILES=%d\n" % len(files))
    sys.stdout.write("ACT=%s\n" % (clean(act) or "-"))
    sys.stdout.write("SAID=%s\n" % (clean(said) or "-"))
' 2>/dev/null
}

# --------------------------------------------------------------------------------------
# aeon_fuse_minutes <bead-id> <worktree> <repo-name> -> minutes since the deliverable
# last moved, "gate" if a live gate is running, or "?" if the probe failed.
#
# THE DELIVERABLE MOVES when either a commit lands ahead of the base ref or a file is
# written in the worktree. Both are repository facts, not session facts — the session can
# grow its log indefinitely by reading the same two files and never move either of these.
# A probe that fails renders "?", never 0 (law-absence-needs-a-positive-control): zero
# reads as "just moved", the reassuring answer and the wrong one when the probe broke.
#
# THE FUSE DOES NOT BURN WHILE A GATE RUNS FOR THIS BEAD. A gate correctly writes
# nothing and commits nothing for many minutes; a fuse that burned there would train the
# operator to ignore it (law-alerts-must-be-actionable). A live gate is detected from
# /proc argv, not from a directory alone — a stale pid file must not mask real silence.
#
# SHARED BETWEEN cockpit.sh (display) AND aeon.sh (trip). Two implementations of "has
# the deliverable moved" is the two-lists defect applied to a probe; one implementation
# ensures the display and the trip always agree on when the fuse is burning. (sp-cuvi)
# --------------------------------------------------------------------------------------
aeon_fuse_minutes() {
    local bead="$1" wt="$2" repo_name="${3:-}"
    local _fuse="?" _fuse_gate=""
    for _gf in "$SPIRA_RUN/gate-run/"*"_${bead}/pid"; do
        [ -f "$_gf" ] || continue
        local _gp; _gp="$(cat "$_gf" 2>/dev/null)"
        [ -n "${_gp:-}" ] && [ -d "/proc/$_gp" ] || continue
        local _gc; { _gc="$(tr '\0' ' ' < "/proc/$_gp/cmdline")"; } 2>/dev/null
        [[ "${_gc:-}" == *gate* ]] && { _fuse_gate=1; break; }
    done
    if [ "${_fuse_gate:-}" = 1 ]; then
        _fuse=gate
    elif [ -d "$wt" ]; then
        local _last_t=0 _ct="" _base=""
        if [ -n "${repo_name:-}" ]; then
            _base="$(spira_landref "$repo_name" 2>/dev/null)" || _base=""
            if [ -n "${_base:-}" ]; then
                _ct="$(git -C "$wt" log --format='%ct' -1 "${_base}..HEAD" 2>/dev/null)" \
                    || _ct=""
                [ -n "${_ct:-}" ] && [ "${_ct:-0}" -gt "$_last_t" ] && _last_t="$_ct"
            fi
        fi
        local _mt
        _mt="$(find "$wt" -not -path '*/.git*' -printf '%T@\n' 2>/dev/null \
               | sort -rn | head -1 | cut -d. -f1)"
        [ -n "${_mt:-}" ] && [ "${_mt:-0}" -gt "$_last_t" ] && _last_t="$_mt"
        # A GATE THAT JUST FINISHED IS ALSO PROGRESS, not merely one that is still
        # running: the live-gate exemption above covers the gate's own runtime, but the
        # moment it exits, its rc/out/started mtimes are the newest fact the session has
        # to act on and must reset the fuse the same as a commit or a write would.
        for _gd in "$SPIRA_RUN/gate-run/"*"_${bead}"/; do
            [ -d "$_gd" ] || continue
            for _gf in "$_gd/rc" "$_gd/out" "$_gd/started"; do
                local _gt
                _gt="$(stat -c %Y "$_gf" 2>/dev/null)" || continue
                [ "${_gt:-0}" -gt "$_last_t" ] && _last_t="$_gt"
            done
        done
        if [ "${_last_t:-0}" -gt 0 ] 2>/dev/null; then
            _fuse=$(( ( $(date +%s) - _last_t ) / 60 ))
        fi
    fi
    printf '%s' "$_fuse"
}

# --------------------------------------------------------------------------------------
# aeon_lease_minutes <bead-id> -> minutes left in the liveness lease, or "?" if the
# deadline file cannot be read.
#
# THE FILE IS THE SINGLE SOURCE, shared between the killer (aeon.sh heartbeat) and this
# function (used by cockpit.sh). Both readers arrive at the same deadline because it comes
# from one write, not from a formula each would hold separately.
#
# NEVER 0 WHEN UNREADABLE. An absent or unreadable file is not "0 minutes remaining"; it
# is an absent reading. "?" makes the gap visible so the operator does not see an all-clear
# where there is no signal (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
aeon_lease_minutes() {
    local bead="${1:-}" lease_file deadline now
    [ -n "$bead" ] || { printf '?'; return 0; }
    lease_file="${SPIRA_RUN}/aeon/${bead}.lease"
    deadline="$(cat "$lease_file" 2>/dev/null)"
    if [ -z "${deadline:-}" ] || ! printf '%d' "$deadline" >/dev/null 2>&1; then
        printf '?'
        return 0
    fi
    now="${SPIRA_NOW:-$(date +%s)}"
    printf '%d' "$(( (deadline - now) / 60 ))"
}

# --------------------------------------------------------------------------------------
# trace_tail <logfile> [n] -> the last n human-readable moments of a session.
#
# The session log is stream-json now, which is the right format for a machine watching for
# progress and the wrong one to put in front of the operator: an escalation carrying 25 lines of
# raw JSON satisfies law-escalations-carry-their-evidence in letter and defeats it in
# substance. This renders the trace as what the agent SAID and DID.
# --------------------------------------------------------------------------------------
trace_tail() {
    local f="$1" n="${2:-25}"
    [ -r "$f" ] || { printf '(no session log)'; return 0; }
    attempt_trace "$f" 200000 2>/dev/null | python3 -c '
import sys, json
out = []
# A result event is a turn boundary, not a terminal state: the session continues
# immediately and resumes the same session_id. Counting them as turns lets the reader
# tell "turn 122 of N, still going" from "done" — which the old "session ended" label
# could not do and caused the operator to conclude the pane was stale when it was live.
turn_n = 0
for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    try: e = json.loads(line)
    except Exception: continue
    t = e.get("type")
    if t == "assistant":
        for c in (e.get("message", {}) or {}).get("content", []) or []:
            if c.get("type") == "text" and c.get("text", "").strip():
                out.append("  " + c["text"].strip().replace("\n", " ")[:200])
            elif c.get("type") == "tool_use":
                inp = c.get("input", {}) or {}
                arg = inp.get("command") or inp.get("file_path") or inp.get("pattern") or ""
                out.append("  $ %s %s" % (c.get("name", "?"), str(arg)[:150]))
    elif t == "user":
        for c in (e.get("message", {}) or {}).get("content", []) or []:
            if c.get("type") == "tool_result":
                body = c.get("content")
                if isinstance(body, list):
                    body = " ".join(x.get("text", "") for x in body if isinstance(x, dict))
                body = str(body or "").strip().replace("\n", " ")
                if body:
                    out.append("    -> " + body[:160])
    elif t == "result":
        turn_n += 1
        out.append("  [turn %d: %s]" % (turn_n, e.get("subtype", "?")))
sys.stdout.write("\n".join(out[-int(sys.argv[1]):]) if out else "(trace had no readable events)")
' "$n" 2>/dev/null || printf '(could not render the trace)'
}

bead_context() {         # bead_context <id> -> a human-readable block
    local id="$1"
    [ -n "$id" ] && [ "$id" != "-" ] || { printf '(no single bead — this is about the plan as a whole)'; return 0; }
    bdjson show "$id" 2>/dev/null | python3 -c '
import sys, json, datetime
try:
    d = json.load(sys.stdin)
    i = (d if isinstance(d, list) else [d])[0]
except Exception:
    print("(could not read the bead — say so rather than pretend)"); raise SystemExit
def age(ts):
    try:
        t = datetime.datetime.fromisoformat(str(ts).replace("Z", "+00:00"))
        h = (datetime.datetime.now(datetime.timezone.utc) - t).total_seconds() / 3600
        return "%dh" % h if h < 48 else "%dd" % (h / 24)
    except Exception:
        return "?"
print("BEAD    %s  [%s, P%s, open %s]" % (i.get("id"), i.get("status"), i.get("priority"), age(i.get("created_at"))))
print("TITLE   %s" % (i.get("title") or "(none)"))
labs = ", ".join(i.get("labels") or []) or "(none)"
print("LABELS  %s" % labs)
print("")
print("WHAT THIS BEAD IS FOR")
print((i.get("description") or "(no description — that is itself the problem)").strip())
# `notes` is a STRING on these beads, not a list — iterating it yielded one character
# per "note" and printed "- c", "- h". Normalise before slicing anything.
notes = i.get("notes")
if isinstance(notes, str):
    notes = [n for n in notes.split("\n") if n.strip()]
elif isinstance(notes, list):
    notes = [(n.get("text") if isinstance(n, dict) else str(n)) for n in notes]
else:
    notes = []
if notes:
    print("")
    print("MOST RECENT NOTES")
    for n in notes[-3:]:
        print("  - %s" % str(n).strip()[:400])
' 2>/dev/null || printf '(could not read %s)' "$id"
}

# land_subject <id> -> "spira: land <id>", or "spira: land <id> — <title>" when the bead
# has a title. Every writer of a landing merge (verdict.sh, landing.sh, queue.sh,
# batcher-cut's Rust seam) calls this, so every reader that widens its own match to a
# trailing title (landed()/landed_sha() below, CHECK5 in sentinel.sh, cockpit.sh,
# overrides.sh) stays in sync with what is actually written.
land_subject() {
    local id="$1" _t
    _t="$(bdjson show "$id" 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
    t = str(d[0].get("title", "")) if d else ""
except Exception:
    t = ""
print(" ".join(t.split())[:120])
' 2>/dev/null)"
    if [ -n "${_t:-}" ]; then
        printf 'spira: land %s — %s' "$id" "$_t"
    else
        printf 'spira: land %s' "$id"
    fi
}

# landed <id> <repo> -> 0 landed, 1 not landed, 2 CANNOT TELL.
#
# THREE OUTCOMES, NOT TWO. A caller that reads "cannot tell" as "not landed" reopens finished
# work, and the state where the answer is unavailable — a repository whose land ref does not
# resolve — is exactly the state this whole change is about. 2 is distinct so it cannot be
# mistaken for a verdict.
#
# THE BASE IS NOT ALWAYS `main`. Some repositories are `master`, and this hardcoded
# main — so sp-pd-ci-green's work merged to master, its PR closed, all four polecat PRs
# closed, and this still reported "no commit on main names it" and reopened the bead four
# times. It reached attempt 4 against a poison threshold of 3: the harness was one pass from
# escalating a finished, merged deliverable as a failure.
landed() {
    local id="$1" repo="${2:-$(repo_root)}" refs _landed_subj
    # THE REPOSITORY'S OWN LAND REF, not `main`, and its local counterpart alongside it. The
    # sentinel lands by pushing from the .landing worktree straight to the remote, and
    # nothing in the harness ever pulls the shared checkout, so the local ref there is
    # however stale the last human left it. That was survivable only while a landed branch
    # was never deleted and CHECK 5 could fall back to "the work exists on spira/<id>"; the
    # reaper removes that branch, so this ref list is now the only thing standing between a
    # landed bead and being reopened. spira_landrefs keeps only refs that resolve, so a
    # repository with no local copy of its base still works.
    refs="$(spira_landrefs "$repo")" || return 2
    # A LANDING RECORD, NOT A MENTION. --grep over the full message treated any commit that
    # named the id ANYWHERE — a dependency list, a "Fixes: <id> (analysis)" cross-reference, a
    # "Filed <id>" note in an unrelated bead's own commit — as proof that id had landed. Five
    # certified branches were reaped and their landstate written LANDED on exactly this: a
    # commit that talked about the bead, not one that landed it (sp-dgaig). --grep is still
    # used to narrow full history to candidates cheaply; only the SUBJECT of each candidate is
    # then trusted, and only two shapes count: the queue's own merge subject
    # ("spira: land <id>", optionally " — <title>", written by land_subject() and produced
    # by batch.sh/verdict.sh/landing.sh), or an aeon's own commit for its own bead
    # ("<id>: ..." — never a substring, the colon must follow immediately).
    # shellcheck disable=SC2086
    while IFS= read -r _landed_subj; do
        case "$_landed_subj" in
            "spira: land $id" | "spira: land $id "*) return 0 ;;
            "$id":*) return 0 ;;
        esac
    done < <(git -C "$repo" log --format='%s' --grep="$id" -F $refs 2>/dev/null)
    return 1
}

# landed_sha <id> <repo> -> the sha of the commit landed() would say yes about, so a
# caller that needs to CITE the landing (a GitHub comment) gets the same commit the
# ancestry check trusted, never a second guess at which one that was.
landed_sha() {
    local id="$1" repo="${2:-$(repo_root)}" refs _sha _subj
    refs="$(spira_landrefs "$repo")" || return 2
    # shellcheck disable=SC2086
    while IFS=$'\t' read -r _sha _subj; do
        case "$_subj" in
            "spira: land $id" | "spira: land $id "*) printf '%s' "$_sha"; return 0 ;;
            "$id":*) printf '%s' "$_sha"; return 0 ;;
        esac
    done < <(git -C "$repo" log --format='%H%x09%s' --grep="$id" -F $refs 2>/dev/null)
    return 1
}

# content_landed <repo> <branch> <baseref> -> 0 if <baseref> already contains every change
# <branch> makes, 1 if it does not.
#
# ANCESTRY IS NOT THE ONLY WAY WORK LANDS, AND ON A SQUASHING REPOSITORY IT IS NEVER THE WAY.
# A squash merge replays the branch's whole diff as ONE NEW COMMIT with a new SHA and a
# parentage the branch does not appear in, so the branch's own commits are not ancestors of
# the base and never will be. Every SHA-based test therefore answers "not landed" about work
# that is demonstrably on the base — and then the rebase that follows CONFLICTS, precisely
# because the base already holds those changes. The harness read that pair as a branch in
# trouble and reopened a finished bead with "does not rebase onto <base>", which was true and
# meant the opposite of what it was taken to mean. It repeats forever, because nothing about
# the situation changes between passes.
#
# So ask the question that actually matters: would merging this branch into the base change
# anything? `merge-tree --write-tree` performs the three-way merge in memory and prints the
# resulting tree; when that tree IS the base's own tree, the merge is a no-op and the content
# is already there. This is deliberately not the rebase's question — a rebase replays commit
# by commit and can conflict on an intermediate patch whose end state is fine, which is
# exactly the false alarm.
#
# It answers NO when the merge conflicts (non-zero exit) and NO when the merged tree differs,
# both of which mean the branch really does carry something the base lacks. That is what makes
# it safe for a caller that DELETES on the answer: it cannot say "landed" about a branch with
# work outstanding. `landed()` above is a different question — whether a commit on the base
# names the BEAD — and is not a substitute here, because a branch may carry commits beyond the
# one that landed.
#
# NO PIPE. `git ... | head -1` under pipefail returns 141 when head closes the pipe first, so
# the check would fail exactly when merge-tree succeeded (law-no-grep-q-under-pipefail).
# Capture whole, then trim.
content_landed() {
    local repo="$1" br="$2" base="$3" merged basetree ahead
    ahead="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null)" || return 1
    # ANCESTOR BRANCHES ARE LANDED: their every commit is already reachable from base,
    # so merging changes nothing. Check before the ahead=0 guard so a superseded branch
    # that was fast-forwarded into the successor's history returns 0 here rather than
    # falling through to the Sending's KEEP path (law-absence-needs-a-positive-control).
    git -C "$repo" merge-base --is-ancestor "$br" "$base" 2>/dev/null && return 0
    [ "${ahead:-0}" -gt 0 ] 2>/dev/null || return 1
    merged="$(git -C "$repo" merge-tree --write-tree "$base" "$br" 2>/dev/null)" || return 1
    merged="${merged%%$'\n'*}"
    [ -n "$merged" ] || return 1
    basetree="$(git -C "$repo" rev-parse "$base^{tree}" 2>/dev/null)" || return 1
    [ "$merged" = "$basetree" ]
}

# bead_cited_commit_on_base <id> <repo> <base> → prints "<sha> <rule>" where rule is
# cited-declared or cited-named.  Returns 0 if found, 1 if not.
#
# Accepts a sha only when the note uses an explicit hand-landed phrase
# ("landed as <sha>" or "hand-landed <sha>") → cited-declared, or when the commit
# message at that sha names the bead id → cited-named.  A bare sha in prose is never
# sufficient (law-closed-is-not-landed).
bead_cited_commit_on_base() {
    local id="$1" repo="$2" base="$3" kind sha _lines
    _lines="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys, json, re
try:
    d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
    notes = d[0].get("notes") if d else None
    if isinstance(notes, str): notes = [n for n in notes.split("\n") if n.strip()]
    elif isinstance(notes, list): notes = [(n.get("text") if isinstance(n, dict) else str(n)) for n in notes]
    else: notes = []
    declared = re.compile(r"(?:landed\s+as|hand-landed)\s+([0-9a-f]{7,40})", re.IGNORECASE)
    sha_pat = re.compile(r"[0-9a-f]{7,40}")
    seen = set()
    for n in notes:
        for m in declared.finditer(str(n).lower()):
            s = m.group(1)
            if s not in seen:
                seen.add(s); print("declared " + s)
    for n in notes:
        for s in sha_pat.findall(str(n).lower()):
            if s not in seen:
                seen.add(s); print("bare " + s)
except Exception:
    pass
' 2>/dev/null)" || return 1
    [ -n "$_lines" ] || return 1
    while IFS=' ' read -r kind sha; do
        [ -n "$sha" ] || continue
        git -C "$repo" rev-parse -q --verify "${sha}^{commit}" >/dev/null 2>&1 || continue
        git -C "$repo" merge-base --is-ancestor "$sha" "$base" 2>/dev/null || continue
        if [ "$kind" = "declared" ]; then
            printf '%s cited-declared\n' "$sha"; return 0
        else
            git -C "$repo" log -1 --format=%B "${sha}^{commit}" 2>/dev/null \
                | grep -qE "(^|[^a-z0-9-])${id}([^a-z0-9-]|$)" \
                && { printf '%s cited-named\n' "$sha"; return 0; }
        fi
    done <<< "$_lines"
    return 1
}

# pr_merged <repo> <branch> -> 0 if a pull request whose head is <branch> is MERGED.
#
# The second reading of "already landed", and the one that survives what content_landed
# cannot: a squash that merged and was then amended on the base. The content differs, so the
# merge test says no, and re-landing the branch would revert whoever amended it.
#
# THIS IS EVIDENCE FOR NOT REOPENING, NEVER EVIDENCE FOR DELETING. A merged pull request says
# the work was accepted; it does not say the ref holds nothing else. A caller about to destroy
# a branch must use content_landed, which is exact and local. This one reaches the network, so
# it belongs behind a cheap check that has already failed — never on the common path.
pr_merged() {
    local repo="$1" br="$2" state
    state="$( cd "$repo" 2>/dev/null && ghq pr view "$br" --json state -q .state 2>/dev/null )" || return 1
    [ "$state" = "MERGED" ]
}

# other_beads_on_conflicts <repo> <branch> <base> <conflicted-files> -> space-separated
# bead ids whose commits touched the conflicted files on the base since this branch
# diverged, excluding the branch's own bead id.
#
# A rebase conflict whose files were changed on the base by a commit naming a DIFFERENT
# bead is the shape a parallel duplicate always takes: two agents implement the same fix
# in different words, one lands, and the other's rebase stops on exactly the files the
# first one changed. The note "resolve the conflict" is misleading in that case — the
# correct resolution may be to drop the branch rather than replay it. This function does
# not decide; it names the evidence so the next aeon can judge.
#
# NO PIPE INTO GREP. `git log | grep` under pipefail returns 141 on a match when grep
# closes the pipe first (law-no-grep-q-under-pipefail). Capture whole, then scan.
other_beads_on_conflicts() {
    local repo="$1" br="$2" base="$3" files="$4" own_id mb subjects ids
    [ -n "$files" ] || return 0
    own_id="${br#spira/}"
    mb="$(git -C "$repo" merge-base "$base" "refs/heads/$br" 2>/dev/null)" || return 0
    # shellcheck disable=SC2086
    subjects="$(git -C "$repo" log --format='%s' "$mb..$base" -- $files 2>/dev/null)" || return 0
    [ -n "$subjects" ] || return 0
    ids="$(grep -oE "${own_id%%-*}-[a-z0-9]+" <<< "$subjects" | sort -u)" || return 0
    ids="$(grep -vxF "$own_id" <<< "$ids")" || return 0
    printf '%s' "$ids" | tr '\n' ' ' | sed 's/ $//'
}

conflict_reopen_note() {
    local repo="$1" br="$2" base="$3" name="$4" conflicts="$5" actor="$6" rq_n="${7:-1}"
    local rn other_beads note base_display
    base_display="${base#refs/remotes/}"
    rn="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo '?')"
    other_beads="$(other_beads_on_conflicts "$repo" "$br" "$base" "$conflicts")"
    note="Reopened by $actor: $br does not rebase onto $base_display in $name; conflicts in ${conflicts:-unknown}. This is rebase-conflict attempt $rq_n on this bead. The branch carries $rn commit(s) from the previous session — resume from the existing work."
    if [ -n "$other_beads" ]; then
        note="$note Those files were changed on $base_display by $other_beads — check whether this work is already landed before resolving."
    else
        note="$note A merge conflict is not an escalation — the next aeon is handed the rebase and must resolve it."
    fi
    printf '%s' "$note"
}

# --------------------------------------------------------------------------------------
# Memory delivery. An aeon has no SessionStart hook, so this is how it reads the law — and
# now also how Ops reads its runbooks, since a statute and an SOP are the same mechanism
# under two prefixes (law- and sop-).
#
# TWO DEFECTS THIS REPLACES. `bd memories` is a LISTING: it truncates every body at ~110
# characters with an ellipsis, so every aeon so far has been reading half-sentences —
# "check YOUR OWN LAST ACTION befo..." teaches nothing, and a statute nobody can finish
# reading is not in force. And the caller then cut the listing with `head -400`, which
# silently drops whichever statutes sort last. Read the JSON, print it whole, and when a
# budget really is exceeded say so in the output rather than trimming in the dark.
#
# The prefix filter is what keeps the two books apart. Without it every builder aeon pays
# for every runbook it will never execute, and the runbooks push the law off the end.
#
# TIERED RENDERING (sp-4e69e). Two tiers, one chokepoint:
#   Core tier   — statutes named in SPIRA_STATUTE_CORE render in full (## slug + paragraph).
#                 The char-budget applies to this tier only. A core statute that would
#                 exceed the budget falls back to an index line rather than vanishing.
#   Index tier  — every other statute in force renders as one slug line under a heading
#                 that states it binds equally, with a command to read the full text.
# Nothing is hidden: slug count == statute count. Every statute is named in every session.
# --------------------------------------------------------------------------------------
# THE CORE SET IS A THIRD ARGUMENT, because WHICH statutes a session needs in full is a
# property of the ROLE, not of the box. The shipped default is builder-shaped — testing,
# gates, landing, guards — and it is the right default for four of the six personas. It is
# the wrong one for an operator session, whose whole job is deciding what to escalate and
# what to decompose, and the gap was not theoretical: the statute that the harness checkout
# is production sat OUTSIDE the core set while the operator's own session violated it for
# thirty-nine turns (sp-u4x). Delivered, it would still have arrived as a slug in an index.
#
# A fayth declares its own with FAYTH_STATUTE_CORE; unset, it gets $SPIRA_STATUTE_CORE, which
# is what every persona got before this parameter existed.
render_memories() {      # render_memories <prefix-csv> [char-budget] [core-csv]
    local prefixes="${1:-law-}" budget="${2:-120000}"
    local core_csv="${3:-${SPIRA_STATUTE_CORE:-}}" harness="${SPIRA_REPO:-<harness>}"
    local cache="${SPIRA_MEMORIES_CACHE:-}" age="${SPIRA_MEMORIES_CACHE_AGE:-300}"
    local mem_json="" now mtime
    if [ -n "$cache" ] && [ -f "$cache" ]; then
        now="$(date +%s)"
        mtime="$(stat -c %Y "$cache" 2>/dev/null || printf 0)"
        [ "$(( now - mtime ))" -lt "$age" ] && mem_json="$(cat "$cache" 2>/dev/null)"
    fi
    # SPIRA_MEMORIES_CMD IS THE SEAM. The cache file above already lets a suite drive the
    # tiering/budget/index logic from a fixture instead of a live bd read; this is the same
    # idea for the read that FILLS the cache. Unset, it queries bd for real — every caller in
    # production leaves it unset.
    if [ -z "$mem_json" ]; then
        if [ -n "${SPIRA_MEMORIES_CMD:-}" ]; then
            mem_json="$(bash -c "$SPIRA_MEMORIES_CMD" 2>/dev/null)"
        else
            mem_json="$(bdjson memories 2>/dev/null)"
        fi
        [ -n "$cache" ] && [ -n "$mem_json" ] \
            && { mkdir -p "$(dirname "$cache")" 2>/dev/null; printf '%s\n' "$mem_json" > "$cache" 2>/dev/null || true; }
    fi
    printf '%s\n' "$mem_json" | python3 -c '
import sys, json, os
prefixes = [p for p in sys.argv[1].split(",") if p]
budget   = int(sys.argv[2])
core_set = {s.strip() for s in sys.argv[3].split(",") if s.strip()}
harness  = sys.argv[4]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
mem = {k: v.strip() for k, v in sorted(d.items())
       if isinstance(v, str) and any(k.startswith(p) for p in prefixes)}

core_out, used, core_fallback = [], 0, []
index_slugs = []

for k, v in mem.items():
    if k in core_set:
        block = f"## {k}\n\n{v}\n"
        if used + len(block) > budget:
            core_fallback.append(k)
        else:
            core_out.append(block)
            used += len(block)
    else:
        index_slugs.append(k)

# Core fallback slugs join the index tier rather than disappearing.
index_slugs = sorted(core_fallback + index_slugs)

parts = []
if core_out:
    parts.append("\n".join(core_out))

if index_slugs:
    # Two namespaces share this tier, fetched by two different tools — one header
    # naming one command left the other namespace unfetchable by it.
    NAMESPACES = {
        "law-": (
            "## Statutes in force — full text on request\n\n"
            "These are law and bind you exactly as the text above does. The slug states\n"
            "the rule; read the reasoning and the scar behind any of them with:\n\n"
            f"    {harness}/rule.sh show <slug-without-law-prefix>\n"
        ),
        "sop-": (
            "## Runbooks on the shelf — full text on request\n\n"
            "These bind exactly as the statutes above do. Read the full runbook —\n"
            "CHECK, FIX, ESCALATE — with:\n\n"
            "    sop show <slug-without-sop-prefix>\n"
        ),
    }
    for ns, header in NAMESPACES.items():
        group = sorted(k for k in index_slugs if k.startswith(ns))
        if group:
            parts.append(header + "\n".join(group))

print("\n\n".join(parts))
' "$prefixes" "$budget" "$core_csv" "$harness" 2>/dev/null
}

# Split a rendered persona prompt on <!-- task --> and write system.md / task.md.
# system_prompt_split <sysfile> <taskfile> <statutes_text> <rendered_prompt>
# Sets SPIRA_SYSTEM_FLAG to the appropriate --*-system-prompt-file value.
# Reads FAYTH_SYSTEM_PROMPT (replace|append, default append): replace uses
# --system-prompt-file; append (or unset) uses --append-system-prompt-file so
# Claude Code's coding guidance stays underneath the persona layer.
system_prompt_split() {
    local sysfile="$1" taskfile="$2" statutes="$3" prompt="$4"
    local sys task
    if [[ "$prompt" == *'<!-- task -->'* ]]; then
        sys="${prompt%%<!-- task -->*}"
        task="${prompt#*<!-- task -->}"
        task="${task#$'\n'}"
    else
        sys=""; task="$prompt"
    fi
    printf '# Memories in force\n\n%s\n\n---\n\n%s' "$statutes" "$sys" > "$sysfile"
    printf '%s' "$task" > "$taskfile"
    case "${FAYTH_SYSTEM_PROMPT:-append}" in
        replace) SPIRA_SYSTEM_FLAG="--system-prompt-file" ;;
        *)       SPIRA_SYSTEM_FLAG="--append-system-prompt-file" ;;
    esac
}




# groom_claims_verified <new-log-lines> <ask-json> <epoch> -> "" (nothing claimed, or every
# claim verified) | comma-space-separated ids claimed but unproven
#
# A groom pass that writes ESCALATED, inquiry or flagged for bead X in its log without a
# matching ask bead created in the same session (type=decision, SPIRA_ASK_LABEL, created at
# or after <epoch>, with X in its title or description) has not escalated — it has claimed
# to. <new-log-lines> is the window: lines this session itself wrote (the caller's own
# GROOM_LOG_LINES_BEFORE slice), never the whole log.
groom_claims_verified() {
    local log="${1:-}" ask_json="${2:-[]}" epoch="${3:-0}"
    [ -n "$log" ] || return 0
    # log AND ask_json ON STDIN / A TEMP FILE, NEVER ARGV (law-payloads-go-on-stdin): a
    # session's log slice and the open-ask set are each unbounded by anything this function
    # controls.
    local _ajf; _ajf="$(mktemp)" || return 1
    printf '%s' "$ask_json" > "$_ajf"
    printf '%s' "$log" | ASK_FILE="$_ajf" python3 -c '
import sys, json, re, datetime, os
log = sys.stdin.read()
with open(os.environ["ASK_FILE"]) as f:
    ask_json = f.read()
epoch = int(sys.argv[1])
ids = []
seen = set()
for line in log.splitlines():
    if re.search(r"ESCALATED|inquiry|flagged", line, re.IGNORECASE):
        for m in re.findall(r"sp-[a-z0-9-]+", line):
            if m not in seen:
                seen.add(m); ids.append(m)
if not ids:
    sys.exit(0)
try:
    d = json.loads(ask_json)
except Exception:
    d = []
if not isinstance(d, list):
    d = [d]
found = set()
for i in d:
    ca = i.get("created_at") or ""
    try:
        dt = datetime.datetime.fromisoformat(ca.replace("Z", "+00:00"))
        if int(dt.timestamp()) < epoch:
            continue
    except Exception:
        continue
    title = i.get("title") or ""
    desc = i.get("description") or ""
    for cid in ids:
        if cid in title or cid in desc:
            found.add(cid)
unproven = [i for i in ids if i not in found]
print(", ".join(unproven))
' "$epoch" 2>/dev/null
    local _rc=$?
    rm -f "$_ajf"
    return $_rc
}






# bead_named_paths <text> <repo> -> path tokens in <text> that exist as tracked files in
# <repo>, one per line, deduplicated. A bead usually names the files it is about in prose —
# matching a bare regex against the tracked tree, rather than trusting the regex alone, is
# what keeps a bead id or a stray URL from being read as a path.
bead_named_paths() {
    local text="$1" repo="$2" tracked tok clean
    [ -e "$repo/.git" ] || return 0
    tracked="$(git -C "$repo" ls-files 2>/dev/null)" || return 0
    printf '%s\n' "$text" | grep -oE '[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)+' 2>/dev/null \
        | while IFS= read -r tok; do
        clean="$tok"
        while :; do
            case "$clean" in
                [\(\`]*) clean="${clean#?}" ;;
                *) break ;;
            esac
        done
        while :; do
            case "$clean" in
                *[.,\;:\)\`]) clean="${clean%?}" ;;
                *) break ;;
            esac
        done
        [ -n "$clean" ] && printf '%s\n' "$clean"
    done | sort -u | while IFS= read -r clean; do
        grep -qxF "$clean" <<<"$tracked" && printf '%s\n' "$clean"
    done
}



# all_partition_members -> every open/in_progress bead a partition's own labels would
# match, one id per line, deduped — EVERY EXCLUSION DROPPED (moved out of spira/attempts.sh,
# sp-rfodk; its own header comment, verbatim: "a poisoned bead is excluded from dispatch and
# is exactly the bead whose count most needs reading, so filtering it out here would hide
# the evidence"). The claimable-set predicate (fayth_ready/bdq) answers a different
# question — what CAN be claimed right now — and applies exclusions to answer it; this
# answers "whose is it", which a poisoned or asked-about bead still is. groomer.sh's
# `deadlocked` and any future census over the full board read this, not the claimable set.
all_partition_members() {
    local labels exclude
    while IFS=$'\t' read -r labels exclude; do
        [ -n "$labels" ] || continue
        bdjson list --status open,in_progress --limit 0 --label "$labels" 2>/dev/null \
            | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]): print(i["id"])' 2>/dev/null
    done < <(fayth_partitions) | awk 'NF && !seen[$0]++'
    return 0
}

# detect_unclaimable_ready -> one UNCLAIMABLE line per ready bead no persona can claim.
#
# THE ALARM THIS CHECK FIRES ON IS DISTINCT FROM AN IDLE QUEUE. "nothing ready" and "a
# ready bead nobody can claim" look identical to CHECK 7: every partition reports 0, a
# genuinely empty queue reports 0, and the pass ends with the same log line either way.
# This check reads the raw ready set — no partition filter — and tests each bead against
# the full chamber. The empty-queue case finds no beads; the unclaimable case finds them.
#
# THE ARITHMETIC MIRRORS bead.sh's claimers(). Not called from there because bead.sh lives
# in the brain repo and this runs in the harness; porting keeps the harness self-contained.
# Both derive from the same chamber files, so they agree by construction.
#
# EXCLUSION SET IS THE PERSONA'S OWN FAYTH_EXCLUDE_LABELS ONLY. The `fayth:<other-persona>`
# terms that fayth_exclude() appends to each `bd ready --exclude-label` call are already
# handled here by the preference check: when a bead carries `fayth:ops`, pref={ops} and
# only ops is tested — no other persona enters the loop at all. Duplicating fayth: terms
# into the exclusion set would be correct but redundant.
#
# OUTPUT NAMES THE BEAD, ITS PREFERENCE AND THE REJECTION REASON so the fix is one label.
# Format: UNCLAIMABLE <id> — <reason>
detect_unclaimable_ready() {
    # Config must come from the production checkout — not from a worktree whose
    # conf.sh carries a different SPIRA_PLAN_LABEL or other partition label.
    # _spira_gitstore returns the shared .git dir; its parent is the main worktree.
    # If that differs from our SPIRA_REPO, re-run via the main checkout with the
    # label vars unset so the production conf.sh defaults take effect.
    local _duc_gcd _duc_prod_root
    _duc_gcd="$(_spira_gitstore "$SPIRA_HOME")" || _duc_gcd=""
    if [ -n "$_duc_gcd" ]; then
        _duc_prod_root="$(cd "$_duc_gcd/.." 2>/dev/null && pwd -P)" || _duc_prod_root=""
        if [ -n "$_duc_prod_root" ] && [ "$_duc_prod_root" != "$SPIRA_REPO" ]; then
            local _duc_prod_home="$_duc_prod_root/${SPIRA_HOME#$SPIRA_REPO/}"
            if [ -f "$_duc_prod_home/lib.sh" ]; then
                env -u SPIRA_PLAN_LABEL -u SPIRA_INCIDENT_LABEL \
                    -u SPIRA_SCOPE_LABEL -u SPIRA_CI_LABEL \
                    -u SPIRA_ASK_LABEL -u SPIRA_NO_LOOP_LABEL \
                    -u SPIRA_CZAR_LABEL -u SPIRA_GROOMER_LABEL \
                    -u SPIRA_GROOM_ASK_LABEL \
                    -u SPIRA_MAECHEN_LABEL -u SPIRA_SPIKE_LABEL \
                    -u SPIRA_READY_SNAPSHOT -u SPIRA_LIST_SNAPSHOT -u SPIRA_READY_CACHE \
                    SPIRA_HOME="$_duc_prod_home" \
                    bash -c ". \"$_duc_prod_home/lib.sh\"; detect_unclaimable_ready"
                return $?
            fi
        fi
    fi

    local parts="" all_parts="" f inc exc
    for f in $(spira_fayths); do
        inc="$(fayth_get "$f" FAYTH_LABELS)"
        exc="$(fayth_get "$f" FAYTH_EXCLUDE_LABELS)"
        [ -n "$inc" ] && parts="${parts}${f}|${inc}|${exc}"$'\n'
    done
    [ -n "$parts" ] || return 0

    # ALL_PARTS: the full chamber, including fayths the active roster omits. Used to
    # distinguish a parked partition (bead claimable by a chamber fayth not in SPIRA_FAYTHS)
    # from a real mislabelling (bead claimable by nobody, full chamber included).
    for f in $(fayth_names); do
        inc="$(fayth_get "$f" FAYTH_LABELS)"
        exc="$(fayth_get "$f" FAYTH_EXCLUDE_LABELS)"
        [ -n "$inc" ] && all_parts="${all_parts}${f}|${inc}|${exc}"$'\n'
    done

    # SPIRA_READY_SNAPSHOT is exactly this query (ready_raw_args), fetched once for the whole
    # pass (sp-bo67y) — read it instead of asking bd again when the caller has one ready.
    if [ -n "${SPIRA_READY_SNAPSHOT:-}" ] && [ -r "$SPIRA_READY_SNAPSHOT" ]; then
        cat "$SPIRA_READY_SNAPSHOT"
    else
        local _det_args; mapfile -t _det_args < <(ready_raw_args)
        bdjson "${_det_args[@]}" 2>/dev/null
    fi \
    | PARTS="$parts" ALL_PARTS="$all_parts" unclaimable.py 2>/dev/null
}

# file_unclaimable_incidents — for each UNCLAIMABLE line in detect_unclaimable_ready output,
# file a P1 incident in the Groomer partition so the Groomer can claim and fix the label.
#
# THE GROOMER IS THE TERMINUS. Only the Groomer can discharge an UNCLAIMABLE finding: it can
# add the scope label, correct the fayth:, or close the row. Ops cannot do any of these and
# must not be the sole recipient. The incident is filed with SPIRA_GROOMER_LABEL.
#
# THE CALL IS IDEMPOTENT. incident.sh dedupes on the unclaimable:<id> ref, so a
# bead that is still unclaimable on the next sentinel pass bumps the recurrence counter
# rather than filing a duplicate.
#
# SPIRA_INCIDENT_SH overrides incident.sh (found by name on PATH). Test suites inject a
# mock here; production uses the default.
file_unclaimable_incidents() {   # file_unclaimable_incidents <detect_unclaimable_ready output>
    local line bid reason inc
    inc="${SPIRA_INCIDENT_SH:-incident.sh}"
    while IFS= read -r line; do
        case "$line" in UNCLAIMABLE\ *) ;; *) continue ;; esac
        bid="${line#UNCLAIMABLE }"; bid="${bid%% —*}"
        reason="${line#*— }"
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=1 \
        SPIRA_INCIDENT_ACTOR=sentinel \
        SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:-spira},${SPIRA_GROOMER_LABEL:-groom}" \
        SPIRA_INCIDENT_REPO="${SPIRA_HOME_REPO:-spira}" \
        SPIRA_INCIDENT_REF="unclaimable:$bid" \
        SPIRA_INCIDENT_CAUSE=unclaimable \
        bash "$inc" file "UNCLAIMABLE: $bid — fix the fayth: or partition label" \
            - <<< "$reason" >/dev/null 2>&1 || true
    done <<< "$1"
}

# detect_branch_collisions -> "COLLISION <id> <repo> <branch> <holder-id> <holder-path>" for
# every open bead whose recorded branch is checked out in a DIFFERENT bead's canonical
# worktree ($SPIRA_RUN/worktree/<id>). The branch label is read from the `bd list` JSON
# already fetched above, not with a per-bead `bd state` call (sp-nsxhd) — bd stores state as
# a `branch:<value>` label (`bd set-state`), so it is already sitting in `labels`.
#
# aeon.sh's worktree-attach guard (law-one-aeon-one-worktree) reacts correctly once a bead is
# claimed — it self-corrects a mislabeled child onto a fresh branch of its own, or dies naming
# the true holder — but reacting is not preventing: nothing in the store changes between
# failed claims, so dispatch re-derives the identical collision every cycle
# (law-a-retry-must-change-an-input). A bead whose own DEFAULT branch is the one squatted (a
# parent shadowed by a child that inherited its name before groomer.sh stopped copying it) can
# never self-correct at all, because its own default IS the squatted name. Read here, before
# a claim is spent, rather than at aeon.sh's refusal.
#
# Already-parked beads (carrying SPIRA_ASK_LABEL) are excluded so a repeat sentinel pass
# stays silent once a bead has been escalated — the point is ONE escalation, not one per pass.
detect_branch_collisions() {
    local _bc_raw _bc_ids
    _bc_raw="$(bdjson list --status open --limit 0 --exclude-type epic,event 2>/dev/null)"
    [ -n "$_bc_raw" ] || return 0
    _bc_ids="$(printf '%s' "$_bc_raw" | SPIRA_ASK_LABEL="${SPIRA_ASK_LABEL:-}" python3 -c '
import json, os, sys
ask = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
try: d = json.load(sys.stdin)
except Exception: d = []
for i in (d if isinstance(d, list) else [d]):
    labels = i.get("labels") or []
    if ask in labels:
        continue
    repo = next((l[5:] for l in labels if l.startswith("repo:")), "")
    branch = next((l[7:] for l in labels if l.startswith("branch:")), "")
    print("%s\t%s\t%s" % (i["id"], repo, branch))
' 2>/dev/null)"
    [ -n "$_bc_ids" ] || return 0

    local -A _bc_maps
    local _bc_id _bc_repo _bc_branch _bc_root _bc_br _bc_holder _bc_holder_id
    while IFS=$'\t' read -r _bc_id _bc_repo _bc_branch; do
        [ -n "$_bc_id" ] || continue
        _bc_repo="${_bc_repo:-$(spira_home_repo)}"
        if [ -z "${_bc_maps[$_bc_repo]+x}" ]; then
            _bc_root="$(repo_root "$_bc_repo" 2>/dev/null)"
            if [ -n "$_bc_root" ] && { [ -d "$_bc_root/.git" ] || [ -f "$_bc_root/.git" ]; }; then
                _bc_maps[$_bc_repo]="$(git -C "$_bc_root" worktree list --porcelain 2>/dev/null \
                    | awk '/^worktree /{w=$2} /^branch /{print $2"\t"w}')"
            else
                _bc_maps[$_bc_repo]=""
            fi
        fi
        [ -n "${_bc_maps[$_bc_repo]}" ] || continue
        _bc_br="${_bc_branch:-spira/$_bc_id}"
        _bc_holder="$(awk -v b="refs/heads/$_bc_br" -F'\t' '$1==b{print $2; exit}' <<< "${_bc_maps[$_bc_repo]}")"
        [ -n "$_bc_holder" ] || continue
        case "$_bc_holder" in
            "$SPIRA_RUN/worktree/"*) _bc_holder_id="${_bc_holder#"$SPIRA_RUN/worktree/"}" ;;
            *) continue ;;
        esac
        [ "$_bc_holder_id" != "$_bc_id" ] || continue
        printf 'COLLISION %s %s %s %s %s\n' "$_bc_id" "$_bc_repo" "$_bc_br" "$_bc_holder_id" "$_bc_holder"
    done <<< "$_bc_ids"
}

# park_branch_collisions <detect_branch_collisions output> — labels each COLLISION bead
# $SPIRA_ASK_LABEL and overseer, once, so dispatch stops spending a claim on a condition that
# cannot change until a human frees the holder or corrects the branch: label. Idempotent
# (re-checks the label directly) so calling this on stale output does not re-note a bead
# detect_branch_collisions itself would already have excluded.
#
# AN INHERITED LABEL IS NOT A COLLISION FOR RYAN (sp-ln4ke). `bd create --parent` copies
# every label onto a child, so a split child can carry the parent's branch:spira/<parent>
# untouched — the child never chose that branch, it cut no branch of its own, and Ryan has
# no decision to make about it. Checked BEFORE the closed/clean free below: even if the
# named bead happens to be closed and clean right now, the label itself is still wrong and
# would send this bead onto that branch on its next claim (the exact incident this fixes —
# two split children committed onto their parent's branch before being parked by hand). The
# fix is mechanical, so it needs no human (law-deterministic-before-inference): strip the
# label so the bead falls back to its own derived default (spira/<id>; the old lib.sh
# bead_branch reader that did this lookup was retired dead at sp-27hsi, aeon resolves the
# state itself now), and note whatever commits it
# already made under the wrong name so they are not silently stranded. Prints "UNLABELED
# <id> <repo> <branch> <other-id>".
#
# BUT A CLOSED, CLEAN, UNHELD SQUATTER NEEDS NO HUMAN EITHER (sp-vcxmz): its aeon already
# finished and left, so nothing but a stale worktree registration stands between the
# blocked bead and a claim. Freed through the one destruction chokepoint
# (spira_destroy_worktree) rather than a bare `git worktree remove` — same salvage and
# liveness fence every other reap goes through, even though salvage will find nothing
# because the clean check already ran. The branch and its commits are never touched.
# Prints "FREED <id> <repo> <branch> <holder-id> <holder-path>" for each one freed, so a
# caller can log and count it apart from what still parks. Open, dirty or live holders
# fall through to the park below unchanged.
park_branch_collisions() {
    local line id repo branch holder_id holder_path labels
    local holder_status holder_dirty holder_repo_root
    local inherited_from inherited_commits pc_sha pc_subj
    while IFS= read -r line; do
        case "$line" in COLLISION\ *) ;; *) continue ;; esac
        read -r _ id repo branch holder_id holder_path <<< "$line"
        labels="$(bdq label list "$id" 2>/dev/null)"
        case "$labels" in *"${SPIRA_ASK_LABEL}"*) continue ;; esac

        inherited_from="${branch#spira/}"
        if [ "$inherited_from" != "$branch" ] && [ -n "$inherited_from" ] \
           && [ "$inherited_from" != "$id" ] && bdq show "$inherited_from" --json >/dev/null 2>&1; then
            case "$labels" in
                *"branch:$branch"*)
                    inherited_commits="$(git -C "$holder_path" log --format='%h%x09%s' --grep="$id:" -F 2>/dev/null \
                        | while IFS=$'\t' read -r pc_sha pc_subj; do
                              case "$pc_subj" in "$id":*) printf '%s %s\n' "$pc_sha" "$pc_subj" ;; esac
                          done)"
                    bdq label remove "$id" "branch:$branch" >/dev/null 2>&1 || true
                    if [ -n "$inherited_commits" ]; then
                        bdq note "$id" "Corrected by detect_branch_collisions: inherited branch:$branch from $inherited_from; cuts its own branch. This bead has its own commit(s) sitting unlanded on $branch, made before this label was removed: $inherited_commits" >/dev/null 2>&1 || true
                    else
                        bdq note "$id" "Corrected by detect_branch_collisions: inherited branch:$branch from $inherited_from; cuts its own branch." >/dev/null 2>&1 || true
                    fi
                    printf 'UNLABELED %s %s %s %s\n' "$id" "$repo" "$branch" "$inherited_from"
                    ;;
            esac
            continue
        fi

        holder_status="$(spira_bead_status "$holder_id")"
        if [ "$holder_status" = closed ] && ! holder_alive "$holder_id"; then
            if holder_dirty="$(git -C "$holder_path" status --porcelain 2>/dev/null)" \
               && [ -z "$holder_dirty" ] \
               && holder_repo_root="$(repo_root "$repo" 2>/dev/null)" && [ -n "$holder_repo_root" ] \
               && spira_destroy_worktree "$holder_id" "$holder_path" "$holder_repo_root" \
                      "branch collision: $holder_id is closed and clean, squatting $branch, blocking $id"; then
                printf 'FREED %s %s %s %s %s\n' "$id" "$repo" "$branch" "$holder_id" "$holder_path"
                continue
            fi
        fi

        bdq label add "$id" "$SPIRA_ASK_LABEL" >/dev/null 2>&1 || true
        bdq label add "$id" "overseer" >/dev/null 2>&1 || true
        # Dual-written, not a replace (sp-ki12s precedent) — the label is still what every
        # fayth's dispatch exclusion reads until that reader is cut over in the same round.
        spira-lc hold "$id" ask "branch $branch squatted by $holder_id's worktree at $holder_path" sentinel || true
        bdq note "$id" "Parked by detect_branch_collisions: recorded branch $branch is checked out in $holder_id's worktree at $holder_path, not this bead's own canonical path. Every summon reaches aeon.sh's law-one-aeon-one-worktree refusal (or a no-op self-correct, when this bead's own default branch is the squatted one) before a session can start, and nothing about the input changes on retry. Labeled $SPIRA_ASK_LABEL and overseer so dispatch stops spending a claim here — free $holder_path or correct the branch: label, then remove $SPIRA_ASK_LABEL." >/dev/null 2>&1 || true
    done <<< "$1"
}

# detect_livelocked -> one LIVELOCK line per open bead that cannot make progress.
#
# THE PROBLEM. A bead is livelocked when it is open but will never advance unless a human
# intervenes — not merely "blocked" by an open dependency (correct sequencing), but stuck
# for a structural reason the harness cannot resolve on its own. "Nothing ready" and "beads
# ready but stuck" look identical to the sentinel; this check names each bead and why.
#
# FOUR CATEGORIES, each found by a different predicate:
#
#   unclaimable          no persona can claim it: fayth: preference does not match the
#                        partition, or the bead carries no partition label at all. The
#                        sentinel reports every queue empty, truthfully; this names which
#                        beads are responsible. (Reuses detect_unclaimable_ready logic.)
#
#   ask-no-overseer      carries SPIRA_ASK_LABEL (excluded from every fayth predicate) but
#                        lacks overseer (the label the decisions pane selects on). The bead
#                        is invisible to both the loop and to the operator — it cannot be
#                        answered and cannot be dispatched.
#
#   ci-stuck             carries awaiting-ci (excluded from every fayth predicate, and from
#                        the stranded-work report) but the repository's land mode is not `pr`,
#                        so no run will ever report back. The label is a permanent hold that
#                        no mechanism will ever clear.
#
#   unmapped-repo        carries repo:<name> where <name> is not in the repo-map. aeon.sh
#                        refuses to claim it at claim time and leaves it open forever.
#
# SKIPS beads that are merely BLOCKED (open dependency), since those are correct
# sequencing — bd ready does not surface them and they need no action.
#
# OUTPUT: "LIVELOCK <id> <category> — <reason>"
# Each category uses its own slug so the rendering can group or colour by kind.
#
# A FAILED QUERY RETURNS NOTHING AND EXITS 0 (law-absence-needs-a-positive-control is
# handled by the caller: livelock_keys emits SP_LIVELOCKED=? when this returns nothing).
detect_livelocked() {
    # ---- unclaimable: reuse detect_unclaimable_ready output, prefixed as LIVELOCK ----
    local unc
    unc="$(detect_unclaimable_ready 2>/dev/null)"
    if [ -n "$unc" ]; then
        printf '%s\n' "$unc" | while IFS= read -r line; do
            # detect_unclaimable_ready prints "UNCLAIMABLE <id> — <reason>"
            # rewrite to "LIVELOCK <id> unclaimable — <reason>"
            case "$line" in UNCLAIMABLE\ *)
                rest="${line#UNCLAIMABLE }"
                bid="${rest%% *}"
                reason="${rest#* — }"
                printf 'LIVELOCK %s unclaimable — %s\n' "$bid" "$reason"
            ;; esac
        done
    fi

    # ---- ask-no-overseer: open beads with SPIRA_ASK_LABEL but without overseer ----
    # The decisions pane selects on `overseer`; without it, the bead is invisible to the operator.
    # The loop excludes SPIRA_ASK_LABEL from every predicate, so no aeon can claim it either.
    local _nr_raw
    _nr_raw="$(bdjson list --limit 0 --label "${SPIRA_ASK_LABEL:?SPIRA_ASK_LABEL is unset — source conf.sh}" 2>/dev/null)"
    if [ -n "$_nr_raw" ]; then
        printf '%s\n' "$_nr_raw" | python3 -c '
import os, sys, json, re
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
ask_label = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
for i in (d if isinstance(d, list) else [d]):
    L = set(i.get("labels") or [])
    if ask_label not in L:
        continue
    if "overseer" in L:
        continue
    title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:60]
    print("LIVELOCK %s ask-no-overseer — missing overseer label; "
          "the decisions pane cannot see this bead and no aeon can claim it; "
          "add overseer label. title: %s" % (i["id"], title))
' 2>/dev/null
    fi

    # ---- ci-stuck: awaiting-ci beads in a repo whose land mode is not `pr` ----
    local _ci_raw
    _ci_raw="$(bdjson list --all --limit 0 --label "$SPIRA_CI_LABEL" 2>/dev/null)"
    if [ -n "$_ci_raw" ]; then
        printf '%s\n' "$_ci_raw" | python3 -c '
import sys, json, re
home = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
for i in (d if isinstance(d, list) else [d]):
    if i.get("status") == "closed":
        continue
    repo = next((l[5:] for l in (i.get("labels") or []) if l.startswith("repo:")), home)
    title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:60]
    # Report all awaiting-ci beads; the shell below checks the land mode.
    print("%s\t%s\t%s" % (i["id"], repo, title))
' "$(spira_home_repo)" 2>/dev/null | while IFS=$'\t' read -r _cid _crepo _ctitle; do
            [ -n "$_cid" ] || continue
            # We only want the STRUCTURAL case: the repo's land mode is not `pr` so no run
            # will ever report back. The old lib.sh spira_ci_park_state (retired dead at
            # sp-27hsi — nothing called it) also checked timing and exited 2 on an empty
            # timestamp, which would have tripped a careless `|| _state=no-ci` even for a
            # pr-mode repo. Use repo_land directly — it is the one test that names the
            # structural fault.
            _land="$(repo_land "$_crepo" 2>/dev/null)"
            if [ "${_land:-push}" != pr ]; then
                printf 'LIVELOCK %s ci-stuck — repo %s land mode is not pr; %s will never clear; strip the label or change the repo land mode. title: %s\n' \
                    "$_cid" "$_crepo" "$SPIRA_CI_LABEL" "$_ctitle"
            fi
        done
    fi

    # ---- unmapped-repo: open beads with repo: label not in the repo-map ----
    if [ -r "${SPIRA_REPO_MAP:-}" ]; then
        local _valid_names _open_raw
        _valid_names="$(awk 'BEGIN{FS="|"} /^[ \t]*#/{next}
            {n=$1; gsub(/^[ \t]+|[ \t]+$/,"",n); if(n!=""&&NF>1) print n}' \
            "$SPIRA_REPO_MAP" 2>/dev/null)"
        _open_raw="$(bdjson list --limit 0 2>/dev/null)"
        if [ -n "$_open_raw" ]; then
            printf '%s\n' "$_open_raw" | VALID_NAMES="$_valid_names" python3 -c '
import os, sys, json, re
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
valid = set(os.environ.get("VALID_NAMES", "").split())
ask_label = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
groom_ask_label = os.environ.get("SPIRA_GROOM_ASK_LABEL", "groom-asked")  # literal-ok: Python fallback for direct invocation without conf.sh
for i in (d if isinstance(d, list) else [d]):
    L = i.get("labels") or []
    # Skip beads already handled by the unclaimable, needs-ryan or groom-ask checks.
    if ask_label in L or groom_ask_label in L or "spira-poison" in L:
        continue
    repo_labels = [l[5:] for l in L if l.startswith("repo:")]
    if not repo_labels:
        continue
    bad = [r for r in repo_labels if r not in valid]
    if not bad:
        continue
    title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:60]
    print("LIVELOCK %s unmapped-repo — repo:%s not in repo-map; aeon.sh refuses to claim it; "
          "fix the label or add the repo to repo-map. title: %s" % (i["id"], ", ".join(bad), title))
' 2>/dev/null
        fi
    fi
}

# detect_landed_but_open -> "STATE <id> landed-but-open — <evidence>" for every open or
# in_progress bead in the WHOLE graph — no partition, no label filter — whose repository's
# base already carries a commit landing it. A bead's landed-but-open state does not depend
# on which partition it happens to carry, so a scan bounded to one partition cannot see one
# filed under another (sp-0qp7s: the groomer's scan read only its own trigger partition).
detect_landed_but_open() {
    local raw home
    home="$(spira_home_repo)"
    raw="$(bdjson list --status open,in_progress --limit 0 2>/dev/null)"
    [ -n "$raw" ] || return 0
    printf '%s\n' "$raw" | WORK_TYPES="${SPIRA_WORK_CLOSE_TYPES:-task bug feature}" python3 -c '
import json, os, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
work_types = set((os.environ.get("WORK_TYPES") or "task bug feature").split())
home = sys.argv[1]
for i in (d if isinstance(d, list) else [d]):
    if (i.get("issue_type") or "") not in work_types:
        continue
    L = i.get("labels") or []
    repo = next((l[5:] for l in L if l.startswith("repo:")), home)
    print("%s\t%s" % (i["id"], repo))
' "$home" 2>/dev/null | while IFS=$'\t' read -r id repo; do
        [ -n "$id" ] || continue
        local r_path sha
        r_path="$(repo_root "${repo:-$home}" 2>/dev/null)" || continue
        [ -n "$r_path" ] || continue
        if landed "$id" "$r_path" 2>/dev/null; then
            sha="$(landed_sha "$id" "$r_path" 2>/dev/null)"
            printf 'STATE %s landed-but-open — %s names it on %s'"'"'s base; close it\n' \
                "$id" "${sha:-a commit}" "$repo"
        fi
    done
}

# detect_closed_unlanded_states -> one STATE line per closed work bead, across every
# partition this host watches (fayth_partitions — every persona's, not the caller's own),
# that carries none of CHECK 5's recognised landing signals (supersedes, spira-dropped,
# delivers:, content-landed) and that `landed()` cannot prove via the base's own commit
# graph. Two shapes:
#
#   STATE <id> closed-no-branch          — no branch: label (or the label names a ref that
#                                           was never pushed): nothing was ever committed.
#   STATE <id> closed-never-landed <verdict> <repo> <branch> <base> — a branch: label names
#                                           a real ref ahead of base; <verdict> is `conflict`
#                                           when it does not merge cleanly (needs a rebase) or
#                                           `batch-ready` when it does (ready to requeue as-is).
#
# This is the same exclusion set sentinel.sh CHECK 5 applies before filing an Ops incident —
# CHECK 5 reports; this classifies for the groomer to act on with judgement (reopen for
# rebase, or reopen as batch-ready).
detect_closed_unlanded_states() {
    local labels exclude home
    home="$(spira_home_repo)"
    {
        while IFS=$'\t' read -r labels exclude; do
            [ -n "$labels" ] || continue
            bdjson list --limit 0 --label "$labels" --status closed 2>/dev/null \
            | SPIRA_EXCL="$exclude" WORK_TYPES="${SPIRA_WORK_CLOSE_TYPES:-task bug feature}" python3 -c '
import json, os, sys
excl = {x for x in (os.environ.get("SPIRA_EXCL") or "").split(",") if x}
work_types = set((os.environ.get("WORK_TYPES") or "task bug feature").split())
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("status") != "closed":
        continue
    if (i.get("issue_type") or "") not in work_types:
        continue
    L = i.get("labels") or []
    if excl & set(L):
        continue
    if any(l.startswith("delivers:") for l in L):
        continue
    if "spira-dropped" in L or "content-landed" in L:
        continue
    if any((x.get("dependency_type") or x.get("type")) == "supersedes" for x in (i.get("dependencies") or [])):
        continue
    repo = next((l[5:] for l in L if l.startswith("repo:")), "")
    br = next((l[7:] for l in L if l.startswith("branch:")), "")
    print("%s\t%s\t%s" % (i["id"], repo, br))
' 2>/dev/null
        done < <(fayth_partitions)
    } | awk -F'\t' '!seen[$1]++' | while IFS=$'\t' read -r id repo br; do
        [ -n "$id" ] || continue
        local r_path refs base
        r_path="$(repo_root "${repo:-$home}" 2>/dev/null)" || continue
        [ -n "$r_path" ] || continue
        landed "$id" "$r_path" 2>/dev/null && continue
        if [ -z "$br" ] || ! git -C "$r_path" show-ref --verify -q "refs/heads/$br" 2>/dev/null; then
            printf 'STATE %s closed-no-branch — repo %s%s; nothing committed, no landing record\n' \
                "$id" "${repo:-$home}" "${br:+ (branch: label $br names no ref)}"
            continue
        fi
        refs="$(spira_landrefs "$r_path" 2>/dev/null)" || refs=""
        base="${refs%% *}"
        [ -n "$base" ] || continue
        content_landed "$r_path" "$br" "$base" 2>/dev/null && continue
        if git -C "$r_path" merge-tree --write-tree "$base" "$br" >/dev/null 2>&1; then
            printf 'STATE %s closed-never-landed batch-ready %s %s %s — merges cleanly, ready to requeue\n' \
                "$id" "${repo:-$home}" "$br" "$base"
        else
            printf 'STATE %s closed-never-landed conflict %s %s %s — does not merge, needs a rebase\n' \
                "$id" "${repo:-$home}" "$br" "$base"
        fi
    done
}

# detect_false_blockers <blocker-ids> -> "STATE <id> blocked-by-unlanded <blocker> — <evidence>"
# for every open or in_progress bead depending (type=blocks) on one of <blocker-ids> (newline
# or space separated). detect_closed_unlanded_states names the blockers this reads: a closed
# bead whose work never landed still counts as "done" to every dependent's `bd ready`, so its
# dependents sit correctly-blocked forever on a false premise (sp-jzfog blocking sp-vsob2).
# The remedy is the blocker's own — reopening it (task 2) is what clears this — so this
# function only makes the false block visible on the bead it was holding shut.
detect_false_blockers() {
    local blockers="${1:-}" raw
    [ -n "$blockers" ] || return 0
    raw="$(bdjson list --status open,in_progress --limit 0 2>/dev/null)"
    [ -n "$raw" ] || return 0
    printf '%s\n' "$raw" | BLOCKERS="$blockers" python3 -c '
import json, os, sys
blockers = set((os.environ.get("BLOCKERS") or "").split())
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    deps = [x.get("depends_on_id") for x in (i.get("dependencies") or [])
            if (x.get("dependency_type") or x.get("type")) == "blocks"]
    for b in blockers.intersection(deps):
        print("STATE %s blocked-by-unlanded %s — depends on %s, which is closed but its work never landed" % (i["id"], b, b))
' 2>/dev/null
}

# detect_incident_needs_builder -> "STATE <id> incident-is-code — <evidence>" for every open
# or in_progress bead carrying SPIRA_INCIDENT_LABEL whose recorded branch: already has a
# commit ahead of the repo's base.
#
# An incident-labelled bead sits in the partition no builder claims; when its remaining work
# is actually a code change, the queue starves it until a human reads the bead and moves the
# label by hand (sp-awm1q, sp-qlcvc, sp-c1ot2, sp-18v9k — the same misroute, caught by hand
# every time). The signal has to be one that cannot fire on a genuinely operational incident:
# ops.fayth gives Ops no Edit or Write tool, so a commit on an incident bead's own branch is
# never Ops's own work. It exists only if code was already written for this bead under some
# other persona — which is exactly "the remaining work is a code change", fully computable,
# with nothing left to a model's judgment.
detect_incident_needs_builder() {
    local raw home inc
    home="$(spira_home_repo)"
    inc="${SPIRA_INCIDENT_LABEL:?SPIRA_INCIDENT_LABEL is unset — source conf.sh}"
    raw="$(bdjson list --status open,in_progress --label "$inc" --limit 0 2>/dev/null)"
    [ -n "$raw" ] || return 0
    printf '%s\n' "$raw" | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    L = i.get("labels") or []
    br = next((l[7:] for l in L if l.startswith("branch:")), "")
    if not br:
        continue
    repo = next((l[5:] for l in L if l.startswith("repo:")), "")
    print("%s\t%s\t%s" % (i["id"], repo, br))
' 2>/dev/null | while IFS=$'\t' read -r id repo br; do
        [ -n "$id" ] || continue
        local r_path refs base ahead
        r_path="$(repo_root "${repo:-$home}" 2>/dev/null)" || continue
        [ -n "$r_path" ] || continue
        git -C "$r_path" show-ref --verify -q "refs/heads/$br" 2>/dev/null || continue
        refs="$(spira_landrefs "$r_path" 2>/dev/null)" || continue
        base="${refs%% *}"
        [ -n "$base" ] || continue
        ahead="$(git -C "$r_path" rev-list --count "$base..$br" 2>/dev/null)" || continue
        [ "${ahead:-0}" -gt 0 ] 2>/dev/null || continue
        printf 'STATE %s incident-is-code — %s commit(s) already on %s ahead of %s; remaining work is a code change, not operational\n' \
            "$id" "$ahead" "$br" "$base"
    done
}

# detect_invalid_closed -> INVALID-CLOSED and UNFILED-FOLLOW lines for closed beads whose
# close reasons admit unfinished work or imply follow-on work that was never filed.
#
# TWO FAMILIES, kept separate because they need different remedies:
#
# INVALID-CLOSED: the close reason contains a statute phrase that says the work itself is
# partial (law-no-close-reason-admits-unfinished: "PERMANENT FIX NEEDED", "temporary",
# "mitigated-only", "TODO"). The statute is prospective; this finds the ones already closed.
#
# UNFILED-FOLLOW: the close reason implies follow-on work exists (contains a phrase like
# "builders should", "the real fix", "follow-up") but names no tracking reference (a bead id
# in the form PREFIX-id, an owner/repo#N GitHub reference, or an https:// URL). A reason
# with a tracking reference has handed off correctly; one without has left work unfiled.
# The word "workaround" is NOT a proxy for either — a workaround can be complete, verified
# and landed. Measure the property (remainder exists and has no tracking), not the word.
#
# OUTPUT: "INVALID-CLOSED <id> — <reason>" or "UNFILED-FOLLOW <id> — <reason>"
detect_invalid_closed() {
    local _closed_raw
    if [ -n "${SPIRA_SCOPE_LABEL:-}" ]; then
        _closed_raw="$(bdjson list --status closed --label "$SPIRA_SCOPE_LABEL" --limit 0 2>/dev/null)"
    else
        _closed_raw="$(bdjson list --status closed --limit 0 2>/dev/null)"
    fi
    [ -n "$_closed_raw" ] || return 0
    printf '%s\n' "$_closed_raw" | SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_ID_PREFIX="${SPIRA_ID_PREFIX:-sp}" SPIRA_RUN="${SPIRA_RUN:-}" python3 -c '
import sys, json, re, os

# Load detect_close_reason and check_unfiled_follow from the shared helper. The detector
# uses detect_close_reason (raw scan, no masking) so all occurrences reach Maechen;
# check_close_reason (quote-masked) belongs to the close-time fence in aeon.sh.
_flags_path = os.path.join(os.environ.get("SPIRA_HOME", ""), "close-reason-flags.py")
try:
    _ns = {"re": re, "__name__": ""}
    exec(open(_flags_path).read(), _ns)
    check_close_reason = _ns.get("detect_close_reason") or _ns["check_close_reason"]
    check_unfiled_follow = _ns["check_unfiled_follow"]
except Exception:
    check_close_reason = lambda r: None
    check_unfiled_follow = lambda r, id_prefix="sp": None

_id_prefix = os.environ.get("SPIRA_ID_PREFIX", "sp")

# ALLOWLIST. Beads whose id appears in $SPIRA_RUN/invalid-closed.allow are reported
# as ALLOWED-IC (not counted) rather than as INVALID-CLOSED or UNFILED-FOLLOW. Each
# line in the allowlist is "<id> <reason>" — the reason is what Maechen recorded when
# it judged the row a false positive (quotation) or resolved it (follow-up filed).
allowlist = {}
_run = os.environ.get("SPIRA_RUN", "")
_allow_path = os.path.join(_run, "invalid-closed.allow") if _run else ""
if _allow_path and os.path.isfile(_allow_path):
    with open(_allow_path) as _af:
        for _line in _af:
            _parts = _line.strip().split(None, 1)
            if _parts and re.match(r"^[a-z0-9]+(?:-[a-z0-9]+)+$", _parts[0]):
                allowlist[_parts[0]] = _parts[1] if len(_parts) > 1 else ""

try: d = json.load(sys.stdin)
except Exception: raise SystemExit
for i in (d if isinstance(d, list) else [d]):
    bid = i["id"]
    reason = i.get("close_reason") or ""
    title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:60]
    reason_short = re.sub(r"\s+", " ", reason.strip())[:120]

    if bid in allowlist:
        print("ALLOWED-IC %s — %s" % (bid, allowlist[bid]))
        continue

    hit = check_close_reason(reason)
    if hit:
        print("INVALID-CLOSED %s — close reason contains %r: %s. title: %s" % (
            bid, hit, reason_short, title))
        continue

    follow_hit = check_unfiled_follow(reason, _id_prefix)
    if follow_hit:
        print("UNFILED-FOLLOW %s — follow-on phrase %r without a tracking reference: %s. title: %s" % (
            bid, follow_hit, reason_short, title))
' 2>/dev/null
}

# --------------------------------------------------------------------------------------
# THE REPOSITORY REGISTRY. Which repository a bead is worked in comes from THE BEAD — a
# `repo:<name>` label, the same partition every imported Gas Town bead already carries —
# and `repo-map` says what that name means on this disk.
#
# It used to come from the fayth, as FAYTH_REPO, which is a constant per persona: every
# fayth in the chamber pointed at the home checkout, so Spira could not touch any other
# repository on the box. Collapsing the per-repository databases into one made the cross-repo
# DEPENDENCY expressible and left the cross-repo WORK impossible, which is most of what the
# collapse was for. It surfaced the day the operator endorsed fixing ~37 grep -q
# pipelines in another repository and there was no aeon that could open the file.
#
# UNKNOWN NAMES FAIL CLOSED. Every lookup here returns non-zero for a name the map does not
# carry, and every caller must treat that as a refusal rather than reach for a default. A
# default of the home checkout is precisely how another repository's bead gets "fixed" in the
# home repo, and the aeon would report success — it committed, on a branch, naming its bead.
# `repo:town` is deliberately unmapped for the same reason it is deliberately unfenced: Gas
# Town is still serving and new dispatch into it is frozen.
# --------------------------------------------------------------------------------------
# SPIRA_REPO_MAP is resolved in conf.sh, which falls back to repo-map.example so a
# clean clone has a map at all. This line is the guard for a lib.sh sourced without it.
SPIRA_REPO_MAP="${SPIRA_REPO_MAP:-$SPIRA_HOME/repo-map}"

# --------------------------------------------------------------------------------------
# CONTAINMENT: a non-prod instance may not name a repo outside its workspaces root, nor
# one with a real (network-reachable) remote. Two independent refusals, both failing
# closed, because a path check alone is not containment — a clone in the right location
# can still push to github.com. What is contained is REACH, not location.
#
# Prod is entirely unaffected: the check is a no-op when SPIRA_INSTANCE is absent or
# 'prod'. The map is read exactly once, at the moment lib.sh is sourced, so the
# harness refuses before it does any work rather than at first use.
#
# A "real remote" is any fetch URL that does not start with '/' (absolute path) and is
# not a file:// URL and is not empty. https://, git@, ssh:// are all real remotes.
# A clone with no remotes at all passes — that is the intended shape for test repos.
# --------------------------------------------------------------------------------------
_spira_remote_is_real() {   # _spira_remote_is_real <url> -> 0 if network-reachable
    local url="${1:-}"
    [ -n "$url" ] || return 1          # no URL is not a real remote
    case "$url" in
        /*)        return 1 ;;         # absolute local path
        file:///*) return 1 ;;         # file:// URL pointing locally
        *)         return 0 ;;         # https://, git@, ssh://, etc.
    esac
}

spira_containment_check() {
    # prod (or unset) is always allowed; the map is unconstrained.
    case "${SPIRA_INSTANCE:-prod}" in prod) return 0 ;; esac

    local ws path url row name bad=0
    ws="${SPIRA_WORKSPACES:-}"

    [ -f "$SPIRA_REPO_MAP" ] || return 0   # no map to check

    while IFS='|' read -r name path rest || [ -n "$name" ]; do
        # strip whitespace and skip comments/blanks
        name="${name#"${name%%[![:space:]]*}"}"; name="${name%"${name##*[![:space:]]}"}"
        path="${path#"${path%%[![:space:]]*}"}"; path="${path%"${path##*[![:space:]]}"}"
        case "$name" in ''|'#'*) continue ;; esac
        [ -n "$path" ] || continue

        # REFUSAL 1: path must be under SPIRA_WORKSPACES.
        if [ -n "$ws" ]; then
            # resolve the workspace root to its canonical prefix
            local ws_real; ws_real="$(cd "$ws" 2>/dev/null && pwd -P)"
            if [ -n "$ws_real" ]; then
                # canonical path of the repo entry (use the directory if it exists, else
                # compare the literal string so an unmade path is still caught by name)
                local path_real; path_real="$(cd "$path" 2>/dev/null && pwd -P)"
                [ -n "$path_real" ] || path_real="$path"
                # Strip the trailing slash before interpolating into the glob pattern.
                # When ws_real is "/" the unstripped form produces "//*", which a shell
                # case statement never matches — a single leading slash cannot satisfy two.
                # Without the slash the prefix becomes "" and "/*" matches every absolute path.
                local ws_pfx="${ws_real%/}"
                case "$path_real" in
                    "$ws_pfx"/*|"$ws_pfx") ;;   # inside workspaces root — ok
                    *) printf 'spira: containment: instance %s is confined to %s — %s (%s) is outside it\n' \
                           "${SPIRA_INSTANCE}" "$ws" "$name" "$path" >&2
                       bad=1 ;;
                esac
            else
                # SPIRA_WORKSPACES does not exist as a directory; compare literal prefix.
                # Same trailing-slash fix applies to the literal path.
                local ws_lit="${ws%/}"
                case "$path" in
                    "$ws_lit"/*|"$ws_lit") ;;
                    *) printf 'spira: containment: instance %s is confined to %s — %s (%s) is outside it\n' \
                           "${SPIRA_INSTANCE}" "$ws" "$name" "$path" >&2
                       bad=1 ;;
                esac
            fi
        fi

        # REFUSAL 2: no real (network) remote on any registered checkout.
        # Only check if the path is a git repository at all.
        if git -C "$path" rev-parse --git-dir >/dev/null 2>&1; then
            while IFS= read -r url; do
                _spira_remote_is_real "$url" || continue
                printf 'spira: containment: instance %s may not have a real remote — %s (%s) has %s\n' \
                    "${SPIRA_INSTANCE}" "$name" "$path" "$url" >&2
                bad=1; break
            done < <(git -C "$path" remote -v 2>/dev/null | awk '/\(fetch\)/ { print $2 }')
        fi
    done < "$SPIRA_REPO_MAP"

    [ "$bad" -eq 0 ] || { printf 'spira: containment check failed for instance %s — halting\n' \
        "${SPIRA_INSTANCE}" >&2; exit 1; }
}

# Run at source time. The cost is one read of the map file and, for non-prod instances,
# one `git remote -v` per registered checkout — a fraction of a second on summon.
spira_containment_check

# The name is DERIVED from the checkout the harness is installed in (conf.sh: basename of
# SPIRA_REPO) and overridable in spira.conf. It used to be the literal `brain`, which is one
# operator's repository written into the mechanism.
spira_home_repo() {      # the repo name a bead means when it names none
    printf '%s' "${SPIRA_HOME_REPO:-$(basename "${SPIRA_REPO:-$SPIRA_HOME}")}"
}

# COLUMNS ARE NAMED, NEVER NUMBERED. This function took an index until `base` was added
# between `land` and `format`, at which point every existing call site silently meant a
# different column — the gate command would have been read as a branch name and a branch
# name run as a gate. A name cannot shift under a new column.
#
# THE ROW SHAPE DECIDES WHERE THE OPTIONAL COLUMNS LIVE, and every read is symmetric about
# it: six fields is the current form, five is the form before `base` existed, four predates
# the formatter too.
#
# Reading any of them from a fixed position is how a format change fails silently, in both
# directions at once. Position 4 in a five-field row holds a FORMATTER, a single token that
# looks exactly like a ref until git is asked — so no heuristic over the field CONTENT can
# tell a formatter from a base, only NF can. And a gate read from a fixed field 6 of a
# five-field row comes back EMPTY, which this file defines as "syntax was the whole trial":
# gate-brain.sh would quietly stop running and every branch would land ungated. That is the
# worse half, because it fails OPEN — and it is reachable in deployment rather than
# hypothetical, since the harness is installed in a checkout and read by systemd
# timers, so lib.sh and repo-map can be read out of step for one pass.
#
# NOTE: no apostrophes inside the awk program below. It is single-quoted, so one in a comment
# closes the string and the shell reports a syntax error pointing at the following line.
repo_field() {           # repo_field <name> <path|land|base|format|gate|lanes> -> the field
    local name="$1" col="$2"
    [ -f "$SPIRA_REPO_MAP" ] || return 1
    awk -v want="$name" -v col="$col" '
        function _lanes_col_idx(    t) {
            if (NF < 7) return 0
            t = $NF; gsub(/^[ \t]+|[ \t]+$/, "", t)
            # An empty trailing field means an explicit empty lanes column.
            if (t == "") return NF
            # A lanes value is a simple identifier: letters, digits, hyphens, commas only.
            # Gate fragments always contain spaces, slashes, dollars, or other shell chars,
            # so this pattern distinguishes them in practice.
            if (t ~ /^[A-Za-z][A-Za-z0-9,_-]*$/) return NF
            return 0
        }
        BEGIN { FS = "|" }
        /^[ \t]*#/ { next }
        {
            n = $1; gsub(/^[ \t]+|[ \t]+$/, "", n)
            if (n == "" || NF < 2 || n != want) next
            li = _lanes_col_idx()
            # The gate is everything from the last fixed column on, rejoined with "|", up
            # to but not including the lanes column when one is present. The formatter is
            # always a single field; the gate is the last command column and may contain
            # pipes (and therefore become multiple awk fields when split on "|").
            #
            # WHICH position gate starts at comes from NF (or li when lanes is present),
            # never from a constant. A six-field row is the current shape; five-field rows
            # predate `base`; anything narrower predates both.
            if (col == "lanes") { v = (li > 0 ? $NF : "") }
            else if (col == "gate") {
                s = (NF >= 6 ? 6 : (NF == 5 ? 5 : 4))
                e = (li > 0 ? NF - 1 : NF)
                v = ""
                for (i = s; i <= e; i++) v = v (i > s ? "|" : "") $i
            }
            else if (col == "path")   v = $2
            else if (col == "land")   v = $3
            else if (col == "base")   v = (NF >= 6 ? $4 : "")
            else if (col == "format") v = (NF >= 6 ? $5 : (NF == 5 ? $4 : ""))
            else                      v = ""
            gsub(/^[ \t]+|[ \t]+$/, "", v)
            print v; exit
        }' "$SPIRA_REPO_MAP" 2>/dev/null
}

repo_names() {           # every repo name in the map, one per line
    [ -f "$SPIRA_REPO_MAP" ] || return 0
    awk 'BEGIN { FS = "|" } /^[ \t]*#/ { next }
         { n = $1; gsub(/^[ \t]+|[ \t]+$/, "", n); if (n != "" && NF > 1) print n }' \
        "$SPIRA_REPO_MAP" 2>/dev/null
}

# repo_root <name> -> the checkout, or non-zero if the map does not carry that name.
#
# SPIRA_REPO still names the HOME repository, because that is the seam every existing suite
# drives a fixture through: a test sets SPIRA_REPO and its beads carry no `repo:` label at
# all. Widening it into a map rather than replacing it is what keeps those suites honest.
repo_root() {
    local name="${1:-}" p
    [ -n "$name" ] || name="$(spira_home_repo)"
    # ONLY WHEN IT IS AN OVERRIDE. conf.sh derives SPIRA_REPO from where the harness sits, so
    # it is now always set — and taking it unconditionally made every home-repository lookup
    # bypass the map. It counts as an override exactly when it differs from that derived
    # value, which is what "somebody set this on purpose" means here.
    if [ "$name" = "$(spira_home_repo)" ] && [ -n "${SPIRA_REPO:-}" ] \
       && [ "$SPIRA_REPO" != "${SPIRA_REPO_DERIVED:-}" ]; then
        printf '%s' "$SPIRA_REPO"; return 0
    fi
    p="$(repo_field "$name" path)"
    [ -n "$p" ] || return 1
    printf '%s' "$p"
}

# spira_same_repo <a> <b> -> 0 if those two paths are the same repository.
#
# BY OBJECT STORE, NEVER BY PATH STRING. A worktree and the checkout it was cut from are one
# repository under two paths, and this harness runs from both — every aeon works in a
# worktree and the landing gate extracts one. A string comparison therefore calls the copy in
# force "some other repository", so a fence keyed on it fires on every branch, and a check
# keyed on it reports a second copy that does not exist.
#
# `--git-common-dir` and not `--git-dir`: a worktree has a private git dir and a shared common
# one, and only the shared one identifies the repository. Resolved by `cd` + `pwd -P` rather
# than `--path-format=absolute`, which is newer than the git a colleague may be running, and
# because the answer is relative when the command is run from inside the repository.
spira_same_repo() {      # spira_same_repo <path-a> <path-b>
    local a b
    a="$(_spira_gitstore "${1:-}")" || return 1
    b="$(_spira_gitstore "${2:-}")" || return 1
    [ -n "$a" ] && [ -n "$b" ] && [ "$a" = "$b" ]
}

_spira_gitstore() {      # _spira_gitstore <path> -> its shared git directory, absolute
    local d
    d="$( cd "${1:-/nonexistent}" 2>/dev/null \
          && d="$(git rev-parse --git-common-dir 2>/dev/null)" && [ -n "$d" ] \
          && cd "$d" 2>/dev/null && pwd -P )" || return 1
    [ -n "$d" ] || return 1
    printf '%s' "$d"
}

# repo_land <name> -> push | pr | hold | queue | queue.local
#
# `queue.forge` NORMALIZES TO `queue` HERE, so every existing `[ "$mode" = queue ]` dispatcher
# in landing.sh, queue.sh, verdict.sh, batch.sh and this file keeps working unchanged against
# the alias — the one place that decides land mode is the one place that needs to know the
# alias exists. `queue.local` stays distinct at this layer: its round lands locally
# (land-local) rather than through a batch PR, so a dispatcher that is specifically about the
# batch-PR pipeline must still spell out `queue` alone. Certification and the periodic
# step/verdict dispatch are not specific to that pipeline — see repo_land_queued.
repo_land() {            # repo_land <name> -> push | pr | hold | queue | queue.local
    local m; m="$(repo_field "${1:-}" land)"
    [ "$m" = "queue.forge" ] && m=queue
    printf '%s' "${m:-push}"
}

# repo_land_queued <name> -> 0 when the repo is EITHER merge-queue mode (queue or
# queue.local), 1 otherwise. Certification (queue.sh submit, aeon.sh's own self-cert), the
# landing pass's "no local rebase" arm, and the periodic step/verdict dispatch all judge a
# SUBMITTED branch the same way regardless of how its round eventually lands — only the
# round's own terminal step (a batch PR vs. land-local) differs between the two.
repo_land_queued() {
    case "$(repo_land "${1:-}")" in
        queue|queue.local) return 0 ;;
        *) return 1 ;;
    esac
}

# spira_repo_lanes <name> -> the granted lane set (space-separated partition labels).
#
# A missing lanes column or empty field admits all known lanes (no restriction). A mode name
# (consume/develop/self) expands to its lane set using the configured SPIRA_*_LABEL values.
# An unknown mode name or unknown lane label is a hard error naming the row — a typo must
# not quietly disable a lane.
spira_repo_lanes() {
    local name="${1:-}" raw
    raw="$(repo_field "$name" lanes 2>/dev/null)"
    [ -z "$raw" ] && {
        local p="${SPIRA_PLAN_LABEL:-plan}"
        local inc="${SPIRA_INCIDENT_LABEL:-incident}"
        local gr="${SPIRA_GROOMER_LABEL:-groom}"
        local mae="${SPIRA_MAECHEN_LABEL:-maechen-sweep}"  # literal-ok: bash fallback; SPIRA_MAECHEN_LABEL set by conf.sh
        local sp="${SPIRA_SPIKE_LABEL:-spike}"
        local cz="${SPIRA_CZAR_LABEL:-czar-trigger}"
        printf '%s %s %s %s %s %s' "$p" "$inc" "$gr" "$mae" "$sp" "$cz"
        return 0
    }
    _spira_expand_lanes "$name" "$raw"
}

_spira_expand_lanes() {  # _spira_expand_lanes <repo-name> <raw> -> space-separated lane labels
    local name="$1" raw="$2"
    local p="${SPIRA_PLAN_LABEL:-plan}"
    local inc="${SPIRA_INCIDENT_LABEL:-incident}"
    local gr="${SPIRA_GROOMER_LABEL:-groom}"
    local mae="${SPIRA_MAECHEN_LABEL:-maechen-sweep}"  # literal-ok: bash fallback; SPIRA_MAECHEN_LABEL set by conf.sh
    local sp="${SPIRA_SPIKE_LABEL:-spike}"
    local cz="${SPIRA_CZAR_LABEL:-czar-trigger}"
    case "$raw" in
        consume) printf '%s' "$p"; return 0 ;;
        develop) printf '%s %s %s %s' "$p" "$inc" "$gr" "$sp"; return 0 ;;
        self)    printf '%s %s %s %s %s %s' "$p" "$inc" "$gr" "$sp" "$mae" "$cz"; return 0 ;;
    esac
    local result="" item
    local IFS=,
    for item in $raw; do
        item="${item#"${item%%[![:space:]]*}"}"; item="${item%"${item##*[![:space:]]}"}"
        [ -z "$item" ] && continue
        case "$item" in
            "$p"|"$inc"|"$gr"|"$mae"|"$sp"|"$cz") ;;
            *) printf 'spira: repo:%s — unknown lane %s (valid: %s)\n' \
                   "$name" "$item" "$p,$inc,$gr,$mae,$sp,$cz" >&2
               return 1 ;;
        esac
        result="${result:+$result }$item"
    done
    [ -n "$result" ] || { printf '%s' "$p"; return 0; }
    printf '%s' "$result"
}

# spira_lane_admitted <lane> -> 0 if the home repo or some repo in SPIRA_REPO_MAP admits
# it, 1 otherwise. Shared by groom-trigger.sh and maechen-trigger.sh (duplicate cluster
# D14) — on a consuming install with no repo willing to run a lane, its trigger would
# otherwise accumulate trigger beads for work nobody can do.
spira_lane_admitted() {
    local lane="$1" hr hl rn rest rl
    hr="$(spira_home_repo 2>/dev/null)" || hr=""
    if [ -n "$hr" ]; then
        hl="$(spira_repo_lanes "$hr" 2>/dev/null)" || hl=""
        case " $hl " in *" $lane "*) return 0 ;; esac
    fi
    [ -f "${SPIRA_REPO_MAP:-}" ] || return 1
    while IFS='|' read -r rn rest; do
        rn="${rn#"${rn%%[![:space:]]*}"}"; rn="${rn%"${rn##*[![:space:]]}"}"
        case "${rn:-}" in ''|'#'*) continue ;; esac
        rl="$(spira_repo_lanes "$rn" 2>/dev/null)" || continue
        case " $rl " in *" $lane "*) return 0 ;; esac
    done < "$SPIRA_REPO_MAP"
    return 1
}

# spira_open_trigger_count <labels> -> count of open-or-in_progress beads carrying every
# label in <labels> (comma-separated), or 0 on a query failure (fail toward filing rather
# than silently going quiet — the caller's own dedup guard is what a false 0 would defeat).
# in_progress is included because a claimed trigger bead leaves --status open, and a dedup
# query scoped to open alone would file a duplicate on the very next tick (sp-mp9s — the
# defect that motivated including it in maechen-trigger.sh, extracted here so
# groom-trigger.sh gets the same fix rather than drifting from it).
spira_open_trigger_count() {
    local labels="$1" json n
    json="$("${SPIRA_BD:-bd}" -C "${SPIRA_DB:-.}" list --status open,in_progress --label "$labels" --json 2>/dev/null)" \
        || json="[]"
    [ -z "$json" ] && json="[]"
    n="$(printf '%s\n' "$json" \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null)" || n=0
    printf '%s' "${n:-0}"
}

repo_gate() {            # repo_gate <name> -> the repo's own gate command, possibly empty
    repo_field "${1:-}" gate
}

# repo_format <name> -> the repo's own formatter, or nothing. ABSENCE MEANS DO NOTHING, and
# that is a decision rather than a gap: running a formatter a repository has not asked for
# turns one bead's rebase into a thousand-line diff nobody requested, and on a repository
# whose base is already unformatted — another, measured once — it rewrites the whole
# tree out from under the work.
repo_format() {
    repo_field "${1:-}" format
}

# repo_base <name> -> the repo's declared base ref, or nothing if the row leaves it to
# spira_landref to resolve. Callers want spira_landref, not this: it is the raw column.
repo_base() {
    repo_field "${1:-}" base
}

# repo_name_at <path> -> the map name for a checkout path, or non-zero.
#
# The reverse of repo_root, and it exists because rebase_branch is addressed by PATH — that
# is the seam test-rebase.sh drives — while the formatter is declared
# per NAME. Paths are unique by construction: two repositories cannot share a directory,
# which is the same property the per-repository scratch worktree is named for. A caller
# that already holds the name should pass it rather than make this guess.
repo_name_at() {
    local p="${1:-}" home n
    [ -n "$p" ] || return 1
    home="$(spira_home_repo)"
    # SPIRA_REPO overrides the map for the home repo, so it must be consulted first or a
    # fixture — which has no map entry at all — resolves to nothing.
    if [ -n "${SPIRA_REPO:-}" ] && [ "$SPIRA_REPO" != "${SPIRA_REPO_DERIVED:-}" ] \
       && [ "$p" = "$SPIRA_REPO" ]; then printf '%s' "$home"; return 0; fi
    while IFS= read -r n; do
        [ -n "$n" ] || continue
        if [ "$(repo_field "$n" path)" = "$p" ]; then printf '%s' "$n"; return 0; fi
    done <<< "$(repo_names)"
    return 1
}

# spira_repos -> every repository this harness manages, one name per line.
#
# The home repo is ALWAYS first and always present, map or no map. A fixture that copies
# lib.sh next to nothing else has no repo-map, and a sentinel that then iterated zero
# repositories would land nothing while reporting a clean pass — the exact false-clean this
# whole file is written against.
spira_repos() {
    local home; home="$(spira_home_repo)"
    printf '%s\n' "$home"
    repo_names | grep -vx -- "$home" || true
}

bead_repo() {            # bead_repo <id> -> its repo name, or the home repo if it names none
    # bd state prints "(no repo state set)" when the dimension has never been written;
    # strip the sentinel before the fallback test.
    local id="$1" name
    name="$(bdq state "$id" repo 2>/dev/null)"
    case "$name" in '('*) name="" ;; esac
    printf '%s' "${name:-$(spira_home_repo)}"
}

# --------------------------------------------------------------------------------------
# THE BASE. Every branch this harness creates, and every branch it lands, is measured
# against the REMOTE-TRACKING ref of its repository's default branch — never the local one.
#
# Nothing in the harness ever advances the shared checkout's default branch. The sentinel
# lands by pushing `landing:<branch>` to the remote from the .landing worktree and never
# pulls the shared checkout, so the local ref there is however stale the last human left it. Every
# Spira bead edits the same handful of files under .claude/spira, and the queue is
# serialised at one aeon, so a branch cut from that stale ref collides with whatever landed
# while the previous aeon worked — by construction, every single time. Measured:
# sp-poison-retry based at 8ed6cca with main already at 5da8438, conflicting in sentinel.sh,
# lib.sh, gate.sh and log.md, none of which it had touched.
#
# THE DEFAULT BRANCH IS NOT `main`, AND ASSUMING IT IS BREAKS THREE THINGS AT ONCE. Measured
# across seven rows of a real repo-map: two had no ref named `main` anywhere, remote or local
# — their default is `master` — and a third had no remote named `origin` at all. For those
# three the old answer named a branch
# that does not exist, so `git worktree add -b spira/<id> "$WORK" "$BASE"` failed and no aeon
# could get a workspace in them at all; CHECK 6's rebase failed and REOPENED finished work
# with "does not rebase onto main"; and `gh pr create --base main` opened against nothing.
#
# So the answer is RESOLVED per repository, in this order, and a repository whose answer
# cannot be established is refused rather than guessed — the same way an unmapped `repo:`
# name is refused. `main` is a guess, and a guess here rebases somebody's work onto a branch
# nobody chose.
#
#   1. repo-map's `base` column. Declared beats derived: the two automatic sources below are
#      both local caches that can be stale, absent, or pointing at whatever branch a human
#      last checked out.
#   2. refs/remotes/origin/HEAD — what the remote said its default was, cached at clone time.
#   3. `git remote set-head <remote> --auto`, which ASKS the remote and caches the answer in
#      exactly the ref rung 2 reads, so it costs one round trip ever rather than one a pass.
#      Only reached when the map is silent and the cache is empty.
#   4. a repository with NO remote at all: its own current branch. Nothing can be stale
#      against a remote that does not exist, so HEAD is the only truth there is. This is the
#      test fixture's case.
#
# THE CHECKOUT'S CURRENT BRANCH IS NEVER CONSULTED FOR A REPOSITORY THAT HAS A REMOTE, and
# that restriction is load-bearing rather than fastidious: measured the same day, two of them
# sat on a DETACHED HEAD, one on a topic branch and another on
# `chore/keep-cf-access-probe`. Deriving the land ref from HEAD would have answered
# the remote form of that topic branch — a worse answer than the bug it replaced,
# because it names a ref that exists.
# --------------------------------------------------------------------------------------
spira_landref() {        # spira_landref [repo-path-or-name] -> the base ref, or non-zero
    local arg="${1:-}" name="" repo="" ref remote remotes
    case "$arg" in
        "")   name="$(spira_home_repo)" ;;
        */*)  repo="$arg" ;;
        *)    name="$arg" ;;
    esac
    if [ -z "$repo" ]; then repo="$(repo_root "$name")" || return 1; fi
    [ -n "$name" ] || name="$(repo_name_at "$repo" 2>/dev/null)" || name=""
    [ -e "$repo/.git" ] || return 1

    # 1 - declared. Verified to exist: a `base` naming a ref this checkout does not have is
    # the very defect being fixed, and shipping it would only move the guess into the map.
    #
    # A row with no base column at all answers empty here and falls through to resolution -
    # repo_field decides that on the row shape, which is the only thing that can tell a
    # missing column from a declared one.
    if [ -n "$name" ]; then
        ref="$(repo_field "$name" base 2>/dev/null)"
        if [ -n "$ref" ]; then
            git -C "$repo" rev-parse --verify -q "$ref" >/dev/null 2>&1 || return 1
            printf '%s' "$ref"; return 0
        fi
    fi

    # 2 - the remote's own declared default, as cached locally.
    ref="$(git -C "$repo" symbolic-ref -q --short refs/remotes/origin/HEAD 2>/dev/null)"
    if [ -n "$ref" ] && git -C "$repo" rev-parse --verify -q "$ref" >/dev/null 2>&1; then
        printf '%s' "$ref"; return 0
    fi

    remotes="$(git -C "$repo" remote 2>/dev/null)"

    # 3 - ask the remote once. `origin` if there is one, otherwise the single remote there
    # is; two unnamed remotes is a genuine ambiguity and is refused. set-head writes the ref
    # rung 2 reads, so this happens once per repository and not once per pass.
    if [ -n "$remotes" ]; then
        if grep -qx origin <<< "$remotes"; then remote=origin
        elif [ "$(wc -l <<< "$remotes")" = 1 ]; then remote="$remotes"
        else remote=""; fi
        if [ -n "$remote" ] \
           && git -C "$repo" remote set-head "$remote" --auto >/dev/null 2>&1; then
            ref="$(git -C "$repo" symbolic-ref -q --short "refs/remotes/$remote/HEAD" 2>/dev/null)"
            if [ -n "$ref" ] && git -C "$repo" rev-parse --verify -q "$ref" >/dev/null 2>&1; then
                printf '%s' "$ref"; return 0
            fi
        fi
        return 1
    fi

    # 4 - no remote at all: the repository's own current branch. Nothing can be stale
    # against a remote that does not exist, so HEAD is the only truth there is. This is the
    # test fixture's case, and the ONLY rung that consults a checkout's HEAD.
    ref="$(git -C "$repo" symbolic-ref -q --short HEAD 2>/dev/null)"
    if [ -n "$ref" ] && git -C "$repo" rev-parse --verify -q "$ref" >/dev/null 2>&1; then
        printf '%s' "$ref"; return 0
    fi
    return 1
}

# Splitting a land ref into its two halves. `origin/master` is what a branch is MEASURED
# against; `master` is what a push targets and what `gh pr create --base` wants; `origin` is
# what a fetch names. Every call site did these two strips by hand as ${base#origin/} and a
# literal `origin`, both of which are assumptions about a remote's name, and a remote need
# not be called that. String operations, not lookups, so a caller holding a ref never re-resolves it.
#
# A queue.local row's ref is `local/main` — a LOCAL branch that happens to contain a slash,
# not a remote-tracking one. Splitting on the first `/` unconditionally would read it as
# remote `local`, branch `main`, and every caller below would then try to fetch a remote
# that does not exist. So when a repo is given, the prefix only counts as a remote when
# `git remote` actually lists it; otherwise the ref is answered as local, same as one with no
# slash at all. Without a repo (a caller that predates this), the old unconditional split is
# kept.
ref_remote() {           # ref_remote <ref> [repo] -> its remote, or non-zero if the ref is local
    local ref="${1:-}" repo="${2:-}" prefix remotes
    case "$ref" in */*) prefix="${ref%%/*}" ;; *) return 1 ;; esac
    if [ -n "$repo" ]; then
        remotes="$(git -C "$repo" remote 2>/dev/null)"
        grep -qx -- "$prefix" <<< "$remotes" || return 1
    fi
    printf '%s' "$prefix"
}
ref_branch() {           # ref_branch <ref> -> the branch name, without any remote
    printf '%s' "${1#*/}"
}
qualify_base_ref() {     # qualify_base_ref <ref> <repo> -> refs/remotes/... or original
    # A bare origin/main is ambiguous when refs/heads/origin/main also exists. Use the
    # fully-qualified remote-tracking ref so git commands resolve it deterministically.
    local ref="$1" repo="$2" remote branch fq
    remote="$(ref_remote "$ref" "$repo")" || { printf '%s' "$ref"; return 0; }
    branch="$(ref_branch "$ref")"
    fq="refs/remotes/$remote/$branch"
    git -C "$repo" rev-parse --verify -q "$fq" >/dev/null 2>&1 \
        && printf '%s' "$fq" || printf '%s' "$ref"
}

# spira_landrefs <repo> -> the land ref, plus its local counterpart when that exists.
# The commit graph is read across BOTH, because a commit can be on the local branch and not
# yet pushed, or pushed and never pulled into this checkout. Two call sites asked this
# question with a literal `main` appended, which for a `master`-based repository added a ref that is not
# there and for a `master` repository omitted the only one that is. The strip is ${base#*/}
# and not ${base#origin/}: a remote need not be called `origin`, so stripping that literal
# leaves a ref like `upstream/master` unchanged and the local ref is silently never consulted.
spira_landrefs() {
    local repo="$1" base lo
    base="$(spira_landref "$repo")" || return 1
    printf '%s' "$base"
    lo="${base#*/}"
    if [ "$lo" != "$base" ] && git -C "$repo" rev-parse --verify -q "$lo" >/dev/null 2>&1; then
        printf ' %s' "$lo"
    fi
}

# spira_publish_forge <name> -> "<remote> <branch>": the forge target a queue.local
# repository's publish queue fast-forwards on a green publish PR. Never ref_remote of the
# land ref — under queue.local that ref is a bare local branch by design (spira_landref's
# own callers refuse a remote-tracking base for this mode), so the forge target cannot be
# derived from it and must be named instead. Defaults: the remote is "origin"
# (SPIRA_PUBLISH_REMOTE, per-repo SPIRA_PUBLISH_REMOTE_<NAME> overrides it — a remote need
# not be called origin); the branch is the land ref's own name with a leading "local/"
# stripped (SPIRA_PUBLISH_BRANCH_<NAME> overrides that).
spira_publish_forge() {
    local name="$1" base key remote branch
    base="$(spira_landref "$name" 2>/dev/null)" || return 1
    key="$(printf '%s' "$name" | tr 'a-z-' 'A-Z_')"
    local remote_var="SPIRA_PUBLISH_REMOTE_$key" branch_var="SPIRA_PUBLISH_BRANCH_$key"
    remote="${!remote_var:-${SPIRA_PUBLISH_REMOTE:-origin}}"
    branch="${!branch_var:-${base#local/}}"
    [ -n "$branch" ] || return 1
    printf '%s %s\n' "$remote" "$branch"
}

# --------------------------------------------------------------------------------------
# worktree_of <branch> [repo] -> the registered worktree path holding it, or empty.
# Ported to Rust (sp-9envm, wave 4.20): the library is `sending::reap`, called in-process
# by the `sending` binary itself; this is a one-line shim for bash callers.
# --------------------------------------------------------------------------------------
worktree_of() {
    sending worktree-of "$1" "${2:-$(repo_root)}"
}

# --------------------------------------------------------------------------------------
# hold_alive <pidfile> -> 0 if the recorded pid is a live process. Ported to Rust
# (sp-9envm); see `sending::reap::hold_alive` for the rationale (unlike aeon_alive this does
# NOT check argv, because the holder can be any process).
# --------------------------------------------------------------------------------------
hold_alive() {
    sending hold-alive "$1"
}

# --------------------------------------------------------------------------------------
# holder_alive <id> -> 0 if a live process is working this bead. Ported to Rust (sp-9envm);
# see `sending::reap::holder_alive` for the rationale (checks BOTH hold pidfiles and aeon
# pidfiles, by different liveness tests, satisfying the SAME predicate the reaper reads).
# --------------------------------------------------------------------------------------
holder_alive() {
    sending holder-alive "$1"
}

# ======================================================================================
# DESTRUCTION. Every removal of a bead's worktree or branch goes through this section, and
# nothing outside it may call `git worktree remove`, `git branch -D` or `rm -rf` on a tree.
#
# PORTED TO RUST (sp-9envm, wave 4.20: `sending::reap`, plus a `sending` CLI with
# `destroy-worktree`/`destroy-branch`/`reap-landed-branch`/`prune`/`salvage`/`witness`
# verbs). Every function below is now a one-line shim; the chokepoint itself, its full
# rationale (why it is one site, what it refuses and why), and its tests live there. This
# keeps working for the 50-odd bash sourcers unchanged — see sending/DESIGN.md §5 for what
# moved and sending/src/reap.rs's own header for the refusal list, reproduced from here.
# ======================================================================================
SPIRA_REAPLOG="${SPIRA_REAPLOG:-$SPIRA_RUN/reap.log}"

# The program that reached the chokepoint. Ported to Rust (sp-9envm); see
# `sending::reap::spira_caller`.
spira_caller() {
    sending caller
}

spira_reaplog() {        # spira_reaplog <verb> <id> <detail> — ported to Rust (sp-9envm)
    sending reaplog "$1" "$2" "${3:-}"
}

# queue_notify_concierge <name> <subject-suffix> <body> — mails the concierge mailbox as a
# machine event for a mutation the owner just made to an open batch (eject, rebuild,
# force-push, merge). spira-mail-deliver.sh watches every registered mailbox and wakes its
# reader the moment new mail lands (law-machine-events-wake-in-real-time), so the Concierge
# learns of it within seconds — never by polling the queue by hand, which is what "nobody
# was told" meant in practice before this existed.
queue_notify_concierge() {
    local name="$1" subject="$2" body="$3"
    printf '## Alert\n%s\n' "$body" \
    | mail send "${SPIRA_MAIL_SESSION_MAILBOX:-concierge}" \
        --from "Spira Queue <queue@spira>" \
        --subject "Merge queue: $name $subject" \
        --kind alert \
        >/dev/null 2>&1 || true
}

# queue_local_check_divergence <name> <repo> <forge-sha> <local-sha> -> 0 when forge-sha is
# an ancestor of local-sha, 1 otherwise — row 4 of the local/main design. Under queue.local
# the forge's main moves only by our own publishes; a forge-sha that is not an ancestor of
# local-sha means something pushed to it outside the publish queue. Mails the concierge
# naming the foreign commits (local-sha..forge-sha) the first time this exact forge-sha is
# seen, tracked in queue/<name>/divergence-alarmed — a repeated call against the SAME
# foreign tip (a retried publish, or a later round build before anyone has fixed it) is
# silent, so ONE alarm covers one divergence, not one per call. The marker clears the moment
# the check is healthy again, so a later, different divergence alarms anew. NEVER REBASES:
# this only detects and alarms, exactly as the design says. Callers decide what "stop
# publishing" means for them — cmd_publish refuses outright; cmd_land_local (no forge round
# trip belongs on its critical path) only alarms and lets the round build proceed.
queue_local_check_divergence() {
    local name="$1" repo="$2" forge_sha="$3" local_sha="$4"
    local statefile="${SPIRA_QUEUE_DIR:?}/$name/divergence-alarmed"
    if git -C "$repo" merge-base --is-ancestor "$forge_sha" "$local_sha" 2>/dev/null; then
        rm -f "$statefile" 2>/dev/null || true
        return 0
    fi
    local already=""
    [ -r "$statefile" ] && already="$(cat "$statefile" 2>/dev/null)"
    if [ "$already" != "$forge_sha" ]; then
        local foreign
        foreign="$(git -C "$repo" log --format='%h %s' "${local_sha}..${forge_sha}" 2>/dev/null)"
        mkdir -p "$(dirname "$statefile")" 2>/dev/null
        printf '%s\n' "$forge_sha" > "$statefile"
        queue_notify_concierge "$name" "divergence: forge is not an ancestor of local/main" \
            "$name's forge target ($forge_sha) is not an ancestor of local/main ($local_sha) — something pushed to the forge outside the publish queue. Foreign commit(s):"$'\n'"${foreign:-<none found>}"$'\n\n'"Publishing is refused until this is reconciled by hand. Never rebase silently."
    fi
    return 1
}

# --------------------------------------------------------------------------------------
# EVENTS — what the harness DID, in a form that survives the next repaint.
#
# Outcomes used to exist only as text. The health pane's RECENT line scraped three log files
# for landings, reopenings, poisonings, reclaims and claims and then `sort -r | head -4`, so
# everything below the fourth line was not aged out — it was never stored. "How many times
# did a bead reopen" was unanswerable without grepping a log that rotates.
#
# An event is a CLOSED `event` bead written through `$SPIRA_NOTIFY`, which is the one place
# that knows the shape: no labels at all, because an OPEN bead carrying `spira,plan` is
# claimable by an aeon and one carrying `overseer` lands in the queue of things awaiting the
# operator; `--kind` for the badge the panel renders; and `--target` for the bead the outcome
# happened TO, in its own column, so the stream filters per bead rather than per substring.
# That column comes back from `bd --json` as `target` — only `event_kind` keeps the prefix
# its flag carries, so a reader spelling it `event_target` gets null from a row that is
# populated and reads it as an emitter that never set it.
#
# THE RATE LIMIT IS PART OF THE EMITTER, NOT A LATER FIX. These fire on a two-minute timer
# and a reclaim storm is real. One event per attempt would bury every pilgrimage.complete in
# the same view, and a stream nobody can read is the log line it replaced. So a (kind, target)
# pair emits at most once per SPIRA_EVENT_COOLDOWN; repeats inside the window are COUNTED,
# not written, and the count rides out on the next event — "+26 more since 09:14Z" is the
# fact worth having about a retry loop, and one row is how it stays readable. A hot loop is
# a steady state, and a steady state is not news.
#
# SUPPRESSED IS NOT DROPPED, and that distinction is the reason the count is carried rather
# than the window simply being silent: a panel that renders a storm as one quiet row is a
# check reporting all-clear on the thing it exists to show.
#
# PER-KEY FILES, NOT ONE TABLE. aeon.sh, landing.sh and strand.sh emit from separate
# processes at the same time, and a read-modify-write of a shared table loses the OTHER
# pairs' counts under a race. One file per pair races only with itself, and the worst
# outcome of that race is one duplicate row.
# --------------------------------------------------------------------------------------
# Not in SPIRA_CONF_KEYS deliberately, alongside SPIRA_GHOST_GRACE and SPIRA_STRAND_GRACE
# in strand.sh: it is an environment knob with a working default, and every key added to
# that allowlist is a key the config file may then carry into a gate.
SPIRA_EVENT_COOLDOWN="${SPIRA_EVENT_COOLDOWN:-3600}"

spira_event() {          # spira_event <kind> <target|-> <title> [detail]
    local kind="${1:-}" target="${2:--}" title="${3:-}" detail="${4:-}"
    local dir="$SPIRA_RUN/events" key f now last=0 supp=0
    [ -n "$kind" ] && [ -n "$title" ] || return 1
    [ "$target" = "-" ] && target=""

    mkdir -p "$dir" 2>/dev/null || return 1
    key="$(printf '%s@%s' "$kind" "${target:-plan}" | tr -c 'a-zA-Z0-9._@-' '_')"
    f="$dir/$key"
    now="${SPIRA_NOW:-$(date -u +%s)}"
    find "$dir" -maxdepth 1 -type f -mmin +"$(( (SPIRA_EVENT_COOLDOWN * 2) / 60 + 1 ))" -delete 2>/dev/null
    [ -s "$f" ] && read -r last supp < "$f"
    case "${last:-}" in ''|*[!0-9]*) last=0 ;; esac
    case "${supp:-}" in ''|*[!0-9]*) supp=0 ;; esac

    if [ "$last" -gt 0 ] && [ "$(( now - last ))" -lt "$SPIRA_EVENT_COOLDOWN" ]; then
        printf '%s %s\n' "$last" "$(( supp + 1 ))" > "$f"
        return 0
    fi
    [ "$supp" -gt 0 ] \
        && title="$title (+$supp more since $(date -u -d "@$last" +%H:%MZ 2>/dev/null || echo 'the last one'))"
    printf '%s 0\n' "$now" > "$f"

    # Events are informational — they go to the event log, not the operator mailbox.
    # Operator asks (question/decision mails) are sent directly by the callers that have
    # the context to write them properly. Claims, reopens, landings, and similar transitions
    # belong in the log; the operator's mailbox holds only decisions.
    printf '%s\tkind: %s\ttarget: %s\t%s%s\n' \
        "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$kind" "${target:--}" "$title" \
        "${detail:+$(printf '\t%s' "$detail")}" \
        >> "$SPIRA_RUN/events.log" 2>/dev/null || true
    return 0
}

# The status witness, and the seam a suite drives it through. `--status-from` is the honest
# manual entry point too: it says exactly what the caller believes about each bead.
#
# SPIRA_STATUS_FILE, added by sp-9envm: a real FILE holding the same rows, for the `sending`
# binary's own `--status-from` (it has no bash array to read). A `-` caller's rows exist only
# on this function's stdin, so they are materialized to a temp file here; a real path is used
# as given. Every shim this file's now-ported chokepoint functions call threads this through
# with `${SPIRA_STATUS_FILE:+--status-from "$SPIRA_STATUS_FILE"}`, so a suite's status seam
# governs the Rust implementation exactly as it governed the bash one.
declare -A SPIRA_STATUS_MAP=()
SPIRA_STATUS_SEAM=0
SPIRA_STATUS_FILE=""
spira_status_seam() {    # spira_status_seam <file|-> — load the map once
    local sid sst src="$1"
    if [ "$src" = - ]; then
        src="$(mktemp "${TMPDIR:-/tmp}/spira-status-seam.XXXXXX")"
        cat > "$src"
    fi
    while IFS=$'\t' read -r sid sst; do
        [ -n "${sid:-}" ] && SPIRA_STATUS_MAP["$sid"]="${sst:-}"
    done < "$src"
    SPIRA_STATUS_SEAM=1
    SPIRA_STATUS_FILE="$src"
}

spira_bead_status() {    # <id> -> open|in_progress|blocked|closed|"" (unknown)
    if [ "$SPIRA_STATUS_SEAM" = 1 ]; then printf '%s' "${SPIRA_STATUS_MAP[$1]:-}"; return; fi
    bdjson show "$1" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print(""); sys.exit()
d = d if isinstance(d, list) else [d]
print(d[0].get("status", "") if d else "")' 2>/dev/null
}

# THE POSITIVE CONTROL for the status witness. An empty answer means "this bead is not in
# progress" only if the probe could have said otherwise; from a database that is down, every
# bead reads as free. Proved once per process by the store listing at least one bead — any
# row proves it can answer, as rebase-stale's own control does (sp-k6m1m: it used to show a
# configured goal bead, and `bd show` of a missing id prints `[]`, so it proved nothing) —
# and cached, because it gates a loop that runs every two minutes over seven repositories.
SPIRA_DB_OK=""
spira_db_reachable() {
    [ "$SPIRA_STATUS_SEAM" = 1 ] && return 0
    if [ -z "$SPIRA_DB_OK" ]; then
        if [ -n "$(bdjson list --all --limit 1 2>/dev/null | tr -d '[:space:]' | sed -n '/^\[{/p')" ]; then
            SPIRA_DB_OK=1
        else
            SPIRA_DB_OK=0
        fi
    fi
    [ "$SPIRA_DB_OK" = 1 ]
}

# spira_holder_witnesses <id> -> 0 and prints WHY somebody may be home; 1 if nobody is.
# Ported to Rust (sp-9envm); see `sending::reap::holder_witnesses`.
spira_holder_witnesses() {
    sending witness ${SPIRA_STATUS_FILE:+--status-from "$SPIRA_STATUS_FILE"} "$1"
}

# salvage <label> <worktree-path> -> 0 saved or nothing to save. Ported to Rust (sp-9envm);
# see `sending::reap::salvage` for the untracked-file-by-content and timestamped-filename
# rationale. $SALVAGED is dropped: nothing outside this file ever read it.
SALVAGED=""
salvage() {
    SALVAGED=""
    local _out _rc
    _out="$(sending salvage "$1" "$2")"; _rc=$?
    # `sending salvage` prints lib.sh's own old line verbatim on success-with-content
    # ("  salvaged uncommitted changes to <path>"), nothing when there was nothing to
    # save. $SALVAGED is kept for spira-world's slay, the one caller that reads it.
    case "$_out" in
        *"salvaged uncommitted changes to "*) SALVAGED="${_out#*salvaged uncommitted changes to }" ;;
    esac
    return "$_rc"
}

# spira_destroy_worktree <id> <path> <repo> <why> -> 0 removed or nothing to remove. Ported
# to Rust (sp-9envm); see `sending::reap::destroy_worktree`.
spira_destroy_worktree() {
    sending destroy-worktree ${SPIRA_STATUS_FILE:+--status-from "$SPIRA_STATUS_FILE"} "$1" "$2" "$3" "$4"
}

# spira_destroy_branch <id> <branch> <repo> <why> [caller] -> 0 gone, 1 refused or survived.
# Ported to Rust (sp-9envm); see `sending::reap::destroy_branch` for the CERTIFIED/BATCHED
# guard, the holder witnesses, the checked-out-worktree guard and the CALLER EXCEPTIONS
# ("sending"/"slain"/"archived"/anything else all skip the content fence; empty applies it).
spira_destroy_branch() {
    local _err; _err="$(sending destroy-branch ${SPIRA_STATUS_FILE:+--status-from "$SPIRA_STATUS_FILE"} "$1" "$2" "$3" "$4" "${5:-}")"
    local _rc=$?
    SPIRA_DESTROY_ERR="$_err"
    return $_rc
}

# spira_reap_landed_branch <id> <branch> <repo> <why> [caller] -> 0 sent, 1 failed, 2 refused
# (certified-queued or the content fence). Sets SPIRA_REAP_ERR on 1 or 2. Ported to Rust
# (sp-9envm); see `sending::reap::reap_landed_branch`. Still resolves the land ref and its
# remote itself (through the unchanged `spira_landref`/family-W seam the `sending` binary
# already has, not through bash), so `bead_close_on_land` — the production hot path for
# every landing, still bash — keeps working unchanged.
spira_reap_landed_branch() {
    local _err; _err="$(sending reap-landed-branch ${SPIRA_STATUS_FILE:+--status-from "$SPIRA_STATUS_FILE"} "$1" "$2" "$3" "$4" "${5:-}")"
    local _rc=$?
    SPIRA_REAP_ERR="$_err"
    return $_rc
}

# spira_prune_worktrees <repo> — `git worktree prune`, with the one case it gets wrong.
# Ported to Rust (sp-9envm); see `sending::reap::prune_worktrees` for the repair-not-prune
# rationale (an entry whose directory still exists would otherwise free its branch and
# leave a live tree registered nowhere).
spira_prune_worktrees() {
    sending prune "$1"
}

# --------------------------------------------------------------------------------------
# format_rebased <branch> <onto> <worktree> [repo-name] -> 0 always; the rebase stands
# whatever the formatter does.
#
# A REBASE PRODUCES A TREE NOBODY FORMATTED. git replays hunks; it does not re-run anyone's
# formatter on the result, so a rebase that resolves perfectly still hands the required
# check a tree that no human or tool ever laid out. It recurs on exactly the shape a rebase
# is best at — two branches adding names to the same import list, struct literal or match
# arm — where each side is individually well-formed and the union is over the line limit.
# The branch then fails `cargo fmt --all -- --check`, a check it passed before the harness
# touched it, and the failure is charged to the aeon that wrote correct code.
#
# ONLY WHAT THE BRANCH TOUCHED IS COMMITTED. The declared command is repository-wide, because
# that is the writing form of the repository-wide check it must satisfy — but a repository
# whose main is already unformatted would otherwise have its entire tree swept into one
# bead's branch. Against a clean main this restriction changes nothing, since a rebase can
# only disturb the layout of files the branch itself touched; against a dirty one it is the
# difference between a format commit and a rewrite.
#
# A FORMATTER THAT FAILS CHANGES NOTHING. `cargo fmt` exits non-zero on a tree it cannot
# parse, and it may have rewritten half of it first. Discard and let the gate render the
# verdict — a formatter is a convenience, and it must never be able to turn a clean rebase
# into a branch full of partial edits.
# --------------------------------------------------------------------------------------
format_rebased() {
    local br="$1" onto="$2" wt="$3" name="${4:-}" cmd paths f staged=0

    [ -n "$name" ] || name="$(repo_name_at "$(git -C "$wt" rev-parse --show-toplevel 2>/dev/null)" 2>/dev/null)" || return 0
    cmd="$(repo_format "$name" 2>/dev/null)"
    [ -n "$cmd" ] || return 0

    # The formatter sees what a gate command sees and nothing else: an ambient variable that
    # can change a formatter's output changes what lands (law-gates-run-in-a-clean-environment).
    # ~/.cargo/bin for the same reason gate.sh names it — lib.sh's PATH is written for
    # systemd and carries no toolchain.
    if ! ( cd "$wt" && env -i PATH="$HOME/.cargo/bin:$PATH" HOME="$HOME" TERM=dumb \
             timeout "${SPIRA_FORMAT_TIMEOUT:-300}" bash -c "$cmd" ) >/dev/null 2>&1; then
        log "format: $name's formatter failed on $br — leaving the rebase unformatted"
        git -C "$wt" checkout -q -- . 2>/dev/null
        return 0
    fi

    # The branch's own files, read from history rather than from the dirty tree: $onto is an
    # ancestor now, so this diff IS the branch's work. Filtered to paths that still exist,
    # because a path the branch deleted cannot have been reformatted and `git add` on it is
    # an error rather than a no-op.
    paths=()
    while IFS= read -r -d '' f; do
        [ -f "$wt/$f" ] && paths+=("$f")
    done < <(git -C "$wt" diff -z --name-only "$onto" HEAD 2>/dev/null)
    [ "${#paths[@]}" -gt 0 ] && git -C "$wt" add -- "${paths[@]}" 2>/dev/null

    # Everything the formatter touched outside the branch's own work goes back. Staged paths
    # are restored from the index, so this only discards the repository-wide remainder.
    git -C "$wt" checkout -q -- . 2>/dev/null
    git -C "$wt" diff --cached --quiet 2>/dev/null || staged=1
    [ "$staged" = 1 ] || return 0

    # The subject names the bead, because for `spira/<id>` branches ${br##*/} IS the id and
    # that string is the only machine-checkable link between a bead and the commit graph
    # (law-aeon-commits-name-their-bead). Through stdin, never an argument: a formatter
    # command containing backticks or $( ) would otherwise be executed by the very quoting
    # that was meant to quote it (law-commit-messages-via-stdin).
    git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" commit -q -F - <<EOF 2>/dev/null
spira: re-format ${br##*/} after rebase onto $onto

The rebase replayed cleanly and nothing re-ran $name's formatter on the result, so
the tree its own check tests was machine-produced. Formatted with: $cmd
EOF
    log "format: re-formatted $br after its rebase onto $onto"
    return 0
}

# --------------------------------------------------------------------------------------
# rebase_branch <branch> <onto> [repo] [repo-name] -> 0 if <branch> now contains <onto>, 1
# if it does not. On failure the branch ref is left EXACTLY as it was and $REBASE_CONFLICTS
# names the paths that collided. On success, and only when commits were actually replayed,
# the repository's own formatter runs on the result and is committed as part of the rebase —
# see format_rebased. The branch tip therefore MOVES on success, and a caller holding a tip
# from before the call is holding a stale one.
#
# THE CALLER MUST HAVE ESTABLISHED THAT NO LIVE AEON HOLDS THE BRANCH. This rewrites
# commits beneath a working tree; doing that under a running aeon destroys work in flight,
# which is the one failure here that is not recoverable. `holder_alive` is the precondition.
#
# WHY THE BRANCH'S OWN WORKTREE. git refuses to move a ref that a worktree has checked out
# — `git branch -f` and `git rebase` both — so when a worktree holds the branch it is the
# only place the rebase can happen. When nothing holds it the rebase still needs SOME
# working tree, and that tree must never be the shared checkout, whose HEAD an interactive
# session is using; a detached scratch worktree costs one checkout.
#
# A rebase is refused by tracked modifications, and those are routine rather than
# exceptional here: wiki/tasks.md is a GENERATED file tracked in git and rewritten by a
# timer, so it is dirty in every worktree within minutes of its creation and would
# otherwise block every rebase for a reason that has nothing to do with the work. Tracked
# changes are salvaged to a patch and discarded; untracked files are left alone, because
# `git diff HEAD` cannot carry their content and discarding them would destroy the one copy.
#
# A FAILURE IS NAMED, BECAUSE ONLY ONE OF THEM IS THE BRANCH'S FAULT. Every way this can
# return 1 used to look the same to a caller — one exit status and an empty $REBASE_CONFLICTS
# — so a caller that reopens a bead on a rebase failure reopened it for a missing ref, an
# unresolvable base and a scratch tree it could not build, all with the words "conflicts in
# unknown". That is a lie about a bead and it costs a session:
#
#   21:51:17  landed spira/<id>            <- pass A lands it
#   21:52:13  landing: starting a pass     <- pass B reads the branch list, <id> still in it
#   21:52:36  REMOVED branch spira/<id>    <- the Sending reaps it
#   22:00:55  reopened <id> — does not rebase onto origin/main; conflicts in unknown
#
# Pass B held an eight-minute-old list, reached a ref that was gone, and this function said
# "1" about it. $REBASE_FAILURE now says which:
#
#   conflict      the rebase RAN and the commits disagree — the branch's own fault, and the
#                 only value on which finished work may be put back on the board
#   no-branch     the ref is gone: reaped, landed, or slain under a stale list
#   no-base       the ref it lands on does not resolve
#   no-worktree   no tree to replay in
#
# The last three are the pass failing to ask the question, never an answer to it.
# --------------------------------------------------------------------------------------
REBASE_CONFLICTS=""
REBASE_FAILURE=""
REBASE_REFUSED_REASON=""
rebase_branch() {
    local br="$1" onto="$2" repo="${3:-$(repo_root)}" name="${4:-}" wt scratch rc=0
    REBASE_CONFLICTS=""; REBASE_FAILURE=""; REBASE_REFUSED_REASON=""
    # The repo NAME, for the formatter that runs on the result. Derived from the path only
    # when the caller did not supply it — both real callers hold it already, having read it
    # off the bead, and a derived value is a convention that breaks the moment two names
    # point at one checkout.
    [ -n "$name" ] || name="$(repo_name_at "$repo" 2>/dev/null)" || name=""

    git -C "$repo" rev-parse --verify -q "$onto" >/dev/null 2>&1 || { REBASE_FAILURE=no-base; return 1; }
    git -C "$repo" show-ref --verify -q "refs/heads/$br" || { REBASE_FAILURE=no-branch; return 1; }
    # Already current. This is the common case once branches are cut from the base ref, and
    # it is what makes running the rebase on every landing pass cheap.
    git -C "$repo" merge-base --is-ancestor "$onto" "refs/heads/$br" 2>/dev/null && return 0

    wt="$(worktree_of "$br" "$repo")"
    if [ -z "$wt" ]; then
        # PER REPOSITORY. One shared `.rebase` tree is registered against exactly one
        # repository, so a second repo asking for it gets a checkout of somebody else's
        # history — or, worse, a `git worktree add` that fails because the directory is
        # already a worktree of another repo, and a rebase that silently never happens.
        # Named for the checkout's own directory, which is unique by construction: two
        # repositories cannot share a path.
        scratch="$SPIRA_RUN/worktree/.rebase.$(basename "$repo")"
        if [ ! -e "$scratch/.git" ]; then
            mkdir -p "$(dirname "$scratch")"
            # Through the chokepoint: a bare prune here would silently unregister any tree
            # whose `.git` link is broken, including a live aeon's, and free its branch.
            spira_prune_worktrees "$repo" >/dev/null 2>&1
            git -C "$repo" worktree add -q --detach "$scratch" "$onto" >/dev/null 2>&1 \
                || { REBASE_FAILURE=no-worktree; return 1; }
        fi
        git -C "$scratch" checkout -q --detach >/dev/null 2>&1
        # A ref that vanished between the check above and here — the reaper runs on its own
        # timer — is still `no-branch`, not a tree we could not build. The distinction is the
        # whole point of naming these, so the narrower window gets the narrower name.
        if ! git -C "$scratch" checkout -q -B "$br" "refs/heads/$br" >/dev/null 2>&1; then
            git -C "$repo" show-ref --verify -q "refs/heads/$br" \
                && REBASE_FAILURE=no-worktree || REBASE_FAILURE=no-branch
            return 1
        fi
        wt="$scratch"
    fi

    if ! git -C "$wt" diff --quiet HEAD 2>/dev/null; then
        # `reset --hard`, not `checkout -- .`: a file STAGED for addition is not restored by
        # checkout, and `git rebase` refuses outright on "your index contains uncommitted
        # changes". reset --hard clears index and tracked worktree together and leaves
        # untracked files exactly where they are.
        salvage "${br##*/}-prerebase" "$wt" >/dev/null
        git -C "$wt" reset -q --hard HEAD 2>/dev/null
    fi

    local _rebase_err
    _rebase_err="$(mktemp)"
    if ! git -C "$wt" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" rebase -q "$onto" >/dev/null 2>"$_rebase_err"; then
        # Name the collisions BEFORE aborting; after the abort there is nothing to read.
        REBASE_CONFLICTS="$(git -C "$wt" diff --name-only --diff-filter=U 2>/dev/null | tr '\n' ' ')"
        REBASE_CONFLICTS="${REBASE_CONFLICTS% }"
        git -C "$wt" rebase --abort >/dev/null 2>&1
        # A non-zero rebase with no unmerged files is not a content conflict — git refused
        # outright (untracked file collision, locked index, etc.). Only a real content conflict
        # may reopen a finished bead; a refusal is the pass failing to ask the question.
        if [ -n "$REBASE_CONFLICTS" ]; then
            REBASE_FAILURE=conflict
        else
            REBASE_FAILURE=rebase-refused
            REBASE_REFUSED_REASON="$(head -1 "$_rebase_err" 2>/dev/null)"
        fi
        rc=1
    else
        # THE REBASE ACTUALLY REPLAYED COMMITS, so the tree is machine-produced and nobody
        # formatted it. This is the only path that reaches here: the already-an-ancestor case
        # returned above without touching anything, and a formatter run on a branch nothing
        # rewrote would be a diff the harness invented.
        format_rebased "$br" "$onto" "$wt" "$name"
    fi
    rm -f "$_rebase_err"

    # Let go of the branch. A scratch tree still holding it is not inert: `git branch -D`
    # refuses a branch a worktree has checked out, which is exactly the defect sending.sh
    # exists to fix, and it would arrive here by a new route.
    if [ "$wt" = "${SPIRA_RUN}/worktree/.rebase.$(basename "$repo")" ]; then
        git -C "$wt" checkout -q --detach >/dev/null 2>&1
    fi
    return $rc
}

# recut_onto — move a branch onto a new base by cherry-picking commits one by one.
#
# Unlike rebase (which stops entirely on the first conflict), this advances the branch
# as far as the commits allow: clean commits are applied, and the first conflicting one
# is noted in RECUT_CONFLICTS. When at least one commit lands, the branch ref is
# force-updated to that commit so the merge-base moves forward. When zero commits land
# the branch ref is left unchanged — moving it to the new base would strip all work and
# leave a trivially-clean branch that the next pass certifies without any content.
# Returns 0 if all commits applied cleanly, 1 if any conflict remains.
recut_onto() {
    local br="$1" onto="$2" repo="${3:-$(repo_root)}" name="${4:-}" scratch old_base rc=0 _cp_err new_tip
    RECUT_CONFLICTS=""; RECUT_APPLIED_COUNT=0
    [ -n "$name" ] || name="$(repo_name_at "$repo" 2>/dev/null)" || name=""
    scratch="$SPIRA_RUN/worktree/.rebase.$(basename "$repo")"
    if [ ! -e "$scratch/.git" ]; then
        mkdir -p "$(dirname "$scratch")"
        spira_prune_worktrees "$repo" >/dev/null 2>&1
        git -C "$repo" worktree add -q --detach "$scratch" "$onto" >/dev/null 2>&1 \
            || { RECUT_CONFLICTS="no-worktree"; return 1; }
    fi
    git -C "$repo" rev-parse --verify -q "$onto" >/dev/null 2>&1 || { RECUT_CONFLICTS="no-base"; return 1; }
    git -C "$repo" show-ref --verify -q "refs/heads/$br" || { RECUT_CONFLICTS="no-branch"; return 1; }
    git -C "$repo" merge-base --is-ancestor "$onto" "refs/heads/$br" 2>/dev/null && return 0
    old_base="$(git -C "$repo" merge-base "$onto" "refs/heads/$br" 2>/dev/null)" || { RECUT_CONFLICTS="no-merge-base"; return 1; }
    git -C "$scratch" checkout -q --detach "$onto" >/dev/null 2>&1 || { RECUT_CONFLICTS="no-checkout"; return 1; }
    _cp_err="$(mktemp)"
    local commit count=0
    while IFS= read -r commit; do
        [ -n "$commit" ] || continue
        if ! git -C "$scratch" \
                -c "user.name=${SPIRA_GIT_NAME:-spira}" \
                -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" \
                cherry-pick "$commit" 2>"$_cp_err"; then
            RECUT_CONFLICTS="$(git -C "$scratch" diff --name-only --diff-filter=U 2>/dev/null | tr '\n' ' ')"
            RECUT_CONFLICTS="${RECUT_CONFLICTS% }"
            [ -n "$RECUT_CONFLICTS" ] || RECUT_CONFLICTS="$(head -1 "$_cp_err" 2>/dev/null)"
            git -C "$scratch" cherry-pick --abort >/dev/null 2>&1
            rc=1
            break
        fi
        count=$(( count + 1 ))
    done < <(git -C "$repo" rev-list --reverse "${old_base}..${br}" 2>/dev/null)
    rm -f "$_cp_err"
    RECUT_APPLIED_COUNT=$count
    new_tip="$(git -C "$scratch" rev-parse HEAD 2>/dev/null)"
    # Only move the branch when at least one commit landed on the new base.
    # With zero commits the branch has no work on the new base, and updating it
    # there strips all content — the next pass would see a trivially clean rebase
    # and certify an empty branch.
    [ "${RECUT_APPLIED_COUNT:-0}" -gt 0 ] && [ -n "$new_tip" ] && \
        git -C "$repo" update-ref "refs/heads/$br" "$new_tip" >/dev/null 2>&1
    git -C "$scratch" checkout -q --detach >/dev/null 2>&1
    return $rc
}


# --------------------------------------------------------------------------------------
# LIVE-AEON CHECK. promote.sh and systemd/install.sh both reset the production checkout,
# which rewrites aeon.sh and lib.sh in place. Running aeons are executing those files;
# an in-place reset disrupts them (law-replace-running-scripts-atomically). Both callers
# share this function so the check cannot drift between them.
#
# Returns the list of active aeon unit names for the current instance (one per line),
# or nothing when no aeons are running.
#
# Uses ${SPIRA_SYSTEMCTL:-systemctl}. Tests inject a mock via SPIRA_PATH, which conf.sh
# prepends to PATH so bare `systemctl` resolves to the mock without a variable override.
# --------------------------------------------------------------------------------------
spira_live_aeons() {
    local sc="${SPIRA_SYSTEMCTL:-systemctl}"
    "$sc" --user list-units --state=active --no-legend \
        "spira-aeon-*-${SPIRA_INSTANCE}.service" 2>/dev/null \
        | tr -s ' \t' '\n\n' \
        | grep -E "^spira-aeon-[^[:space:]]+-${SPIRA_INSTANCE}\.service$" | sort -u || true
}

# _prune_candidates retired with activate.sh (sp-jsnbm): release install-tarball's own
# prune (release/src/install.rs) replaces it; nothing else called this function.

LANDSTATE="${SPIRA_RUN}/landstate"
# Reasons written by the batch/queue eviction machinery. Only these warrant the eviction-race
# reopen in aeon.sh; no-rebase@*, gate and confine are landing.sh REDs with their own paths.
LAND_EVICTION_REASONS="ejected conflicts-with-base rebase-suite-red"

# _tsd_landing_event <id> <state> <tip> [reason]
# Best-effort: appends a landing-event row (run/tsd/) via tsd-write. Never affects the
# caller's exit status — an unbuilt or missing binary means the family stays unwritten, not
# that landing itself fails.
_tsd_landing_event() {
    local id="$1" state="$2" tip="${3:-none}" reason="${4:-}"
    if [ -n "$reason" ]; then
        tsd-write --family landing-event --root "${SPIRA_RUN:-}" \
            --field-str "bead=$id" --field-str "state=$state" --field-str "tip=$tip" \
            --field-str "reason=$reason" >/dev/null 2>&1 || true
    else
        tsd-write --family landing-event --root "${SPIRA_RUN:-}" \
            --field-str "bead=$id" --field-str "state=$state" --field-str "tip=$tip" \
            >/dev/null 2>&1 || true
    fi
}

# _tsd_kv_field "<k=v k=v ...>" <key> -> the value for <key>, or "?" when absent. Reads the
# space-separated key=value string session_result_fields (below) already built — never a
# second pass over the trace it was parsed from.
_tsd_kv_field() {
    local kv
    for kv in $1; do
        case "$kv" in "$2="*) printf '%s' "${kv#*=}"; return 0 ;; esac
    done
    printf '?'
}

# _tsd_aeon_session <bead> <fayth> <rc> <status> <fields> — appends this session's
# aeon-session row (run/tsd/), reusing the fields session_result_fields already computed
# for ledger_done's own ledger line. Best-effort, like _tsd_landing_event.
_tsd_aeon_session() {
    local bead="$1" fayth="$2" rc="$3" status="$4" fields="$5"
    tsd-write --family aeon-session --root "${SPIRA_RUN:-}" \
        --field-str "bead=$bead" --field-str "fayth=$fayth" \
        --field "rc=$rc" --field-str "status=$status" \
        --field "wall_s=$(_tsd_kv_field "$fields" wall_s)" \
        --field "api_s=$(_tsd_kv_field "$fields" api_s)" \
        --field "turns=$(_tsd_kv_field "$fields" turns)" \
        --field "cost_usd=$(_tsd_kv_field "$fields" cost_usd)" \
        >/dev/null 2>&1 || true
}

# _tsd_slots_sample <fragment-file> — appends the slots family's row (run/tsd/) from a
# successful slots probe's own fragment (collect.sh). Best-effort, like every tsd producer
# here. Reads the fragment directly, never the merged cockpit.env: the fragment is this
# probe's own fresh sample, and the merge's first-wins rule can otherwise repeat a stale one.
_tsd_slots_sample() {
    local frag="$1" k v
    local live="?" ceiling="?" lanes_live="?" ready="?" paused="?"
    while IFS='=' read -r k v; do
        case "$k" in
            SP_SLOTS_LIVE)            live="$v" ;;
            SP_SLOTS_CEILING)         ceiling="$v" ;;
            SP_SLOTS_LANES_LIVE)      lanes_live="$v" ;;
            SP_SLOTS_READY)           ready="$v" ;;
            SP_SLOTS_CAPACITY_PAUSED) paused="$v" ;;
        esac
    done < "$frag"
    tsd-write --family slots --root "${SPIRA_RUN:-}" \
        --field "live=$live" --field "ceiling=$ceiling" --field "lanes_live=$lanes_live" \
        --field "ready=$ready" --field "capacity_paused=$paused" \
        >/dev/null 2>&1 || true
}

# THE WHITELIST IS THE GUARANTEE, the same reason _tsd_round_phase's now-retired whitelist
# existed (sp-27hsi): these four are the classes sp-6vd2s defines and no others belong in
# this family.
_TSD_ESCAPE_CLASSES=" mapping_gap gate_gap environment_gap flake "

# _tsd_escape <member> <suite> <class> [batch_id] — appends one escape record (run/tsd/
# escape): a round red attributed to <member> on <suite>, classified per sp-6vd2s
# (escape-classify.sh). Best-effort, like every tsd producer here.
_tsd_escape() {
    local member="$1" suite="$2" class="$3" batch_id="${4:-}"
    case "$_TSD_ESCAPE_CLASSES" in *" $class "*) ;; *) return 0 ;; esac
    tsd-write --family escape --root "${SPIRA_RUN:-}" \
        --field-str "member=$member" --field-str "suite=$suite" --field-str "class=$class" \
        --field-str "batch_id=$batch_id" \
        >/dev/null 2>&1 || true
}

land_mark() {    # land_mark <id> <state> <tip> [reason] [extra]
    mkdir -p "$(dirname "$LANDSTATE/$1")" 2>/dev/null || return 0
    printf '%s %s %s %s' "$2" "${3:-none}" "$(date +%s)" "${4:-}" \
        > "$LANDSTATE/$1.$$" 2>/dev/null
    [ -n "${5:-}" ] && printf ' %s' "$5" >> "$LANDSTATE/$1.$$" 2>/dev/null
    mv -f "$LANDSTATE/$1.$$" "$LANDSTATE/$1" 2>/dev/null
    local rc=$?
    _tsd_landing_event "$1" "$2" "${3:-}" "${4:-}"
    return "$rc"
}

land_state() {   # land_state <id> -> "<state> <tip> <at> [reason]" or empty
    local f="$LANDSTATE/$1"
    [ -r "$f" ] || return 1
    tr -d '\n' < "$f" 2>/dev/null
}


queue_certified_list() {
    local br id f st tip epoch
    git -C "$1" for-each-ref --format='%(refname:short) %(objectname)' 'refs/heads/spira/*' \
        2>/dev/null \
    | while read -r br _; do
        id="${br#spira/}"
        f="$SPIRA_RUN/landstate/$id"
        [ -f "$f" ] || continue
        st=""; { read -r st tip epoch _ < "$f"; } 2>/dev/null || [ -n "$st" ] || continue
        [ "$st" = "CERTIFIED" ] || continue
        printf '%s %s %s\n' "$id" "$tip" "$epoch"
    done
}

# queue_cancel_branch_runs <forge> <repo-dir> <branch> [<log-tag>]
# Cancels every non-completed Gate run on <branch> and logs each attempt to
# landing.log. GitHub does not cancel a workflow run when its PR closes, and
# each batch branch is a fresh spira/queue/<stamp>, so the gate-${ref}
# concurrency group has no earlier run on that branch to collide with and
# cancel for free — closing the PR must cancel the run itself.
# A failed cancel is logged loudly (stderr) rather than swallowed: the run
# stays non-completed and its PR stays closed, so the next abandon retries it. The old
# lib.sh orphan-run sweep (queue_sweep_orphan_runs, a periodic backstop for a run orphaned
# some other way — a hand-closed PR, or one left over from before this cancel existed) was
# retired dead at sp-27hsi: nothing called it, bash or Rust. No in-process replacement
# exists; file one if the plan still wants that backstop.
queue_cancel_branch_runs() {
    local forge="$1" repo="$2" branch="$3" tag="${4:-QUEUE}"
    [ -n "$branch" ] || return 0
    local run_id status rc=0
    while read -r run_id status; do
        [ -n "$run_id" ] || continue
        if "$forge" run-cancel "$repo" "$run_id" >/dev/null 2>&1; then
            printf '%s RUN_CANCEL %s branch=%s run=%s status=%s\n' \
                "$tag" "$(date +%s)" "$branch" "$run_id" "$status" \
                >> "${SPIRA_RUN:-/tmp}/landing.log" 2>/dev/null || true
        else
            rc=1
            printf '%s RUN_CANCEL_FAILED %s branch=%s run=%s status=%s\n' \
                "$tag" "$(date +%s)" "$branch" "$run_id" "$status" \
                >> "${SPIRA_RUN:-/tmp}/landing.log" 2>/dev/null || true
            printf 'spira: WARN failed to cancel run %s for %s — will retry\n' \
                "$run_id" "$branch" >&2
        fi
    done < <("$forge" runs-for-branch "$repo" "$branch" 2>/dev/null)
    return $rc
}

# queue_is_suite_transition <repo-path> <tip> <base-sha>
# 0 if the tip modifies SPIRA_SUITE_STATE_FILE relative to base-sha.
queue_is_suite_transition() {
    git -C "$1" diff --name-only "$3" "$2" 2>/dev/null \
        | grep -qF "${SPIRA_SUITE_STATE_FILE:-spira/suite-state}"
}

# queue_sort_rows <repo-path> <base-sha>
# Read "<id> <tip> <epoch>" lines from stdin; write sort-key rows sorted by batcher order:
#   "<express_flag> <prio_pad> <trans_flag> <epoch_pad> <id> <tip>"
# Set PRIO_JSON env to a bdjson array for priority (and label) lookups (defaults to []).
# Sort order: express first (flag=0), then priority asc. A spira/suite-state transition
# (flag=0) is only a tiebreaker within a priority class (sp-ihxa0: a suite-state edit going
# stale is already handled at cut time by the conflict check, not by cutting it first).
# Epoch asc breaks any tie still remaining. This is the canonical batcher sort used by both
# batch.sh and the cockpit.
#
# EXPRESS RANKS FIRST, AHEAD OF EVERYTHING ELSE (sp-ebx8b). batch.sh's express trigger only
# guarantees a cut HAPPENS when an express bead is certified; without an express key here,
# the sort could still leave that bead out of the cut it triggered, behind older same-priority
# beads at BATCH_MAX.
#
# PRIO_JSON NEVER REACHES A CHILD PROCESS. Callers pass the full `bd show --json` of every
# certified bead, and at 40 beads that was 266 KiB -- past Linux's 128 KiB limit on a single
# environment string. Every exec in here then failed E2BIG, the sort ran under 2>/dev/null,
# and it returned zero rows: batch.sh cut nothing and the cockpit showed an empty queue for
# nine hours with 40 branches waiting. A cliff that the stall itself pushes the backlog
# further over (sp-m5iq3). So the payload goes to a file and is unset before the first exec,
# here in the callee, where no caller can reintroduce it.
#
# AND THE SORT FAILS OPEN. Ranking is an optimisation; dropping every row is the
# catastrophic outcome. If ranking breaks the rows come out unranked, and it says so.
queue_sort_rows() {
    local repo="$1" base_sha="$2"
    # Copied into an unexported local and unset BEFORE anything forks: mktemp is an exec
    # too, and the first version of this fix called it first and died of the same E2BIG.
    local _pj="${PRIO_JSON:-[]}" _pjf _out _rc
    unset PRIO_JSON
    _pjf="$(mktemp)" || return 1
    printf '%s' "$_pj" > "$_pjf"
    _pj=""

    local _elab="${SPIRA_EXPRESS_LABEL:-express}"

    local _id _tip _epoch _is_trans _buf=""
    while read -r _id _tip _epoch; do
        _is_trans=0
        queue_is_suite_transition "$repo" "$_tip" "$base_sha" && _is_trans=1 || true
        _buf="${_buf}${_id} ${_tip} ${_epoch%% *} ${_is_trans}"$'\n'
    done

    _out="$(printf '%s' "$_buf" | PRIO_FILE="$_pjf" EXPRESS_LABEL="$_elab" python3 -c "
import sys, json, os
try:
    with open(os.environ['PRIO_FILE']) as f: prios = json.load(f)
except Exception: sys.exit(1)
prios = prios if isinstance(prios, list) else [prios]
lbl = os.environ.get('EXPRESS_LABEL', 'express')
prio_map = {}
express_map = {}
for x in prios:
    if not isinstance(x, dict) or not x.get('id'): continue
    try: prio_map[x['id']] = int(x.get('priority', 9))
    except (TypeError, ValueError): prio_map[x['id']] = 9
    express_map[x['id']] = int(lbl in (x.get('labels') or []))
rows = []
for line in sys.stdin:
    parts = line.strip().split()
    if len(parts) < 4: continue
    bid, tip, epoch, is_trans = parts[0], parts[1], int(parts[2]), int(parts[3])
    rows.append((1 - express_map.get(bid, 0), prio_map.get(bid, 9), 1 - is_trans, epoch, bid, tip))
rows.sort()
for r in rows:
    print('%d %09d %d %010d %s %s' % r)
")"
    _rc=$?
    rm -f "$_pjf"

    if [ "$_rc" -ne 0 ] || { [ -z "$_out" ] && [ -n "$_buf" ]; }; then
        printf 'queue_sort_rows: ranking failed (rc=%s) -- returning rows unranked\n' "$_rc" >&2
        printf '%s' "$_buf" | awk 'NF >= 4 { printf "%d %09d %d %010d %s %s\n", 1, 9, 1 - $4, $3, $1, $2 }'
        return 0
    fi
    [ -n "$_out" ] && printf '%s\n' "$_out"
    return 0
}

# gh_issue_closeout — comment and close the GitHub issue linked to a landed bead.
#
# The write-back complement to gh-intake's one-way ingest. Intake holds no
# credential; this runs only from the credentialed landing path. The comment
# cites commit sha and subject — both public on the repo — and a link. No bead
# notes, bodies or internal judgement reach the public tracker
# (law-beads-is-never-public).
#
# Idempotent: a closed issue is recorded and skipped; $SPIRA_RUN/gh-closed/<id>
# prevents a second attempt even if the issue is re-opened.
gh_issue_closeout() {  # gh_issue_closeout <bead-id> <landed-sha> <repo-path>
    local id="$1" sha="$2" repo_path="$3"
    local ext_ref gh_part gh_repo issue_n sha_short subject comment_body st
    local closed_mark="${SPIRA_RUN:?}/gh-closed/$id"

    [ -e "$closed_mark" ] && return 0

    ext_ref="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
d = d if isinstance(d, list) else [d]
if d: print(d[0].get("external_ref") or "")' 2>/dev/null)" || ext_ref=""

    case "${ext_ref:-}" in github:*) ;; *) return 0 ;; esac

    gh_part="${ext_ref#github:}"
    gh_repo="${gh_part%%#*}"
    issue_n="${gh_part##*#}"
    case "$issue_n" in
        ''|*[!0-9]*) log "gh-closeout $id: malformed external_ref $ext_ref — skipping"; return 0 ;;
    esac

    st="$(ghq issue view "$issue_n" --repo "$gh_repo" --json state -q .state 2>/dev/null)" \
        || st=""
    if [ "${st:-}" = CLOSED ]; then
        mkdir -p "${SPIRA_RUN}/gh-closed" 2>/dev/null || true
        : > "$closed_mark"
        return 0
    fi

    sha_short="$(git -C "$repo_path" rev-parse --short "$sha" 2>/dev/null)" \
        || sha_short="${sha:0:7}"
    subject="$(git -C "$repo_path" log --format='%s' -1 "$sha" 2>/dev/null)" || subject=""

    comment_body="$(printf 'Fixed in %s%s\n\nhttps://github.com/%s/commit/%s' \
        "$sha_short" "${subject:+ ($subject)}" "$gh_repo" "$sha")"

    if ghq issue comment "$issue_n" --repo "$gh_repo" --body "$comment_body" >/dev/null 2>&1 \
    && ghq issue close   "$issue_n" --repo "$gh_repo"                        >/dev/null 2>&1
    then
        mkdir -p "${SPIRA_RUN}/gh-closed" 2>/dev/null || true
        : > "$closed_mark"
        log "gh-closeout $id: closed $ext_ref as $sha_short"
    else
        log "gh-closeout $id: could not comment or close $ext_ref"
    fi
}

# bead_is_work_type <issue-type> -> 0 if it is one of SPIRA_WORK_CLOSE_TYPES (task bug
# feature by default) — the types a builder's own close is converted to submitted instead
# of left closed (aeon.sh, at session teardown). Non-code types (spike, ask, insight,
# investigation, event, chore, epic) close by the agent's own hand, unchanged.
bead_is_work_type() {
    local t="$1"
    [ -n "$t" ] || return 1
    case " ${SPIRA_WORK_CLOSE_TYPES:-task bug feature} " in
        *" $t "*) return 0 ;;
        *) return 1 ;;
    esac
}

# bead_close_on_land — the only place a work bead is closed for a landed reason.
#
# A builder's own close of a work bead is converted back to open carrying
# SPIRA_SUBMITTED_LABEL instead of staying closed (aeon.sh, at session teardown, once the
# close has already happened — not a PreToolUse hook refusing the tool call); this
# closes it for real once the commit is actually on the base, citing the sha. Called from
# every LANDED land_mark site, right beside gh_issue_closeout.
#
# Idempotent both ways: a bead already closed is left alone, and a bead never marked
# submitted (an older-style direct close, or a non-code type) is left alone too — this is
# not the only path that closes a bead, only the landing path for the new one.
#
# REAPS THE BRANCH AND WORKTREE HERE TOO (sp-jci6o), the same verified deletion sending.sh
# uses (spira_reap_landed_branch), so a bead closed by any landing path — push, pr, hold or
# queue — loses its worktree and branch the moment it is known landed rather than waiting
# for the next per-pass Sending scan to rediscover it by ancestry. Best-effort: the caller's
# own repo:/branch: labels resolve the branch, spira_destroy_branch's content fence refuses
# if that branch's diff is somehow not on the repository's base, and either kind of miss is
# still caught by the Sending, which remains the backstop for everything this cannot reach.
bead_close_on_land() {   # bead_close_on_land <bead-id> <landed-sha>
    local id="$1" sha="${2:-}"
    local st repo_label br_label
    read -r st repo_label br_label <<< "$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
d = d if isinstance(d, list) else [d]
if not d: raise SystemExit(0)
row = d[0]
labs = row.get("labels") or []
submitted = sys.argv[1] in labs
repo = next((l[5:] for l in labs if l.startswith("repo:")), "-")
br = next((l[7:] for l in labs if l.startswith("branch:")), "-")
st = row.get("status") or "-"
if st != "closed" and submitted: st = "submitted"
print(f"{st} {repo} {br}")
' "${SPIRA_SUBMITTED_LABEL:-spira-submitted}" 2>/dev/null)"
    [ -n "${st:-}" ] || return 0
    [ "$st" = closed ] && return 0
    [ "$st" = submitted ] || return 0
    if bdq close "$id" --reason-file - <<REASON >/dev/null 2>&1
OUTCOME: landed
Closed by the landing pass: work landed at ${sha:-unknown} (law-closed-is-not-landed).
REASON
    then
        log "land-close $id: closed at ${sha:-unknown} (submitted -> landed)"
        land_mark "$id" LANDED "$sha" "Closed by landing pass"
    else
        log "land-close $id: bd close failed — left submitted, CHECK 5 will report it"
        return 0
    fi
    if [ "$repo_label" != "-" ] && [ "$br_label" != "-" ]; then
        local _rp
        if _rp="$(repo_root "$repo_label" 2>/dev/null)" \
           && git -C "$_rp" show-ref --verify -q "refs/heads/$br_label" 2>/dev/null; then
            if spira_reap_landed_branch "$id" "$br_label" "$_rp" "landed at ${sha:-unknown}"; then
                log "land-close $id: reaped branch $br_label"
            else
                log "land-close $id: branch $br_label not reaped: ${SPIRA_REAP_ERR:-unknown} — left for the Sending"
            fi
        fi
    fi
}

# _gh_close_ask_unblock — backfill: convert any blocking "Close GitHub issue" ask
# for a work bead to dep relate. One log line per conversion.
_gh_close_ask_unblock() {  # _gh_close_ask_unblock <subject> <work-bead-id>
    local _subj="$1" _id="$2" _ask_id _blocks
    [ -n "${SPIRA_DB:-}" ] || return 0
    _ask_id="$(bdjson list --status open \
        --label "${SPIRA_ASK_LABEL:?SPIRA_ASK_LABEL is unset — source conf.sh}" --limit 0 2>/dev/null \
        | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
rows = d if isinstance(d, list) else [d]
want = sys.argv[1]
for r in rows:
    if want == (r.get("title") or ""):
        print(r.get("id", ""))
        break
' "$_subj" 2>/dev/null)"
    [ -z "$_ask_id" ] && return 0
    _blocks="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$_id" --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
d = d if isinstance(d, list) else [d]
ask = sys.argv[1]
deps = d[0].get("dependencies") or []
print("yes" if any(
    (dep.get("dependency_type") or "") == "blocks"
    and (dep.get("id") or "") == ask
    for dep in deps
) else "")' "$_ask_id" 2>/dev/null)"
    [ "$_blocks" != "yes" ] && return 0
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" dep remove "$_id" "$_ask_id" >/dev/null 2>&1 || true
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" dep relate "$_ask_id" "$_id" >/dev/null 2>&1 || true
    log "gh-closeout $_id: converted blocking ask $_ask_id to relates_to"
}

# gh_issue_ask_unlanded — ask the operator what to do about a GitHub issue whose
# bead closed without a commit landing on the base branch.
#
# One ask per issue, deduped through ask_already_open while it is still open — and
# through ask_closed_subject once he has answered it (writes gh-closed/<id> so
# answering sticks). Wired with dep relate, never dep add, so a reopened bead is
# not stranded behind the ask.
gh_issue_ask_unlanded() {  # gh_issue_ask_unlanded <bead-id> <external-ref> [draft]
    local id="$1" ext_ref="$2" draft="${3:-}"
    local _subj _dflt gh_part gh_repo issue_n _st _err _rc _answered

    gh_part="${ext_ref#github:}"
    gh_repo="${gh_part%%#*}"
    issue_n="${gh_part##*#}"
    case "$issue_n" in ''|*[!0-9]*) return 0 ;; esac

    local closed_mark="${SPIRA_RUN:?}/gh-closed/$id"
    [ -e "$closed_mark" ] && return 0

    # Issue already closed on the forge: write the marker so future scans skip it.
    _st="$(ghq issue view "$issue_n" --repo "$gh_repo" --json state -q .state 2>/dev/null)" \
        || _st=""
    if [ "${_st:-}" = CLOSED ]; then
        mkdir -p "${SPIRA_RUN}/gh-closed" 2>/dev/null || true
        : > "$closed_mark"
        log "gh-closeout $id: $ext_ref already closed on forge — skipping"
        return 0
    fi

    _subj="Close GitHub issue $ext_ref for bead $id"
    # Backfill: if an existing ask blocks this work bead, convert to relates_to.
    _gh_close_ask_unblock "$_subj" "$id"
    ask_already_open "$_subj" && return 0

    _answered="$(ask_closed_subject "$_subj")"
    if [ -n "$_answered" ]; then
        mkdir -p "${SPIRA_RUN}/gh-closed" 2>/dev/null || true
        : > "$closed_mark"
        log "gh-closeout $id: ask $_answered already answered — marker written, no re-ask"
        return 0
    fi

    _dflt="${draft:-post a comment explaining the resolution and close the issue}"

    _err="$(mail send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" \
        --bead "$id" <<MAILEOF 2>&1 >/dev/null
## Question
$_subj

## Default
${_dflt}

$id was closed without a commit landing on the base branch, but it links to GitHub issue $ext_ref which is still open.

Suggested public reply: "${_dflt}"
MAILEOF
    )"; _rc=$?
    if [ "$_rc" -eq 0 ]; then
        log "gh-closeout $id: asked operator about $ext_ref"
    elif [ -n "${_err:-}" ]; then
        log "gh-closeout $id: ask refused (${_err})"
    else
        log "gh-closeout $id: ask refused — probe fault: mail produced no reason"
    fi
}

# _gh_resolve_stale_asks — an open "Close GitHub issue" ask whose issue is now CLOSED
# (by gh_issue_closeout above, or by a human directly) is an answered question still
# sitting in the operator's queue. ask_already_open only checks whether one is open;
# nothing else ever closed it (law-close-the-loop-on-confirmation).
_gh_resolve_stale_asks() {
    local _tmp _ask_id _ext _bid _gh_part _gh_repo _issue_n _st
    _tmp="$(mktemp)" || return 0
    bdjson list --status open --label "${SPIRA_ASK_LABEL:?SPIRA_ASK_LABEL is unset — source conf.sh}" --limit 0 2>/dev/null \
        | python3 -c '
import sys, json, re
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
rows = d if isinstance(d, list) else [d]
pat = re.compile(r"^Close GitHub issue (\S+) for bead (\S+)$")
for r in rows:
    m = pat.match(r.get("title") or "")
    if not m: continue
    aid = r.get("id", "")
    if not aid: continue
    print(f"{aid}\t{m.group(1)}\t{m.group(2)}")
' 2>/dev/null > "$_tmp" || { rm -f "$_tmp"; return 0; }

    while IFS=$'\t' read -r _ask_id _ext _bid; do
        [ -n "$_ask_id" ] || continue
        _gh_part="${_ext#github:}"
        _gh_repo="${_gh_part%%#*}"
        _issue_n="${_gh_part##*#}"
        case "$_issue_n" in ''|*[!0-9]*) continue ;; esac
        _st="$(ghq issue view "$_issue_n" --repo "$_gh_repo" --json state -q .state 2>/dev/null)" || _st=""
        [ "${_st:-}" = CLOSED ] || continue
        mkdir -p "${SPIRA_RUN}/gh-closed" 2>/dev/null || true
        : > "${SPIRA_RUN}/gh-closed/$_bid"
        printf '%s is closed on GitHub — resolved automatically; nothing further for the operator.\n' "$_ext" \
            | bdq close "$_ask_id" --reason-file - >/dev/null 2>&1
        log "gh-closeout $_bid: $_ext found closed — resolved stale ask $_ask_id"
    done < "$_tmp"
    rm -f "$_tmp"
}

# _gh_unlanded_scan — run at the end of a landing pass to ask about GitHub issues
# whose beads closed without a landing. Called once per pass; one ask per issue
# via ask_already_open (and ask_closed_subject once answered).
#
# THE GRAPH IS CONSULTED BEFORE THE LANDSTATE FILE, NEVER THE OTHER WAY. landstate is
# a record of the last branch seen for a bead id, and a second, later branch for the
# SAME id that goes RED against the base overwrites a correct LANDED entry with a
# wrong one — the file then contradicts the commit graph rather than merely lagging
# it. landed_sha() answers the only question that matters — is a commit naming this
# id an ancestor of the repository's own land ref, for the bead or for its superseder —
# and when it can, that answer wins over whatever landstate says.
_gh_unlanded_scan() {
    local _tmp _id _ext _superseder _repo_label _repo_path _land_sha _closed_at
    local _ls_file _ls_st _sup_ls _sup_st _sup_sha _draft
    local _wait_dir _wait_file _now _last _age _grace

    _gh_resolve_stale_asks

    _tmp="$(mktemp)" || return 0
    _wait_dir="${SPIRA_RUN:-/tmp}/gh-wait-log"
    # \x01-SEPARATED, NOT TAB. bash's `read` treats tab as IFS WHITESPACE regardless of
    # what IFS is set to, so a run of them — an empty field followed by a non-empty one,
    # e.g. no superseder but a repo: label — collapses and every field after the gap
    # shifts left. \x01 is not whitespace to `read`, so an empty field stays a field.
    bdjson list --all --limit 0 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
rows = d if isinstance(d, list) else [d]
for r in rows:
    if r.get("status") != "closed": continue
    ext = r.get("external_ref") or ""
    if not ext.startswith("github:"): continue
    bid = r.get("id", "")
    if not bid: continue
    superseder = ""
    for dep in (r.get("dependencies") or []):
        if (dep.get("dependency_type") or dep.get("type")) == "supersedes":
            superseder = dep.get("id") or dep.get("blocked_by") or ""
            break
    repo_label = ""
    for l in (r.get("labels") or []):
        if l.startswith("repo:"):
            repo_label = l[5:]; break
    ca = r.get("closed_at") or ""
    print(f"{bid}\x01{ext}\x01{superseder}\x01{repo_label}\x01{ca}")
' 2>/dev/null > "$_tmp" || { rm -f "$_tmp"; return 0; }

    while IFS=$'\x01' read -r _id _ext _superseder _repo_label _closed_at; do
        [ -n "$_id" ] || continue
        [ -e "${SPIRA_RUN}/gh-closed/$_id" ] && continue

        _repo_path="$(repo_root "$_repo_label" 2>/dev/null)" || _repo_path=""
        if [ -n "$_repo_path" ]; then
            _land_sha="$(landed_sha "$_id" "$_repo_path" 2>/dev/null)"
            if [ -n "$_land_sha" ]; then
                gh_issue_closeout "$_id" "$_land_sha" "$_repo_path" || true
                continue
            fi
            if [ -n "${_superseder:-}" ]; then
                _land_sha="$(landed_sha "$_superseder" "$_repo_path" 2>/dev/null)"
                if [ -n "$_land_sha" ]; then
                    gh_issue_closeout "$_id" "$_land_sha" "$_repo_path" || true
                    continue
                fi
            fi
        fi

        # Repo unresolvable, or the commit graph plainly does not have it: landstate is
        # the fallback, not the first word — a cache can be stale in the other direction
        # too (written LANDED for a squash whose subject grep missed), but only when the
        # commit graph itself could not be asked.
        _ls_file="$SPIRA_RUN/landstate/$_id"
        _ls_st=""
        [ -r "$_ls_file" ] && { read -r _ls_st _ < "$_ls_file" 2>/dev/null || true; }
        [ "${_ls_st:-}" = LANDED ] && continue

        # In-flight: commit is on its way; ask only when it genuinely needs attention.
        case "${_ls_st:-}" in
            CERTIFIED|BATCHED|GATED|REBASED|CONTENT)
                _wait_file="$_wait_dir/$_id"
                _now="$(date +%s)"
                _last=""
                [ -f "$_wait_file" ] && { read -r _last _ < "$_wait_file" 2>/dev/null || true; }
                if [ -z "${_last:-}" ] || [ "$(( _now - _last ))" -gt 3600 ]; then
                    log "gh-closeout $_id: $_ext in flight (${_ls_st}) — waiting on landing"
                    mkdir -p "$_wait_dir" 2>/dev/null || true
                    printf '%s\n' "$_now" > "$_wait_file"
                fi
                continue
                ;;
        esac

        _draft=""
        if [ -n "${_superseder:-}" ]; then
            _sup_ls="$SPIRA_RUN/landstate/$_superseder"
            if [ -r "$_sup_ls" ]; then
                _sup_st=""; _sup_sha=""
                read -r _sup_st _sup_sha _ < "$_sup_ls" 2>/dev/null || true
                [ "${_sup_st:-}" = LANDED ] \
                    && _draft="This issue was fixed by $_superseder (${_sup_sha:0:8})"
            fi
        fi

        # GRACE PERIOD. Closing the bead and landing its commit are separate passes; a
        # bead closed a moment ago has simply not had its turn yet, and asking about it
        # immediately is the same false alarm as trusting a stale landstate — just on a
        # clock instead of a cache. Only once no landing has shown up for a while does
        # "closed, no commit on the base" become a fact worth the operator's attention
        # rather than a timing artifact.
        _grace="${SPIRA_GH_ASK_GRACE_SECS:-3600}"
        if [ -n "${_closed_at:-}" ]; then
            _now="$(date -u +%s)"
            _last="$(date -u -d "$_closed_at" +%s 2>/dev/null)" || _last=""
            if [ -n "$_last" ]; then
                _age=$(( _now - _last ))
                [ "$_age" -lt "$_grace" ] && continue
            fi
        fi

        gh_issue_ask_unlanded "$_id" "$_ext" "${_draft:-}" || true
    done < "$_tmp"
    rm -f "$_tmp"
}

# spira_git_push <repo> [push-args...] — push with GitHub App identity when configured.
# When SPIRA_GH_APP_ID and SPIRA_GH_APP_INSTALLATION_ID are set, routes the push over
# HTTPS using the App installation token as the credential, so pushes are attributed to
# the App rather than to the operator's SSH key.
spira_git_push() {
    local repo="$1"; shift
    if [ -n "${SPIRA_GH_APP_ID:-}" ] && [ -n "${SPIRA_GH_APP_INSTALLATION_ID:-}" ]; then
        git -C "$repo" \
            -c "credential.helper=!git-credential-app.sh" \
            -c "url.https://github.com/.insteadOf=git@github.com:" \
            push "$@"
    else
        git -C "$repo" push "$@"
    fi
}
