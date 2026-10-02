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
# suite-covers.sh is NOT sourced here (wave 4.36, sp-bobsp): nothing in lib.sh calls its
# accessors, and the five scripts that do (plan-lint.sh, suite-coverage-json.sh,
# escape-classify.sh, testenv-guard.sh, testlib.sh) now call `suite-select header ...`
# instead — the one Rust parser (suite-select/src/header.rs) that spira-lint and
# batcher-cut already read. The bash file itself is left on disk, unsourced: dozens of
# install/lib fixtures still `cp`/`ln -s` it alongside lib.sh/conf.sh from before this
# change, and none of them need editing since nothing reads the copy either.
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
#
# WHY THIS FILE STILL EXISTS (sp-hlng2, "wave 4.37", the close-out bead for the lib.sh/
# conf.sh peel — wave4-decomposition.md). Every function below either IS log/die, or IS a
# shim: the family-by-family move named in that plan has landed, in-process callers read
# the owning crate directly, and a bash caller that still types the OLD name by habit —
# `repo_root`, `land_mark`, `content_landed`, `bdq`, whatever — reaches the same logic one
# subprocess call away. `spira-lint`'s `lib-sh-shims` rule enforces this mechanically: a
# function here that is not a shim fails the gate unless it is named, with why, in
# `spira-lint/lib-sh-shims-allow` — today that is `host_cores`; `ready_raw_args`/
# `ready_count` (and the bare `READY_ARGS` array); `fayth_free`; aeon.sh's own bead-machine
# seam (`lc_bead_row` through `park_unmapped`, row I's "aeon half"); `content_landed`/
# `spira_status_seam`/`spira_bead_status`/`spira_db_reachable`/`_tsd_slots_sample`, each
# with a live bash caller a Rust port has not yet replaced; and conf.sh's own locator
# family, which cannot shim onto `spira-config` because it is what finds `spira-config`'s
# own inputs — see that allow file for the reasoning on each, not repeated here.
#
# THIS FILE'S OWN DELETION IS THEREFORE BLOCKED ON GROUPS 5-7 OF
# wiki/projects/spira/remaining-bash-inventory.md (operator surface, install/units, persona
# passes), not on anything left to port here. About twenty of THEIR scripts still `.
# "$HERE/lib.sh"` for a name this file shims — acceptance-local.sh, branch-guard.sh,
# branch-sweep.sh, cadence.sh, citations.sh, deploy.sh, disk-remedy.sh, escape-classify.sh,
# fleet-status.sh, gate-locks.sh, groom-trigger.sh, held.sh, hold.sh, holds.sh,
# pr-notify.sh, publish-backlog.sh, queue-certified-list.sh, released-defects.sh,
# release.sh, review.sh, tokens.sh, unhold.sh — plus roughly fifty of this directory's own
# `test-*.sh` suites that exercise a shim (or one of the allow-listed functions) directly
# as bash, rather than through the crate's own unit tests. Wave 4 does not own any of
# those; this file goes away only once each is rewritten in its own turn ("rewrite when
# touched", the inventory's own words for groups 5-7) and stops sourcing it.
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

# ask_already_open/ask_closed_subject retired (sp-j3fim, wave 4.31, family AB): both
# ported natively into gh-intake/src/closeout.rs — the GitHub-closeout family (AB) was
# the last bash caller left after sp-31hjr (wave 4.30) ported family C's own callers, so
# no bash form remains anywhere in the tree. `gh-intake closeout`/`unlanded-scan`/
# `backfill` carry the dedupe logic now; sentinel and landing-pass each keep their own
# separate native copy (sp-31hjr), not shared with this one — same reasoning as there.

# spira_ask_machinery, spira_ask_machinery_class, spira_land_noverdict,
# spira_is_generated_file, spira_ask_rebase_loop, spira_ask_red_recurring,
# spira_ask_rebase_refused, spira_ask_budget_deferred, spira_ask_refresh_loop and
# land_escalate retired (sp-31hjr, wave 4.30, family C): ported natively into
# landing-pass/src/real.rs + ask.rs (land_escalate into sentinel/src/dispatch.rs).
# No caller remained in bash — landing-pass's own lib.sh seam was the only one, and
# it calls the Rust versions in-process now. `landing-pass noverdict ...` and
# `sentinel --land-escalate` drive the native versions standalone for the
# real-sender suites. ask_already_open itself retired with the GitHub-closeout family
# (sp-j3fim, wave 4.31) — see the note above json_only.

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
# fleet-status.sh calls ready_count; detect_unclaimable_ready called ready_raw_args here
# too, until wave 4.28 (sp-fbqsv) ported it — its own fallback now calls
# `store::ready_raw_args` in-process, the Rust mirror of this same function), and routing
# them through a `spira-claim` subprocess at lib.sh
# SOURCE TIME was tried and reverted: it corrupted aeon's own seam snapshot read (every
# `. lib.sh` the aeon crate's seam performs now pays this at sourcing, not only a lazy
# call), turning test-aeon-elastic-concurrency.sh red. `READY_ARGS` as ONE CONST is
# satisfied on the Rust side alone — `spira_claim::READY_ARGS_BASE`, which cockpit-collect
# now links in-process instead of keeping its own copy (`probes/queue.rs`). `fayth_ready`/
# `fayth_exclude`/`bulk_ready_by_fayth`'s OWN Rust ports (below) build their own ready
# query independently, in spira-claim/src/ready.rs — a second, Rust-only copy of this
# predicate's SHAPE, not a bash caller asking two different functions the same question.

# MACHINE_READY_ARGS — READY_ARGS's own candidate set, widened past bd's own blocker filter
# (spira-claim/DESIGN.md §5 item 11). `bd ready` hides a bead whose blocker is CERTIFIED but
# not yet LANDED, because bd's status field only knows open/closed; `bd list` applies the
# same predicate with no blocker judgment at all, leaving that call to `spira-claim select
# --blockers machine`. Read only when lifecycle_enforce is on (aeon/src/run.rs ready_args()).
MACHINE_READY_ARGS=(list --status open --no-assignee --exclude-type epic,event --limit 0)
[[ -n "${SPIRA_SCOPE_LABEL:-}" ]] && MACHINE_READY_ARGS+=(--label "$SPIRA_SCOPE_LABEL")
[[ -n "${SPIRA_NO_LOOP_LABEL:-}" ]] && MACHINE_READY_ARGS+=(--exclude-label "$SPIRA_NO_LOOP_LABEL")

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

# check7_pool_decision/lane_rotate/ck7_pool/ck7_throttled/ck7_fill_cap RETIRED OUTRIGHT
# (wave 4.27, family G, sp-gzmd2): each was pure arithmetic with no caller left but
# `_ck7_summon_body`, itself retired the same bead onto `sentinel`'s own in-process
# `ck7_summon_pass` (sentinel/src/summon.rs). Ported, not just deleted — see
# `summon::tests::check7_pool_decision_matches_the_throttle_leak_fix`,
# `lane_rotate_moves_last_and_everything_before_it_to_the_end`,
# `ck7_pool_subtracts_task_live_and_floors_at_zero`,
# `ck7_throttled_is_the_stamp_unless_the_override_pins_it_clear` and
# `ck7_fill_cap_stops_on_either_the_pool_or_the_per_persona_cap` for the same cases
# test-watchtower-throttle.sh/test-express-lane.sh/test-summon-fayth.sh used to assert
# directly against the bash originals.

# mark_queue_waiters / close_landed_queue_waiters — apply/remove SPIRA_QUEUE_WAIT_LABEL on
# beads whose closed blocker is in the queue pipeline (CERTIFIED/BATCHED, not yet LANDED),
# and close out a labeled bead whose landstate already reads LANDED (it never got a branch
# to land, so the normal close-on-land path never visited it). PERMANENT (wave4-decomposition
# row H): the lifecycle-flip plan keeps this family even once lc.sh's own calls are gone —
# stacked dependents still read the label. Ported to Rust (sp-fbqsv, "wave 4.28"); see
# `sentinel::waiters` for the one-pass decision (no release-then-apply flip-flop) and the
# landstate scan (now in-process, no per-file awk fork).
mark_queue_waiters() {
    sentinel --mark-queue-waiters
}
close_landed_queue_waiters() {
    sentinel --close-landed-queue-waiters
}

# mark_open_children is CHECK 3c in the sentinel binary (sentinel/src/open_children.rs,
# sp-du8bv): it decides from the pass's one store snapshot instead of one `bd children` per
# candidate, which cost 302 s a pass. `sentinel --open-children` runs it alone.

# bead_reopen <id> <cause> [note] [suites] — hand a bead back to the graph so the NEXT aeon
# can claim it: withdraws a CERTIFIED landstate (unless <cause> is admission-exempt — see
# _census_deliberate_reopen_causes below), writes the <suites> sidecar, reopens, strips the
# submitted label, releases the claim and records the cause. Ported to spira-claim (wave
# 4.19, sp-3wfcb, row I, safety note (c7)); see spira-claim/src/reopen.rs for the contract
# and the scar (a reopen that keeps the assignee is claimable by nobody). Non-zero RC means
# bdq reopen, release_claim or the note each separately failed.
bead_reopen() {
    spira-claim reopen "$@"
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
    spira-claim release "$1"
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
# world_gate/summon_refill_argv/summon_argv/summon_fayth/_ck7_summon_body/ck7_summon_pass/
# named_unit_stop RETIRED INTO sentinel (wave 4.27, family G, sp-gzmd2): the whole summon
# loop — world (halt/drain) gate, the account's capacity, the fleet ceiling, the elastic
# and inverted last-slot reservations, the lane-then-pool fill loop, and the flock that
# serializes it against every other caller — now runs in-process in
# sentinel/src/summon.rs (`Sentinel::world_gate`/`summon_fayth`/`ck7_summon_body`/
# `ck7_summon_pass`), under a real OS flock on the SAME `$SPIRA_RUN/summon.lock` rather
# than a bash seam under one. These shims exist only for `aeon --escape`'s own seam
# (world_gate, summon_argv — still bash, out of this bead's scope) and
# `acceptance-local.sh` (named_unit_stop); `summon_fayth` itself has no bash caller left
# but czar-pass, which now calls `sentinel --summon` directly, with no lib.sh sourcing at
# all. `_ck7_summon_body`'s own unlocked-race test row is retired with it (wave4-
# decomposition.md (c)8): a compiled binary offers no shell-function-by-name override to
# stub around the lock, so the race it proved is now structurally impossible rather than
# merely refused — see test-summon-fast-path.sh's own note where that row used to be.
world_gate() {         # world_gate <fayth> <log-prefix> -> 0 if summons are permitted, 1 if halted or draining
    sentinel --world-gate "$1" "$2"
}
summon_argv() {         # summon_argv <fayth> -> systemd-run property/setenv flags, one per line
    sentinel --summon-argv "$1"
}
summon_fayth() {         # summon_fayth <fayth> [pool-remaining] [require-label] -> 0 if an aeon was started, 1 otherwise
    sentinel --summon "$1" "${2:-}" "${3:-}"
}
ck7_summon_pass() {
    sentinel --summon-pass
}
named_unit_stop() {      # named_unit_stop <systemd --user unit glob> -> stop every live unit matching it, by NAME
    sentinel --named-unit-stop "$1"
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
# Kept callable by name here so tests can call it directly without parsing census.
#
# PORTED (wave 4.35, sp-kelr2, row M): `census.sh` no longer exists for this to move "bash
# to bash" into — the `census` crate replaced it in an earlier bead, deliberately leaving
# this SQL behind (census/src/ports.rs's own doc: "The SQL itself... UNCHANGED, still
# lib.sh"). It now lives in `census::sql`, checked byte-for-byte against this file's own
# prior output before landing (see the bead's report), with `census/src/real.rs`'s World
# impl calling it in-process — these six are now one-line shims onto the `census sql ...`
# CLI door that gives every OTHER caller (the test suites, below) the same text and the
# same retry behaviour without a second implementation.
# --------------------------------------------------------------------------------------
_census_events_sql() {   # _census_events_sql [since_epoch_s]
    census sql events "${1:-}"
}
_census_handwritten_sql() {
    census sql handwritten
}
census_handwritten_run_sql() {   # -> tabular output of actor-excluded events, empty when none
    census sql run-handwritten
}
_census_class_fold_map() {
    census sql class-fold-map
}

# _census_deliberate_reopen_causes -> "<cause> <admission-exempt:0|1>" pairs, one per
# line — the single declared list of reopen causes that are the system working, not a
# fault (law-a-deliberate-state-is-not-a-fault): firing queue.sh eject, or converting a
# closed work bead to submitted, is a correct outcome, so census must count these but
# never rank them for a Maechen remedy (sp-eiatd). Both bead_reopen and
# _census_events_sql/_census_deliberate_sql read this one list; it now lives in
# spira-claim (wave 4.19, sp-3wfcb, row I) since bead_reopen does — this and
# _census_reopen_admission_exempt are shims so row M's SQL producers below see the exact
# same declared set without a second copy to drift (see spira-claim/src/reopen.rs).
_census_deliberate_reopen_causes() {
    spira-claim deliberate-causes
}

# _census_reopen_admission_exempt <cause> -> 0 (stays CERTIFIED/submitted) or 1 (does not)
_census_reopen_admission_exempt() {
    spira-claim deliberate-exempt "$1"
}

# _census_deliberate_causes_sql_list -> a quoted, comma-separated SQL IN-list of every
# deliberate cause's name.
#
# PORTED (wave 4.35, sp-kelr2, row M) onto `census sql deliberate-causes`. No second copy
# of the cause list: census's own Real fetches the names from `spira-claim
# deliberate-causes` (row I, sp-3wfcb — the canonical list is
# spira_claim::reopen::DELIBERATE_CAUSES now) the same way this file's own
# _census_deliberate_reopen_causes shim does, above. This shim exists for symmetry with
# the rest of row M; grepped the whole tree and found no caller, bash or Rust, of this name
# specifically.
_census_deliberate_causes_sql_list() {
    census sql deliberate-causes
}

# _census_deliberate_sql [since_epoch_s] -> the deliberate-cause reopen counts excluded
# from _census_events_sql's ranked list above. Not ranked, never selectable, but still a
# real count (law-absence-needs-a-positive-control) — census shows it under
# --with-suppressed the same way _census_handwritten_sql's actor-excluded rows are shown.
#
# PORTED (wave 4.35, sp-kelr2, row M) — see _census_events_sql's own comment above.
_census_deliberate_sql() {
    census sql deliberate "${1:-}"
}
census_deliberate_run_sql() {   # census_deliberate_run_sql [since_epoch_s] -> tabular output of deliberate-cause reopen counts, empty when none
    census sql run-deliberate "${1:-}"
}
# census_events_run_sql [since_epoch_s] -> tabular output; exits non-zero when unreachable.
#
# PORTED (wave 4.35, sp-kelr2, row M): the 3-attempt retry (CENSUS_RETRY_DELAY_S doubling
# each time, default 2s) moved with the query into census/src/real.rs — the one piece of
# this family that was never pure SQL text. `CENSUS_RETRY_DELAY_S`/`SPIRA_BD`/`SPIRA_DB`
# need no re-threading: all three are ordinary (exported-when-set) env vars, not the
# conf.sh-withheld kind `fayth_get`'s shim has to carry across the exec boundary by hand.
census_events_run_sql() {
    census sql run-events "${1:-}"
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
#
# PORTED (wave 4.35, sp-kelr2, row Q): the tiering/cache/seam logic above lives in `rule`
# now (`rule/src/memories.rs` for the pure render, `rule/src/main.rs` for the cache and the
# SPIRA_MEMORIES_CMD/bdq read) — this is the one-line shim onto its `render-memories` CLI
# door, concierge.sh's own call site unchanged. SPIRA_HOME and SPIRA_REPO are threaded
# explicitly — conf.sh deliberately never exports either (the EXEC-BOUNDARY TRAP comment at
# the top of this file) — SPIRA_REPO is the harness path the index tier's own "rule.sh show"
# hint names, caught red by test-render-memories.sh's "rule.sh path is executable" the one
# time this shim left it unthreaded. SPIRA_STATUTE_CORE/SPIRA_MEMORIES_CACHE/
# SPIRA_MEMORIES_CACHE_AGE/SPIRA_MEMORIES_CMD need no re-threading — test-render-memories.sh's
# own fixture calls already export them (bash prefix assignment exports for that command's
# whole subtree), and production always passes core_csv as $3 rather than relying on the
# env fallback.
render_memories() {      # render_memories <prefix-csv> [char-budget] [core-csv]
    SPIRA_HOME="${SPIRA_HOME:-}" SPIRA_REPO="${SPIRA_REPO:-}" \
        rule --home "${SPIRA_HOME:-}" render-memories "$@"
}

# Split a rendered persona prompt on <!-- task --> and write system.md / task.md.
# system_prompt_split <sysfile> <taskfile> <statutes_text> <rendered_prompt>
# Sets SPIRA_SYSTEM_FLAG to the appropriate --*-system-prompt-file value.
# Reads FAYTH_SYSTEM_PROMPT (replace|append, default append): replace uses
# --system-prompt-file; append (or unset) uses --append-system-prompt-file so
# Claude Code's coding guidance stays underneath the persona layer.
#
# PORTED (wave 4.35, sp-kelr2, row Q): the split/file-write itself is `rule`'s
# `system-prompt-split` CLI door (`rule::memories::split`); aeon.sh retired its own call to
# this function already (sp-j89pd, wave 4.2 — it renders through aeon/src/brief.rs
# in-process), so test-render-memories.sh's G-08 delivery-fence check is this shim's one
# live caller now. The SPIRA_SYSTEM_FLAG assignment has no caller left either (same retirement)
# but stays here, computed locally rather than moved into the binary, since it is a plain
# shell-variable side effect a subprocess cannot set in its caller's shell.
system_prompt_split() {
    local sysfile="$1" taskfile="$2" statutes="$3" prompt="$4"
    rule system-prompt-split "$sysfile" "$taskfile" "$statutes" "$prompt"
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
# Ported to Rust (sp-8ofmt, "wave 4.29" — family T-b); see `strand::detectors::all_partition_members`.
all_partition_members() {
    strand all-partition-members
}

# detect_unclaimable_ready -> one UNCLAIMABLE line per ready bead no persona can claim.
# file_unclaimable_incidents <detect_unclaimable_ready output> -> one P1 incident per line,
# filed into the Groomer partition (idempotent: incident.sh dedupes on unclaimable:<id>).
# detect_branch_collisions -> one COLLISION line per open bead whose recorded branch is
# checked out in a DIFFERENT bead's canonical worktree. park_branch_collisions <that output>
# frees a closed-clean-unheld squatter, cuts an inherited branch: label, or parks the rest
# with SPIRA_ASK_LABEL/overseer.
#
# Ported to Rust (sp-fbqsv, "wave 4.28"); see `sentinel::detect` for the full rationale
# each of these carried (the fifteen-hour fayth:-preference strand, the unclaimable-
# reporting-its-own-report cycle guard, the sp-lyglx/sp-vcxmz branch-collision incidents).
# `detect_unclaimable_ready` still classifies through `spira/unclaimable.py`, unchanged and
# untouched — split out on purpose so a fixture-JSON table (test-unclaimable.sh) and
# cockpit-collect's own cross-check (test-cockpit-unclaimable.sh, UC-dispatch-17) can drive
# it directly; this bead only ports the bash orchestration around it. The bash-only re-exec
# that used to re-source lib.sh from the production checkout when called from a worktree
# (sp-b0j0s) is retired, not ported: it was a workaround for a bash function having no
# persistent, correctly-resolved context across calls, and this binary resolves its config
# once at startup the same way for every check in the pass (law-a-binary-resolves-the-
# config-it-reads) — see test-unclaimable-worktree.sh for the suite kept to prove this.
detect_unclaimable_ready() {
    sentinel --detect-unclaimable
}
file_unclaimable_incidents() {   # file_unclaimable_incidents <detect_unclaimable_ready output>
    sentinel --file-unclaimable <<< "$1"
}
detect_branch_collisions() {
    sentinel --detect-collisions
}
park_branch_collisions() {   # park_branch_collisions <detect_branch_collisions output>
    sentinel --park-collisions <<< "$1"
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
# Ported to Rust (sp-8ofmt, "wave 4.29" — family T-b); see `strand::detectors::detect_livelocked`.
detect_livelocked() {
    strand detect-livelocked
}

# detect_landed_but_open -> "STATE <id> landed-but-open — <evidence>" for every open or
# in_progress bead in the WHOLE graph — no partition, no label filter — whose repository's
# base already carries a commit landing it. A bead's landed-but-open state does not depend
# on which partition it happens to carry, so a scan bounded to one partition cannot see one
# filed under another (sp-0qp7s: the groomer's scan read only its own trigger partition).
# Ported to Rust (sp-8ofmt, "wave 4.29" — family T-b); see `strand::detectors::detect_landed_but_open`.
detect_landed_but_open() {
    strand detect-landed-but-open
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
# Ported to Rust (sp-8ofmt, "wave 4.29" — family T-b); see `strand::detectors::detect_closed_unlanded_states`.
detect_closed_unlanded_states() {
    strand detect-closed-unlanded-states
}

# detect_false_blockers <blocker-ids> -> "STATE <id> blocked-by-unlanded <blocker> — <evidence>"
# for every open or in_progress bead depending (type=blocks) on one of <blocker-ids> (newline
# or space separated). detect_closed_unlanded_states names the blockers this reads: a closed
# bead whose work never landed still counts as "done" to every dependent's `bd ready`, so its
# dependents sit correctly-blocked forever on a false premise (sp-jzfog blocking sp-vsob2).
# The remedy is the blocker's own — reopening it (task 2) is what clears this — so this
# function only makes the false block visible on the bead it was holding shut.
# Ported to Rust (sp-8ofmt, "wave 4.29" — family T-b); see `strand::detectors::detect_false_blockers`.
detect_false_blockers() {
    strand detect-false-blockers "$@"
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
# Ported to Rust (sp-8ofmt, "wave 4.29" — family T-b); see `strand::detectors::detect_incident_needs_builder`.
detect_incident_needs_builder() {
    strand detect-incident-needs-builder
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
# Ported to Rust (sp-8ofmt, "wave 4.29" — family T-b); see `strand::detectors::detect_invalid_closed`.
detect_invalid_closed() {
    strand detect-invalid-closed
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
#
# PORTED (wave 4.35, sp-kelr2, row V) to maechen-trigger's `lanes` module, called in-process
# by its own sweep; this is now the one-line shim onto its `repo-lanes` CLI door, which
# groom-trigger.sh (the one surviving bash caller) and test-repo-lanes.sh both reach the
# same way. SPIRA_HOME is threaded explicitly — conf.sh deliberately never exports it (the
# EXEC-BOUNDARY TRAP comment at the top of this file). `_spira_expand_lanes`, the internal
# helper this function used to call, had no caller outside this one and is retired rather
# than given its own shim.
spira_repo_lanes() {
    SPIRA_HOME="${SPIRA_HOME:-}" maechen-trigger --home "${SPIRA_HOME:-}" repo-lanes "$1"
}

# spira_lane_admitted <lane> -> 0 if the home repo or some repo in SPIRA_REPO_MAP admits
# it, 1 otherwise. Shared by groom-trigger.sh and maechen-trigger (duplicate cluster D14) —
# on a consuming install with no repo willing to run a lane, its trigger would otherwise
# accumulate trigger beads for work nobody can do.
#
# PORTED (wave 4.35, sp-kelr2, row V) — see spira_repo_lanes above; same shim pattern, same
# CLI binary, its `lane-admitted` door.
spira_lane_admitted() {
    SPIRA_HOME="${SPIRA_HOME:-}" maechen-trigger --home "${SPIRA_HOME:-}" lane-admitted "$1"
}

# spira_open_trigger_count <labels> -> count of open-or-in_progress beads carrying every
# label in <labels> (comma-separated), or 0 on a query failure (fail toward filing rather
# than silently going quiet — the caller's own dedup guard is what a false 0 would defeat).
# in_progress is included because a claimed trigger bead leaves --status open, and a dedup
# query scoped to open alone would file a duplicate on the very next tick (sp-mp9s — the
# defect that motivated including it in maechen-trigger.sh, extracted here so
# groom-trigger.sh gets the same fix rather than drifting from it).
#
# PORTED (wave 4.35, sp-kelr2, row V) — see spira_repo_lanes above; SPIRA_DB/SPIRA_BD are
# already exported by conf.sh (`spira_config::resolve::EXPORT_KEYS`), so no re-threading is
# needed here the way SPIRA_HOME needs it above.
spira_open_trigger_count() {
    maechen-trigger --home "${SPIRA_HOME:-}" open-trigger-count "$1"
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

# _tsd_kv_field is RETIRED (wave 4.35, sp-kelr2, row AC): its one intended caller,
# session_result_fields, was already dead (row N, retired in wave 4.2's bead 2 — "RETIRE
# rather than port" applies to this function too, since the plan's "port to tsd" assumed a
# live caller that the inventory for THIS bead found gone). Grepped the whole tree: no
# other caller, bash or Rust, ever existed.

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

# _tsd_escape <member> <suite> <class> [batch_id] — appends one escape record (run/tsd/
# escape): a round red attributed to <member> on <suite>, classified per sp-6vd2s
# (escape-classify.sh). Best-effort, like every tsd producer here.
#
# PORTED (wave 4.35, sp-kelr2, row AC): the class whitelist and the field mapping now live
# in `tsd-write`'s own `escape` subcommand (ESCAPE_CLASSES, tsd/src/main.rs) — the one
# source of truth for the four classes, where it used to be a second copy of
# _TSD_ESCAPE_CLASSES here that could drift from sp-6vd2s's own definition. This is the
# one-line shim onto it; escape-classify.sh (the one live caller) is unchanged.
_tsd_escape() {
    tsd-write escape "$@" >/dev/null 2>&1 || true
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

# gh_issue_closeout retired (sp-j3fim, wave 4.31, family AB): ported natively into
# gh-intake/src/closeout.rs. landing-pass and queue shell to "gh-intake closeout <id>
# <sha> <repo>" directly now -- no lib.sh seam call left for this family.

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

# _gh_close_ask_unblock/gh_issue_ask_unlanded/_gh_resolve_stale_asks/_gh_unlanded_scan
# retired (sp-j3fim, wave 4.31, family AB): ported natively into gh-intake/src/closeout.rs
# as gh_close_ask_unblock/gh_issue_ask_unlanded/gh_resolve_stale_asks/gh_unlanded_scan.
# "gh-intake unlanded-scan" drives the whole family now (landing-pass's and queue's own
# seams call the binary directly); "gh-intake backfill" replaces gh-issue-backfill.sh.

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
