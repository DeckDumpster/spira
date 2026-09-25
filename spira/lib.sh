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
                bash "$(dirname "${BASH_SOURCE[0]}")/czar-fence.sh" "${SPIRA_CZAR_CLASS}" || return 1 ;;
            update|close)
                if [ "${2:-}" != "${SPIRA_CZAR_TRIGGER_BEAD:-__none__}" ]; then
                    bash "$(dirname "${BASH_SOURCE[0]}")/czar-fence.sh" "${SPIRA_CZAR_CLASS}" || return 1
                fi ;;
        esac
    fi
    timeout "${BD_TIMEOUT:-180}" "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"
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
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    ask_already_open "$br cannot be judged" && return 0
    local _subj="$br cannot be judged: $outcome x$n in a row ($reason)"
    local _dflt="raise the budget or clear the contention this reason names, then let the next pass take it; if it is not obvious, run \`$SPIRA_HOME/gate.sh $br $repo\` by hand and read the whole output"
    local _why="$outcome means the machinery could not reach a verdict — the branch has NOT been judged and has NOT been charged, and $id is not at fault. It has now failed to be judged $n times, so this is no longer a queue clearing itself. Nothing on $br can land until a verdict is reached, and every other branch of $repo is behind the same fault."
    local _ev; _ev="$(printf '%s' "$out" | tail -20)"
    "$SPIRA_HOME/mail.sh" send operator \
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

# spira_ask_rebase_loop — escalate a bead whose rebase keeps failing.
#
# Seven reopens on sp-dvlq, each one handing the next aeon "resolve the conflict" against a
# branch whose correct resolution was "drop it". The repetition is the signal: a bead that
# cannot rebase N times in a row is not learning from the reopen, and repeating it is
# machinery cycling on itself (law-alerts-must-be-actionable at the machinery level).
spira_ask_rebase_loop() {  # <bead> <branch> <repo-name> <requeue-count> <conflicts> <other-beads> [<repo-dir> <base>]
    local id="$1" br="$2" name="$3" n="$4" conflicts="$5" others="$6"
    local repo_dir="${7:-}" base_ref="${8:-}"
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    ask_already_open "$br rebase loop" && return 0
    # Fetch bead title and status so the escalation names the work and its current state.
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
    # Subject: title first so the operator knows what the work is (law-escalations-lead-with-the-bead).
    local _subj
    if [ -n "$bead_title" ]; then
        _subj="${bead_title}: $br rebase loop x$n in $name"
    else
        _subj="$br rebase loop x$n in $name"
    fi
    # Default: no empty slots — omit the duplicate clause when others is empty.
    local _dflt
    if [ "${nfiles:-0}" -ge "${SPIRA_REBASE_DECOMPOSE_FILES:-4}" ]; then
        _dflt="split $br into smaller beads by file/deliverable and land those independently, rather than rebasing the whole thing by hand again"
    elif [ -n "$others" ]; then
        _dflt="check whether $br is a duplicate of $others and close it if so; if the work is genuinely new, rebase by hand and push"
    else
        _dflt="rebase $br by hand and push, or close it if the work is already landed"
    fi
    # Extra lines for the body: status and branch info.
    local _extra=""
    [ -n "$bead_status" ] && _extra="Status: ${bead_status}."
    if [ -n "$tip_short" ] && [ -n "$ahead" ]; then
        _extra="${_extra:+$_extra$'\n'}Branch: ${tip_short} (${ahead} commit(s) ahead of ${base_ref})."
    fi
    "$SPIRA_HOME/mail.sh" send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_subj

## Default
$_dflt

$id has been reopened for a rebase conflict $n times and the loop is not converging. Conflicts in: ${conflicts:-unknown}.$ctx$decompose_ctx

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
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    ask_already_open "$br red recurring $reason_class" && return 0
    local elapsed_h=0
    [ "${first_epoch:-0}" -gt 0 ] && \
        elapsed_h=$(( ( $(date +%s) - first_epoch ) / 3600 ))
    local _subj="$br red recurring: $reason_class twice on $id in $name"
    local _dflt="investigate why $br cannot land ($reason_class); close the bead if the work is superseded, or rebase by hand if the root cause is external"
    "$SPIRA_HOME/mail.sh" send operator \
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
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    ask_already_open "$br rebase refused" && return 0
    local _subj="$br rebase refused in $name: $reason"
    local _dflt="fix the infrastructure; $id stays closed and its branch will land on the next pass"
    "$SPIRA_HOME/mail.sh" send operator \
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
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    ask_already_open "$br budget-deferred" && return 0
    local _subj="$br budget-deferred: $n consecutive passes in $name"
    "$SPIRA_HOME/mail.sh" send operator \
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
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    ask_already_open "$id refresh cap" && return 0
    behind="$(git -C "$repo" rev-list --count "$br..$base" 2>/dev/null)" || behind="?"
    local _subj="Spira: $id's pull request has been rebased $n time(s) and still has not merged"
    local _dflt="reopen $id at P0 so an aeon owns the pull request's own failure, and leave the branch alone until it does"
    local _ev; _ev="$(printf 'BRANCH    %s in %s\nBASE      %s, %s commit(s) ahead of the branch\nREFRESHED %s time(s); the cap is %s\n\n%s\n' \
         "$br" "$name" "$base" "$behind" "$n" "${SPIRA_PR_REFRESH_MAX:-5}" "$(bead_context "$id")")"
    "$SPIRA_HOME/mail.sh" send operator \
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

# spira_ask_timeout_loop — escalate a bead that keeps timing out in a capped lane.
#
# A bead routed to a lane with FAYTH_TIMEOUT_SECONDS is killed when the cap expires. One
# timeout is expected for work that arrived labelled as an incident but carries more code than
# the 8-minute cap allows. N timeouts in a row is a loop: the cap is the wrong lane, and the
# right answer is either a persona with no cap, or splitting the work.
#
# Deduped on the bead id so the ask fires once per (id, timeout-count): a new timeout is new
# information even if a previous ask about the same bead was already closed.
spira_ask_timeout_loop() {  # <bead> <branch> <fayth> <cap-seconds> <timeout-count>
    local id="$1" br="$2" fayth="$3" cap="$4" n="$5"
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    ask_already_open "$id timed out $n" && return 0
    local _subj="$id timed out $n times in the $fayth lane (${cap}s cap)"
    local _dflt="move the bead to a persona with no cap (e.g. a builder) by replacing the 'incident' label with 'plan', or split the work into pieces that fit the lane"
    "$SPIRA_HOME/mail.sh" send operator \
        --from "Landing gate <gate@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$_dflt" <<MAILEOF >/dev/null 2>&1
## Question
$_subj

## Default
$_dflt

$br has been killed by the ${cap}s cap $n times without committing anything. This is the lane routing the bead to a wall it cannot finish inside, not a verdict about the approach. The work is neither wrong nor charged; it is stuck in a lane too short for it.
MAILEOF
}

# land_log_tail — last N lines of landing.log for escalation evidence.
#
# landing.log is PLAIN TEXT, so it is tailed with `tail` and not with trace_tail. trace_tail
# renders a stream-json session log and silently drops every line that does not start with
# `{` — pointed at this file it returns the empty string, and an escalation whose evidence
# section is empty is the same failure as no escalation at all, arriving with a reassuring
# shape (law-escalations-carry-their-evidence).
land_log_tail() {
    local f="$SPIRA_RUN/landing.log" n="${1:-30}"
    [ -r "$f" ] || { printf '(no landing log — the worker has never written one)'; return 0; }
    tail -n "$n" "$f" 2>/dev/null
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
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    echo "$now" > "$cd"
    local _subj="Spira is landing nothing — $why"
    local _dflt="run \`$SPIRA_HOME/landing.sh\` by hand to see the failure, then file the fix as a bead"
    "$SPIRA_HOME/mail.sh" send operator \
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
    grep -qF 'aeon.sh' <<< "$cmd" || return 1
    return 0
}

aeon_count() {           # how many aeons of a fayth are genuinely running
    local fayth="$1" n=0 pf
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
fayth_names() {          # every persona defined in the chamber, one per line
    local f n
    for f in "$SPIRA_HOME"/chamber/*.fayth; do
        [ -e "$f" ] || continue
        n="${f##*/}"; printf '%s\n' "${n%.fayth}"
    done
}

spira_fayths() {         # the personas this harness runs, space separated, IN PRIORITY ORDER
    # SPIRA_FAYTHS still overrides, because which personas a HOST runs is deployment
    # configuration; the default is every fayth present rather than one name.
    if [ -n "${SPIRA_FAYTHS:-}" ]; then printf '%s' "$SPIRA_FAYTHS"; return 0; fi
    # THE ORDER IS NOW LOAD-BEARING, so the default may not be alphabetical. The pool is
    # drawn down in this order, and `fayth_names` returned "builder ops" — which puts the
    # elastic persona that scales to fill the box AHEAD of the on-call one, exactly backwards.
    # Elastic personas sort last and everything else keeps its name order, so a host that
    # configures nothing still gets a sensible priority instead of an alphabetical accident.
    local f fixed="" elastic=""
    for f in $(fayth_names); do
        if [ "$(fayth_get "$f" FAYTH_ELASTIC 0)" = 1 ]
        then elastic="$elastic $f"
        else fixed="$fixed $f"
        fi
    done
    printf '%s' "${fixed# }${elastic:+ }${elastic# }"
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
    local f out=""
    for f in $(spira_fayths); do
        [ "$(fayth_get "$f" FAYTH_SUMMON auto)" = auto ] || continue
        [ "$(fayth_get "$f" FAYTH_ROLE task)" = party ] && continue
        [ -n "$(fayth_get "$f" FAYTH_LANE "")" ] && continue
        out="$out $f"
    done
    printf '%s' "${out# }"
}

# spira_operator_fayths -> the personas only a human starts. FAYTH_SUMMON is not `auto`.
#
# A PERSONA THE SENTINEL MUST NEVER SUMMON IS A DECLARED FACT, NOT AN ACCIDENT OF ITS SHAPE.
# The concierge is the case: it holds no partition, claims no bead and runs for days as the
# operator's Remote Control session, so every property that would have kept it out of the
# pool by side effect — an empty FAYTH_LABELS, an absent FAYTH_LANE — is a property somebody
# could reasonably change while meaning something else entirely, and the sentinel would then
# start summoning a session that has no bead to work and no deadline to end it.
#
# The cockpit's allocator taught this the same week, in miniature: it decided which sections
# were served first by testing `base == max`, and the day two sections were capped they
# satisfied that test by accident and starved a third. A property's MEANING and the shape it
# happens to have are two facts, and deriving one from the other breaks at the moment somebody
# changes the shape for an unrelated reason.
#
# IT READS THE CHAMBER, NOT `spira_fayths`, AND THAT IS THE ONE DIFFERENCE FROM EVERY LIST
# ABOVE. `spira_fayths` honours $SPIRA_FAYTHS, which is a HOST's declaration of which personas
# it runs — a deployment list, and a list of things that get SUMMONED. An operator persona is
# summoned by nobody, so whether this box runs a worker pool containing it is not a question
# that applies: the operator can start it whether or not the sentinel has ever heard of it.
#
# The practical consequence is the safe one. On a host whose $SPIRA_FAYTHS omits the concierge
# — which is every host today — the sentinel cannot summon it because it is not in the roster,
# AND `FAYTH_SUMMON` would stop it if it were. Two independent reasons, neither relying on the
# other, which is what is wanted for the one persona that must never be started by a machine.
spira_operator_fayths() {
    local f out=""
    for f in $(fayth_names); do
        [ "$(fayth_get "$f" FAYTH_SUMMON auto)" = auto ] && continue
        out="$out $f"
    done
    printf '%s' "${out# }"
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
    local f out=""
    for f in $(spira_fayths); do
        # THE SAME EXCLUSION AS THE TASK POOL, because the sentinel summons from BOTH lists
        # and a persona kept out of one is summoned by the other. A lane is how a persona gets
        # capacity of its own; FAYTH_SUMMON is whether anything may summon it at all.
        [ "$(fayth_get "$f" FAYTH_SUMMON auto)" = auto ] || continue
        [ -n "$(fayth_get "$f" FAYTH_LANE "")" ] && out="$out $f"
    done
    printf '%s' "${out# }"
}

SPIRA_ROSTER_WARN_STAMP="${SPIRA_ROSTER_WARN_STAMP:-$SPIRA_RUN/roster-warn.stamp}"

# A NARROWED ROSTER SAYS SO. SPIRA_FAYTHS is a legitimate host override — which personas a
# HOST runs is deployment configuration — but it is also the exact shape of the defect this
# section exists under: a fayth present in the chamber and absent from the roster is a persona
# that landed complete and will never run, and nothing about that looks wrong from the outside.
# It went unnoticed for a day. Name it instead.
#
# The warning fires once per change to the exclusion set, not once per pass. A warning that
# fires every pass on a deliberate configuration trains operators to skip the log
# (law-alerts-must-be-actionable).
roster_warnings() {      # roster_warnings <roster> -> a WARN line per fayth left out
    local roster=" $1 " f excluded stamp
    excluded=""
    for f in $(fayth_names); do
        grep -qw -- "$f" <<< "$roster" && continue
        excluded="$excluded $f"
    done
    excluded="${excluded# }"
    if [ -z "$excluded" ]; then
        rm -f "$SPIRA_ROSTER_WARN_STAMP"
        return 0
    fi
    stamp="$(printf '%s' "$excluded" | tr ' ' '\n' | sort | tr '\n' ' ')"
    [ "$(cat "$SPIRA_ROSTER_WARN_STAMP" 2>/dev/null)" = "$stamp" ] && return 0
    printf '%s' "$stamp" > "$SPIRA_ROSTER_WARN_STAMP"
    for f in $excluded; do
        log "WARN $f.fayth is in the chamber but not in SPIRA_FAYTHS — that persona will never be summoned here"
    done
    return 0
}

fayth_get() {            # fayth_get <fayth> <VAR> [default] -> one field of a fayth
    local f="$1" var="$2" def="${3:-}" F="$SPIRA_HOME/chamber/$1.fayth"
    [ -f "$F" ] || { printf '%s' "$def"; return 1; }
    # A SUBSHELL, always. Sourcing a fayth sets FAYTH_* in the caller, so reading two
    # personas in one loop without one leaves the second wearing the first's predicate —
    # the same defect this section exists to close, arriving by a different route.
    # shellcheck disable=SC1090
    ( . "$F" 2>/dev/null; eval "printf '%s' \"\${$var:-\$def}\"" )
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
# `--exclude-type epic` is not optional. The goal epic has no blockers, so it reads as ready
# and would be claimed and "implemented", which is not a thing an epic means.
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

# ready_count <labels> <exclude-labels> -> how many beads that predicate can claim.
ready_count() {
    bdq "${READY_ARGS[@]}" --label "$1" --exclude-label "$2" \
        --json 2>/dev/null | json_only | json_count
}

# check2_protect_waiting — protect IN_PROGRESS beads blocked solely on operator-ask deps
# from the time-based dead-worker reaper (CHECK 2 in sentinel.sh).
#
# An aeon that exits because its bead's only open dep carries the ask label is not a dead
# worker — it followed its contract: "record state and exit". Its lease goes stale and the
# time-based reaper fires, finds an IN_PROGRESS bead with a stale lease, and reclaims it.
# The next aeon re-derives the same diagnosis and exits, and the 180m loop repeats. The
# reclaim is wrong: sp-mfa4 hit it six times before this fix existed.
#
# The fix: mark qualifying beads with SPIRA_RECLAIM_SKIP_LABEL so the reaper's
# --exclude-label flag skips them. Remove the label when the dep closes, which lets the
# reaper reclaim the stale lease on that same pass — no latency, no polling.
#
# TWO STEPS, ONE PASS, BATCHED:
# (a) Beads already carrying the skip label: re-check. If no open dep still carries the
#     ask label, remove the skip label so the reaper can fire normally.
# (b) IN_PROGRESS beads with deps but no skip label: if ALL open deps carry the ask label
#     (and there is at least one), apply the skip label.
#
# CONSERVATIVE: only marks when EVERY open dep carries the ask label. One open dep without
# it means other blockers exist; the bead is handled by normal reclaim or claim paths, and
# protecting it would mask a genuine dead-worker case.
#
# BATCHED: one `bd list` + one `bd show` regardless of how many IN_PROGRESS beads exist.
# In practice there are only a few at a time, so this is cheap.
check2_protect_waiting() {
    local ask_label="$SPIRA_ASK_LABEL"
    local skip_label="$SPIRA_RECLAIM_SKIP_LABEL"

    # Collect IN_PROGRESS beads that need dep inspection.
    # Output: one line per bead: "<id> <has_skip:0|1> <dep_count>"
    local bead_lines
    bead_lines="$(bdjson list --all --status in_progress --limit 0 2>/dev/null \
        | python3 -c '
import sys, json
skip = sys.argv[1]
try: d = json.load(sys.stdin)
except: sys.exit(0)
for item in (d if isinstance(d, list) else [d]):
    labels = item.get("labels") or []
    dep_count = item.get("dependency_count", 0)
    has_skip = 1 if skip in labels else 0
    if has_skip or dep_count > 0:
        print(item["id"], has_skip, dep_count)
' "$skip_label" 2>/dev/null)"

    [ -n "$bead_lines" ] || return 0

    # Collect IDs for re-check and for candidate protection.
    local remove_ids=() add_ids=()
    local id has_skip dep_count
    while IFS=' ' read -r id has_skip dep_count; do
        [ -n "$id" ] || continue
        if [ "${has_skip:-0}" = 1 ]; then
            remove_ids+=("$id")
        elif [ "${dep_count:-0}" -gt 0 ]; then
            add_ids+=("$id")
        fi
    done <<< "$bead_lines"

    [ "${#remove_ids[@]}" -eq 0 ] && [ "${#add_ids[@]}" -eq 0 ] && return 0

    # One batch show call for all IDs that need dep inspection.
    local all_ids=("${remove_ids[@]}" "${add_ids[@]}")
    local show_json
    show_json="$(bdjson show "${all_ids[@]}" 2>/dev/null)"
    [ -n "$show_json" ] || return 0

    # Decide: add or remove the skip label.
    local decisions
    decisions="$(python3 -c '
import sys, json
ask, skip = sys.argv[1], sys.argv[2]
# Reconstruct the sets from newline-separated args 3 and 4.
remove_set = set(filter(None, sys.argv[3].split(","))) if sys.argv[3] else set()
add_set    = set(filter(None, sys.argv[4].split(","))) if sys.argv[4] else set()
try: d = json.load(sys.stdin)
except: sys.exit(0)
for item in (d if isinstance(d, list) else [d]):
    bid = item.get("id", "")
    deps = item.get("dependencies") or []
    open_deps    = [x for x in deps if x.get("status") != "closed"]
    ask_open     = [x for x in open_deps if ask in (x.get("labels") or [])
                and (x.get("dependency_type") or x.get("type")) == "blocks"]
    non_ask_open = [x for x in open_deps if ask not in (x.get("labels") or [])]
    if bid in remove_set and not ask_open:
        print("remove", bid)
    elif bid in add_set and open_deps and ask_open and not non_ask_open:
        print("add", bid)
' "$ask_label" "$skip_label" \
    "$(IFS=,; printf '%s' "${remove_ids[*]}")" \
    "$(IFS=,; printf '%s' "${add_ids[*]}")" \
    <<< "$show_json")"

    local action
    while IFS=' ' read -r action id; do
        [ -n "$id" ] || continue
        case "$action" in
            add)
                bdq label add "$id" "$skip_label" >/dev/null 2>&1 || true
                log "CHECK2 $id: only open dep(s) carry $ask_label — labeled $skip_label, excluded from reclaim"
                act "protected $id from reclaim: waiting on $ask_label dep"
                ;;
            remove)
                bdq label remove "$id" "$skip_label" >/dev/null 2>&1 || true
                log "CHECK2 $id: $ask_label dep no longer blocking — removed $skip_label, re-enters the reaper"
                act "unprotected $id: $ask_label dep closed"
                ;;
        esac
    done <<< "$decisions"
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
fayth_exclude() {        # fayth_exclude <fayth> -> comma-separated exclusions
    local me="$1" own="${2:-}" f out
    out="$own"
    [ -n "${SPIRA_QUEUE_WAIT_LABEL:-}" ] && out="${out:+$out,}${SPIRA_QUEUE_WAIT_LABEL}"
    for f in $(spira_fayths 2>/dev/null); do
        [ "$f" = "$me" ] && continue
        out="${out:+$out,}fayth:$f"
    done
    printf '%s' "$out"
}

fayth_ready() {          # fayth_ready <fayth> -> claimable beads under ITS OWN predicate
    local f="$1" F="$SPIRA_HOME/chamber/$1.fayth"
    [ -f "$F" ] || { printf '0'; return 1; }
    # shellcheck disable=SC1090
    ( . "$F" 2>/dev/null
      ready_count "${FAYTH_LABELS:-}" "$(fayth_exclude "$f" "${FAYTH_EXCLUDE_LABELS:-}")" )
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

# mark_queue_waiters — apply/remove SPIRA_QUEUE_WAIT_LABEL on beads whose closed blocker
# is in the queue pipeline (CERTIFIED or BATCHED) and has not yet reached LANDED.
#
# bd considers a dep resolved once the blocker is closed, so the dependent appears in
# `bd ready`. In queue mode, CLOSED ≠ LANDED — the work is not yet on base. This label
# keeps fayth_ready from counting those beads until the blocker's landstate reaches LANDED.
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
    local sf id state tip qblockers=""

    # Active queue blockers: CERTIFIED or BATCHED beads with a real commit tip.
    # tip="none" means no commit was recorded (design, diagnosis, superseded bead);
    # such a bead will never reach LANDED and is treated as already satisfied.
    if [ -d "$landstate_dir" ]; then
        for sf in "$landstate_dir/"*; do
            [ -f "$sf" ] || continue
            id="$(basename "$sf")"
            state="$(awk 'NR==1{print $1}' "$sf" 2>/dev/null)"
            tip="$(awk   'NR==1{print $2}' "$sf" 2>/dev/null)"
            case "$state" in
                CERTIFIED|BATCHED)
                    [ -n "$tip" ] && [ "$tip" != "none" ] \
                        && qblockers="${qblockers}${id} "
                    ;;
            esac
        done
    fi

    # Collect labeled beads and ready beads, then decide each bead's label state
    # once — no separate release and apply passes that can clear for one blocker
    # and re-apply for another in the same run.
    local labeled_json ready_json
    labeled_json="$(bdjson list --status open --label "$label" --limit 0 2>/dev/null)" \
        || labeled_json=""
    ready_json=""
    [ -n "$qblockers" ] \
        && { ready_json="$(bdjson "${READY_ARGS[@]}" 2>/dev/null)" || ready_json=""; }

    # labeled_json goes through the environment (typically a small set of beads);
    # ready_json goes through stdin to avoid the env-size ceiling on large partitions.
    QUEUE_BLOCKERS="$qblockers" QUEUE_LABEL="$label" \
    LABELED_JSON="${labeled_json:-[]}" python3 -c '
import json, sys, os

active = set(os.environ.get("QUEUE_BLOCKERS", "").split())
label  = os.environ["QUEUE_LABEL"]

def parse(s):
    if not s: return []
    try: d = json.loads(s); return d if isinstance(d, list) else [d]
    except Exception: return []

labeled = {b["id"]: b for b in parse(os.environ.get("LABELED_JSON", "")) if b.get("id")}
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
                log "mark_queue_waiters: $id — queue-wait applied"
                ;;
            remove)
                bdq label remove "$id" "$label" >/dev/null 2>&1 || true
                log "mark_queue_waiters: $id — blocker landed, cleared"
                ;;
        esac
    done
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

# bead_reopen <id> <cause> [note] — hand a bead back to the graph so the NEXT aeon can claim it.
#
# REOPENING IS NOT ENOUGH. `bd reopen` keeps the assignee, and `bd ready --claim` skips any
# bead that has one even though `bd ready` lists it — so a bead reopened by the landing
# pass (a rebase conflict, a red gate) or by the aeon's own closed-without-commit check went
# back into the graph wearing a dead aeon's name and was never claimed again. Seven sat that
# way for four to eight hours at P0 while aeons took P1 work around them, and every one of
# the 23 reopens the landing log holds had the same defect. Clearing the assignee is what
# makes a reopen a reopen; it is done here so no site can forget it.
#
# IT ALWAYS RETURNS 0, and that is load-bearing rather than sloppy. Callers reopen under
# `set -e`, and a bd that refuses either half would otherwise abort the caller partway —
# leaving a bead reopened with no record of WHY, so the next aeon reads an ordinary open
# bead and repeats whatever produced it. That is worse than not reopening at all. Each half
# is guarded for the same reason, and bd's refusal is reported on stderr where the harness
# log keeps it.
#
# <cause> is a stable slug (gate-red, rebase-conflict, closed-without-commit, …) written
# as a harness event row so census.sh can break sp-reopen into classified subclasses.
# It is written AFTER bd's own `reopened` event, under event_type='reopen', so the two
# rows are distinct and the census never double-counts a harness reopen.
bead_reopen() {
    local id="$1" cause="${2:-unrecorded}" note="${3:-}" rc=0
    bdq reopen "$id" >/dev/null 2>&1 || rc=1
    release_claim "$id" || rc=1
    _bump_write_event "$id" reopen "$cause" || rc=1
    [ -n "$note" ] && { bdq note "$id" "$note" >/dev/null 2>&1 || rc=1; }
    [ "$rc" = 0 ] || printf 'bead_reopen: %s — bd refused the reopen, the release or the note\n' "$id" >&2
    return 0
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
    bdq update "$id" --status open --assignee "" >/dev/null 2>&1
}

# orphan_claims [labels] -> "<id>\t<assignee>" for every bead holding a claim nobody works.
#
# The predicate is status=open AND an assignee AND no lease in the future. in_progress is
# deliberately NOT here: a live claim is `bd reclaim`'s to time out and strand.sh's to
# witness, and a third opinion about liveness is how work gets robbed mid-flight.
orphan_claims() {
    bdjson list --status open --limit 0 --label "${1:-${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan}" 2>/dev/null | python3 -c '
import sys, json, datetime
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
now = datetime.datetime.now(datetime.timezone.utc)
for i in (d if isinstance(d, list) else [d]):
    if not (i.get("assignee") or "").strip(): continue
    lease = i.get("lease_expires_at")
    if lease:
        try:
            if datetime.datetime.fromisoformat(str(lease).replace("Z", "+00:00")) > now: continue
        except Exception:
            continue   # unparseable is not evidence of death; leave it alone
    print("%s\t%s" % (i.get("id"), i.get("assignee")))' 2>/dev/null
}

release_orphan_claims() {   # release_orphan_claims [labels] -> a RELEASED line per bead freed
    local id who
    while IFS=$'\t' read -r id who; do
        [ -n "${id:-}" ] || continue
        # Report only what actually moved. An assign that lost a race to a real claim
        # returns non-zero and changes nothing, and counting it would be a check reporting
        # an action it did not take.
        release_claim "$id" && printf 'RELEASED\t%s\t%s\n' "$id" "$who"
    done < <(orphan_claims "${1:-${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan}")
    return 0
}

# fayth_free <fayth> [pool-remaining] -> free concurrency slots, never negative.
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
# in the party at all and the role exists only on paper. watchtower.sh is what keeps it
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
fayth_free() {           # fayth_free <fayth> [pool-remaining]
    local f="$1" pool="${2:-}" max have free
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
    have="$(aeon_count "$f")"
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
    local f l seen=""
    for f in $(spira_fayths); do
        l="$(fayth_get "$f" FAYTH_LABELS)"
        [ -n "$l" ] || continue
        case "$seen" in *"|$l|"*) continue ;; esac
        seen="$seen|$l|"
        printf '%s\t%s\n' "$l" "$(fayth_get "$f" FAYTH_EXCLUDE_LABELS)"
    done
    return 0
}

fayths_for_labels() {    # fayths_for_labels <labels> -> personas whose partition IS <labels>
    # For the question "is anything working THESE beads". Counting every live aeon would
    # let a running Ops aeon mask a genuinely starved plan, which is the same
    # one-predicate-for-every-persona defect seen from the other side.
    local want="$1" f
    for f in $(fayth_names); do
        [ "$(fayth_get "$f" FAYTH_LABELS)" = "$want" ] && printf '%s\n' "$f"
    done
    return 0
}

# summon_fayth <fayth> [pool-remaining] [require-label] -> 0 if an aeon was started, 1
# otherwise.
#
# require-label is passed to the aeon as SPIRA_REQUIRE_LABEL, which it adds to its own
# FAYTH_LABELS before claiming (aeon.sh). Set it only when the slot itself is restricted —
# an express grant, say — so the aeon summoned under it cannot claim a bead outside that
# restriction. A normal summon leaves it unset and claims under the fayth's own predicate
# exactly as before.
#
# THE STATUS IS THE ANSWER, not a word on stdout. A caller that captured the output to look
# for "summoned" would swallow the log lines below with it, and the sentinel's stdout IS the
# sentinel log — so the one pass that did something would be the one that explained itself
# least.
summon_fayth() {         # summon_fayth <fayth> [pool-remaining] [require-label]
    local f="$1" pool="${2:-}" require_label="${3:-}" r free
    # HALTED — world.sh stop writes this stamp; only world.sh start removes it.
    # Checked before drain: halt is indefinite and requires explicit operator action.
    if [ -f "${SPIRA_RUN:-}/world.halted" ]; then
        log "CHECK7 $f: halted — not summoning (world.sh start to lift)"
        return 1
    fi
    # DRAINING — the operator asked for an empty pool and is waiting on it. Checked FIRST,
    # ahead of capacity and readiness, because it is the only condition here a person is
    # actively blocked on: a rollout that must not kill work in flight — install.sh, a schema
    # change, swapping the checkout aeon.sh itself is read from — needs the pool to reach
    # zero, and it never does while summons continue.
    #
    # THE GATE IS HERE, NOT ON THE TIMER, and that is the whole design. Landing is a LEG of
    # the sentinel pass (sentinel.sh starts spira-landing) and not a timer of its own, so
    # stopping spira-sentinel.timer to halt summons also halts landing and strands every
    # finished branch — measured 2026-09-08, three branches unlanded across a 16-minute
    # hand-drain. Gate the spawn; leave the loop running.
    # A DRAIN EXPIRES, AND THE GATE IS WHAT EXPIRES IT. A drain is a held breath: right for
    # the minutes an operation needs, never for an hour. Whoever sets one can die before
    # lifting it, and on 2026-09-09 one did — an Ops sweep drained at 18:02:03, finished its
    # SOP at 18:04:29, exited without resuming, and the world sat gated for 59 minutes with
    # 25 beads ready and no aeons. Every pass logged "pass complete — goal reached" while it
    # happened, because a drain is a MODE and nothing treated the mode as a fault.
    #
    # LIFTING IT HERE IS LOUD, NEVER SILENT. An expiry that quietly resumed would hide the
    # forgotten resume, and the forgotten resume is the defect worth seeing.
    #
    # A STAMP WITH NO `expires` LINE IS TREATED AS EXPIRED AT stamp-mtime + TTL, so a drain
    # written by the older world.sh cannot wedge the loop forever either. Fail toward
    # summoning: one aeon summoned during an operation costs an attempt, and a stuck gate
    # costs the whole pipeline.
    _dstamp="${SPIRA_RUN:-}/world.draining"
    if [ -f "$_dstamp" ]; then
        _dexp="$(sed -n 's/^expires \([0-9][0-9]*\)$/\1/p' "$_dstamp" 2>/dev/null | head -1)"
        if [ -z "$_dexp" ]; then
            _dmt="$(stat -c %Y "$_dstamp" 2>/dev/null || echo 0)"
            _dexp=$(( _dmt + ${SPIRA_DRAIN_TTL:-1800} ))
        fi
        if [ "$(date +%s)" -ge "$_dexp" ]; then
            rm -f "$_dstamp"
            log "CHECK7 $f: DRAIN EXPIRED — lifting a drain nobody resumed (deadline $(date -d "@$_dexp" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || echo '?'), TTL ${SPIRA_DRAIN_TTL:-1800}s). Whoever drained did not resume; summons are live again."
        else
            log "CHECK7 $f: draining — not summoning (world.sh resume to lift)"
            return 1
        fi
    fi
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
        # while any lane has ready work and lanes are below their collective cap. This
        # inverts the old lane-last-slot rule: lanes are preferred when a slot is scarce,
        # so ops and groomer are not starved by builders under a steady task backlog.
        # Requires SPIRA_LANES_MAX_LIVE; without it, no preference applies.
        if [ -n "${SPIRA_LANES_MAX_LIVE:-}" ] && [ -z "$(fayth_get "$f" FAYTH_LANE "")" ]; then
            local inv_slots_free; inv_slots_free=$(( SPIRA_MAX_LIVE_AEONS - ${live_all:-0} ))
            if [ "${inv_slots_free:-0}" -eq 1 ] 2>/dev/null; then
                local live_lanes; live_lanes="$(aeons_live_lanes)"
                if [ "${live_lanes:-0}" -lt "$SPIRA_LANES_MAX_LIVE" ] 2>/dev/null; then
                    local lf lf_r
                    for lf in $(spira_lane_fayths); do
                        lf_r="$(fayth_ready "$lf" 2>/dev/null)" || continue
                        if [ "${lf_r:-0}" -gt 0 ] 2>/dev/null; then
                            log "CHECK7 $f: 1 fleet slot remaining, held back — $lf has $lf_r ready lane work"
                            return 1
                        fi
                    done
                fi
            fi
        fi
    fi
    r="$(fayth_ready "$f")" || { log "CHECK7 $f: no fayth in the chamber — skipped"; return 1; }
    if [ "${r:-0}" -eq 0 ]; then log "CHECK7 $f: nothing ready in its partition"; return 1; fi
    free="$(fayth_free "$f" "$pool")"
    if [ "${free:-0}" -eq 0 ]; then
        log "CHECK7 $f: $r ready, at concurrency cap"
        return 1
    fi

    # A TRANSIENT UNIT, not a background child. This service is Type=oneshot with the
    # default KillMode=control-group, so systemd tears down the whole cgroup the moment the
    # pass finishes — which killed the first aeon it summoned within the same second, after
    # 1.6s of CPU, leaving an empty log and a sentinel that cheerfully reported "summoned"
    # every two minutes. systemd-run puts the aeon in its own cgroup, quota and journal.
    log "CHECK7 $f: $r ready, $free free — summoning${require_label:+, restricted to '$require_label'}"
    "${SPIRA_SUMMON:-systemd-run}" --user --collect --quiet \
        --unit="spira-aeon-$f-$(date +%s)" \
        --property=CPUQuota="${SPIRA_AEON_CPU_QUOTA:-70%}" --property=Nice=10 \
        --property=TimeoutStartSec="$(fayth_get "$f" FAYTH_TIMEOUT_SECONDS 3600)" \
        --setenv=PATH="$PATH" --setenv=HOME="$HOME" \
        ${require_label:+--setenv=SPIRA_REQUIRE_LABEL="$require_label"} \
        "$SPIRA_HOME/aeon.sh" "$f" 2>/dev/null
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
# so an operator whose pool runs a different model can match the probe to it. Timeouts are
# treated as refusals — an API that does not answer in SPIRA_CAPACITY_PROBE_TIMEOUT seconds
# is not evidence the account is open, and the conservative direction is to keep the pause.
capacity_probe() {
    printf 'ok' | timeout "${SPIRA_CAPACITY_PROBE_TIMEOUT:-30}" \
        "${SPIRA_AGENT:-claude}" -p --model "${SPIRA_CAPACITY_PROBE_MODEL:-claude-sonnet-4-6}" \
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
    printf "select greatest(sum(case when event_type='claimed' or (event_type='status_changed' and new_value like '%%in_progress%%') then 1 else 0 end) - sum(case when event_type='closed' then 1 else 0 end) - sum(case when event_type='requeued' and (new_value='thrash' or new_value like 'unjudged%%') then 1 else 0 end), 0) from events where issue_id='%s'" "$1"
}

attempts_of() {          # attempts_of <id> -> count of in_progress status-change events
    local id="$1" q result=""
    q="$(_attempts_sql_query "$id")"
    if result="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" 2>/dev/null | sed -n '3p' \
                  | tr -d ' ')" && [ -n "$result" ] \
       && printf '%d' "$result" >/dev/null 2>&1; then
        printf '%d' "$result"; return 0
    fi
    printf '0'
}

reopens_of() {         # reopens_of <id> -> count of reopened events
    local id="$1" q result=""
    q="$(printf "select count(*) from events where issue_id='%s' and event_type='reopened'" "$id")"
    if result="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" 2>/dev/null | sed -n '3p' \
                  | tr -d ' ')" && [ -n "$result" ] \
       && printf '%d' "$result" >/dev/null 2>&1; then
        printf '%d' "$result"; return 0
    fi
    printf '0'
}

# BUMP FUNCTIONS. bump_attempt and bump_timeout are no-ops (attempts are counted via
# status_changed events that bd writes natively; timeouts have no census role). The
# remaining three — bump_reclaim, bump_requeue, bump_recur — write a typed event row so
# that census.sh can aggregate failure classes across the whole store (sp-2lk).
#
# Failure is silent — a missed counter is acceptable; a crash in a caller is not.
# bump_attempt and bump_timeout remain no-ops so that callers compile without change
# and test-check4-events.sh criterion 1 (no label writes) holds.
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
    q="INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$etype', '$actor', '$cause', NOW())"
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" >/dev/null 2>&1 && return 0
    return 1
}

_bump_write_event() { _bump_write_event_try "$@" >/dev/null 2>&1; return 0; }
bump_attempt() { return 0; }
bump_reclaim() { _bump_write_event "${1:-}" reclaimed "${2:-unrecorded}"; }
bump_requeue() { _bump_write_event "${1:-}" requeued  "${2:-unrecorded}"; }
bump_timeout() { return 0; }
bump_recur()   { _bump_write_event "${1:-}" recurred  "${2:-unrecorded}"; }
bump_lapsed()  { _bump_write_event "${1:-}" lapsed    "${2:-unrecorded}"; }
bump_reopen()  {
    # new_value is intentionally empty for reopened events — cannot use _bump_write_event
    # which defaults to 'unrecorded' when the cause argument is empty or absent.
    local id="${1:-}"; [ -n "$id" ] || return 0
    local actor="${BEADS_ACTOR:-harness}" uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))' 2>/dev/null)" || return 0
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
        "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'reopened', '$actor', '', NOW())" \
        >/dev/null 2>&1; return 0
}

# DIAGNOSTIC ACCESSORS — requeue/reclaim/recur counters are read from the events
# table via bd sql (sp-2lk). timeouts_of returns 0 (no census role; events-based
# timeout detection is a separate future deliverable).
# --------------------------------------------------------------------------------------
# _counter_events_sql <id> <event_type> — SQL that returns a single integer count.
_counter_events_sql() {
    printf "SELECT COUNT(*) FROM events WHERE issue_id='%s' AND event_type='%s'" "$1" "$2"
}
_counter_events_query() {   # _counter_events_query <id> <event_type> -> count
    local id="$1" etype="$2" q result=""
    q="$(_counter_events_sql "$id" "$etype")"
    if result="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" 2>/dev/null | sed -n '3p' \
                  | tr -d ' ')" && [ -n "$result" ] \
       && printf '%d' "$result" >/dev/null 2>&1; then
        printf '%d' "$result"; return 0
    fi
    printf '0'
}
reclaims_of() { _counter_events_query "${1:-}" reclaimed; }
requeues_of() { _counter_events_query "${1:-}" requeued;  }
timeouts_of() { printf '0'; }
recurs_of()   { _counter_events_query "${1:-}" recurred;  }

# CENSUS SQL — the query and runner used by census.sh to aggregate failure classes.
# Kept in lib.sh so that tests can call it directly without parsing census.sh.
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
    printf "SELECT event_type, COALESCE(new_value, ''), COUNT(DISTINCT issue_id) AS beads, COUNT(*) AS events FROM events WHERE event_type IN ('requeued', 'reclaimed', 'recurred', 'lapsed', 'reopen') AND NOT %s AND NOT (event_type = 'reopen' AND new_value = 'rebase-conflict') AND NOT %s AND NOT %s AND NOT %s AND NOT %s%s GROUP BY event_type, new_value UNION ALL SELECT 'reopen', 'rebase-conflict', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE ((event_type = 'reopen' AND new_value = 'rebase-conflict') OR %s OR %s)%s HAVING COUNT(DISTINCT issue_id) > 0 UNION ALL SELECT 'reopen', 'eviction-race', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE %s%s HAVING COUNT(DISTINCT issue_id) > 0 UNION ALL SELECT 'reopen', 'prod-dirty', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE %s%s HAVING COUNT(DISTINCT issue_id) > 0 UNION ALL SELECT 'reopen', 'unfinished-reason', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE %s%s HAVING COUNT(DISTINCT issue_id) > 0 UNION ALL SELECT 'reopened', 'unrecorded', COUNT(DISTINCT issue_id), COUNT(*) FROM events WHERE event_type = 'reopened'%s AND issue_id NOT IN (SELECT issue_id FROM events WHERE event_type = 'reopen' OR %s) HAVING COUNT(DISTINCT issue_id) > 0 ORDER BY 3 DESC" "$conflict_fold" "$rebase_aeon_fold" "$eviction_fold" "$prod_dirty_fold" "$unfinished_fold" "$since_clause" "$conflict_fold" "$rebase_aeon_fold" "$since_clause" "$eviction_fold" "$since_clause" "$prod_dirty_fold" "$since_clause" "$unfinished_fold" "$since_clause" "$since_clause" "$conflict_fold"
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

# counter_of, counter_label, counter_causes, bump_counter, attempt_causes, requeue_causes,
# recur_causes: retained for compatibility with attempts.sh and test-attempts.sh which
# read historical labels. These do not write anything.
counter_of() {           # counter_of <id> <prefix> -> integer (empty when unset)
    bdq label list "$1" 2>/dev/null | grep -oE "$2-[0-9]+" | grep -oE '[0-9]+$' \
        | sort -n | tail -1 || true
}
counter_label() {
    local all hit
    all="$(bdq label list "$1" 2>/dev/null | sed -n 's/^ *- //p')" || all=""
    hit="$(grep -xE "$2-$3(-.*)?" <<<"$all")" || return 1
    printf '%s' "$(sed -n 1p <<<"$hit")"
}
counter_causes() {
    local all rungs
    all="$(bdq label list "$1" 2>/dev/null | sed -n 's/^ *- //p')" || all=""
    rungs="$(grep -E "^$2-[0-9]+(-|$)" <<<"$all")" || return 0
    sed -E "s/^$2-([0-9]+)$/\1 unrecorded/;s/^$2-([0-9]+)-(.*)$/\1 \2/" <<<"$rungs" | sort -n
}
bump_counter()   { return 0; }  # no-op: counter labels are no longer written (sp-lzt)
attempt_causes() { counter_causes "$1" sp-attempt; }
requeue_causes() { counter_causes "$1" sp-requeue; }
recur_causes()   { counter_causes "$1" sp-recur; }

# --------------------------------------------------------------------------------------
# WHAT ENDED THIS SESSION — AND THE DEFAULT IS "WE DO NOT KNOW".
#
# The poison threshold means "we know this work keeps failing", so only an outcome that
# names what the WORK did wrong may charge against it. Everything else — an exit the harness
# cannot classify included — is evidence about the worker or about nothing at all. It used
# to be default-ALLOW, charging anything a small list of exemptions did not positively
# excuse, and every failure mode that cost the most was unenumerated when it fired: one bead
# poisoned on a lease reclaim, a red CI run and a claim, with a single rate-limit line in its
# log, and exempting the rate-limit case alone would not have saved it.
#
# The two directions are not symmetric, which is why the default goes this way. Default-deny
# fails by retrying a genuinely bad bead more often than necessary, and that costs passes.
# Default-allow fails by the queue destroying itself during an outage, and that costs the
# work — permanently, since poison is state a transient condition has no business writing.
#
#   unlanded  the session ran to its own end and the bead is not closed. A verdict about the
#             WORK exists: an aeon looked at it and did not finish it. THIS IS THE ONLY
#             OUTCOME THAT CHARGES AN ATTEMPT.
#   refused   the API turned the session away; it never acted. When a rate limit is reached
#             every summon dies in seconds with a rejection, burning lives off beads nobody
#             has looked at.
#   killed    it acted, then vanished without writing a terminal record — the host died, the
#             cgroup was torn down, or its worktree was deleted under it.
#   unknown   the trace cannot say. Charges nothing, on purpose.
#
# Deterministic and cheap (law-deterministic-before-inference): the presence of one tool call
# and of a terminal `result` record answers the whole question. The test is the TRACE and not
# the exit status — a session can exit non-zero having done real work, and a refused one exits
# the same way. `--output-format stream-json` emits an event per message and per tool call,
# so a segment carrying none of them is itself an answer: the session never got to speak.
#
# A MISSING FILE IS `unknown`, NOT `refused`. Zero has to be distinguishable from "we looked
# in the wrong place" or the check reads as all-clear when it is broken
# (law-absence-needs-a-positive-control). Both decline to charge, so the recorded cause is the
# only thing that differs — which is exactly the point, because it is what a human reads when
# the counts stop making sense.
#
# Every branch is an `if`, never a bare `cmd && return`: an AND-list that fails is a failed
# command, and this is called from an EXIT trap, where one of those can end the shell
# mid-teardown.
# --------------------------------------------------------------------------------------
session_outcome() {      # session_outcome <trace-file> -> unlanded|refused|killed|unknown
    local f="${1:-}" seg last acted=1
    if [ -z "$f" ] || [ ! -e "$f" ]; then printf 'unknown'; return 0; fi
    # THE LAST ATTEMPT'S SEGMENT, NEVER THE WHOLE FILE. The trace is appended to across
    # attempts, so a session the account refused before it wrote a single event would inherit
    # the PREVIOUS attempt's terminal `result` record — and a previous attempt that ran to its
    # own end reads as `unlanded`. That charges the refusal as a verdict about the work, which
    # is default-allow restored through the back door, in the one case the rule exists for.
    # attempt_trace is the boundary every reader of this file has to go through.
    seg="$(attempt_trace "$f" 2>/dev/null)" || seg=""
    # `grep -c`, never `grep -q`, and counted rather than tested: -q exits at the first match
    # and closes its input, which under pipefail is reported as failure — so the test would
    # read FALSE exactly when it succeeded (law-no-grep-q-under-pipefail).
    #
    # The segment carries its own attempt mark, so "is the file non-empty" is not the
    # question; "did this session emit any event at all" is. A segment of nothing but the mark
    # is a session that never got to speak.
    if [ "$(grep -c '^{' <<<"$seg")" = 0 ]; then printf 'refused'; return 0; fi
    if [ "$(grep -cF '"type":"tool_use"' <<<"$seg")" != 0 ]; then acted=0; fi
    last="$(grep -F '"type":"result"' <<<"$seg" | tail -1)" || last=""
    if [ -z "$last" ]; then
        # No terminal record. It either never started or was killed part-way; the tool calls
        # are what tell those apart.
        if [ "$acted" = 0 ]; then printf 'killed'; else printf 'refused'; fi
        return 0
    fi
    case "$last" in
        *'"api_error_status":null'*) : ;;  # null value — key present but no actual refusal
        *'"api_error_status"'*|*'"error":"rate_limit"'*) printf 'refused'; return 0 ;;
    esac
    # It ran to its own end. If it never called a tool it decided nothing about the work
    # either, so that is still not a verdict.
    if [ "$acted" = 0 ]; then printf 'unlanded'; else printf 'refused'; fi
}

# Does this outcome say something about the WORK? Exactly one does. Kept as a function rather
# than an inline test so that adding an outcome forces a decision here instead of silently
# inheriting whichever default the call site happens to have.
outcome_charges() {      # outcome_charges <outcome> -> rc 0 when it may charge an attempt
    case "${1:-}" in unlanded) return 0 ;; *) return 1 ;; esac
}

# session_yield_headless <logfile> -> 0 if the session's last turn ended waiting for a
# background task notification. In headless mode there is no such channel: the session
# terminates and background tasks are killed, leaving rc=0 with no diagnostic signal.
session_yield_headless() {
    local f="${1:-}"
    [ -r "$f" ] || return 1
    attempt_trace "$f" 50000 2>/dev/null | python3 -c '
import sys, json, re
PATTERNS = [
    r"background task notification",
    r"background.{0,30}(wait|waiting|woken|wake|notification)",
    r"(wait|waiting).{0,40}background.{0,30}task",
    r"will be woken",
    r"run_in_background",
]
last_text = ""
for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"): continue
    try: e = json.loads(line)
    except Exception: continue
    if e.get("type") != "assistant": continue
    for c in (e.get("message", {}) or {}).get("content", []) or []:
        if c.get("type") == "text" and c.get("text", "").strip():
            last_text = c["text"]
if not last_text:
    sys.exit(1)
for p in PATTERNS:
    if re.search(p, last_text, re.IGNORECASE | re.DOTALL):
        sys.exit(0)
sys.exit(1)
' 2>/dev/null
}

# --------------------------------------------------------------------------------------
# session_result_fields <trace-file> -> one line of `key=value` pairs saying what the
# attempt COST, ready to append to a ledger line:
#
#   wall_s          seconds the session ran, wall clock
#   api_s           seconds of that spent inside the API
#   turns           num_turns, the client's own count
#   in_tok          fresh input tokens
#   cache_read_tok  prompt cache re-reads — two orders of magnitude larger, and the figure
#                   that predicts a rate limit
#   out_tok         output tokens
#   think_tok       of those, thinking
#   cost_usd        total_cost_usd, four decimals
#
# WHY IT IS KEPT AT ALL. Every one of these is already computed by the client and written to
# the terminal `result` record of every session, and nothing kept any of it — so the first
# time anyone asked where an aeon's hours went it took a purpose-built script over tens of
# megabytes of traces to answer, once, by hand. On the ledger line it is an awk one-liner
# over one small file, and cost per landed bead becomes a number a panel can read.
#
# ALL `result` RECORDS OF THE LAST ATTEMPT, through attempt_trace and never the raw file.
# The trace is appended to across attempts, so a session that was refused before it could
# speak would otherwise be billed the previous attempt's tokens — the same boundary error
# that charges a refusal as a verdict about the work, arriving as a cost figure instead of a
# poison count. Several `result` records in one segment is not a malformed trace: a session
# woken by a task notification emits a second record for the notification turn only, so the
# last record alone would mix scopes — its per-turn metrics describe the wake-up while its
# total_cost_usd is the session cumulative. Per-turn fields are SUMMED across all records
# so the session total is reported. Cumulative fields (duration_api_ms, total_cost_usd) are
# taken from the last record, where the client has fully accumulated them.
#
# EVERY FIELD IS INDEPENDENTLY `?`, AND NEVER 0. A record with no `duration_ms` has been
# observed in the wild beside one carrying it, so "the trace had no result at all" and "this
# client did not report that figure" have to be distinguishable from "the session spent
# nothing" — a refused session really did cost nearly nothing, and averaging the unreadable
# in with it is how a cost-per-bead figure comes out reassuring exactly when the harness is
# failing (law-absence-needs-a-positive-control).
#
# The program arrives on FD 3 because stdin is the trace; `python3 - <<PY` reads the heredoc
# as the data and discards the pipe unread.
# --------------------------------------------------------------------------------------
session_result_fields() {
    local f="${1:-}" out
    # PIPED, NEVER THROUGH A VARIABLE. A segment can be tens of megabytes, and this runs in a
    # teardown that must not be the reason an aeon fails to record what it did. An absent or
    # unreadable file feeds the same program an empty stream, so the "no result record" line
    # below is rendered by one code path rather than by a second copy of the key names.
    out="$( { [ -n "$f" ] && [ -r "$f" ] && attempt_trace "$f" 0 2>/dev/null; true; } \
            | python3 /dev/fd/3 3<<'PY' 2>/dev/null
import json, sys

def obj(parent, key):
    v = parent.get(key)
    return v if isinstance(v, dict) else {}

def num(v, div=1):
    # bool IS AN int IN PYTHON, and a JSON `true` rendered as 1 would be a token count that
    # was never a token count. Anything that is not a real number is unknown, which includes
    # the JSON null this client writes for fields it did not fill in.
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return "?"
    return "%d" % round(v / div)

def money(v):
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return "?"
    return "%.4f" % v

# A CHEAP PREFILTER FIRST. Nearly every line of a trace is an assistant or tool event and
# parsing all of them to find result records is the difference between a millisecond and a
# second on a large segment. The parse below is still the test — the substring only
# decides what is worth parsing.
records = []
for line in sys.stdin:
    if '"type":"result"' not in line:
        continue
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        d = json.loads(line)
    except ValueError:
        continue          # a partial last line is normal on a killed session
    if isinstance(d, dict) and d.get("type") == "result":
        records.append(d)

def sumf(field_fn):
    """Sum field_fn(d) across all records; None if the field never appeared."""
    total = None
    for d in records:
        v = field_fn(d)
        if isinstance(v, bool) or not isinstance(v, (int, float)):
            continue
        total = (total or 0) + v
    return total

def lastf(field_fn):
    """Last non-None numeric value of field_fn across all records."""
    result = None
    for d in records:
        v = field_fn(d)
        if not isinstance(v, bool) and isinstance(v, (int, float)):
            result = v
    return result

# PER-TURN FIELDS: summed across all result records so the session total is
# reported even when a notification wake-up adds a second result record. Taking
# only the last record would report only that turn, while total_cost_usd is
# already the session cumulative — producing the impossible wall_s < api_s.
wall_ms   = sumf(lambda d: d.get("duration_ms"))
turns     = sumf(lambda d: d.get("num_turns"))
in_tok    = sumf(lambda d: obj(d, "usage").get("input_tokens"))
cache_tok = sumf(lambda d: obj(d, "usage").get("cache_read_input_tokens"))
out_tok   = sumf(lambda d: obj(d, "usage").get("output_tokens"))
think_tok = sumf(lambda d: obj(obj(d, "usage"), "output_tokens_details").get("thinking_tokens"))

# CUMULATIVE FIELDS: the client accumulates these across all turns and writes the
# running session total into each result record, so the last record holds the
# session total. api_s > wall_s is structurally impossible once wall_s is also
# the session total, and any trace that produced it is self-evidently broken.
api_ms = lastf(lambda d: d.get("duration_api_ms"))
cost   = lastf(lambda d: d.get("total_cost_usd"))

sys.stdout.write("wall_s=%s api_s=%s turns=%s in_tok=%s cache_read_tok=%s out_tok=%s think_tok=%s cost_usd=%s" % (
    num(wall_ms, 1000),
    num(api_ms, 1000),
    num(turns),
    num(in_tok),
    num(cache_tok),
    num(out_tok),
    num(think_tok),
    money(cost),
))
PY
    )"
    # THE ONE CASE THE PROGRAM ABOVE CANNOT RENDER: no python3 to run it. Nothing else in
    # this library works without one, so this is insurance rather than a path — but a ledger
    # line silently missing its fields would read as an older line rather than as a broken
    # one, and every key here has to exist for a reader to be able to tell.
    printf '%s' "${out:-wall_s=? api_s=? turns=? in_tok=? cache_read_tok=? out_tok=? think_tok=? cost_usd=?}"
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

poison_asked() {         # poison_asked <id> <n> -> 0 if this exact (bead, count) already asked
    local f="$SPIRA_POISON_ASKED/$1"
    # grep reads the file itself. There is no pipe here on purpose: `... | grep -q` under
    # `set -o pipefail` returns 141 when it MATCHES, which in this position would read as
    # "not yet asked" exactly when it had been (law-no-grep-q-under-pipefail).
    [ -r "$f" ] && grep -qxF -- "$2" "$f" 2>/dev/null
}

poison_asked_mark() {    # poison_asked_mark <id> <n>
    mkdir -p "$SPIRA_POISON_ASKED" 2>/dev/null || return 1
    printf '%s\n' "$2" >> "$SPIRA_POISON_ASKED/$1"
}

SPIRA_REQUEUE_ASKED="${SPIRA_REQUEUE_ASKED:-$SPIRA_RUN/requeue-asked}"

requeue_asked() {        # requeue_asked <id> -> 0 if this bead already asked
    [ -s "$SPIRA_REQUEUE_ASKED/$1" ]
}

requeue_asked_mark() {   # requeue_asked_mark <id> <n>
    mkdir -p "$SPIRA_REQUEUE_ASKED" 2>/dev/null || return 1
    printf '%s\n' "$2" >> "$SPIRA_REQUEUE_ASKED/$1"
}

SPIRA_RECLAIM_ASKED="${SPIRA_RECLAIM_ASKED:-$SPIRA_RUN/reclaim-asked}"

reclaim_asked() {        # reclaim_asked <id> -> 0 if this bead already asked
    [ -s "$SPIRA_RECLAIM_ASKED/$1" ]
}

reclaim_asked_mark() {   # reclaim_asked_mark <id> <n>
    mkdir -p "$SPIRA_RECLAIM_ASKED" 2>/dev/null || return 1
    printf '%s\n' "$2" >> "$SPIRA_RECLAIM_ASKED/$1"
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

# spira_trace_mark <logfile> <aeon> -> the separator line that opens a new attempt.
#
# THE ORDINAL COUNTS MARKS ALREADY IN THE FILE, not the bead's `sp-attempt-N` labels. An
# attempt is only CHARGED when a session fails, and a session refused by the account is
# deliberately charged nothing — so the label count answers "how many failures were blamed on
# this work", which is a different question from "which session am I reading" and was off by
# every successful and every refused run. Here the file is its own authority.
#
# `kept=` IS THE METER, and it is here because nothing prunes these logs. Appending trades
# bounded disk for a complete history, and the quantity given away is exactly the bytes
# already retained for this bead — so it is recorded at the head of every attempt rather than
# left to be discovered when a volume fills (law-take-the-simple-fix-with-a-meter). A bead
# whose mark lines show `kept=` climbing into the hundreds of megabytes is the signal that
# this simple choice has stopped being adequate.
spira_trace_mark() {
    local f="${1:-}" who="${2:-?}" kept n
    kept="$(stat -c %s "$f" 2>/dev/null || echo 0)"
    # BOTH READS ARE ALLOWED TO FAIL, and both say so. The first attempt on a bead finds no
    # file at all and a fresh one finds no mark, so `stat` exits 1 and `grep -c` exits 1
    # having printed `0` — ordinary answers, not errors. Under a caller running `set -e` a
    # bare assignment from either would abort the shell at the exact line that opens the
    # log, which is the one place a failure costs the whole attempt.
    n="$(grep -c "^$SPIRA_TRACE_MARK " "$f" 2>/dev/null || true)"
    printf '%s %s aeon=%s at=%s kept=%s\n' \
        "$SPIRA_TRACE_MARK" "$(( ${n:-0} + 1 ))" "$who" \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${kept:-0}"
}

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
# still_waiting <logfile> -> 0 if the silence is a legitimate wait, 1 if it is a stall.
#
# (the operator, verbatim: "before you kill an aeon for being idle ... do a quick inference
# check to see whether the last log message indicates that it's WAITING for something that
# might take longer than 10 minutes and extend the deadline accordingly".)
#
# Two tiers, cheapest first — the same rule the sentinel follows. A session blocked on
# `gh run watch` emits no trace for the whole of a CI run and is the likeliest long silence
# here; recognising that by pattern costs nothing. Inference is reached only when the last
# action is not a known wait, which is precisely the case where there is no rule to apply.
# --------------------------------------------------------------------------------------
still_waiting() {
    local f="$1" last verdict
    last="$(trace_last "$f")"
    [ -n "$last" ] || return 1          # nothing to judge: treat as stalled

    # TIER 1 — known long waits, by pattern. No model, no cost, no latency.
    case "$last" in
        *"gh run watch"*|*"gh pr checks"*|*"gh run view"*|*"--watch"*) return 0 ;;
        *"cargo build"*|*"cargo test"*|*"npm test"*|*"npm run build"*|*"make "*) return 0 ;;
        *"sleep "*|*"until "*|*"while "*|*"docker build"*|*"podman build"*) return 0 ;;
        *"git clone"*|*"git fetch"*|*"bd import"*|*"dolt "*) return 0 ;;
    esac

    # TIER 2 — judgement, only because tier 1 had no answer. Small model, tight question,
    # short ceiling: this runs while a lease is on the line and must not itself hang.
    command -v claude >/dev/null 2>&1 || return 1
    verdict="$(printf 'A background agent has produced no output for several minutes. Its last action was:\n\n%s\n\nIs it plausibly WAITING on something that legitimately takes more than ten minutes (a CI run, a build, a large clone, a long test suite, a rate limit), or is it STUCK? Answer with exactly one word: WAITING or STUCK.' "$last" \
        | timeout 90 claude -p --model claude-haiku-4-5-20251001 2>/dev/null | tr -d "[:space:]" | tr "[:lower:]" "[:upper:]")"
    case "$verdict" in *WAITING*) return 0 ;; esac
    return 1
}

# --------------------------------------------------------------------------------------
# heartbeat_model_idle <logfile> -> "idle_s state"
#
# THE SIGNAL THAT TELLS BLOCKED FROM STUCK. The stream-json trace grows whenever anything
# happens, including tool_progress heartbeats that the CLI emits every ~30s while the model
# is blocked on a single tool call. So log file size is evidence the PROCESS is alive, not
# that WORK is happening — a fully blocked aeon's log grows steadily, and a size check
# cannot tell it from one doing real work.
#
# elapsed_time_seconds on the trailing heartbeat is the discriminating fact: it is exactly
# how long since the model last ACTED, carried by the event itself rather than inferred
# from outside.
#
#   idle_s:  seconds since the model last acted (0 if it just acted, -1 if unreadable)
#   state:   "acting"  — the trailing log line is a model action
#            "blocked" — the trailing log line is a tool_progress heartbeat
#            "silent"  — trace mark only, no model output yet
#            "?"       — the line could not be parsed
# --------------------------------------------------------------------------------------
heartbeat_model_idle() {
    local f="$1"
    [ -r "$f" ] || { echo "-1 ?"; return; }
    tail -1 "$f" 2>/dev/null | python3 -c '
import sys, json, time, re, calendar
line = sys.stdin.readline().strip()
if not line:
    print("-1 ?"); raise SystemExit
if line.startswith("=== spira attempt"):
    m = re.search(r"at=(\S+Z)", line)
    born = calendar.timegm(time.strptime(m.group(1), "%Y-%m-%dT%H:%M:%SZ")) if m else 0
    print(int(time.time()) - born if born else 0, "silent")
    raise SystemExit
try:
    d = json.loads(line)
except Exception:
    print("-1 ?"); raise SystemExit
if d.get("type") == "tool_progress" and d.get("heartbeat"):
    print(d.get("elapsed_time_seconds", 0), "blocked")
else:
    print(0, "acting")
' 2>/dev/null || echo "-1 ?"
}

# --------------------------------------------------------------------------------------
# youngest_in_subtree <root_pid> [exclude_pid] -> epoch of the youngest process in
# root_pid's subtree, excluding the subtree rooted at exclude_pid. 0 if none found.
#
# argv from /proc, never pgrep -f: the pattern is a substring of the caller's own command
# line, and pgrep matching it killed the shell that invoked it once already.
# --------------------------------------------------------------------------------------
youngest_in_subtree() {
    local root="$1" exclude="${2:-0}" newest=0
    local p pid st cur hops hit excl parent
    # Build entire ppid map with ONE awk pass, then walk ancestry via array lookups.
    # Old: forked awk per ancestor hop per pid (up to 40 hops * 461 processes = 18k forks).
    # Scales to 4.8s on a loaded box; kills suites faster than they can finish.
    # New: one pass builds the map; ancestry walks as O(1) lookups.
    declare -A ppid
    # PARSE AFTER THE COMM, NOT BY FIELD NUMBER. /proc/<pid>/stat is "pid (comm) state ppid",
    # and comm is an arbitrary process name in parentheses that MAY CONTAIN SPACES — right now
    # this box is running "(Web Content)", "(Socket Process)" and "(tmux: server)". For those,
    # $4 is the state character, not the ppid, so the ancestry walk looks up ppid["S"], finds
    # nothing, and silently stops one hop in. A worker descending from any such process would
    # not be attributed to its root, and youngest_in_subtree exists to say whether an aeon is
    # alive: under-reporting here reaps an aeon that is working. Split on the LAST ")" instead,
    # which is unambiguous because comm is the only parenthesised field.
    while IFS=' ' read -r pid parent; do
        [ -n "$pid" ] && ppid["$pid"]="$parent"
    done < <(awk '{ n = match($0, /^[0-9]+ \(/); if (!n) next
                    close_paren = 0
                    for (i = length($0); i > 0; i--) if (substr($0, i, 1) == ")") { close_paren = i; break }
                    if (!close_paren) next
                    rest = substr($0, close_paren + 2)      # "state ppid ..."
                    split(rest, a, " ")
                    print $1, a[2] }' /proc/*/stat 2>/dev/null)
    for p in /proc/[0-9]*/stat; do
        pid="${p%%/stat}"; pid="${pid##*/}"
        [ "$pid" = "$exclude" ] && continue
        cur="$pid"; hops=0; hit=0; excl=0
        while [ "$hops" -lt 40 ]; do
            [ "$cur" = "$root" ] && { hit=1; break; }
            [ "$cur" = "$exclude" ] && { excl=1; break; }
            [ "$cur" = "1" ] || [ -z "$cur" ] && break
            cur="${ppid["$cur"]}" || break
            hops=$((hops+1))
        done
        [ "$hit" = 1 ] && [ "$excl" = 0 ] || continue
        st=$(stat -c %Y "/proc/$pid" 2>/dev/null) || continue
        [ "$st" -gt "$newest" ] && newest="$st"
    done
    echo "$newest"
}

# --------------------------------------------------------------------------------------
# subtree_has_flock <root_pid> -> 0 if a `flock` process exists in root_pid's subtree.
#
# A flock is a legitimate queue wait — typically for the shared test fixture — and a
# heartbeat that calls it stuck gets something killed that should not be.
# --------------------------------------------------------------------------------------
subtree_has_flock() {
    local root="$1"
    local p pid cur hops hit comm parent
    # Build a ppid map from a /proc/*/stat snapshot for ancestor walks.
    # A flock process that appeared after the snapshot will be absent from the
    # map even though it is visible via /proc/PID/comm; reading its stat directly
    # in the comm scan below closes that TOCTOU window for the first hop.
    # PARSE AFTER THE COMM, NOT BY FIELD NUMBER, to handle comms with spaces.
    declare -A ppid
    while IFS=' ' read -r pid parent; do
        ppid["$pid"]="$parent"
    done < <(awk '{ n = match($0, /^[0-9]+ \(/); if (!n) next
                    close_paren = 0
                    for (i = length($0); i > 0; i--) if (substr($0, i, 1) == ")") { close_paren = i; break }
                    if (!close_paren) next
                    rest = substr($0, close_paren + 2)      # "state ppid ..."
                    split(rest, a, " ")
                    print $1, a[2] }' /proc/*/stat 2>/dev/null)
    for p in /proc/[0-9]*/comm; do
        comm="$(cat "$p" 2>/dev/null)" || continue
        [ "$comm" = "flock" ] || continue
        pid="${p%%/comm}"; pid="${pid##*/}"
        # Read this flock process's ppid directly from its stat file rather than
        # from the snapshot: a freshly-spawned flock may not yet have been in
        # /proc when the map was built, so ppid["$pid"] would be absent and the
        # walk would stop at the first step.
        cur=$(awk '{ n = match($0, /^[0-9]+ \(/); if (!n) next
                     close_paren = 0
                     for (i = length($0); i > 0; i--) if (substr($0, i, 1) == ")") { close_paren = i; break }
                     if (!close_paren) next
                     rest = substr($0, close_paren + 2)
                     split(rest, a, " ")
                     print a[2]; exit }' "/proc/$pid/stat" 2>/dev/null)
        hops=0; hit=0
        while [ "$hops" -lt 40 ]; do
            [ "$cur" = "$root" ] && { hit=1; break; }
            [ "$cur" = "1" ] || [ -z "$cur" ] && break
            cur="${ppid["$cur"]}"
            hops=$((hops+1))
        done
        [ "$hit" = 1 ] && return 0
    done
    return 1
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
    # ("spira: land <id>", produced by batch.sh/landing.sh), or an aeon's own commit for its
    # own bead ("<id>: ..." — never a substring, the colon must follow immediately).
    # shellcheck disable=SC2086
    while IFS= read -r _landed_subj; do
        case "$_landed_subj" in
            "spira: land $id") return 0 ;;
            "$id":*) return 0 ;;
        esac
    done < <(git -C "$repo" log --format='%s' --grep="$id" -F $refs 2>/dev/null)
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
    if [ -z "$mem_json" ]; then
        mem_json="$(bdjson memories 2>/dev/null)"
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
    rule_cmd = f"    {harness}/rule.sh show <slug-without-law-prefix>"
    header = (
        "## Statutes in force — full text on request\n\n"
        "These are law and bind you exactly as the text above does. The slug states\n"
        "the rule; read the reasoning and the scar behind any of them with:\n\n"
        f"{rule_cmd}\n"
    )
    parts.append(header + "\n".join(index_slugs))

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

# THE GOAL EPIC IS ONE PILGRIMAGE, NOT "THE WORK". This answers "is the pilgrimage under
# $SPIRA_GOAL finished", which is what CHECK 1, CHECK 3 and CHECK 8 reason about. It is the
# wrong question for anything that must cover what the harness DISPATCHES: a bead carrying a
# fayth's labels but parented elsewhere — or parented nowhere — is summoned every pass and
# does not appear here at all. Use dispatchable_open for that.
goal_open_children() {   # beads under the goal epic that are not closed
    bdjson children "$SPIRA_GOAL" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d if isinstance(d, list) else [d]
for i in d:
    if i.get("id") != "'"$SPIRA_GOAL"'" and i.get("status") != "closed":
        print(i["id"])
' 2>/dev/null
}

# dispatchable_open -> every non-closed bead the summoner can reach, one id a line.
#
# THE SET ANY CHECK ABOUT "THE WORK" MUST ITERATE. Two predicates for "which beads are ours"
# is the defect: summoning goes through fayth_ready, which asks each persona its own
# FAYTH_LABELS, while the poison valve iterated the goal epic's children — so a bead labelled
# for a partition and parented outside the goal was dispatchable and unpoisonable. It could
# be summoned every pass, fail every time, and never trip the valve that exists to stop
# exactly that; one measured 9 attempts against a threshold of 3, and the 8 children examined
# stood for 66 beads dispatched. The fix is the one fayth_ready already made one check along:
# ask each persona's own predicate.
#
# A PARTITION'S EXCLUSIONS ARE ITS OWN, applied here exactly as claiming applies them, so
# this set and the claimable set cannot disagree. Epics go too — the summoner passes
# --exclude-type epic,event, and a container or a record is not work.
#
# EMPTY WHEN THE CHAMBER IS EMPTY, and it says so on stderr rather than returning a quiet
# zero: nothing dispatchable and nothing watched are the same silence otherwise
# (law-absence-needs-a-positive-control).
dispatchable_open() {
    local labels exclude n=0
    {
        while IFS=$'\t' read -r labels exclude; do
            [ -n "$labels" ] || continue
            n=$((n+1))
            bdjson list --limit 0 --label "$labels" 2>/dev/null \
            | SPIRA_EXCL="$exclude" python3 -c '
import json, os, sys
excl = {x for x in (os.environ.get("SPIRA_EXCL") or "").split(",") if x}
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("status") == "closed" or i.get("issue_type") in ("epic", "event"):
        continue
    if excl & set(i.get("labels") or []):
        continue
    print(i["id"] + "\t" + ",".join(i.get("labels") or []))
' 2>/dev/null
        done < <(fayth_partitions)
        [ "$n" -gt 0 ] || log "WARN no persona in the chamber declares a partition — no bead is dispatchable, and none is being examined" >&2
    } | awk -F'\t' 'NF && !seen[$1]++'
    return 0
}

# check4_closed_branched -> closed beads in fayth partitions with a branch: label.
# id TAB comma-separated-labels, one per line; deduped. Used by sentinel CHECK 4 to
# find beads that cycle closed without landing so the requeue cap can reach them.
check4_closed_branched() {
    local labels exclude
    {
        while IFS=$'\t' read -r labels exclude; do
            [ -n "$labels" ] || continue
            bdjson list --limit 0 --label "$labels" --status closed 2>/dev/null \
            | SPIRA_EXCL="$exclude" python3 -c '
import json, os, sys
excl = {x for x in (os.environ.get("SPIRA_EXCL") or "").split(",") if x}
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("status") != "closed":
        continue
    if i.get("issue_type") in ("epic", "event"):
        continue
    if excl & set(i.get("labels") or []):
        continue
    lbls = i.get("labels") or []
    if not any(l.startswith("branch:") for l in lbls):
        continue
    print(i["id"] + "\t" + ",".join(lbls))
' 2>/dev/null
        done < <(fayth_partitions)
    } | awk -F'\t' 'NF && !seen[$1]++'
    return 0
}

# _check4_bulk_sql <in-clause> -> SQL returning attempts and reopens for each id in the clause
_check4_bulk_sql() {
    printf "select issue_id, greatest(sum(case when event_type='claimed' or (event_type='status_changed' and new_value like '%%in_progress%%') then 1 else 0 end) - sum(case when event_type='closed' then 1 else 0 end) - sum(case when event_type='requeued' and (new_value='thrash' or new_value like 'unjudged%%') then 1 else 0 end), 0) as att, sum(case when event_type='reopened' then 1 else 0 end) as rep from events where issue_id in (%s) group by issue_id" "$1"
}

# check4_bulk_data <dispatchable-output> -> id TAB attempts TAB reopens, one per bead
# One GROUP BY replaces attempts_of and reopens_of called per bead in the CHECK 4 loop.
# Beads with no events are absent from the output; callers default missing entries to 0.
check4_bulk_data() {
    local input="${1:-}"; [ -n "$input" ] || return 0
    local in_clause
    in_clause="$(printf '%s' "$input" | awk -F'\t' '{print $1}' | grep -v '^$' \
        | sed "s/.*/'&'/" | paste -sd,)"
    [ -n "$in_clause" ] || return 0
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$(_check4_bulk_sql "$in_clause")" 2>/dev/null \
    | python3 -c '
import sys
for line in sys.stdin:
    s = line.rstrip("\n")
    if "|" in s:
        if s.lstrip().startswith("+-"):
            continue
        cols = [c.strip() for c in s.strip("|").split("|")]
        if len(cols) >= 3 and cols[0] and cols[0] not in ("issue_id",):
            try:
                print(cols[0] + "\t" + str(int(cols[1] or 0)) + "\t" + str(int(cols[2] or 0)))
            except (ValueError, IndexError):
                pass
    else:
        parts = s.split("\t") if "\t" in s else s.split()
        if len(parts) >= 3:
            try:
                a = int(parts[1].strip())
                r = int(parts[2].strip())
                bid = parts[0].strip()
                if bid:
                    print(bid + "\t" + str(a) + "\t" + str(r))
            except (ValueError, IndexError):
                pass
' 2>/dev/null
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
                    -u SPIRA_MAECHEN_LABEL -u SPIRA_SPIKE_LABEL \
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

    local _det_args=(ready --limit 0 --exclude-type epic,event -u)
    [[ -n "${SPIRA_NO_LOOP_LABEL:-}" ]] && _det_args+=(--exclude-label "$SPIRA_NO_LOOP_LABEL")
    bdjson "${_det_args[@]}" 2>/dev/null \
    | PARTS="$parts" ALL_PARTS="$all_parts" python3 -c '
import json, os, sys

try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
beads = d if isinstance(d, list) else [d]

def parse_parts(env_key):
    result = {}
    for line in os.environ.get(env_key, "").splitlines():
        line = line.strip()
        if not line:
            continue
        name, inc_str, exc_str = line.split("|", 2)
        result[name] = (set(filter(None, inc_str.split(","))),
                        set(filter(None, exc_str.split(","))))
    return result

parts = parse_parts("PARTS")
all_parts = parse_parts("ALL_PARTS")

scope_label = os.environ.get("SPIRA_SCOPE_LABEL", "spira")
partition_labels = sorted({lab for inc, _ in all_parts.values() for lab in inc if lab != scope_label})
ci_label = os.environ.get("SPIRA_CI_LABEL", "awaiting-ci")  # literal-ok: Python fallback for direct invocation without conf.sh
ask_label = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh

for bead in beads:
    L = set(bead.get("labels") or [])
    bid = bead.get("id", "?")
    if L & {ask_label, "spira-poison"}:
        continue
    # A REPORT ABOUT AN UNCLAIMABLE BEAD IS NOT ITSELF A SUBJECT. The report is filed into
    # the incident partition, so whenever that partition is unservable the report is
    # unclaimable too — and reporting it files another report, which is also unclaimable.
    # The dedup key is unclaimable:<subject>, so every link in the chain is a NEW subject
    # and dedup never fires.
    #
    # Measured 2026-09-12: one watchtower incident seeded a 128-deep chain and took the
    # store from 58 beads to 310 in minutes, while the roster was narrowed to a single
    # persona for a focus period. Nothing here was wrong except the missing base case: the
    # detector was correctly reporting a condition it was itself creating more of.
    #
    # external_ref is the structured mark the filer already sets, so this needs no new
    # label and cannot drift from the title text.
    if str(bead.get("external_ref") or "").startswith("unclaimable:"):
        continue
    # CI-parked beads are intentionally excluded from every persona predicate;
    # the exclusion is not a misconfiguration, so they must not appear here.
    if ci_label and ci_label in L:
        continue

    # A bead missing the scope label cannot be claimed by any persona; every predicate
    # requires it. When SPIRA_SCOPE_LABEL is non-empty, READY_ARGS already filters these
    # out at the bd level and this branch is unreachable. It fires only when scope
    # restriction is disabled (SPIRA_SCOPE_LABEL=""), where it correctly names the gap.
    if scope_label not in L:
        print("UNCLAIMABLE %s — missing scope label (%s); "
              "no persona can claim a bead without this label; "
              "add %s or remove from the ready queue" % (bid, scope_label, scope_label))
        continue

    pref = {x.split(":", 1)[1] for x in L if x.startswith("fayth:")}
    claimers = []
    for name, (inc, exc) in parts.items():
        if not inc <= L:
            continue
        if L & exc:
            continue
        if pref and name not in pref:
            continue
        claimers.append(name)

    if claimers:
        continue

    # A bead whose partition belongs to a parked fayth (defined in the chamber but absent
    # from the active roster because SPIRA_FAYTHS was narrowed) is WAITING, not UNCLAIMABLE.
    # The roster is a deliberate operator choice; UNCLAIMABLE is reserved for labels that
    # match no persona in the full chamber — the only case where "add one of: ..." is sound.
    if any(
        inc <= L and not (L & exc) and (not pref or name in pref)
        for name, (inc, exc) in all_parts.items()
        if name not in parts
    ):
        continue

    # Build a diagnostic naming the preference and why each named persona was rejected.
    # Lookup uses all_parts so parked personas are named correctly in the fayth: case.
    if pref:
        reasons = []
        for p in sorted(pref):
            if p not in all_parts:
                reasons.append("%s (not in chamber)" % p)
            elif not all_parts[p][0] <= L:
                missing = sorted(all_parts[p][0] - L)
                reasons.append("%s (partition %s, missing %s)" % (
                    p, sorted(all_parts[p][0]), missing))
            elif L & all_parts[p][1]:
                blocked = sorted(L & all_parts[p][1])
                reasons.append("%s (excluded by own labels %s)" % (p, blocked))
            else:
                reasons.append("%s (unknown reason)" % p)
        pref_str = ", ".join(sorted(pref))
        print("UNCLAIMABLE %s — fayth:%s narrows to %s, but none can claim it: %s; "
              "fix: drop the fayth: label or add the named persona'\''s partition labels" % (
                  bid, pref_str, ", ".join(sorted(pref)), "; ".join(reasons)))
    else:
        print("UNCLAIMABLE %s — spira with no matching partition; "
              "no persona'\''s partition labels are all present; "
              "add one of: %s" % (bid, ", ".join(partition_labels)))
' 2>/dev/null
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
# SPIRA_INCIDENT_SH overrides the path to incident.sh. Test suites inject a mock here;
# production uses the default.
file_unclaimable_incidents() {   # file_unclaimable_incidents <detect_unclaimable_ready output>
    local line bid reason inc
    inc="${SPIRA_INCIDENT_SH:-$(dirname "$0")/incident.sh}"
    [ -x "$inc" ] || return 0
    while IFS= read -r line; do
        case "$line" in UNCLAIMABLE\ *) ;; *) continue ;; esac
        bid="${line#UNCLAIMABLE }"; bid="${bid%% —*}"
        reason="${line#*— }"
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_INCIDENT_TYPE=task \
        SPIRA_INCIDENT_PRIORITY=1 \
        SPIRA_INCIDENT_ACTOR=sentinel \
        SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:-spira},${SPIRA_GROOMER_LABEL:-groom}" \
        SPIRA_INCIDENT_REPO="${SPIRA_SCOPE_LABEL:-spira}" \
        SPIRA_INCIDENT_REF="unclaimable:$bid" \
        SPIRA_INCIDENT_CAUSE=unclaimable \
        bash "$inc" file "UNCLAIMABLE: $bid — fix the fayth: or partition label" \
            - <<< "$reason" >/dev/null 2>&1 || true
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
            # will ever report back. spira_ci_park_state also checks timing and exits 2 on an
            # empty timestamp, which would trigger `|| _state=no-ci` even for pr-mode repos.
            # Use repo_land directly — it is the one test that names the structural fault.
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
for i in (d if isinstance(d, list) else [d]):
    L = i.get("labels") or []
    # Skip beads already handled by the unclaimable or needs-ryan checks.
    if ask_label in L or "spira-poison" in L:
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
    printf '%s\n' "$_closed_raw" | SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_ID_PREFIX="${SPIRA_ID_PREFIX:-sp}" python3 -c '
import sys, json, re, os

# Load RED_FLAGS and check_close_reason from the shared helper so the detector and the
# close-time fence in aeon.sh cannot disagree about what constitutes an unfinished close.
_flags_path = os.path.join(os.environ.get("SPIRA_HOME", ""), "close-reason-flags.py")
try:
    _ns = {"re": re, "__name__": ""}
    exec(open(_flags_path).read(), _ns)
    check_close_reason = _ns["check_close_reason"]
except Exception:
    check_close_reason = lambda r: None

# Phrases that imply a follow-on obligation. Only a violation when no tracking reference
# is cited. "upstream" names a destination, not an unfinished remainder, so it is not here.
FOLLOW_ON = [
    "builders should",
    "at scale",
    "the real fix",
    "follow-up",
]

_prefix = re.escape(os.environ.get("SPIRA_ID_PREFIX", "sp"))
TRACKING_RE = re.compile(
    r"(?:\b" + _prefix + r"-[a-z0-9]+"
    r"|[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*#\d+"
    r"|https?://\S+)",
    re.IGNORECASE
)

try: d = json.load(sys.stdin)
except Exception: raise SystemExit
for i in (d if isinstance(d, list) else [d]):
    reason = i.get("close_reason") or ""
    title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:60]
    reason_short = re.sub(r"\s+", " ", reason.strip())[:120]

    hit = check_close_reason(reason)
    if hit:
        print("INVALID-CLOSED %s — close reason contains %r: %s. title: %s" % (
            i["id"], hit, reason_short, title))
        continue

    follow_hit = next((f for f in FOLLOW_ON if f.lower() in reason.lower()), None)
    if follow_hit and not TRACKING_RE.search(reason):
        print("UNFILED-FOLLOW %s — follow-on phrase %r without a tracking reference: %s. title: %s" % (
            i["id"], follow_hit, reason_short, title))
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

repo_land() {            # repo_land <name> -> push | pr | hold | queue
    local m; m="$(repo_field "${1:-}" land)"
    printf '%s' "${m:-push}"
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

_spira_modes_lanes() {    # <path> -> lane set from .spira/modes; empty if absent
    local mf="$1/.spira/modes" raw
    [ -f "$mf" ] || return 0
    raw="$(tr -d '[:space:]' < "$mf" 2>/dev/null)" || return 0
    [ -z "$raw" ] && return 0
    _spira_expand_lanes ".spira/modes" "$raw"
}

_spira_fayths_lane_set() {    # -> space-separated lane labels the configured fayths admit
    local p="${SPIRA_PLAN_LABEL:-plan}"
    local inc="${SPIRA_INCIDENT_LABEL:-incident}"
    local gr="${SPIRA_GROOMER_LABEL:-groom}"
    local mae="${SPIRA_MAECHEN_LABEL:-maechen-sweep}"  # literal-ok: bash fallback; SPIRA_MAECHEN_LABEL set by conf.sh
    local sp="${SPIRA_SPIKE_LABEL:-spike}"
    local cz="${SPIRA_CZAR_LABEL:-czar-trigger}"
    local result="" f labels lbl saved="$IFS"
    for f in $(spira_fayths); do
        labels="$(fayth_get "$f" FAYTH_LABELS "" 2>/dev/null)"
        [ -z "$labels" ] && continue
        IFS=,
        for lbl in $labels; do
            IFS="$saved"
            lbl="${lbl#"${lbl%%[![:space:]]*}"}"; lbl="${lbl%"${lbl##*[![:space:]]}"}"
            case "$lbl" in
                "$p"|"$inc"|"$gr"|"$mae"|"$sp"|"$cz")
                    case " $result " in *" $lbl "*) ;; *) result="${result:+$result }$lbl" ;; esac ;;
            esac
            IFS=,
        done
        IFS="$saved"
    done
    [ -n "$result" ] || result="$p"
    printf '%s' "$result"
}

# _spira_lane_diag <name> <path> — effective lanes and per-lane refusal diagnostics.
#
# Outputs one line per category:
#   effective: lane1 lane2 ...
#   refused: <lane> by <side>[, <side>]
#   modes-error: <message>
#
# The effective set is (fayths ∩ repo-map ∩ .spira/modes when present).
# Absent .spira/modes does not restrict. A lane only generates a refused line when
# at least one explicit source (map column or .spira/modes file) proposes it and
# another source denies it — so absent column + absent file produces no diagnostics.
_spira_lane_diag() {
    local name="$1" path="$2"
    local p="${SPIRA_PLAN_LABEL:-plan}"
    local inc="${SPIRA_INCIDENT_LABEL:-incident}"
    local gr="${SPIRA_GROOMER_LABEL:-groom}"
    local mae="${SPIRA_MAECHEN_LABEL:-maechen-sweep}"  # literal-ok: bash fallback; SPIRA_MAECHEN_LABEL set by conf.sh
    local sp="${SPIRA_SPIKE_LABEL:-spike}"
    local cz="${SPIRA_CZAR_LABEL:-czar-trigger}"
    local all_known="$p $inc $gr $mae $sp $cz"
    local raw_map_col map_lanes fayths_lanes
    local modes_exists modes_lanes modes_raw _ml_err_file _ml_rc
    local effective lane in_map in_modes who
    raw_map_col="$(repo_field "$name" lanes 2>/dev/null)"
    map_lanes="$(spira_repo_lanes "$name")" || return 1
    fayths_lanes="$(_spira_fayths_lane_set)"
    modes_exists=0 modes_lanes="$all_known"
    if [ -f "$path/.spira/modes" ]; then
        modes_exists=1
        modes_raw="$(tr -d '[:space:]' < "$path/.spira/modes" 2>/dev/null)"
        if [ -n "$modes_raw" ]; then
            _ml_err_file="$(mktemp)"
            _ml_rc=0
            modes_lanes="$(_spira_modes_lanes "$path" 2>"$_ml_err_file")" || _ml_rc=$?
            if [ "$_ml_rc" -ne 0 ]; then
                printf 'modes-error: %s\n' "$(cat "$_ml_err_file")"
                modes_lanes="$p"
            fi
            rm -f "$_ml_err_file"
        else
            modes_lanes="$p"
        fi
    fi
    effective=""
    for lane in $all_known; do
        case " $map_lanes " in *" $lane "*) ;; *) continue ;; esac
        case " $fayths_lanes " in *" $lane "*) ;; *) continue ;; esac
        if [ "$modes_exists" = 1 ]; then
            case " $modes_lanes " in *" $lane "*) ;; *) continue ;; esac
        fi
        effective="${effective:+$effective }$lane"
    done
    [ -n "$effective" ] || effective="$p"
    printf 'effective: %s\n' "$effective"
    for lane in $all_known; do
        in_map=0 in_modes=0
        [ -n "$raw_map_col" ] && case " $map_lanes " in *" $lane "*) in_map=1 ;; esac
        [ "$modes_exists" = 1 ] && case " $modes_lanes " in *" $lane "*) in_modes=1 ;; esac
        [ "$in_map" = 1 ] || [ "$in_modes" = 1 ] || continue
        case " $effective " in *" $lane "*) continue ;; esac
        who=""
        [ "$in_map" = 0 ] && who="repo-map"
        if [ "$modes_exists" = 1 ] && [ "$in_modes" = 0 ]; then
            who="${who:+$who, }.spira/modes"
        fi
        case " $fayths_lanes " in *" $lane "*) ;; *) who="${who:+$who, }SPIRA_FAYTHS" ;; esac
        [ -n "$who" ] && printf 'refused: %s by %s\n' "$lane" "$who"
    done
}

# --------------------------------------------------------------------------------------
# THE CI PARK, AND THE TWO WAYS IT BECOMES A LIE.
#
# An aeon parks a bead on `$SPIRA_CI_LABEL` once its pull request is open, so nothing pays an
# Opus session to sit and watch a test suite. The label is excluded from every fayth's
# predicate AND from the stalled-work report, which is what stops parked work looking
# abandoned — and is exactly what makes a park applied where no run exists permanent and
# invisible: not claimable, not reported, and displayed as "in CI", the one description that
# stops anybody looking for the real cause. A bead reached 22 reclaims that way, not one of
# them a work failure.
#
#   no-ci    the repository does not land through pull requests, so there is no run and never
#            will be one. Only `pr` mode opens one: `push` merges the branch itself and `hold`
#            leaves it for a human, and for both of those the landing gate IS the gate, so
#            once it passes there is nothing further to wait for. An UNMAPPED repository
#            answers here too, and should — a repository the map cannot resolve cannot land
#            at all, so a park on it is waiting for something that has no mechanism.
#            This is also the case a check made at the moment of parking could not catch: a
#            bead that MOVED repository while parked was parked correctly and is not now.
#   expired  whatever the repository, a park older than the longest plausible run is not
#            parked, it is lost. Expiring it hands the bead back to the report that would
#            have found it (law-absence-needs-a-positive-control).
#   watch    a pull-request repository, inside the deadline. Leave it alone.
#
# RC 2 MEANS THE PARK COULD NOT BE AGED — a missing or unparseable timestamp. It prints
# `watch` with it, because the two callers want different things from that and neither wants
# a guess: the sweep must not strip a label on the strength of a clock it could not read,
# while the pane must not paint an unreadable check as normal. A broken check that renders as
# all-clear displaces the suspicion that would have prompted a look.
#
# Pure decision — no database, no network, no writes — so the sweep and the pane share one
# answer instead of two that can disagree, and a suite can drive every branch of it.
# --------------------------------------------------------------------------------------
spira_ci_park_state() {  # spira_ci_park_state <repo-name> <updated-at> -> watch|no-ci|expired
    local name="${1:-}" ts="${2:-}" max t now
    [ "$(repo_land "$name")" = pr ] || { printf 'no-ci'; return 0; }
    max="${SPIRA_CI_PARK_MAX:-5400}"
    case "$max" in ''|*[!0-9]*) max=5400 ;; esac
    [ "$max" -gt 0 ] || { printf 'watch'; return 0; }   # 0 disables the deadline, deliberately
    # THE EMPTY TIMESTAMP IS REFUSED BEFORE `date` SEES IT. `date -d ""` does not fail — it
    # answers midnight today — so an absent updated_at read as a park several hours old and
    # expired itself, silently, on a field the caller never had. A missing input must reach
    # the caller as "could not age this", never as a verdict.
    [ -n "$ts" ] || { printf 'watch'; return 2; }
    # `date -d` and not python: this is called once per parked bead from a pane that repaints,
    # and an interpreter start per bead is the cost that makes a dashboard shell out and freeze.
    t="$(date -u -d "$ts" +%s 2>/dev/null)" || t=""
    [ -n "$t" ] || { printf 'watch'; return 2; }
    now="$(date -u +%s)"
    if [ "$(( now - t ))" -gt "$max" ]; then printf 'expired'; else printf 'watch'; fi
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

# repo_of_labels <label...> -> the `repo:` name carried by a label list, or nothing.
# Reads from labels already in hand rather than issuing a query, because the caller that
# matters — aeon.sh — is holding the JSON `bd ready --claim` just handed it.
repo_of_labels() {
    local l
    for l in "$@"; do
        case "$l" in repo:*) printf '%s' "${l#repo:}"; return 0 ;; esac
    done
    return 1
}

bead_branch() {          # bead_branch <id> -> its recorded branch, or the derived default
    # The recorded affinity, read back via bd state. Falls through to the derived name so
    # a bead filed before branches were recorded still resolves (law-branch-affinity-is-recorded).
    # bd state prints "(no branch state set)" when the dimension has never been written;
    # strip the sentinel before the fallback test.
    local id="$1" br
    br="$(bdq state "$id" branch 2>/dev/null)"
    case "$br" in '('*) br="" ;; esac
    printf '%s' "${br:-spira/$id}"
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
ref_remote() {           # ref_remote <ref> -> its remote, or non-zero if the ref is local
    case "${1:-}" in */*) printf '%s' "${1%%/*}" ;; *) return 1 ;; esac
}
ref_branch() {           # ref_branch <ref> -> the branch name, without any remote
    printf '%s' "${1#*/}"
}
qualify_base_ref() {     # qualify_base_ref <ref> <repo> -> refs/remotes/... or original
    # A bare origin/main is ambiguous when refs/heads/origin/main also exists. Use the
    # fully-qualified remote-tracking ref so git commands resolve it deterministically.
    local ref="$1" repo="$2" remote branch fq
    remote="$(ref_remote "$ref")" || { printf '%s' "$ref"; return 0; }
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

# --------------------------------------------------------------------------------------
# worktree_of <branch> [repo] -> the registered worktree path holding it, or empty.
# Read from `git worktree list --porcelain` rather than guessed from the bead id, so a
# worktree someone put somewhere else is still found.
# --------------------------------------------------------------------------------------
worktree_of() {
    local br="$1" repo="${2:-$(repo_root)}"
    git -C "$repo" worktree list --porcelain 2>/dev/null | python3 -c '
import sys
want = "refs/heads/" + sys.argv[1]
path = None
for line in sys.stdin:
    line = line.rstrip("\n")
    if line.startswith("worktree "): path = line[9:]
    elif line.startswith("branch ") and line[7:] == want and path:
        print(path); break
' "$br" 2>/dev/null
}

# --------------------------------------------------------------------------------------
# hold_alive <pidfile> -> 0 if the recorded pid is a live process. Unlike aeon_alive this
# does NOT check argv, because the holder can be any process — a brain session, the
# concierge, a hand-run tool. The recycled-pid risk is accepted: a hold is short-lived
# manual work, and a false positive only delays reclamation, while a false negative (the
# aeon_alive failure this fixes) steals work out from under an operator mid-landing.
# --------------------------------------------------------------------------------------
hold_alive() {
    local pf="$1" pid
    [ -f "$pf" ] || return 1
    pid="$(cat "$pf" 2>/dev/null)"
    [ -n "${pid:-}" ] || return 1
    [ -d "/proc/$pid" ] || return 1
    return 0
}

# --------------------------------------------------------------------------------------
# holder_alive <id> -> 0 if a live process is working this bead. Checks BOTH hold
# pidfiles (non-aeon actors: brain session, concierge, hand-run tools) and aeon pidfiles.
# The two use different liveness tests: a hold is checked by pid only (the holder can be
# anything), an aeon is checked by pid AND argv (a recycled pid must not resurrect a dead
# aeon's claim). Both satisfy the SAME predicate the reaper reads, so the two can never
# disagree.
# --------------------------------------------------------------------------------------
holder_alive() {
    local id="$1" pf
    for pf in "$SPIRA_RUN"/hold-"$id".pid; do
        [ -e "$pf" ] || continue
        hold_alive "$pf" && return 0
    done
    for pf in "$SPIRA_RUN"/aeon-*-"$id".pid; do
        [ -e "$pf" ] || continue
        aeon_alive "$pf" && return 0
    done
    return 1
}

# ======================================================================================
# DESTRUCTION. Every removal of a bead's worktree or branch goes through this section, and
# nothing outside it may call `git worktree remove`, `git branch -D` or `rm -rf` on a tree.
#
# WHY IT IS ONE SITE. A bead's worktree and branch were both destroyed while its aeon was
# mid-edit and its lease was live, taking forty minutes of uncommitted work, writing no
# salvage, and NAMING THE BEAD IN NO LOG — the Sending's own passes bracket the deletion and
# report the tree HELD on one side and 0 reaped on the other, so the actor was some other
# process entirely. It had happened twenty times to the same bead, every note reading
# "Reclaimed by strand.sh", which is the signature an aeon leaves when its workspace vanishes
# underneath it: twenty aeons spent re-deriving work the harness then ate.
#
# The lesson is not "fix that caller". Deletion was SPREAD ACROSS SIX SITES, each with its own
# guard or none, reachable by anything that sources this file with the default environment —
# including a test suite run from the installed tree, which is how an operator's real
# checkouts were once swept (see test-sending.sh's own header). A rule enforced at six sites
# is a rule enforced at whichever of them the next caller does not use. So the rule lives at
# one site, it is unconditional, and it does not care who is calling.
#
# WHAT IT REFUSES, and why each is not optional:
#
#   • A path outside `$SPIRA_RUN/worktree/`. A deleter handed anything else has been
#     misconfigured — a fixture that forgot to set SPIRA_RUN, a repo-map naming a real
#     checkout — and misconfiguration must not be able to reach `rm -rf`.
#   • TWO liveness witnesses, not one. `holder_alive` reads a pidfile that is absent for the
#     seconds between `bd ready --claim` and the aeon writing it; the bead's status is stale
#     for as long as it takes a killed aeon's lease to be reclaimed. Either alone has a blind
#     spot the other covers, so BOTH must say nobody is home.
#   • A status witness that could not be examined at all. An unreachable database answers
#     "not in_progress" exactly as a genuinely open bead does, and the wrong one of those
#     reads as permission (law-absence-needs-a-positive-control). The probe is proved able to
#     answer once per process before any absence is believed.
#   • A salvage that did not succeed. Salvage runs BEFORE the removal, and its failure aborts
#     the removal rather than being stepped over.
#
# And it LOGS EVERY DECISION, naming the bead and the calling program, to $SPIRA_REAPLOG —
# so the next occurrence is one `grep` rather than four hours of correlating timestamps.
# ======================================================================================
SPIRA_REAPLOG="${SPIRA_REAPLOG:-$SPIRA_RUN/reap.log}"

# The program that reached the chokepoint, read from /proc rather than matched against a
# command line: a pattern matches the searcher's own argv, which is how `pgrep -f` reported
# a collector healthy by finding the shell that was killing it. Walks the ancestor chain,
# because the interesting name is rarely the immediate one — `sending.sh` tells you nothing,
# `test-repo.sh -> sending.sh` tells you everything.
spira_caller() {
    local pid=$$ n=0 out="" cmd first
    while [ "$n" -lt 6 ] && [ "$pid" != 1 ] && [ -r "/proc/$pid/cmdline" ]; do
        cmd="$(tr '\0' '\n' < "/proc/$pid/cmdline" 2>/dev/null | grep -v '^-' | head -3 | tr '\n' ' ')"
        first="$(printf '%s' "$cmd" | tr ' ' '\n' | grep -E '\.(sh|py)$' | head -1)"
        [ -n "$first" ] && out="$(basename "$first")${out:+ -> $out}"
        pid="$(awk '{print $4}' "/proc/$pid/stat" 2>/dev/null)" || break
        [ -n "${pid:-}" ] || break
        n=$((n+1))
    done
    printf '%s' "${out:-unknown}"
}

spira_reaplog() {        # spira_reaplog <verb> <id> <detail>
    mkdir -p "$(dirname "$SPIRA_REAPLOG")" 2>/dev/null
    printf '%s %-9s %-22s %s [by %s]\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$2" "${3:-}" "$(spira_caller)" \
        >> "$SPIRA_REAPLOG" 2>/dev/null || true
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
    now="$(date -u +%s)"
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
declare -A SPIRA_STATUS_MAP=()
SPIRA_STATUS_SEAM=0
spira_status_seam() {    # spira_status_seam <file|-> — load the map once
    local sid sst
    while IFS=$'\t' read -r sid sst; do
        [ -n "${sid:-}" ] && SPIRA_STATUS_MAP["$sid"]="${sst:-}"
    done < <(if [ "$1" = - ]; then cat; else cat "$1"; fi)
    SPIRA_STATUS_SEAM=1
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
# bead reads as free. Proved once per process against a bead known to exist — the goal bead,
# which is the one row this harness cannot run without — and cached, because it gates a loop
# that runs every two minutes over seven repositories.
SPIRA_DB_OK=""
spira_db_reachable() {
    [ "$SPIRA_STATUS_SEAM" = 1 ] && return 0
    if [ -z "$SPIRA_DB_OK" ]; then
        if [ -n "$(bdjson show "$SPIRA_GOAL" 2>/dev/null | json_only | head -c 1)" ]; then
            SPIRA_DB_OK=1
        else
            SPIRA_DB_OK=0
        fi
    fi
    [ "$SPIRA_DB_OK" = 1 ]
}

# spira_holder_witnesses <id> -> 0 and prints WHY somebody may be home; 1 if nobody is.
spira_holder_witnesses() {
    local id="$1" st
    if holder_alive "$id"; then
        printf 'a live process holds it'; return 0
    fi
    if ! spira_db_reachable; then
        printf 'the bead database did not answer, so the status witness proves nothing'; return 0
    fi
    st="$(spira_bead_status "$id")"
    if [ "$st" = in_progress ]; then
        printf 'in_progress — the lease has not been released'; return 0
    fi
    return 1
}

# --------------------------------------------------------------------------------------
# Salvage before destroying, and REFUSE TO DESTROY IF IT FAILS. Anything uncommitted in a
# dead aeon's worktree is usually scratch — the aeon's real work is committed, which is what
# made the branch eligible — but "usually" is not a licence, and it has cost real work: the
# uncommitted state WAS the work, forty minutes of it, four times over.
#
# UNTRACKED FILES ARE CARRIED BY CONTENT, not by name. `git diff HEAD` cannot see them, so
# the old salvage listed them and let them go — and a new file is exactly what an aeon
# building something has most of, so the patch was emptiest precisely when it mattered most.
# They go into a tar beside the patch, filtered by `--exclude-standard` so a build directory
# does not turn a salvage into a gigabyte.
#
# THE FILENAME CARRIES A TIMESTAMP because the old one did not. Every reap of a bead wrote
# `<id>.patch`, so twenty reaps of one bead left exactly one patch: nineteen salvages destroyed
# by the salvage machinery itself, silently, each one reported as a success.
# --------------------------------------------------------------------------------------
SALVAGED=""
salvage() {              # salvage <label> <worktree-path> -> 0 saved or nothing to save
    local id="$1" w="$2" dirty out="$SPIRA_RUN/reaped" stamp base rc=0 untracked
    SALVAGED=""
    # A worktree whose status cannot be read is not a clean one; it is a question. Fail
    # closed — the caller aborts its removal.
    if ! dirty="$(git -C "$w" status --porcelain 2>/dev/null)"; then
        spira_reaplog SALVAGE "$id" "cannot read the status of $w — refusing to call it clean"
        return 1
    fi
    [ -n "$dirty" ] || return 0
    mkdir -p "$out" || { spira_reaplog SALVAGE "$id" "cannot create $out"; return 1; }
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    base="$out/$id.$stamp"
    # `|| true` on the diff, and the verdict taken from the FILE rather than the group. A
    # worktree whose branch ref was deleted underneath it has an unborn HEAD, so `git diff
    # HEAD` legitimately fails there — and that is the orphan case, the one where salvage
    # matters most. Letting its exit status stand as the group's turned every orphan salvage
    # into a refusal. What is actually being asked is "did the bytes get written".
    {
        printf '# %s — uncommitted at reap time, %s\n' "$id" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '# tracked changes are below; untracked file CONTENT is in %s\n' "$(basename "$base").untracked.tar"
        printf '%s\n\n' "$dirty"
        git -C "$w" diff HEAD 2>/dev/null || true
    } > "$base.patch" 2>/dev/null
    [ -s "$base.patch" ] || rc=1

    untracked="$(git -C "$w" ls-files --others --exclude-standard 2>/dev/null)"
    if [ -n "$untracked" ]; then
        git -C "$w" ls-files --others --exclude-standard -z 2>/dev/null \
            | tar -C "$w" --null -T - -cf "$base.untracked.tar" 2>/dev/null || rc=1
        [ -s "$base.untracked.tar" ] || rc=1
    fi

    if [ "$rc" -ne 0 ]; then
        spira_reaplog SALVAGE "$id" "FAILED to write $base.patch — the removal must not proceed"
        return 1
    fi
    SALVAGED="$base.patch"
    spira_reaplog SALVAGE "$id" "wrote $base.patch${untracked:+ and $base.untracked.tar}"
    printf '  salvaged uncommitted changes to %s\n' "$base.patch"
    return 0
}

# --------------------------------------------------------------------------------------
# spira_destroy_worktree <id> <path> <repo> <why> -> 0 removed or nothing to remove
# --------------------------------------------------------------------------------------
spira_destroy_worktree() {
    local id="$1" w="$2" repo="$3" why="${4:-}" held
    [ -n "$w" ] || return 0
    # ABSENT DIRECTORY FIRST — before the fence. A path whose directory no longer exists
    # needs no removal: only a registry prune to clear the dangling entry. git worktree prune
    # touches nothing on disk, so it is safe regardless of where the path points. The fence
    # below guards rm -rf; it does not apply here.
    if [ ! -e "$w" ]; then
        # The directory has already gone but its REGISTRATION may not have, and a live
        # registration is enough to make `git branch -D` refuse — which is how an interrupted
        # reap leaves a branch that can never be deleted. Nothing here is left to salvage or
        # destroy, so clear the entry (through the prune that repairs rather than orphans)
        # and report success.
        spira_prune_worktrees "$repo" >/dev/null 2>&1
        return 0
    fi
    # THE FENCE. A deleter handed a path outside the harness's own scratch directory has been
    # misconfigured — a fixture that forgot to set SPIRA_RUN, a repo-map naming a real
    # checkout — and a misconfigured caller must not rm -rf an arbitrary path. The
    # absent-directory case is handled above; the fence guards only paths whose directories exist.
    case "$w" in
        "$SPIRA_RUN/worktree/"?*) ;;
        *) spira_reaplog REFUSED "$id" "$w is not under $SPIRA_RUN/worktree — refusing to remove it"
           return 1 ;;
    esac
    if held="$(spira_holder_witnesses "$id")"; then
        spira_reaplog REFUSED "$id" "worktree $w — $held"
        return 1
    fi
    if ! salvage "$id" "$w"; then
        spira_reaplog REFUSED "$id" "worktree $w — salvage failed, so the removal is abandoned"
        return 1
    fi
    # Logged BEFORE the act as well as after: a process killed between the two leaves a
    # record that it was about to delete this tree. The absence of that one line turned the
    # incident this section exists for into a four-hour forensic exercise.
    spira_reaplog REMOVING "$id" "worktree $w ($why)"
    git -C "$repo" worktree remove --force "$w" 2>/dev/null \
        || { rm -rf "$w"; spira_prune_worktrees "$repo"; }
    if [ -e "$w" ]; then
        spira_reaplog FAILED "$id" "worktree $w survived removal"
        return 1
    fi
    spira_reaplog REMOVED "$id" "worktree $w"
    return 0
}

# --------------------------------------------------------------------------------------
# spira_destroy_branch <id> <branch> <repo> <why> [caller] -> 0 gone, 1 refused or survived.
# The witnesses are re-read rather than inherited from the worktree removal: they are two
# /proc reads and a cached status, and the alternative is a decision made before the act.
#
# CONTENT, NOT ANCESTRY. This function guards its deletion with content_landed, the same
# predicate the Sending uses to SELECT branches for deletion — "would merging this branch
# change the base tree?" An empty-commit branch (e.g. a review-only bead) passes: its diff
# is empty, so the merge is a no-op, and the content is already on the base. Ancestry alone
# answers "not landed" about such a branch forever, because the squash commit is not a
# direct ancestor — which is precisely why the selector rejected ancestry as the question.
# The fence must ask the same question or the two sides contradict: the selector says yes,
# the fence says no, and the branch accretes refusals until a human notices.
#
# CALLER EXCEPTIONS. Pass a non-empty fifth argument when the caller has already verified
# that deletion is safe, so the content check is not repeated here:
#   "sending" — the Sending's selector already ran content_landed (or the superseded
#               exception), and send_branch confirmed liveness once more before this call.
#   "slain"   — slay.sh has already parked any unlanded commits at refs/slain/<id>; the
#               branch still carries content not on the base, but the durable copy is no
#               longer in refs/heads alone, so deletion is safe.
# Any other non-empty value is treated the same way (future callers that have verified
# safety by their own means). An empty fifth argument applies the content fence.
# --------------------------------------------------------------------------------------
spira_destroy_branch() {
    local id="$1" br="$2" repo="$3" why="${4:-}" caller="${5:-}" held wt err base
    git -C "$repo" show-ref --verify -q "refs/heads/$br" || return 0
    # CERTIFIED/BATCHED GUARD. A branch whose landstate is CERTIFIED or BATCHED is in the
    # merge queue. batch.sh selects by ref; deleting the ref drops the branch from the next
    # batch with no log line. No caller bypass: even the Sending must not race verdict.sh's
    # LANDED write.
    local _ls_file="${SPIRA_RUN:-}/landstate/$id"
    if [ -r "$_ls_file" ]; then
        local _lstate
        read -r _lstate _ < "$_ls_file" 2>/dev/null || _lstate=""
        case "${_lstate:-}" in
            CERTIFIED|BATCHED)
                SPIRA_DESTROY_ERR="certified-queued"
                spira_reaplog REFUSED "$id" "branch $br — landstate is $_lstate; not deleting a queued branch"
                return 1 ;;
        esac
    fi
    if held="$(spira_holder_witnesses "$id")"; then
        spira_reaplog REFUSED "$id" "branch $br — $held"
        return 1
    fi
    # A branch a worktree still holds is not deletable, and forcing the issue by pruning the
    # registration out from under it is how a live tree becomes an orphan.
    wt="$(worktree_of "$br" "$repo")"
    if [ -n "$wt" ] && [ -e "$wt" ]; then
        spira_reaplog REFUSED "$id" "branch $br is checked out at $wt"
        return 1
    fi
    # CONTENT FENCE. The fence fires only when the caller has not already verified safety.
    # NEVER use ancestry (merge-base --is-ancestor) here: that rejects empty-commit branches
    # whose squash commit is not a direct ancestor, contradicting the selector that approved them.
    if [ -z "$caller" ] \
       && base="$(spira_landref "$repo" 2>/dev/null)" \
       && git -C "$repo" rev-parse -q --verify "$base" >/dev/null 2>&1; then
        if ! content_landed "$repo" "$br" "$base"; then
            spira_reaplog REFUSED "$id" "branch $br — content not on $base, refusing to destroy unlanded work ($why)"
            return 1
        fi
    fi
    spira_reaplog REMOVING "$id" "branch $br ($why)"
    # SPIRA_REF_SANCTIONED is what the reference-transaction hook reads. Set it ONLY
    # here and on this one command: every guard that makes a deletion safe — the
    # content_landed check above, the worktree check, the reaplog entry — has already
    # run by this line. Exporting it any wider would hand the override to the callers
    # the hook exists to stop.
    err="$(SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$br" 2>&1)"
    if git -C "$repo" show-ref --verify -q "refs/heads/$br"; then
        spira_reaplog FAILED "$id" "branch $br survived deletion: $(head -1 <<< "$err")"
        SPIRA_DESTROY_ERR="$(head -1 <<< "$err")"
        return 1
    fi
    spira_reaplog REMOVED "$id" "branch $br"
    return 0
}

# --------------------------------------------------------------------------------------
# worktree_evict_foreign <work> <repo> — move a worktree aside unless it demonstrably belongs
# to <repo>: another repository's, or one whose `.git` resolves to nothing. Prints the path it
# was moved to. rc 0 = moved, 1 = nothing to do, 2 = refused.
#
# A WORKTREE PATH IS KEYED ON THE BEAD, AND A BEAD'S REPOSITORY CAN CHANGE. `repo:` is a
# label, and correcting one is a deliberate mechanism — the landing gate refuses a branch cut
# in the wrong repository, and the answer is to repoint the bead so the next aeon works it in
# the right checkout. But the worktree path is derived from the bead id alone, so it is the
# same path before and after, and a caller that reuses whatever it finds there makes the
# correction unenforceable: one repointed bead kept the OLD repository's worktree, and every
# summon after it attached to that tree, rebased the old repository's branch onto the old
# repository's base, and handed the aeon a checkout in which the files the bead names do not
# exist. Nothing failed and nothing said so, because `git worktree add` was never reached.
#
# MOVED ASIDE, NEVER REMOVED. The tree may hold uncommitted work from an aeon that died, and
# a harness that deletes a tree to unblock itself is one that can destroy the only copy of
# something. `git worktree move` keeps both registrations honest; a plain mv followed by
# `worktree repair` is the fallback for a git that refuses the move.
#
# THE COMPARISON IS THE COMMON GIT DIR, resolved absolute, not the path or the remote. Two
# checkouts of the same repository are legitimately different directories, and a worktree
# always shares its parent's object store — so the common dir is the one identity that
# answers "is this tree part of that repository" without a guess.
worktree_evict_foreign() {
    local work="$1" repo="$2" have want other aside
    [ -e "$work/.git" ] || return 1
    want="$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 1
    [ -n "$want" ] || return 1

    # AN UNREADABLE TREE IS EVICTED TOO, and it is the case that most needs it. A `.git` that
    # resolves to nothing still satisfies the caller's existence check, so leaving it in place
    # hands the aeon a broken checkout by the same silent route a foreign one does — and
    # `git worktree add` is never reached, so again nothing fails. "Not demonstrably ours" is
    # the test, not "demonstrably another's".
    have="$(git -C "$work" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || have=""
    [ "$have" != "$want" ] || return 1

    other="$(basename "$(dirname "$have")")"
    aside="$work.${other:-foreign}"
    [ -e "$aside" ] && aside="$aside.$(date +%s)"
    # `worktree move` keeps the OWNING repository's registration pointing at the tree, which a
    # prune of the wanted repository cannot do — the foreign tree is registered in the foreign
    # repo, so pruning this one leaves that one advertising a path that has gone. `repair` is
    # the same job after a plain mv, and it is BEST EFFORT: once the directory has moved the
    # eviction has happened, and reporting "refused" for a failed re-registration would make
    # the caller die over a tree that is already out of the way.
    if ! git -C "$work" worktree move "$work" "$aside" >/dev/null 2>&1; then
        mv "$work" "$aside" 2>/dev/null || return 2
        git -C "$aside" worktree repair "$aside" >/dev/null 2>&1 || true
    fi
    printf '%s' "$aside"
    return 0
}

# spira_prune_worktrees <repo> — `git worktree prune`, with the one case it gets wrong.
#
# Prune is safe on the reading everyone has of it: it drops admin entries for directories
# that are already gone, and one witness is plenty for a directory that does not exist. But
# an entry is ALSO prunable when the worktree's own `.git` file is missing or unreadable
# while the directory is entirely intact and full of work. Pruning that entry frees the
# branch for `git branch -D` and leaves a live tree registered nowhere — the exact state
# PASS 2 of the Sending then classifies as an orphan and removes.
#
# So: anything prune would drop whose DIRECTORY STILL EXISTS is repaired instead, and every
# entry that really is pruned is named in the reap log. `git worktree repair` restores the
# link both ways and is a no-op on a healthy tree.
# --------------------------------------------------------------------------------------
# `prune --dry-run --verbose` reports on STDERR, not stdout. Reading it with a plain `2>/dev/null`
# — the shape every other git call in this harness uses — yields nothing at all, and a guard
# fed an empty list approves everything (law-absence-needs-a-positive-control).
spira_prune_worktrees() {
    local repo="$1" line name path common still=0
    common="$(git -C "$repo" rev-parse --git-common-dir 2>/dev/null)" || return 0
    case "$common" in /*) ;; *) common="$repo/$common" ;; esac

    _spira_prunable_path() {   # <entry-name> -> the worktree directory git recorded for it
        local gd; gd="$(cat "$common/worktrees/$1/gitdir" 2>/dev/null)"; printf '%s' "${gd%/.git}"
    }

    while IFS= read -r line; do
        case "$line" in "Removing "*) ;; *) continue ;; esac
        name="${line#Removing }"; name="${name#worktrees/}"; name="${name%%:*}"
        [ -n "$name" ] || continue
        path="$(_spira_prunable_path "$name")"
        if [ -n "$path" ] && [ -d "$path" ]; then
            spira_reaplog REPAIRED "$name" "prune would have dropped a worktree whose directory EXISTS at $path — repairing instead"
            git -C "$repo" worktree repair "$path" >/dev/null 2>&1
        else
            spira_reaplog PRUNED "$name" "$line"
        fi
    done < <(git -C "$repo" worktree prune -n -v 2>&1 >/dev/null)

    # Re-read after the repairs. `git worktree prune` has no way to skip one entry, so if any
    # live directory is STILL prunable the only safe move is not to prune at all: a leaked
    # admin entry is untidy, and unregistering a tree an aeon is working in is not recoverable.
    while IFS= read -r line; do
        case "$line" in "Removing "*) ;; *) continue ;; esac
        name="${line#Removing }"; name="${name#worktrees/}"; name="${name%%:*}"
        path="$(_spira_prunable_path "$name")"
        if [ -n "$path" ] && [ -d "$path" ]; then
            spira_reaplog REFUSED "$name" "still prunable with its directory intact at $path — skipping the prune entirely"
            still=1
        fi
    done < <(git -C "$repo" worktree prune -n -v 2>&1 >/dev/null)
    unset -f _spira_prunable_path
    [ "$still" = 1 ] && return 1

    git -C "$repo" worktree prune 2>/dev/null
    return 0
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
# THE OWNERSHIP FENCE. An installation that imported a predecessor's databases holds
# thousands of beads that predecessor is still writing to. An aeon that claims one of them is
# racing a live worker, and both of them will do the work.
#
# `spira` is the ownership marker: measured, every native bead carried it and no imported one
# did. That makes it the one label a predicate can be
# REQUIRED to have — where `plan` or `incident` are each one persona's partition, `spira`
# is the boundary of the whole system. Until now the boundary held only because two config
# strings happened to be right, and a new fayth written without `spira` in FAYTH_LABELS
# would consume the replica with nothing objecting. Refuse instead.
#
# This is not an embargo that expires at cutover. After cutover an imported bead becomes
# Spira's by being LABELLED `spira`, one bead or one batch at a time and deliberately, so
# the fence goes on meaning "Spira owns this" rather than "not yet".
#
# The herestring is not a pipe: `grep -q` closing it early cannot SIGPIPE a writer, which
# is the trap law-no-grep-q-under-pipefail names.
# --------------------------------------------------------------------------------------
fayth_fenced() {         # fayth_fenced <name> <FAYTH_LABELS> -> 0 if safe to claim
    local name="$1" labels="${2:-}"
    if [ -z "$labels" ]; then
        log "FENCE $name: FAYTH_LABELS is empty — that predicate selects the whole database."
        return 1
    fi
    # When SPIRA_SCOPE_LABEL is empty the operator has explicitly disabled scope restriction;
    # any non-empty predicate is intentional. When it is non-empty it must appear in the
    # predicate, so a misconfigured fayth cannot see beads this fleet does not own.
    if [ -z "${SPIRA_SCOPE_LABEL:-}" ]; then
        return 0
    fi
    grep -qx "$SPIRA_SCOPE_LABEL" <<< "${labels//,/$'\n'}" && return 0
    log "FENCE $name: FAYTH_LABELS='$labels' does not require '$SPIRA_SCOPE_LABEL' (SPIRA_SCOPE_LABEL)."
    log "FENCE $name: Add '$SPIRA_SCOPE_LABEL' to it, or set SPIRA_SCOPE_LABEL= to allow unrestricted scope."
    return 1
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

LANDSTATE="${SPIRA_RUN}/landstate"
# Reasons written by the batch/queue eviction machinery. Only these warrant the eviction-race
# reopen in aeon.sh; no-rebase@*, gate and confine are landing.sh REDs with their own paths.
LAND_EVICTION_REASONS="ejected conflicts-with-base rebase-suite-red"

land_mark() {    # land_mark <id> <state> <tip> [reason]
    mkdir -p "$(dirname "$LANDSTATE/$1")" 2>/dev/null || return 0
    printf '%s %s %s %s' "$2" "${3:-none}" "$(date +%s)" "${4:-}" \
        > "$LANDSTATE/$1.$$" 2>/dev/null \
        && mv -f "$LANDSTATE/$1.$$" "$LANDSTATE/$1" 2>/dev/null
}

land_mark_at() { # land_mark_at <id> <state> <tip> <epoch>
    mkdir -p "$(dirname "$LANDSTATE/$1")" 2>/dev/null || return 0
    printf '%s %s %s' "$2" "${3:-none}" "$4" \
        > "$LANDSTATE/$1.$$" 2>/dev/null \
        && mv -f "$LANDSTATE/$1.$$" "$LANDSTATE/$1" 2>/dev/null
}

land_state() {   # land_state <id> -> "<state> <tip> <at> [reason]" or empty
    local f="$LANDSTATE/$1"
    [ -r "$f" ] || return 1
    tr -d '\n' < "$f" 2>/dev/null
}

# queue_certified_list <repo-path>
# Print "<id> <tip> <epoch>" for each CERTIFIED branch with a live ref.
# Reads $SPIRA_RUN/landstate/<id>.
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
# stays non-completed and its PR stays closed, so the orphan-run sweep
# (queue_sweep_orphan_runs) or the next abandon retries it.
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

# queue_sweep_orphan_runs <forge> <repo-dir>
# Cancels every non-completed Gate run on a spira/queue/* branch whose PR is not
# open. Catches runs orphaned before queue_cancel_branch_runs existed, and a PR
# closed by hand outside the abandon/eject/eviction paths. The live batch's own
# branch and main's push runs are unaffected: the live branch's PR is open, and
# a push run's branch is never spira/queue/*.
queue_sweep_orphan_runs() {
    local forge="$1" repo="$2"
    local run_id branch status open_pr rc=0
    while read -r run_id branch status; do
        [ -n "$run_id" ] || continue
        open_pr="$("$forge" pr-number "$repo" "$branch" 2>/dev/null)"
        [ -n "$open_pr" ] && continue
        if "$forge" run-cancel "$repo" "$run_id" >/dev/null 2>&1; then
            printf 'QUEUE SWEEP_CANCEL %s branch=%s run=%s status=%s\n' \
                "$(date +%s)" "$branch" "$run_id" "$status" \
                >> "${SPIRA_RUN:-/tmp}/landing.log" 2>/dev/null || true
        else
            rc=1
            printf 'QUEUE SWEEP_CANCEL_FAILED %s branch=%s run=%s status=%s\n' \
                "$(date +%s)" "$branch" "$run_id" "$status" \
                >> "${SPIRA_RUN:-/tmp}/landing.log" 2>/dev/null || true
            printf 'spira: WARN failed to cancel orphaned run %s for %s\n' \
                "$run_id" "$branch" >&2
        fi
    done < <("$forge" runs-queue-branches "$repo" 2>/dev/null)
    return $rc
}

# compute_gate_key <repo-path> <repo-name> <branch-ref> <base-ref>
# Compute the gate key for a branch. Matches gate.sh's gate_key() computation so that
# a stored key can be compared against the current one to detect gate-command changes.
# Exits non-zero if any input is unreadable or the harness hash is empty.
compute_gate_key() {
    local repo="$1" name="$2" br="$3" base="$4"
    local tree files files_h CMD cmd_h harness_h
    tree="$(git -C "$repo" rev-parse --verify -q "$br^{tree}" 2>/dev/null)" || return 1
    files="$(git -C "$repo" diff --name-only "$base...$br" 2>/dev/null)" || return 1
    files_h="$(printf '%s' "$files" | sha256sum | cut -d" " -f1)"
    CMD="$(repo_gate "$name" 2>/dev/null)"
    cmd_h="$(printf '%s' "$CMD" | sha256sum | cut -d" " -f1)"
    harness_h="$(cat "${SPIRA_HOME:?}/gate.sh" "$SPIRA_HOME/exclude.sh" "$SPIRA_HOME/skew.sh" \
        2>/dev/null | sha256sum | cut -d" " -f1)"
    [ -n "$harness_h" ] || return 1
    printf '%s\n' "$name $tree $files_h $cmd_h $harness_h" | sha256sum | cut -d" " -f1
}

# queue_is_suite_transition <repo-path> <tip> <base-sha>
# 0 if the tip modifies SPIRA_SUITE_STATE_FILE relative to base-sha.
queue_is_suite_transition() {
    git -C "$1" diff --name-only "$3" "$2" 2>/dev/null \
        | grep -qF "${SPIRA_SUITE_STATE_FILE:-spira/suite-state}"
}

# queue_sort_rows <repo-path> <base-sha>
# Read "<id> <tip> <epoch>" lines from stdin; write sort-key rows sorted by batcher order:
#   "<trans_flag> <prio_pad> <epoch_pad> <id> <tip>"
# Set PRIO_JSON env to a bdjson array for priority lookups (defaults to []).
# Sort order: suite-transition first (flag=0), then priority asc, then epoch asc.
# This is the canonical batcher sort used by both batch.sh and the cockpit.
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

    local _id _tip _epoch _is_trans _buf=""
    while read -r _id _tip _epoch; do
        _is_trans=0
        queue_is_suite_transition "$repo" "$_tip" "$base_sha" && _is_trans=1 || true
        _buf="${_buf}${_id} ${_tip} ${_epoch%% *} ${_is_trans}"$'\n'
    done

    _out="$(printf '%s' "$_buf" | PRIO_FILE="$_pjf" python3 -c "
import sys, json, os
try:
    with open(os.environ['PRIO_FILE']) as f: prios = json.load(f)
except Exception: sys.exit(1)
prios = prios if isinstance(prios, list) else [prios]
prio_map = {}
for x in prios:
    if not isinstance(x, dict) or not x.get('id'): continue
    try: prio_map[x['id']] = int(x.get('priority', 9))
    except (TypeError, ValueError): prio_map[x['id']] = 9
rows = []
for line in sys.stdin:
    parts = line.strip().split()
    if len(parts) < 4: continue
    bid, tip, epoch, is_trans = parts[0], parts[1], int(parts[2]), int(parts[3])
    rows.append((1 - is_trans, prio_map.get(bid, 9), epoch, bid, tip))
rows.sort()
for r in rows:
    print('%d %09d %010d %s %s' % r)
")"
    _rc=$?
    rm -f "$_pjf"

    if [ "$_rc" -ne 0 ] || { [ -z "$_out" ] && [ -n "$_buf" ]; }; then
        printf 'queue_sort_rows: ranking failed (rc=%s) -- returning rows unranked\n' "$_rc" >&2
        printf '%s' "$_buf" | awk 'NF >= 4 { printf "%d %09d %010d %s %s\n", 1 - $4, 9, $3, $1, $2 }'
        return 0
    fi
    [ -n "$_out" ] && printf '%s\n' "$_out"
    return 0
}

# ---------------------------------------------------------------------------
# QUEUE BISECT — a red batch with no attributable suite (a build failure, or
# any harness-shaped red with no annotations) narrows by binary search rather
# than by re-running the priority sort, which would just re-select the same
# culprit forever (sp-y931m: PRs 302-304 cycled the same P0 build-breaker).
#
# State is one group ("id:tip id:tip ...") per line in $SPIRA_QUEUE_DIR/<repo>/bisect.
# Line 1 is the group batch.sh must cut next; later lines are untested siblings
# parked by an earlier split, most-recently-parked first (a LIFO worklist), so a
# nested split finishes the branch it is on before returning to an outer sibling.
# ---------------------------------------------------------------------------

queue_bisect_file() { printf '%s/%s/bisect' "${SPIRA_QUEUE_DIR:?}" "$1"; }

# queue_bisect_current <repo-name> -> the forced-next group ("id:tip id:tip"),
# one line. Empty output (rc 1) means no bisect is in progress.
queue_bisect_current() {
    local f; f="$(queue_bisect_file "$1")"
    [ -s "$f" ] || return 1
    head -1 "$f"
}

# queue_bisect_current_certified <repo-name> <certs-blob>
# Print "<id> <tip>" for each member of the current forced group that is still
# CERTIFIED, using its live certified tip from <certs-blob> (lines "id tip
# epoch", as queue_certified_list prints) rather than the tip recorded when the
# group was split — a member re-certified since then is tested at its current
# head. A member no longer CERTIFIED (ejected or landed another way) is
# dropped silently; an empty result means the group has nothing left to cut.
queue_bisect_current_certified() {
    local name="$1" certs="$2" _line _bl _id
    _line="$(queue_bisect_current "$name" 2>/dev/null)" || return 0
    for _bl in $_line; do
        _id="${_bl%%:*}"
        printf '%s\n' "$certs" | awk -v id="$_id" '$1==id{print $1, $2; exit}'
    done
}

# queue_bisect_split <repo-name> <id:tip> [<id:tip> ...]
# The given members just came back red together with no attributable suite.
# Halve them (first half smaller or equal, matching the batcher's existing
# epoch convention) and record the first half as the next forced cut. Any
# group still pending from an earlier split is kept beneath the new second
# half, so it is tested once the branch just opened resolves.
queue_bisect_split() {
    local name="$1"; shift
    local members=("$@")
    local half=$(( ${#members[@]} / 2 )) i
    local a=() b=()
    for i in "${!members[@]}"; do
        if [ "$i" -lt "$half" ]; then a+=("${members[$i]}"); else b+=("${members[$i]}"); fi
    done
    local f; f="$(queue_bisect_file "$name")"
    local rest=""
    [ -s "$f" ] && rest="$(tail -n +2 "$f")"
    mkdir -p "$(dirname "$f")" 2>/dev/null || true
    {
        printf '%s\n' "${a[*]}"
        printf '%s\n' "${b[*]}"
        [ -n "$rest" ] && printf '%s\n' "$rest"
        true
    } > "$f.$$" && mv -f "$f.$$" "$f"
}

# queue_bisect_advance <repo-name>
# The current group is resolved (ejected down to nothing left to test, or
# landed clean) — drop it and promote the next pending group, if any. Removes
# the state file once the worklist is empty, ending the bisect.
queue_bisect_advance() {
    local f; f="$(queue_bisect_file "$1")"
    [ -s "$f" ] || return 0
    local rest; rest="$(tail -n +2 "$f")"
    if [ -n "$rest" ]; then
        printf '%s\n' "$rest" > "$f.$$" && mv -f "$f.$$" "$f"
    else
        rm -f "$f"
    fi
}

# queue_bisect_resolve <repo-name> <members-str>
# <members-str> (space-separated "id:tip") just landed clean. If it is exactly
# the group the bisect is currently testing, that group is innocent — advance
# to the next pending group so the culprit is sought there next.
queue_bisect_resolve() {
    local name="$1" members_str="$2" _cur
    _cur="$(queue_bisect_current "$name" 2>/dev/null)" || return 0
    [ "$(printf '%s\n' $_cur | sort)" = "$(printf '%s\n' $members_str | sort)" ] \
        && queue_bisect_advance "$name"
    return 0
}

# gh_issue_closeout — comment and close the GitHub issue linked to a landed bead.
#
# The write-back complement to gh-intake.sh's one-way ingest. Intake holds no
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

# gh_issue_ask_unlanded — ask the operator what to do about a GitHub issue whose
# bead closed without a commit landing on the base branch.
#
# One ask per issue, deduped through ask_already_open.
gh_issue_ask_unlanded() {  # gh_issue_ask_unlanded <bead-id> <external-ref> [draft]
    local id="$1" ext_ref="$2" draft="${3:-}"
    local _subj _dflt gh_part gh_repo issue_n _st _err _rc

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

    [ -x "${SPIRA_HOME}/mail.sh" ] || return 0
    _subj="Close GitHub issue $ext_ref for bead $id"
    ask_already_open "$_subj" && return 0

    _dflt="${draft:-post a comment explaining the resolution and close the issue}"

    _err="$("${SPIRA_HOME}/mail.sh" send operator \
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
        log "gh-closeout $id: ask refused — probe fault: mail.sh produced no reason"
    fi
}

# _gh_unlanded_scan — run at the end of a landing pass to ask about GitHub issues
# whose beads closed without a landing. Called once per pass; one ask per issue
# via ask_already_open.
_gh_unlanded_scan() {
    local _tmp _id _ext _superseder _ls_file _ls_st _sup_ls _sup_st _sup_sha _draft
    local _wait_dir _wait_file _now _last
    _tmp="$(mktemp)" || return 0
    _wait_dir="${SPIRA_RUN:-/tmp}/gh-wait-log"
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
    print(f"{bid}\t{ext}\t{superseder}")
' 2>/dev/null > "$_tmp" || { rm -f "$_tmp"; return 0; }

    while IFS=$'\t' read -r _id _ext _superseder; do
        [ -n "$_id" ] || continue
        [ -e "${SPIRA_RUN}/gh-closed/$_id" ] && continue

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

        gh_issue_ask_unlanded "$_id" "$_ext" "${_draft:-}" || true
    done < "$_tmp"
    rm -f "$_tmp"
}

# --------------------------------------------------------------------------------------
# PR-MODE LANDING PRIMITIVES
#
# These are called by both landing.sh (for legacy paths) and landing-pass (the Rust
# binary's shell helper, pr-pass-branch.sh). Moving them here rather than keeping them
# only in landing.sh lets the helper source lib.sh and get the full implementation
# without sourcing a script that would execute on source.
# --------------------------------------------------------------------------------------

SUBMITTED="${SUBMITTED:-$SPIRA_RUN/submitted}"

submitted() {            # submitted <id> <tip> -> 0 if nothing more to do for this tip
    # rec_n IS LOAD-BEARING even though nothing here reads it. `read` puts every word past
    # the last variable into that variable, so a three-variable read of a four-field record
    # gives rec_state the value "pr 3" — and the `failed` comparison below, which decides
    # whether a failed submission is retried, would then never be true again.
    local id="$1" tip="$2" f="$SUBMITTED/$1" rec_tip rec_at rec_state rec_n
    [ -f "$f" ] || return 1
    read -r rec_tip rec_at rec_state rec_n < "$f" 2>/dev/null || return 1
    [ "$rec_tip" = "$tip" ] || return 1
    [ "$rec_state" = failed ] || return 0
    [ $(( $(date +%s) - rec_at )) -lt 3600 ]
}

submitted_rec() {        # submitted_rec <id> state|refreshes
    local f="$SUBMITTED/$1" tip at state n
    [ -f "$f" ] || return 1
    read -r tip at state n < "$f" 2>/dev/null || return 1
    case "$2" in
        state)     printf '%s' "${state:-}" ;;
        refreshes) printf '%s' "${n:-0}" ;;
    esac
}

mark_submitted() {       # mark_submitted <id> <tip> <state> [refreshes]
    mkdir -p "$SUBMITTED"
    printf '%s %s %s %s\n' "$2" "$(date +%s)" "$3" "${4:-0}" > "$SUBMITTED/$1"
}

PR_REFRESH_MAX="${SPIRA_PR_REFRESH_MAX:-5}"
PR_REFRESH_N=0

pr_state() {             # pr_state <repo> <branch> -> OPEN|MERGED|CLOSED, non-zero if unknown
    local st
    st="$( cd "$1" && ghq pr view "$2" --json state -q .state 2>/dev/null )"
    [ -n "$st" ] || return 1
    printf '%s' "$st"
}

# needs_refresh — 0 when this already-submitted branch should be rebased and force-pushed.
needs_refresh() {        # needs_refresh <repo> <name> <branch> <id> <base> <tip>
    local repo="$1" name="$2" br="$3" id="$4" base="$5" tip="$6" st n base_fq
    base_fq="$(qualify_base_ref "$base" "$repo")"
    PR_REFRESH_N=0
    git -C "$repo" merge-base --is-ancestor "$base_fq" "refs/heads/$br" 2>/dev/null && return 1
    case "$(submitted_rec "$id" state)" in
        stale) log "$id: $br is behind $base and already escalated — leaving it standing"; return 1 ;;
        done)  return 1 ;;
    esac
    st="$(pr_state "$repo" "$br")" || {
        log "$id: $br is behind $base but gh will not say whether its pull request is open — not touching it"
        return 1; }
    if [ "$st" != OPEN ]; then
        log "$id: $br is behind $base but its pull request is $st — nothing to refresh"
        mark_submitted "$id" "$tip" done
        return 1
    fi
    n="$(submitted_rec "$id" refreshes)"
    case "${n:-}" in ''|*[!0-9]*) n=0 ;; esac
    if [ "$n" -ge "$PR_REFRESH_MAX" ]; then
        spira_ask_refresh_loop "$repo" "$name" "$br" "$id" "$base_fq" "$n"
        mark_submitted "$id" "$tip" stale "$n"
        log "$id: escalated — its pull request will not merge after $n refresh(es)"
        return 1
    fi
    PR_REFRESH_N=$(( n + 1 ))
    log "$id: $br is behind $base — rebasing its pull request onto it (refresh $PR_REFRESH_N of $PR_REFRESH_MAX)"
    return 0
}

# land_pr <repo> <branch> <id> <base-ref> -> 0 if a pull request is open for this tip.
# Force-pushes with a lease because the pass rebases before pushing and the remote is
# routinely behind by a rewrite rather than a divergence.
# THE REMOTE AND THE BASE BRANCH BOTH COME OUT OF THE BASE REF, never the literals
# "origin" and "main" — two repositories here default to master.
land_pr() {
    local repo="$1" br="$2" id="$3" baseref="$4" num title remote base dup
    remote="$(ref_remote "$baseref")" || remote=origin
    base="$(ref_branch "$baseref")"
    if ! git -C "$repo" push -q --force-with-lease -u "$remote" "$br" 2>/dev/null; then
        log "$id: could not push $br to $remote"
        return 1
    fi
    num="$( cd "$repo" && ghq pr view "$br" --json number -q .number 2>/dev/null )"
    # DOES ANOTHER OPEN PR ALREADY CARRY THIS WORK? Catches the sp-pd-ci case: #114 carried
    # all nineteen commits of #113, and both sat open burning CI minutes.
    if [ -z "${num:-}" ]; then
        dup="$( cd "$repo" && ghq pr list --state open --json number,headRefName \
                  -q '.[] | "\(.number) \(.headRefName)"' 2>/dev/null \
                | while read -r n ref; do
                      [ "$ref" = "$br" ] && continue
                      if [ -z "$(git -C "$repo" log --format='%H' "$base..$br" 2>/dev/null \
                                 | while read -r c; do
                                       git -C "$repo" merge-base --is-ancestor "$c" "origin/$ref" 2>/dev/null || echo x
                                   done)" ]; then
                          echo "$n"; break
                      fi
                  done | head -1 )"
        if [ -n "${dup:-}" ]; then
            log "$id: #$dup already carries every commit on $br — not opening a second pull request"
            bdq note "$id" "Not opening a pull request: #$dup already carries every commit on $br. Continue the review there rather than splitting it across two threads." >/dev/null 2>&1
            return 1
        fi
        title="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
d = d if isinstance(d, list) else [d]
print(d[0].get("title", "") if d else "")' 2>/dev/null)"
        if ! ( cd "$repo" && ghq pr create --head "$br" --base "$base" \
                 --title "$id: ${title:-Spira}" --body-file - >/dev/null 2>&1 ) <<PRBODY
Filed by Spira for bead $id. The bead is closed in the Spira database; this
pull request is how the work lands, so it is not done until this merges.

Auto-merge is armed — a green run merges it without anyone waiting on it.
PRBODY
        then
            log "$id: gh pr create failed for $br"
            return 1
        fi
        num="$( cd "$repo" && ghq pr view "$br" --json number -q .number 2>/dev/null )"
    fi
    ( cd "$repo" && ghq pr merge --auto --squash "$br" >/dev/null 2>&1 ) \
        || log "$id: pull request ${num:-?} is open but auto-merge could not be armed"
    log "$id: pull request ${num:-?} open on $br — its CI is the gate now"
    return 0
}
