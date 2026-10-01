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
# shell's unexported variables, the way the old in-shell `( subshell )` did.
#
# THE EXPORT THIS COMMENT ONCE DESCRIBED IS RETIRED (wave 4.9, sp-k80sa): this file used to
# `export SPIRA_CZAR_LABEL SPIRA_GROOMER_LABEL SPIRA_MAECHEN_LABEL
# SPIRA_BATCH_JUDGEMENT_LABEL SPIRA_HOME_REPO` here, the same narrow re-export
# spira/bead.sh:49 and rule.sh:50 carried for the same reason — a `.fayth`'s FAYTH_LABELS
# line references these by parameter expansion (grepped: spira/chamber/*.fayth), and the
# `spira-config fayth` subprocess that sources it needs them in ITS OWN environment, not
# this shell's. `spira-config/src/chamber.rs`'s `fayth_get` now resolves all five itself —
# `fayth_label_overlay` calls `spira_config::resolve::resolve_for_process` in-process and
# sets them explicitly on the sourcing subshell's `Command` — so this shell no longer needs
# to carry them across the exec boundary at all. An unexported key here is no longer a trap:
# the resolution lives where the sourcing happens, not in whichever caller sourced lib.sh.
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
# bdq AND ITS THREE CREATE-TIME FENCES ARE NOW bead::bdq (sp-w3h16, wave 4.14,
# wave4-decomposition.md row A, safety note (c1)): one library plus a `bdq` binary every
# caller already types by that bare name. Every fence, the czar-fence dispatch, the
# SPIRA_BDJSON_FIXTURE route, the empty-SPIRA_DB refusal, the SOP_APPLIED_TRACE wrapper and
# the invalid-connection retry loop moved with it — see bead/src/bdq.rs and
# bead/src/bin/bdq.rs.
#
# EXEC-BOUNDARY TRAP: conf.sh deliberately never exports SPIRA_HOME, SPIRA_REPO,
# SPIRA_REPO_DERIVED, SPIRA_HOME_REPO or SPIRA_REPO_MAP (each a per-copy fact, not
# configuration — conf.sh's own comment on SPIRA_REPO_DERIVED says why). The repo-label fence
# reads the repo registry IN-PROCESS now (spira_config::repos::Registry, not another
# `spira-config repo ...` shell-out), so the binary needs these five in its OWN environment —
# `command bdq` only inherits what this shim threads through explicitly, the same shape
# `_spira_config_repo` above already uses. Every other value `bdq` reads (SPIRA_DB, SPIRA_BD,
# SPIRA_ASK_LABEL, SPIRA_RUN) is already on conf.sh's own export list; SPIRA_FAYTH/
# SPIRA_CZAR_CLASS/SPIRA_CZAR_TRIGGER_BEAD/SPIRA_BDJSON_FIXTURE/BD_TIMEOUT/
# SPIRA_BDQ_CONN_RETRIES/SOP_APPLIED_TRACE* are never conf.sh keys at all — set (if at all) by
# whatever already-exported environment spawned this shell — so none of those need
# re-threading here.
#
# `command bdq` (not a bare `bdq`) bypasses this very function: bash's own function lookup
# shadows a same-named command on PATH, so an unqualified `bdq "$@"` inside this function
# would recurse into itself forever.
bdq() {
    SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_REPO="${SPIRA_REPO:-}" \
    SPIRA_REPO_DERIVED="${SPIRA_REPO_DERIVED:-}" SPIRA_HOME_REPO="${SPIRA_HOME_REPO:-}" \
    SPIRA_REPO_MAP="${SPIRA_REPO_MAP:-}" \
        command bdq "$@"
}

# Each of these three is also called directly, by name, from several test suites
# (test-repo-label.sh, test-destructive-bead.sh) — not only from inside bdq() above — so each
# gets its own shim onto the binary's `__fence` subcommand rather than relying on bdq()'s own
# dispatch to reach them.
_bdq_check_repo_label() {   # _bdq_check_repo_label <create-args> -> 0 or refuse
    SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_REPO="${SPIRA_REPO:-}" \
    SPIRA_REPO_DERIVED="${SPIRA_REPO_DERIVED:-}" SPIRA_HOME_REPO="${SPIRA_HOME_REPO:-}" \
    SPIRA_REPO_MAP="${SPIRA_REPO_MAP:-}" \
        command bdq __fence repo-label "$@"
}

_bdq_check_destructive() {  # _bdq_check_destructive <create-args> -> 0 or refuse
    command bdq __fence destructive "$@"
}

_bdq_check_schema_delete() {  # _bdq_check_schema_delete <create-args> -> 0 or refuse
    command bdq __fence schema-delete "$@"
}

# `gh` gets the same treatment and for the same reason. The pull-request landing path is the
# part of this harness that reaches OUTSIDE the box, so it is the part that most needs a
# fixture — and, like bd, a stub cannot be put in front of it by prepending to PATH, because
# the export above throws that away.
# Moved with bdq into bead::bdq (sp-w3h16, wave4-decomposition.md row B): GH_TIMEOUT and
# SPIRA_GH are never conf.sh keys, so nothing needs re-exporting across the exec boundary.
ghq() { command bdq __ghq "$@"; }

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
# Moved into bead::bdq (sp-w3h16, wave4-decomposition.md row B) as a pure stdin filter;
# json_only here is a generic pipe filter (pilgrimage.sh and others pipe arbitrary output
# through it, not only bdq's own), so it keeps its own `__json_only` subcommand rather than
# being folded into bdjson's.
json_only() { command bdq __json_only; }

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

# spira_ask_machinery, spira_ask_machinery_class, spira_land_noverdict,
# spira_is_generated_file, spira_ask_rebase_loop, spira_ask_red_recurring,
# spira_ask_rebase_refused, spira_ask_budget_deferred, spira_ask_refresh_loop and
# land_escalate retired (sp-31hjr, wave 4.30, family C): ported natively into
# landing-pass/src/real.rs + ask.rs (land_escalate into sentinel/src/dispatch.rs).
# No caller remained in bash — landing-pass's own lib.sh seam was the only one, and
# it calls the Rust versions in-process now. `landing-pass noverdict ...` and
# `sentinel --land-escalate` drive the native versions standalone for the
# real-sender suites. ask_already_open stays (the GitHub-closeout family, not yet
# ported, still calls it directly).

# How many rows a `bd --json` payload carries. Never `| wc -l` and never a grep: the payload
# is one line, and a warning printed before it would be counted as a row.
# Moved into bead::bdq (sp-w3h16, wave4-decomposition.md row B).
json_count() { command bdq __json_count; }           # stdin: JSON; stdout: an integer, 0 on anything unparseable

# --------------------------------------------------------------------------------------
# Liveness. NEVER pgrep -f: the pattern is a substring of any command line that mentions
# it, including the caller's own, so a `pgrep -f 'aeon.sh builder'` inside a script named
# in that pattern reports itself alive. pgrep may nominate; /proc decides, on the actual
# argv of the recorded pid.
#
# aeon_alive/aeon_count/aeons_live_total/aeons_live_lanes are SHIMS onto `strand aeon-alive
# / aeon-count / aeons-live-total / aeons-live-lanes` (wave 4.23, sp-0ffox: lib.sh family E
# -> strand, the owning crate; collapses the bead/cockpit-collect copies of aeon_alive onto
# this same implementation). The logic — including the exclude-unit threading through
# aeon_count and the FAYTH_NAME resolution in aeons_live_lanes — lives in
# strand/src/probe.rs now; this file keeps the names so bash sourcers (fleet-status.sh,
# hold.sh) need no change.
#
# aeons_live_lanes ALONE threads SPIRA_HOME/SPIRA_FAYTHS through explicitly: conf.sh
# deliberately never exports either (a fact about this one copy of the harness, not
# configuration — see _spira_config_fayth's own comment), so a bare exec would see neither
# and silently count zero lane aeons forever, the exact shape of sp-nki5w's scar. The other
# three need only SPIRA_RUN/SPIRA_SUMMON/SPIRA_SYSTEMCTL, all already exported.
# --------------------------------------------------------------------------------------
aeon_alive() {           # aeon_alive <pidfile> -> 0 if the recorded pid is a live aeon
    strand aeon-alive "$1"
}

aeon_count() {           # aeon_count <fayth> [exclude-unit] -> live aeons of that persona
    strand aeon-count "$@"
}

aeons_live_total() {     # how many aeons exist right now, across every persona and lane
    strand aeons-live-total
}

aeons_live_lanes() {     # how many lane aeons exist right now, across all lane fayths
    SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_FAYTHS="${SPIRA_FAYTHS:-}" strand aeons-live-lanes
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
    _spira_config_fayth names
}

spira_fayths() {         # the personas this harness runs, space separated, IN PRIORITY ORDER
    _spira_config_fayth roster
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
    _spira_config_fayth task
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
    _spira_config_fayth lane
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
    _spira_config_fayth model "$1" "${2:-}"
}

fayth_get() {            # fayth_get <fayth> <VAR> [default] -> one field of a fayth
    _spira_config_fayth get "$1" "$2" "${3:-}"
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
# READY_ARGS, ready_raw_args, ready_count STAY bash (wave 4.25, sp-obhv6): none of the
# three is named in this bead's scope, each still has live bash callers outside family F
# (drain.sh and aeon/src/seam.rs's own bash snippet read `${READY_ARGS[@]}` directly;
# fleet-status.sh calls ready_count; detect_unclaimable_ready — family T, a later bead —
# calls ready_raw_args), and routing them through a `spira-claim` subprocess at lib.sh
# SOURCE TIME was tried and reverted: it corrupted aeon's own seam snapshot read (every
# `. lib.sh` the aeon crate's seam performs now pays this at sourcing, not only a lazy
# call), turning test-aeon-elastic-concurrency.sh red. `READY_ARGS` as ONE CONST is
# satisfied on the Rust side alone — `spira_claim::READY_ARGS_BASE`, which cockpit-collect
# now links in-process instead of keeping its own copy (`probes/queue.rs`). `fayth_ready`/
# `fayth_exclude`/`bulk_ready_by_fayth`'s OWN Rust ports (below) build their own ready
# query independently, in spira-claim/src/ready.rs — a second, Rust-only copy of this
# predicate's SHAPE, not a bash caller asking two different functions the same question.
ready_raw_args() {
    local args=(ready --limit 0 --exclude-type epic,event -u)
    [[ -n "${SPIRA_NO_LOOP_LABEL:-}" ]] && args+=(--exclude-label "$SPIRA_NO_LOOP_LABEL")
    printf '%s\n' "${args[@]}"
}
READY_ARGS=(ready --limit 0 --exclude-type epic,event -u)
[[ -n "${SPIRA_SCOPE_LABEL:-}" ]] && READY_ARGS+=(--label "$SPIRA_SCOPE_LABEL")
[[ -n "${SPIRA_NO_LOOP_LABEL:-}" ]] && READY_ARGS+=(--exclude-label "$SPIRA_NO_LOOP_LABEL")

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

# `_spira_claim`: the exec-boundary shim for the rest of family F (epic_parent_lookup
# through bulk_ready_by_fayth, plus ready_shared_exclude) — same pattern
# `_spira_config_fayth`/`_spira_config_repo` use. `SPIRA_QUEUE_WAIT_LABEL`/
# `SPIRA_OPEN_CHILDREN_LABEL` are threaded explicitly because conf.sh never exports them
# (the exec-boundary trap); `SPIRA_NO_LOOP_LABEL` is exported but threaded anyway for
# defence in depth. `SPIRA_CLAIM_RETRIES`/`SPIRA_CLAIM_RETRY_DELAY_S`/`SPIRA_SCOPE_LABEL`/
# `SPIRA_SUBMITTED_LABEL` need no entry here: the first two are resolved by spira-claim
# itself from spira.toml, and the last two ARE exported.
_spira_claim() {
    SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_FAYTHS="${SPIRA_FAYTHS:-}" \
    SPIRA_NO_LOOP_LABEL="${SPIRA_NO_LOOP_LABEL:-}" SPIRA_QUEUE_WAIT_LABEL="${SPIRA_QUEUE_WAIT_LABEL:-}" \
    SPIRA_OPEN_CHILDREN_LABEL="${SPIRA_OPEN_CHILDREN_LABEL:-}" \
        spira-claim "$@"
}

# epic_parent_lookup, epic_rank_rows -> moved to spira-claim's `epics`/`select` verbs
# (sp-f0qhr's DESIGN.md §5 items 8-9, applied here at wave 4.25, sp-obhv6: the design
# predates this bead, "not performed" under the operator's "leave lib.sh alone" directive
# during the earlier cutover; the world is stopped now, so it is).
epic_parent_lookup() {
    printf '%s' "$1" | _spira_claim epics
}

epic_rank_rows() {
    local _lkf _rsf _rc
    _lkf="$(mktemp)" || return 1
    _rsf="$(mktemp)" || { rm -f "$_lkf"; return 1; }
    printf '%s' "$2" > "$_lkf"
    printf '%s' "${3:-}" > "$_rsf"
    printf '%s' "$1" | _spira_claim select --fayth "${FAYTH:-any}" --epics "$_lkf" --resumable "$_rsf"
    _rc=$?
    rm -f "$_lkf" "$_rsf"
    return $_rc
}

# claim_retry, fayth_exclude, fayth_ready, bulk_ready_by_fayth, ready_shared_exclude ->
# moved to spira-claim (wave 4.25, sp-obhv6). `ready_shared_exclude`'s only caller besides
# `fayth_exclude` is test-dispatch-open-children.sh, which calls it directly — kept as its
# own shim (onto `shared-exclude`, spira-claim/src/ready.rs `shared_exclude3`) rather than
# deleted. `express_ready_in_task_pool` has had no live caller since sentinel.sh (the only
# thing that ever called it) was retired for the Rust sentinel crate — deleted outright
# rather than ported (see `test-express-lane.sh`, trimmed to match).
claim_retry() {
    _spira_claim claim-retry "$@"
}

fayth_exclude() {        # fayth_exclude <fayth> -> comma-separated exclusions
    _spira_claim fayth-exclude "$1" "${2:-}"
}

fayth_ready() {
    _spira_claim fayth-ready "$1"
}

bulk_ready_by_fayth() {
    _spira_claim bulk-ready-by-fayth
}

ready_shared_exclude() {
    _spira_claim shared-exclude
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
#
# DELIBERATELY NOT SHIMMED (wave 4.23, sp-0ffox), unlike aeon_count/aeons_live_total above.
# strand/src/probe.rs carries its own Rust copy of this same arithmetic for any FUTURE
# in-process Rust caller, but THIS body stays bash: it calls fayth_get/aeon_count BY NAME,
# and six suites (test-summon-fayth.sh, test-aeon-elastic-concurrency.sh, test-fayth.sh,
# test-builder-qa-proposed.sh, test-czar-partition.sh, test-dependents.sh) redefine those
# bash functions after sourcing this file, to control fayth_free's inputs without a real
# process table. Reducing fayth_free itself to a one-line exec shim would move that
# arithmetic into a separate process where a bash-level override of aeon_count can never
# reach it again (the EXEC-BOUNDARY TRAP) — breaking every one of those suites for no
# behavior change in production (aeon_count below is itself already a shim, so fayth_free
# calling it by name already reaches strand exactly as a one-line shim would).
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
    _spira_config_fayth partitions
    return 0
}

fayths_for_labels() {    # fayths_for_labels <labels> -> personas whose partition IS <labels>
    _spira_config_fayth for-labels "$1"
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

# --------------------------------------------------------------------------------------
# WAVE 4.26 ("capacity pause (K) -> aeon"): every function below is now a one-line shim
# onto `aeon capacity <verb>` (aeon/src/capacity.rs). aeon is the probe's one owner —
# `capacity_probe_maybe`/`capacity_probe` are retired outright, not shimmed: nothing but
# `capacity_paused` ever called them, and that logic now lives entirely in the binary
# (wave4-decomposition.md (c)3: "keep exactly one owner for the probe"). The pause file
# stays the contract every language reads: `$SPIRA_CAPACITY_PAUSE`, `<epoch> <iso> <why>`,
# unchanged. Streams: the verb's plain answer is on stdout, any `log`-style chatter is on
# stderr — `capacity_paused`'s own `SPIRA_CAPACITY_LEFT="$(aeon capacity paused)"` below
# captures only the number, never the chatter.
# --------------------------------------------------------------------------------------

# capacity_reset_at <session-log> -> prints the epoch the window reopens; rc 0 if the
# session was ended by the account running out of capacity, rc 1 for anything else (the
# log does not exist, is unparseable, or the session failed for its own reasons — reading
# a genuine failure as an outage would stop a bead ever being poisoned).
capacity_reset_at() {
    aeon capacity reset-at "${1:-}"
}

# capacity_pause_set <epoch> <reason> — record that the account is out until <epoch>. An
# existing pause is only ever EXTENDED, never shortened.
capacity_pause_set() {
    aeon capacity pause-set "${1:-0}" "${2:-unknown}"
}

capacity_pause_until() {  # -> the epoch a pause runs to, or 0 if none is recorded
    aeon capacity pause-until
}

# capacity_paused -> rc 0 while the window is still shut, and sets $SPIRA_CAPACITY_LEFT to
# the seconds remaining ("?" when the pause file exists but could not be read or parsed —
# fail closed, never treated as open). Probes when the horizon is far out
# (SPIRA_CAPACITY_PROBE_WINDOW) and lifts the pause early on a served probe, same as
# before — that whole decision is `aeon::capacity::paused` now.
SPIRA_CAPACITY_LEFT=0
capacity_paused() {
    SPIRA_CAPACITY_LEFT="$(aeon capacity paused)"
}

# Path for the probe-last timestamp. Lives beside the other capacity state files.
SPIRA_CAPACITY_PROBE_LAST="${SPIRA_CAPACITY_PROBE_LAST:-$SPIRA_RUN/capacity-probe-last}"

capacity_pause_why() {   # -> what was being worked when the account ran out
    aeon capacity pause-why
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
# if there is no readable content to fingerprint. sha2 in-process now (aeon::capacity),
# replacing the sha256sum/cksum fallback — both the write and the read are this one
# binary's now, so only internal consistency matters, never the algorithm's name.
capacity_log_fingerprint() {
    aeon capacity log-fingerprint "${1:-}"
}

capacity_withdrawn_fp() {   # capacity_withdrawn_fp <id> -> the fingerprint already paid back, or nothing
    aeon capacity withdrawn-fp "${1:-}"
}

# capacity_withdrawn_mark <id> <fingerprint> <attempt> — record that this exact log has been
# paid back. Written whole rather than appended: one line per bead is the entire question,
# and a file that only ever grows is one more thing to prune.
capacity_withdrawn_mark() {
    aeon capacity withdrawn-mark "${1:-}" "${2:-}" "${3:-0}"
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
# PORTED TO spira-claim (wave 4.18, sp-sn1re): the SQL that used to live in
# _attempts_sql_query (the thrash/unjudged exemption, the created_at poison.cleared floor,
# the three sp-lzt/sp-rp4g4/sp-418h5 constraints its own comment used to carry) is now
# `events::fold` (spira-claim/src/events.rs, DESIGN.md §3), unit-tested by
# `cargo test -p spira-claim events::tests` and proven against the identical b1..b8 fixture
# this file's own test-attempts-sql.sh still seeds. attempts_of is a one-line shim so every
# existing caller (capacity.sh, the Rust seams in aeon/landing-pass/rebase-stale/etc. that
# still call it by name) keeps working unchanged.
#
# FAIL CLOSED, NOT OPEN. A query failure used to fall through to `printf '0'` with a 0 exit —
# indistinguishable from a bead that genuinely never failed, so a poisoned bead's per-bead
# re-check (sentinel.sh's stale-poison-clear scan) read a false zero as "below threshold" and
# cleared it, only for the next pass's bulk query to see the true count and poison it right
# back (law-a-control-that-cannot-check-must-refuse). Prints nothing and returns 1 on error;
# callers must treat that as "cannot tell", never default it to 0.
attempts_of() {
    local id="${1:-}" out
    [ -n "$id" ] || return 1
    out="$(command spira-claim attempts "$id" --db "$SPIRA_DB" 2>/dev/null)" || return 1
    [ -n "$out" ] || return 1
    printf '%d' "$out"
}

# BUMP FUNCTIONS. bump_requeue writes a typed event row so that census can aggregate
# failure classes across the whole store (sp-2lk); bump_lapsed and bump_poison_cleared
# (below) write through the same helper for their own event types.
#
# Failure is silent — a missed counter is acceptable; a crash in a caller is not.
#
# _bump_write_event_try — the real write, reporting whether it was accepted. PORTED TO
# spira-claim (wave 4.18, sp-sn1re): `spira-claim write-event` is the same INSERT (now
# properly quoted through store::sql_quote rather than interpolated raw), reached by name
# so doctor's events-substrate probe and every test fixture that calls this directly with
# an arbitrary event_type (census's recurred/reclaimed local wrappers) keep working.
# _bump_write_event must keep returning 0: aeon.sh runs under `set -e` and calls
# bump_requeue bare, so a failing return would kill a live aeon mid-requeue.
_bump_write_event_try() {
    local id="${1:-}" etype="${2:-}" cause="${3:-unrecorded}"
    [ -n "$id" ] && [ -n "$etype" ] || return 0
    command spira-claim write-event "$id" "$etype" "$cause" --db "$SPIRA_DB"
}

_bump_write_event() { _bump_write_event_try "$@" >/dev/null 2>&1; return 0; }
bump_requeue() { _bump_write_event "${1:-}" requeued  "${2:-unrecorded}"; }
bump_lapsed()  { _bump_write_event "${1:-}" lapsed    "${2:-unrecorded}"; }

# write_lapse_record <bead> <quiet_s> <last_action> <tip> -> $SPIRA_RUN/lapsed/<bead>-<ts>
# PORTED TO spira-claim (wave 4.18, sp-sn1re): same record format (test-watchtower.sh's
# gap G8 lifts this function out of lib.sh and runs it standalone to prove the real
# writer's output still matches the hand-written fixtures the rest of that suite plants).
write_lapse_record() {
    command spira-claim lapse-record "${1:-}" "${2:-}" "${3:-}" "${4:-}"
}

# bump_poison_cleared <id> <cause> — the event _attempts_sql_query/_check4_bulk_sql floor on
# (sp-qd2ul). Written by `spira-claim unpoison` (or `groomer.sh deadlocked`'s call into
# `spira-claim deadlocked`), never by a bare label removal: a clear that leaves no trace here
# is indistinguishable from one that was never judged, and the next CHECK 4 pass reads the
# unchanged count against a bare label and re-poisons within minutes.
bump_poison_cleared() { _bump_write_event "${1:-}" 'poison.cleared' "${2:-unrecorded}"; }

# bead_metadata <id> <key> -> the value bd update --set-metadata wrote, or empty when unset.
# PORTED TO spira-claim (wave 4.18, sp-sn1re): --long is still required there too (bd show
# --json omits metadata by default, sp-4rzlw) — see counters::bead_metadata.
bead_metadata() {
    command spira-claim bead-metadata "${1:-}" "${2:-}" --db "$SPIRA_DB" 2>/dev/null
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
# PORTED TO spira-claim (wave 4.18, sp-sn1re): same metadata keys, same note truncation
# (tr '\n\r' '  ' | cut -c1-300) — see counters::{next_streak,truncate_note,thrash_streak_bump}.
thrash_streak_bump() {
    local id="${1:-}"
    [ -n "$id" ] || { printf '0'; return 0; }
    command spira-claim thrash-streak-bump "$id" "${2:-?}" "${3:-}" --db "$SPIRA_DB" 2>/dev/null
}

# DIAGNOSTIC ACCESSOR — the requeue counter is read from the events table via bd sql
# (sp-2lk). PORTED TO spira-claim (wave 4.18, sp-sn1re): `count-events` is the same raw
# COUNT(*), still generic over event_type — doctor's events-substrate probe and the
# reclaims_of() test fixtures (test-attempts.sh, test-unpoison.sh) call this directly with
# event types other than 'requeued'. _counter_events_sql had no caller left once this
# shimmed directly and was retired outright.
_counter_events_query() {   # _counter_events_query <id> <event_type> -> count, or '?' if bd sql fails
    # '?' ON FAILURE, NOT '0'. A query that cannot reach the events table and one that
    # reached it and found nothing print the same digit if both return '0' — the reader
    # cannot tell "no activity" from "the driver is down" (law-absence-needs-a-positive-
    # control). recurs_of's caller (incident.sh's Sin decision, before recurs_of moved to
    # the incident crate) once folded that silence into zero recurrences and stayed
    # silent through an outage forever (sp-39yd3).
    local id="${1:-}" etype="${2:-}" out
    if out="$(command spira-claim count-events "$id" "$etype" --db "$SPIRA_DB" 2>/dev/null)" && [ -n "$out" ]; then
        printf '%d' "$out"
        return 0
    fi
    printf '?'
    return 1
}
# requeues_of <id> -> raw COUNT(*) of 'requeued' events, or '?' if it cannot be told.
# NOT the judged, exemption-aware count `spira-claim requeues` answers for CHECK 4's own
# accounting (DESIGN.md §6) — this stays the bare _counter_events_query census.sh,
# landing-pass and aeon/teardown.rs already depend on counting EVERY requeue, including
# a rebase-conflict one that attempts_of exempts (test-census-events.sh sp-9edq8,
# test-landing-rebase.sh, test-aeon-teardown-e2e.sh all assert exactly that). Tried
# shimming this onto `spira-claim requeues` first; all three suites went red on that
# exact distinction, which is the regression this comment now guards against.
requeues_of() { _counter_events_query "${1:-}" requeued; }

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
# PORTED TO spira-claim (wave 4.18, sp-sn1re): same exact-or-dash-suffixed match, now over
# `bd show --json`'s labels array instead of `bdq label list`'s text — see
# counters::counter_label.
counter_label() {
    local out
    out="$(command spira-claim counter-label "$1" "$2" "$3" --db "$SPIRA_DB" 2>/dev/null)" || return 1
    [ -n "$out" ] || return 1
    printf '%s' "$out"
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

# poison_asked_clear <id> — drop this bead's ask history. A poison.cleared event floors
# attempts_of back to zero (sp-qd2ul), so a genuinely new run of failures can reach the same
# raw count (e.g. 3) the pre-clear history already has a "3" entry for, and poison_asked would
# read that stale entry as "already asked" and suppress the new ask. The count it dedups on
# was just reset; its history must reset with it.
#
# PORTED TO spira-claim (wave 4.18, sp-sn1re): `spira-claim ask-clear` IS this call —
# `spira-claim unpoison`'s own write already performs the identical clear_ask_history step
# as step 2 of its sequence (unpoison.rs); this is that same step exposed standalone,
# because the one real caller left (groomer's work-fault triage, groomer/src/cmds.rs's
# triage_poison, reached through this same lib.sh seam the way bump_poison_cleared is)
# wants exactly this write and none of unpoison's threshold/label/lifecycle/note machinery
# around it. SPIRA_POISON_ASKED, read by the new verb from the environment (DESIGN.md
# §8.3), no longer needs a lib.sh-side default — conf.sh exports SPIRA_RUN, which
# `spira-claim`'s own resolution falls back to exactly as `unpoison` already does.
poison_asked_clear() {
    command spira-claim ask-clear "${1:-}" --db "$SPIRA_DB" >/dev/null 2>&1 || true
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
#
# aeon_name_take is RETIRED (wave 4.23, sp-0ffox), not shimmed: its only caller, besides
# its own definition, was this crate's own seam call from run.rs/sweep.rs — a whole-tree
# grep found no bash caller and no other Rust seam reaching it — so `aeon` now computes it
# in-process (aeon/src/naming.rs) with no lib.sh round trip left to shim. SPIRA_AEON_NAMES
# moves there too (the `NAMES` constant); nothing else read it.
#
# aeon_named keeps a one-line shim: cockpit-collect still calls it by name, across the
# crate boundary.
# --------------------------------------------------------------------------------------
aeon_named() {           # aeon_named <pidfile> -> the name held by that aeon, if any
    aeon aeon-named "$1"
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
# live one's. `attempt_trace` was that boundary; it is `aeon::ledger::attempt_trace` now
# (Rust, wave 4.26 — its last bash caller, `capacity_reset_at`, is a shim onto the binary),
# and it is the only place the mark is parsed.
#
# The mark is a constant rather than a configuration key because it is a FORMAT, not a path:
# an operator who changed it would make every log already on disk unreadable by the code that
# writes the next line of it. It is defined once here (still a plain shell variable — the
# bash seam reads it off the sourced lib.sh for every Rust caller that has not yet moved to
# `spira-config`) and written by aeon.sh through spira_trace_mark, so the writer and the
# readers cannot drift.
#
# It is not JSON and does not start with `{`, which is what makes it inert: every consumer of
# this trace already skips any line that is not a JSON object, so the mark passes through
# `aeon::capacity::reset_at` without special handling here, and through aeon::trace
# (trace_last, trace_stats, trace_tail, wiki_write_paths — wave 4.34) the same way.
# --------------------------------------------------------------------------------------
SPIRA_TRACE_MARK='=== spira attempt'


# bead_context retired (sp-31hjr, family C): ported natively into landing-pass/src/ask.rs
# (bead_context) + real.rs (RealBeads::context). No caller remained — spira_ask_refresh_loop
# was the only one, and it's native now too. sentinel's and strand's own `bead_context`
# (render.rs, check.rs) are separate, pre-existing Rust copies, untouched by this bead.

# land_subject <id> -> "spira: land <id>", or "spira: land <id> — <title>" when the bead
# has a title. Every writer of a landing merge (verdict.sh, landing.sh, queue.sh,
# batcher-cut's Rust seam) calls this, so every reader that widens its own match to a
# trailing title (landed()/landed_sha() below, CHECK5 in sentinel.sh, cockpit.sh,
# overrides.sh) stays in sync with what is actually written. Ported to Rust (sp-81t4d,
# "wave 4.17" — family R, landed verification); see `land_verify::land_subject`.
land_subject() {
    landing-pass land-subject "$1"
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
# Ported to Rust (sp-81t4d, "wave 4.17" — family R, landed verification); see
# `land_verify::landed`. Both shims resolve the same optional `<repo>` default (`repo_root`)
# bash always did and hand the binary an explicit path — the one piece of its own work this
# shim still does, since `repo_root` lives in bash (family U) either way.
landed() {
    local id="$1" repo="${2:-$(repo_root)}"
    landing-pass landed "$id" "$repo" >/dev/null
}

# landed_sha <id> <repo> -> the sha of the commit landed() would say yes about, so a
# caller that needs to CITE the landing (a GitHub comment) gets the same commit the
# ancestry check trusted, never a second guess at which one that was. Ported to Rust
# (sp-81t4d, "wave 4.17"); same binary as landed(), stdout kept this time.
landed_sha() {
    local id="$1" repo="${2:-$(repo_root)}"
    landing-pass landed "$id" "$repo"
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
# sufficient (law-closed-is-not-landed). Ported to Rust (sp-81t4d, "wave 4.17" — family R);
# see `land_verify::bead_cited_commit_on_base`.
bead_cited_commit_on_base() {
    landing-pass cited-commit "$1" "$2" "$3"
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
# it belongs behind a cheap check that has already failed — never on the common path. Ported
# to Rust (sp-81t4d, "wave 4.17" — family R); see `land_verify::pr_merged`.
pr_merged() {
    landing-pass pr-merged "$1" "$2"
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
# not decide; it names the evidence so the next aeon can judge. Ported to Rust (sp-81t4d,
# "wave 4.17" — family R); see `land_verify::other_beads_on_conflicts`.
other_beads_on_conflicts() {
    landing-pass other-beads "$1" "$2" "$3" "$4"
}

# Ported to Rust (sp-81t4d, "wave 4.17" — family R); see `land_verify::conflict_reopen_note`.
conflict_reopen_note() {
    landing-pass conflict-note "$1" "$2" "$3" "$4" "$5" "$6" "${7:-1}"
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
# THE REPO REGISTRY'S CLI DOOR (spira_config::repos, sp-37rmg, "wave 4.11"). SPIRA_HOME,
# SPIRA_REPO, SPIRA_REPO_DERIVED, SPIRA_HOME_REPO and SPIRA_REPO_MAP are deliberately NOT
# exported by conf.sh (each is a fact about this one copy of the harness; conf.sh's own
# comment on SPIRA_REPO_DERIVED says why) — a bare `spira-config repo ...` run from a
# function below would see none of them, so every shim threads them through explicitly
# instead of trusting export.
# _spira_config_fayth <verb> ... -> `spira-config fayth <verb> ...` with the two values it reads
# passed per call. conf.sh deliberately never exports SPIRA_HOME or SPIRA_FAYTHS, and a bare
# exec saw neither: in production the roster came back EMPTY (sp-nki5w), which on world start
# summons no one. Same shape as _spira_config_repo below.
_spira_config_fayth() {
    SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_FAYTHS="${SPIRA_FAYTHS:-}" spira-config fayth "$@"
}

_spira_config_repo() {
    SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_REPO="${SPIRA_REPO:-}" \
    SPIRA_REPO_DERIVED="${SPIRA_REPO_DERIVED:-}" SPIRA_HOME_REPO="${SPIRA_HOME_REPO:-}" \
    SPIRA_REPO_MAP="${SPIRA_REPO_MAP:-}" \
        spira-config repo "$@"
}

# Ported to spira_config::containment (sp-eekjm) and wired into spira-config's own
# `resolve()` there; this is now the one-line shim onto the CLI door for every bash caller
# (wave4-decomposition.md row U, sp-37rmg, "wave 4.11"). `_spira_remote_is_real` had no
# caller outside this function and is retired rather than ported. SPIRA_INSTANCE/
# SPIRA_WORKSPACES ARE already exported by conf.sh, but SPIRA_REPO_MAP is not, so this still
# goes through the same threading helper as every other repo-registry shim.
spira_containment_check() {
    # prod (or unset) is always allowed; the map is unconstrained — kept as a bash-only
    # fast path (never shells to spira-config) so every lib.sh source, in production and in
    # every test fixture that never sets SPIRA_INSTANCE, costs exactly what it always did:
    # nothing. Only a genuinely confined instance pays for the real check.
    case "${SPIRA_INSTANCE:-prod}" in prod) return 0 ;; esac
    _spira_config_repo containment-check && return 0
    exit 1     # the bash original halted the whole sourcing process on a violation, not just this call
}

# Run at source time. The cost is one read of the map file and, for non-prod instances,
# one `git remote -v` per registered checkout — a fraction of a second on summon.
spira_containment_check

# The name is DERIVED from the checkout the harness is installed in (conf.sh: basename of
# SPIRA_REPO) and overridable in spira.conf. It used to be the literal `brain`, which is one
# operator's repository written into the mechanism.
# Ported to spira_config::repos::home_repo (sp-37rmg, "wave 4.11") — a one-line shim.
spira_home_repo() {      # the repo name a bead means when it names none
    _spira_config_repo home-repo
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
# Ported to spira_config::repos (sp-37rmg, "wave 4.11") — see that module's own doc for the
# row-shape logic this used to spell out in awk; `repo_field`/`repo_root`/`repo_names` below
# are now one-line shims onto it.
repo_field() {           # repo_field <name> <path|land|base|format|gate|lanes> -> the field
    _spira_config_repo field "$1" "$2"
}

repo_names() {           # every repo name in the map, one per line
    _spira_config_repo names
}

# repo_root <name> -> the checkout, or non-zero if the map does not carry that name.
#
# SPIRA_REPO still names the HOME repository, because that is the seam every existing suite
# drives a fixture through: a test sets SPIRA_REPO and its beads carry no `repo:` label at
# all. Widening it into a map rather than replacing it is what keeps those suites honest.
repo_root() {
    _spira_config_repo root "${1:-}"
}

# spira_same_repo <a> <b> -> 0 if those two paths are the same repository.
#
# BY OBJECT STORE, NEVER BY PATH STRING. A worktree and the checkout it was cut from are one
# repository under two paths, and this harness runs from both — every aeon works in a
# worktree and the landing gate extracts one. A string comparison therefore calls the copy in
# force "some other repository", so a fence keyed on it fires on every branch, and a check
# keyed on it reports a second copy that does not exist. Ported (sp-37rmg); `_spira_gitstore`
# below stays bash — it has a caller outside this family (duc's own worktree-dir lookup).
spira_same_repo() {      # spira_same_repo <path-a> <path-b>
    spira-config repo same "${1:-}" "${2:-}"
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
    _spira_config_repo land "${1:-}"
}

# repo_land_queued <name> -> 0 when the repo is EITHER merge-queue mode (queue or
# queue.local), 1 otherwise. Certification (queue.sh submit, aeon.sh's own self-cert), the
# landing pass's "no local rebase" arm, and the periodic step/verdict dispatch all judge a
# SUBMITTED branch the same way regardless of how its round eventually lands — only the
# round's own terminal step (a batch PR vs. land-local) differs between the two.
repo_land_queued() {
    _spira_config_repo land-queued "${1:-}"
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
    _spira_config_repo gate "${1:-}"
}

# repo_format <name> -> the repo's own formatter, or nothing. ABSENCE MEANS DO NOTHING, and
# that is a decision rather than a gap: running a formatter a repository has not asked for
# turns one bead's rebase into a thousand-line diff nobody requested, and on a repository
# whose base is already unformatted — another, measured once — it rewrites the whole
# tree out from under the work.
repo_format() {
    _spira_config_repo format "${1:-}"
}

# repo_base <name> -> the repo's declared base ref, or nothing if the row leaves it to
# spira_landref to resolve. Callers want spira_landref, not this: it is the raw column.
repo_base() {
    _spira_config_repo base "${1:-}"
}

# repo_name_at <path> -> the map name for a checkout path, or non-zero.
#
# The reverse of repo_root, and it exists because rebase_branch is addressed by PATH — that
# is the seam test-rebase.sh drives — while the formatter is declared
# per NAME. Paths are unique by construction: two repositories cannot share a directory,
# which is the same property the per-repository scratch worktree is named for. A caller
# that already holds the name should pass it rather than make this guess.
repo_name_at() {
    _spira_config_repo name-at "${1:-}"
}

# spira_repos -> every repository this harness manages, one name per line.
#
# The home repo is ALWAYS first and always present, map or no map. A fixture that copies
# lib.sh next to nothing else has no repo-map, and a sentinel that then iterated zero
# repositories would land nothing while reporting a clean pass — the exact false-clean this
# whole file is written against.
spira_repos() {
    _spira_config_repo all
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
#
# Ported to spira_config::repos (sp-o88bx, "wave 4.12", wave4-decomposition.md row W) — the
# four rungs above, `ref_remote`/`ref_branch`'s split and `spira_landrefs`/
# `spira_publish_forge`'s own callers all now live there, with this essay reproduced as that
# module's own doc comment. Every function below is a one-line shim. `ref_branch` and
# `qualify_base_ref` never consult the repo-map at all (they only ever shell to git on the
# repo they are given), so unlike `spira_landref`/`spira_landrefs`/`spira_publish_forge` they
# skip `_spira_config_repo`'s env threading and call `spira-config` directly.
# --------------------------------------------------------------------------------------
spira_landref() {        # spira_landref [repo-path-or-name] -> the base ref, or non-zero
    _spira_config_repo landref "${1:-}"
}
ref_remote() {           # ref_remote <ref> [repo] -> its remote, or non-zero if the ref is local
    spira-config repo ref-remote "${1:-}" "${2:-}"
}
ref_branch() {           # ref_branch <ref> -> the branch name, without any remote
    spira-config repo ref-branch "${1:-}"
}
qualify_base_ref() {     # qualify_base_ref <ref> <repo> -> refs/remotes/... or original
    spira-config repo qualify-base-ref "$1" "$2"
}
spira_landrefs() {       # spira_landrefs <repo> -> the land ref, plus its local counterpart
    _spira_config_repo landrefs "$1"
}
spira_publish_forge() {  # spira_publish_forge <name> -> "<remote> <branch>"
    _spira_config_repo publish-forge "$1"
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

# queue_notify_concierge and queue_local_check_divergence are RETIRED (sp-hwjsq,
# "wave 4.32" — queue decomposition row AA): ported in process into the queue crate
# (queue/src/ops/helpers.rs `notify`/`check_divergence`), called only from queue's own
# in-process Lib::notify/Lib::divergence. Neither had any caller left outside that seam —
# no bash script called them by name — so there is no shim here to keep working.

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
    # Ported to `bead event` (sp-ogu8x, wave 4.24, family Z — events are the audit trail
    # census/tsd/cockpit read; see bead/DESIGN.md "event", including why strand's own
    # independent copy is not also collapsed onto this one yet). SPIRA_RUN/
    # SPIRA_EVENT_COOLDOWN/SPIRA_NOW are already in this process's environment (none of the
    # three is an unexported conf.sh derivation), so nothing needs re-threading across this
    # exec, unlike `bdq`'s shim.
    command bead event "$@"
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
# rebase_branch <branch> <onto> [repo] [repo-name] -> 0 if <branch> now contains <onto>, 1
# if it does not. On failure the branch ref is left EXACTLY as it was and $REBASE_CONFLICTS
# names the paths that collided ($REBASE_FAILURE says which of conflict | rebase-refused |
# no-base | no-branch | no-worktree; $REBASE_REFUSED_REASON is set only for rebase-refused).
# On success, and only when commits were actually replayed, the repository's own formatter
# runs on the result and is committed as part of the rebase. The branch tip therefore MOVES
# on success, and a caller holding a tip from before the call is holding a stale one.
#
# THE CALLER MUST HAVE ESTABLISHED THAT NO LIVE AEON HOLDS THE BRANCH. This rewrites
# commits beneath a working tree; doing that under a running aeon destroys work in flight.
# `holder_alive` is the precondition — unchanged; this shim adds no liveness check of its
# own, exactly as the function it replaces did not.
#
# Ported to Rust (wave 4.21, sp-07jcz): see `rebase-stale/src/branch.rs` for the full
# rationale this header used to carry (why the branch's own worktree, why failures are
# named, why a formatter failure changes nothing, why only the branch's own paths are
# committed). `format_rebased` folded into the port; nothing else called it. Any worktree
# pruning or pre-reset salvage in the port goes through `sending`'s destruction chokepoint
# in-process, never a raw git removal.
# --------------------------------------------------------------------------------------
REBASE_CONFLICTS=""
REBASE_FAILURE=""
REBASE_REFUSED_REASON=""
rebase_branch() {
    local br="$1" onto="$2" repo="${3:-$(repo_root)}" name="${4:-}" fmtcmd out rc
    REBASE_CONFLICTS=""; REBASE_FAILURE=""; REBASE_REFUSED_REASON=""
    [ -n "$name" ] || name="$(repo_name_at "$repo" 2>/dev/null)" || name=""
    fmtcmd="$(repo_format "$name" 2>/dev/null)"
    out="$(rebase-stale rebase-branch "$br" "$onto" "$repo" "$name" "$fmtcmd")"; rc=$?
    if [ "$rc" != 0 ]; then
        REBASE_FAILURE="$(sed -n 1p <<<"$out")"
        REBASE_CONFLICTS="$(sed -n 2p <<<"$out")"
        REBASE_REFUSED_REASON="$(sed -n 3p <<<"$out")"
    fi
    return "$rc"
}

# recut_onto — move a branch onto a new base by cherry-picking commits one by one.
#
# Unlike rebase (which stops entirely on the first conflict), this advances the branch
# as far as the commits allow: clean commits are applied, and the first conflicting one
# is noted in RECUT_CONFLICTS. When at least one commit lands, the branch ref is
# force-updated to that commit so the merge-base moves forward. When zero commits land
# the branch ref is left unchanged — moving it to the new base would strip all work and
# leave a trivially-clean branch that the next pass certifies without any content.
# $RECUT_CONFLICTS is a real conflict's path list, a git error line, or one of the sentinel
# words no-base | no-branch | no-worktree | no-merge-base | no-checkout on an early refusal.
# Returns 0 if all commits applied cleanly, 1 if any conflict remains.
#
# Ported to Rust (wave 4.21, sp-07jcz): see `rebase-stale/src/branch.rs`.
RECUT_CONFLICTS=""
RECUT_APPLIED_COUNT=0
recut_onto() {
    local br="$1" onto="$2" repo="${3:-$(repo_root)}" name="${4:-}" out rc
    RECUT_CONFLICTS=""; RECUT_APPLIED_COUNT=0
    [ -n "$name" ] || name="$(repo_name_at "$repo" 2>/dev/null)" || name=""
    out="$(rebase-stale recut-onto "$br" "$onto" "$repo" "$name")"; rc=$?
    RECUT_APPLIED_COUNT="$(sed -n 1p <<<"$out")"
    RECUT_CONFLICTS="$(sed -n 2p <<<"$out")"
    return "$rc"
}


# spira_live_aeons RETIRED (wave 4.23, sp-0ffox): its callers, promote.sh and
# systemd/install.sh, are both gone — the install crate's own `checks::live_aeons`
# (install/src/checks.rs, reimplemented directly against its own Systemctl port) is the
# live-aeon guard now, independent of lib.sh. A whole-tree grep found no remaining caller.

# _prune_candidates retired with activate.sh (sp-jsnbm): release install-tarball's own
# prune (release/src/install.rs) replaces it; nothing else called this function.

LANDSTATE="${SPIRA_RUN}/landstate"
# Reasons written by the batch/queue eviction machinery. Only these warrant the eviction-race
# reopen in aeon.sh; no-rebase@*, gate and confine are landing.sh REDs with their own paths.
LAND_EVICTION_REASONS="ejected conflicts-with-base rebase-suite-red"

# _tsd_landing_event is RETIRED (sp-cnnt6, "wave 4.16"): its one caller was land_mark, now a
# shim onto `landing-pass mark`, which does the landing-event dual-write itself, in-process
# (landing-pass/src/landstate.rs), rather than shelling to tsd-write.

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

# land_mark/land_state are PORTED (sp-cnnt6, "wave 4.16" — family S, the landstate ledger):
# landing-pass/src/landstate.rs is the one writer now; these are one-line shims so every
# bash sourcer here keeps working unchanged. Both read only $SPIRA_RUN — no lib.sh seam, no
# containment check — so a caller that already has it exported pays one process, not a
# bash-plus-source.
land_mark() {    # land_mark <id> <state> <tip> [reason] [extra]
    landing-pass mark "$@"
}

land_state() {   # land_state <id> -> "<state> <tip> <at> [reason]" or empty
    landing-pass state "$@"
}


# Ported to Rust, queue's own crate (sp-hwjsq, "wave 4.32" — queue decomposition row AA):
# queue/src/ops/helpers.rs `certified_list`, same selection any cutter (the batcher,
# the reconciler's mergeability check, cockpit-collect's "next up" pane) draws from. Kept
# as a shim: queue-certified-list.sh and cockpit-collect still call this by name.
#
# EXECS `queue-helpers`, NOT `queue` — a SEPARATE binary, on purpose (same crate, a second
# [[bin]]). The first cut named this subcommand on `queue` itself; that broke production
# the moment it ran under any fixture that stubs `queue` by NAME on PATH to isolate
# dispatch behaviour (sp-gypjk's convention — test-certify.sh's `queue-bin` stub is one).
# The stub logs argv and returns 0; it has no idea it was just asked to do a real git
# push, and the caller saw a quiet success with nothing moved. A plain git/mail primitive
# must never share a name with the big multi-purpose operator CLI that tests routinely
# replace wholesale.
queue_certified_list() {
    queue-helpers certified-list "$1"
}

# queue_cancel_branch_runs and queue_is_suite_transition are RETIRED (sp-hwjsq,
# "wave 4.32" — queue decomposition row AA): ported in process into the queue crate
# (queue/src/ops/helpers.rs `cancel_branch_runs`, called from queue's own in-process
# Lib::cancel_runs; `is_suite_transition`, called from the queue_sort_rows port below).
# Neither had any caller left outside that seam — no bash script called them by name — so
# there is no shim here to keep working. The old lib.sh orphan-run sweep
# (queue_sweep_orphan_runs, a periodic backstop for a run orphaned some other way — a
# hand-closed PR, or one left over from before cancel_branch_runs existed) was retired dead
# at sp-27hsi: nothing called it, bash or Rust. No in-process replacement exists; file one
# if the plan still wants that backstop.

# queue_sort_rows <repo-path> <base-sha>
# Read "<id> <tip> <epoch>" lines from stdin; write sort-key rows sorted by batcher order:
#   "<express_flag> <prio_pad> <trans_flag> <epoch_pad> <id> <tip>"
# Set PRIO_JSON env to a bdjson array for priority (and label) lookups (defaults to []).
# Sort order: express first (flag=0), then priority asc. A spira/suite-state transition
# (flag=0) is only a tiebreaker within a priority class (sp-ihxa0: a suite-state edit going
# stale is already handled at cut time by the conflict check, not by cutting it first).
# Epoch asc breaks any tie still remaining. This is the canonical batcher sort used by both
# the batcher and the cockpit.
#
# EXPRESS RANKS FIRST, AHEAD OF EVERYTHING ELSE (sp-ebx8b).
#
# RANKING FAILS OPEN (sp-m5iq3): a 266 KiB PRIO_JSON once blew past Linux's 128 KiB
# single-environment-string limit, every exec inside the old bash failed E2BIG, and the
# batcher cut nothing for nine hours with 40 branches certified and waiting. Ported to
# Rust now (sp-hwjsq, "wave 4.32"), queue/src/ops/helpers.rs `sort_rows` — same ranking,
# same fail-open contract — called in process from queue's own Lib::sort_rows. Kept as a
# shim: test-queue-sort-large.sh and cockpit-collect still call this by name. PRIO_JSON is
# written to a temp file and unset BEFORE this shim execs the `queue-helpers` binary (a
# SEPARATE binary from `queue` itself — see queue_certified_list's note above; a stub of
# `queue` must never also swallow this), for exactly the reason above: the payload must
# never cross a process boundary as an environment variable, and an exec is still a
# process boundary even when the function calling it is this thin.
queue_sort_rows() {
    local repo="$1" base_sha="$2"
    local _pj="${PRIO_JSON:-[]}" _pjf _rc
    unset PRIO_JSON
    _pjf="$(mktemp)" || return 1
    printf '%s' "$_pj" > "$_pjf"
    _pj=""
    queue-helpers sort-rows "$repo" "$base_sha" --prio-file "$_pjf" --express-label "${SPIRA_EXPRESS_LABEL:-express}"
    _rc=$?
    rm -f "$_pjf"
    return "$_rc"
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
# investigation, event, chore, epic) close by the agent's own hand, unchanged. Ported to
# Rust (sp-81t4d, "wave 4.17" — family R); see `land_verify::is_work_type`.
bead_is_work_type() {
    landing-pass is-work-type "$1"
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
#
# Ported to Rust (sp-81t4d, "wave 4.17" — family R); see `land_verify::close_on_land`. Every
# caller already discards this function's exit code (`|| true`), so the shim's own `|| true`
# below is belt-and-suspenders, not a behaviour change.
bead_close_on_land() {   # bead_close_on_land <bead-id> <landed-sha>
    landing-pass close-on-land "$1" "${2:-}" || true
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
#
# Ported to Rust, queue's own crate (sp-hwjsq, "wave 4.32" — queue decomposition row AA):
# queue/src/ops/helpers.rs `git_push_cmd`. Kept as a shim: branch-sweep.sh,
# test-git-push-app.sh and landing-pass's own separate seam (a different crate, not
# touched here) still call this by name. Neither this function nor the Rust it calls
# redirects stdout or stderr — that stays the caller's choice, exactly as before.
#
# SCAR (sp-hwjsq, round 160): the first cut execed `queue git-push`, the operator CLI
# itself. test-certify.sh stubs `queue` by NAME on PATH to isolate "queue step" dispatch
# (sp-gypjk's convention), so production's push-mode landing silently hit that stub —
# exit 0, nothing pushed, "not ok 30 - push-mode remote actually moved" — while every
# suite this bead ran was one that never stubs `queue`, so nothing caught it before it
# landed. Execs `queue-helpers` now, a SEPARATE binary (see queue_certified_list's note
# above) immune to a `queue` stub, exactly because production pushes through this path.
spira_git_push() {
    local repo="$1"; shift
    queue-helpers git-push "$repo" "$@"
}
