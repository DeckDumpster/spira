#!/usr/bin/env bash
#
# suites.sh — run every suite the landing gate does not, on a schedule.
#
#   suites.sh list           every spira/test-*.sh, where it runs, and its last result
#   suites.sh run            run the timed set inside the budget; file a bead per red
#   suites.sh status         the compact block the watchtower puts on the sweep
#
# WHY THIS EXISTS. A suite that nothing runs is not a cheap test, it is a false record of
# coverage — worse than no suite, because its existence is what stops anybody writing the
# check it was supposed to be (law-absence-needs-a-positive-control). Measured once: nine
# suites in the tree, four named by the gate, FIVE executed by nothing at all, three of those
# landed the same night with their beads closed citing them as verification. The growth
# problem was never that the gate gets longer as features arrive. It is that a new suite is
# written, goes green once in its author's session, and is then run by nothing, forever.
#
# WHY A TIMED RUN AND NOT SELECTION. Selection picks from a registry; these suites were in no
# registry, so no selector would have found them either. Selection is also unsound in the one
# direction that matters until this exists: an unmapped file falls back to "run everything",
# and "everything" was a hand-maintained list already missing five of nine. The full run is
# what makes selection safe later, not the other way round — and the `# covers:` lines the
# selector will want are still on eight of the suites and cost nothing to keep.
#
# DISCOVERY IS A GLOB, NEVER A LIST. `spira/test-*.sh` is the whole population. A list is the
# thing that just failed, so a new suite is run by existing to be found, and a deleted one
# stops being run with no edit anywhere. The ONE hand-written list is the gate's, in
# `gate-suites`, and this runs its complement — so a suite dropped from the gate moves here
# rather than out of the world, and neither set can be edited into overlapping the other.
#
# A SUITE WITH NO `# covers:` LINE IS STILL RUN. The annotation is for selection, which is a
# later bead; a missing one is reported as an omission and never as a reason to skip. Skipping
# it would reproduce this defect exactly, in a mechanism written to end it.
#
# IT DOES NOT REOPEN, BLOCK OR POISON ANYTHING. A red files a bead and nothing more. By
# law-reversibility-outranks-coverage a failure caught twenty minutes after landing is fine
# when a revert undoes it, and a timed runner that could refuse work would be the seventeen
# minutes back on the critical path under a different name.
#
# WHERE IT RUNS: inside the Ops session, which is why there is no timer unit for it. The
# watchtower NAMES this scan on the sweep it files and Ops runs it. That division is not
# decoration — the watchtower's whole contract is to be deterministic and cheap, because the
# one thing a detector may not be is another thing that is down during an outage, and a
# several-minute test run inside it would make it exactly that.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"

STATE="${SPIRA_SUITES_STATE:-$SPIRA_RUN/suites}"
BUDGET="${SPIRA_SUITES_BUDGET:-420}"
# SPIRA_SUITES_MAXSEC is the systemd timeout that will kill this process, injected by the
# unit's Environment= line. Cap the budget 60 seconds under it so cleanup always completes.
# A conf override of SPIRA_SUITES_BUDGET cannot then schedule work past the kill deadline.
if [ -n "${SPIRA_SUITES_MAXSEC:-}" ]; then
    _suites_cap=$(( SPIRA_SUITES_MAXSEC - 60 ))
    [ "$BUDGET" -gt "$_suites_cap" ] && BUDGET="$_suites_cap"
    unset _suites_cap
fi
PER_SUITE="${SPIRA_SUITE_TIMEOUT:-600}"
STALE="${SPIRA_SUITES_STALE:-21600}"
PRIORITY="${SPIRA_SUITES_PRIORITY:-2}"
GATE_LIST="${SPIRA_GATE_SUITES:-$HERE/gate-suites}"
INC="${SPIRA_INCIDENT:-$HERE/incident.sh}"
CURSOR="$STATE/cursor"
# RUNNER_VARS: variables the systemd unit's Environment= lines inject, plus variables that
# conf.sh derives from the runner's installation and exports to child processes.  An aeon's
# environment does not carry them.  A suite that fails because one of these is set will
# produce a red the aeon cannot reproduce — it runs the same reproduce line in a clean
# environment and finds the suite green.  Configurable for tests.
#
# SPIRA_DB IS INCLUDED. conf.sh exports the production SPIRA_DB, which may point at a
# server-backed database.  When a suite sources testdb.sh, conf.sh runs and checks
# SPIRA_DB/.beads — if the server is down the check exits 1 before testdb_up ever runs.
# Stripping SPIRA_DB causes conf.sh to derive a default path that has no .beads on any
# stock install, skipping the check entirely; testdb_up then sets SPIRA_DB to the fixture.
RUNNER_VARS="${SPIRA_SUITES_RUNNER_VARS:-SPIRA_HOME SPIRA_SUITES_MAXSEC SPIRA_DB}"

# --------------------------------------------------------------------------------------
# THE POPULATION, AND THE PARTITION OF IT.
# --------------------------------------------------------------------------------------
# all_suites -> every suite in the tree, basenames, sorted. The glob is the definition.
#
# `printf '%s\n' "$HERE"/test-*.sh` on a directory with no match yields the PATTERN itself,
# which would be reported as one suite named `test-*.sh` that cannot be read — so the
# existence of each is tested rather than assumed.
all_suites() {
    local f
    for f in "$HERE"/test-*.sh; do
        [ -f "$f" ] || continue
        basename "$f"
    done | sort
}

# gated_suites -> the basenames gate-suites names. Empty when the file is unreadable, and
# the caller must treat that as "unknown", never as "the gate runs nothing" — reading it as
# nothing would put every gated suite into the timed run and double the cost of a pass while
# reporting that it had found more work to do.
gated_suites() {
    local line
    [ -r "$GATE_LIST" ] || return 1
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%%#*}"
        line="${line#"${line%%[![:space:]]*}"}"; line="${line%"${line##*[![:space:]]}"}"
        [ -n "$line" ] || continue
        basename "$line"
    done < "$GATE_LIST" | sort
}

# is_gated <basename> -> 0 when the gate already runs it
is_gated() { case " $GATED " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }


# priority_of <basename> -> the priority a red from this suite is filed at.
#
# THE SUITE DECLARES IT, with `# priority: N` beside its `# covers:` line, and the default is
# routine. How urgent a failure in some code is, is a claim about that code, and the only
# place that claim can live without becoming a second list to keep in step with the glob is
# in the suite itself. A malformed value is ignored rather than passed to `bd`, which would
# fail the create and lose the finding to a typo.
priority_of() {
    local p; p="$(sed -n 's/^# *priority: *//p' "$HERE/$1" 2>/dev/null | head -1 | tr -d '[:space:]')"
    case "$p" in [0-4]) printf '%s' "$p" ;; *) printf '%s' "$PRIORITY" ;; esac
}

# timeout_of <basename> -> the suite's declared `# timeout: N` in seconds, or empty.
#
# A suite that drives the real harness — spinning up aeons, building fixture databases,
# running full lifecycles — cannot be reliably bounded by the default PER_SUITE ceiling
# without risking false timeouts under load. The suite author knows the expected worst-case
# runtime; declaring it here sets both the watchdog ceiling for that suite and the minimum
# budget required before the runner starts it. A malformed or absent value yields empty,
# which preserves PER_SUITE as the ceiling and last_secs as the skip threshold.
timeout_of() {
    local t; t="$(sed -n 's/^# *timeout: *//p' "$HERE/$1" 2>/dev/null | head -1 | tr -d '[:space:]')"
    case "$t" in [0-9]*) printf '%s' "$t" ;; esac
}

# --------------------------------------------------------------------------------------
# THE RECORD. One file per suite: `<status> <epoch> <seconds> <fingerprint>`.
#
# GREEN IS RECORDED, NOT SILENT. "No bead was filed" reads identically whether every suite
# passed or the runner has not run since the box came up, and an empty directory gives the
# reassuring reading (law-absence-needs-a-positive-control). So a pass writes a positive
# record with a timestamp, and `status` reports a record older than SPIRA_SUITES_STALE as
# unrun rather than as green — which is what lets the pane render `?` instead of a zero.
# --------------------------------------------------------------------------------------
record_write() {         # record_write <basename> <status> <seconds> <fingerprint>
    mkdir -p "$STATE" 2>/dev/null || return 1
    printf '%s %s %s %s\n' "$2" "$(date +%s)" "$3" "${4:--}" > "$STATE/$1.result"
}
record_read() {          # record_read <basename> -> `<status> <epoch> <seconds> <fp>` or empty
    local f="$STATE/$1.result" st at secs fp
    [ -r "$f" ] || return 1
    # The trailing-status of `read` is ignored for the same reason the watchtower ignores it:
    # a file without a trailing newline populates every variable and then reports failure at
    # EOF. The guards below judge the CONTENT, which a truncated file cannot satisfy.
    read -r st at secs fp < "$f" 2>/dev/null || true
    [ -n "${st:-}" ] || return 1
    case "${at:-}" in ''|*[!0-9]*) return 1 ;; esac
    printf '%s %s %s %s' "$st" "$at" "${secs:-?}" "${fp:--}"
}

# --------------------------------------------------------------------------------------
# THE UNREACHED FLAG. Separate from the result record so that a suite skipped by the
# budget guard does not overwrite the verdict and runtime it recorded on its last real run.
#
# A SUITE CAN BE RED AND UNREACHED AT THE SAME TIME. "red" is the last verdict; "unreached"
# means the budget did not reach this suite in the most recent pass. Both facts must survive:
# the red drives the bead count on the sweep; the unreached flag explains why the age column
# has not advanced. Merging the two into one field (the pre-sp-u1g behaviour) made them
# mutually exclusive, so 117 of 210 result files showed "unreached" with no verdict at all.
#
# CLEARING. When a suite runs in a pass — any outcome — its .unreached file is removed.
# When a suite is not reached, its .unreached file is written (with the pass epoch). The
# .unreached file's epoch compared with the .result epoch tells cmd_list whether the suite's
# last record is from before or after its last skipped pass, but neither cmd_list nor
# cmd_status requires that comparison: the presence of the file is the signal.
# --------------------------------------------------------------------------------------
unreached_write() {   # unreached_write <basename>  — record that this pass skipped the suite
    mkdir -p "$STATE" 2>/dev/null || return 1
    printf '%s\n' "$(date +%s)" > "$STATE/$1.unreached"
}
unreached_clear() {   # unreached_clear <basename>  — suite ran this pass; clear the flag
    rm -f "$STATE/$1.unreached"
}
unreached_read() {    # unreached_read <basename> -> epoch when last skipped, or fails
    local f="$STATE/$1.unreached"
    [ -r "$f" ] || return 1
    local ep; read -r ep < "$f" 2>/dev/null || true
    printf '%s' "${ep:-}"
}

# --------------------------------------------------------------------------------------
# THE FINGERPRINT — the second half of the dedupe key, the first being the suite.
#
# A PERSISTENT FAILURE MUST FILE ONCE, NOT ONCE A CYCLE. Six passes an hour against a suite
# that stays red is 144 beads a day, and a queue holding 144 copies of one problem is a queue
# nobody reads. So the ref is `<suite>:<fingerprint>` and incident.sh dedupes on it: the first
# red files, every identical red after it bumps a recurrence on the same bead, and a red that
# CHANGES is new information and files afresh.
#
# WHAT IS HASHED. The suite's FAIL lines, which every suite in this tree emits — three
# different assertion helpers, one shared word. When there are none, the tail of the output
# and the exit status, which is the honest fallback for a suite that died rather than failed
# an assertion. Both are normalised first: scratch directories, wall-clock timestamps and any
# run of three or more digits all vary between passes and must be stripped to a fixed token
# before hashing. Timestamps are normalised BEFORE digit collapse so that two-digit fields
# (month, day, hour, minute, second) that survive the [0-9]{3,} rule do not fork the
# fingerprint — that failure filed one bead per cycle for an identically-failing suite.
# --------------------------------------------------------------------------------------
fingerprint() {          # fingerprint <rc> <output> -> a short stable digest
    local rc="$1" out="$2" sig
    # NO PIPE INTO A MATCHER THAT EXITS EARLY, and `|| true` because grep exits 1 on no match
    # and pipefail would make that the value of the assignment (law-no-grep-q-under-pipefail).
    sig="$(printf '%s\n' "$out" | grep -F 'FAIL' || true)"
    [ -n "$sig" ] || sig="$(printf '%s\n' "$out" | tail -n 20)"
    printf 'rc=%s\n%s\n' "$rc" "$sig" \
        | sed -e 's#/tmp/[A-Za-z0-9._-]*#/tmp/X#g' \
              -e 's#/[A-Za-z0-9._/-]*/sptest_[A-Za-z0-9_]*#/X#g' \
              -e 's/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9][.0-9]*Z\{0,1\}/TIMESTAMP/g' \
              -e 's/[0-9][0-9]:[0-9][0-9]:[0-9][0-9]/TIME/g' \
              -e 's/[0-9]\{3,\}/N/g' \
        | cksum | tr -d ' \t'
}

# --------------------------------------------------------------------------------------
# CAUSE FINGERPRINT — the first FAIL line normalised to a stable hash, for clustering.
#
# DISTINCT FROM fingerprint(): that function hashes ALL FAIL lines against a specific
# suite and exit code, producing a per-suite cross-pass dedup key. This function hashes
# only the FIRST FAIL line, stripping the same run-varying tokens, to produce a key that
# two suites failing on the same assertion will share. Suites with no FAIL lines are
# keyed on their exit code, which clusters them by failure code when assertions are absent.
# --------------------------------------------------------------------------------------
cause_fp() {           # cause_fp <rc> <output> -> stable first-FAIL-line hash
    local rc="$1" out="$2" first_fail
    # || true: grep exits 1 on no match; pipefail would fail the assignment.
    first_fail="$(printf '%s\n' "$out" | grep -F 'FAIL' | head -1 || true)"
    [ -n "$first_fail" ] || first_fail="rc=$rc"
    printf '%s\n' "$first_fail" \
        | sed -e 's#/tmp/[A-Za-z0-9._-]*#/tmp/X#g' \
              -e 's#/[A-Za-z0-9._/-]*/sptest_[A-Za-z0-9_]*#/X#g' \
              -e 's/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9][.0-9]*Z\{0,1\}/TIMESTAMP/g' \
              -e 's/[0-9][0-9]:[0-9][0-9]:[0-9][0-9]/TIME/g' \
              -e 's/[0-9]\{3,\}/N/g' \
        | cksum | tr -d ' \t'
}

# --------------------------------------------------------------------------------------
# FILING A RED. Through incident.sh, which is the intake that already exists: it spools the
# payload to disk BEFORE touching the database and clears the spool only once the bead
# exists, it dedupes on the external ref, it bumps a recurrence rather than filing a second,
# and past SIN_AT recurrences it escalates once to the operator. None of that is worth a
# second implementation, and the recurrence count is exactly the signal wanted here: a suite
# red for five cycles is a suite nobody is fixing.
#
# THE LABELS ARE THE CALLER'S, so this lands in the BUILDER's partition rather than in Ops's.
# A broken suite is a defect in the harness — work for whoever can change the code — and Ops
# has eight minutes and a runbook, which is the wrong shape for it entirely.
#
# THE BEAD CARRIES THE OUTPUT. An incident that says "go and run it yourself" makes the aeon
# spend its first minutes reproducing what this pass has already got, and it would reproduce
# it against a tree that has moved (law-escalations-carry-their-evidence).
# --------------------------------------------------------------------------------------
file_red() {             # file_red <basename> <status> <rc> <seconds> <fp> <output> [priority]
    local s="$1" status="$2" rc="$3" secs="$4" fp="$5" out="$6" cov id=""
    local _prio="${7:-$(priority_of "$s")}"
    cov="$(suite_covers_of "$HERE/$s")"
    if [ ! -r "$INC" ]; then
        log "suites: no intake at $INC — $s is red and the finding reaches nobody"
        return 1
    fi
    # THE ID IS THE LAST LINE, NOT THE WHOLE OUTPUT. incident.sh logs through `tee`, so its
    # progress lines share stdout with the id it returns, and a bare capture takes both — the
    # first version reported a bead id of "…incident: spooled … at /run/incident-spool/…",
    # which is a filing failure rendered as a success, complete with a plausible-looking
    # identifier. The status is checked first and the shape of the id after it, because a
    # spooled event exits non-zero and is a RETRY rather than a loss.
    local out_inc rc_inc
    out_inc="$(SPIRA_INCIDENT_TYPE=bug \
          SPIRA_INCIDENT_PRIORITY="$_prio" \
          SPIRA_INCIDENT_ACTOR=suites \
          SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
          SPIRA_INCIDENT_REPO="$SPIRA_HOME_REPO" \
          SPIRA_INCIDENT_REF="suite:$s:$fp" \
          SPIRA_INCIDENT_PATH="$HERE/$s" \
          SPIRA_INCIDENT_CAUSE=suite-red \
          SPIRA_DB="$SPIRA_DB" \
          bash "$INC" file "$s is $status in the timed suite run" - <<PAYLOAD
The timed full run — every \`spira/test-*.sh\` the landing gate does not run — found this
suite $status. It is filed and nothing is blocked: this run reopens no bead and refuses no
branch, so the fix lands as ordinary work.

  suite            $s
  status           $status (rc=$rc) after ${secs}s
  covers           ${cov:-NOTHING DECLARED — this suite has no \`# covers:\` line}
  fingerprint      $fp
  reproduce        bash spira/$s

The fingerprint is over the suite's FAIL lines with scratch paths and numbers normalised out,
and it is half the dedupe key: an identical failure next cycle bumps a recurrence on this
bead rather than filing another, and a failure that CHANGES files a new one.

--- output ---------------------------------------------------------------------------
$(printf '%s\n' "$out" | tail -c 6000)
PAYLOAD
)"; rc_inc=$?
    if [ "$rc_inc" -ne 0 ]; then
        log "suites: the intake could not file $s — it stays spooled and drain will retry"
        return 1
    fi
    id="$(printf '%s\n' "$out_inc" | tail -n 1 | tr -d '[:space:]')"
    case "${id:-}" in
        ''|*[!A-Za-z0-9-]*|-*|*-) log "suites: the intake returned no id for $s"; return 1 ;;
    esac
    printf '%s' "$id"
}

# --------------------------------------------------------------------------------------
# FILING AN ENVIRONMENT MISMATCH. Called when a suite is red under the runner but passes in
# an aeon's environment (runner vars stripped). Do not send an aeon to fix this: it cannot
# reproduce the failure. File a finding that names the variables that differ so the owner of
# those variables can make the suite robust to them (or strip them before launching suites).
#
# THE REF IS SUITE-SCOPED, NOT FINGERPRINT-SCOPED. The failure is in the runner's environment,
# not in the suite's output, so the same runner state produces the same symptom on every pass.
# Deduplicating on the suite name alone means each cycle of this mismatch bumps recurrence on
# one bead rather than filing a fresh one with a new fingerprint.
# --------------------------------------------------------------------------------------
file_env_red() {    # file_env_red <basename> <rc> <seconds> <fp> <output> <differing_vars>
    local s="$1" rc="$2" secs="$3" fp="$4" out="$5" differing="$6" cov id=""
    cov="$(suite_covers_of "$HERE/$s")"
    if [ ! -r "$INC" ]; then
        log "suites: no intake at $INC — $s env mismatch and the finding reaches nobody"
        return 1
    fi
    local out_inc rc_inc
    out_inc="$(SPIRA_INCIDENT_TYPE=bug \
          SPIRA_INCIDENT_PRIORITY="$(priority_of "$s")" \
          SPIRA_INCIDENT_ACTOR=suites \
          SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
          SPIRA_INCIDENT_REPO="$SPIRA_HOME_REPO" \
          SPIRA_INCIDENT_REF="runner-env:$s" \
          SPIRA_INCIDENT_CAUSE=runner-env \
          SPIRA_INCIDENT_PATH="$HERE/$s" \
          SPIRA_DB="$SPIRA_DB" \
          bash "$INC" file "$s is red under the timed runner but passes in an aeon's environment" - <<PAYLOAD
The timed runner found this suite red, but a confirming run without the runner's injected
variables passed. Do not send an aeon to reproduce this: the bead's reproduce line runs in an
aeon's environment and will be green.

The runner's environment carries variables an aeon's does not, and at least one is causing the
failure. Fix: make the suite robust to these variables, or strip them before running suites.

  suite            $s
  status           red (rc=$rc) after ${secs}s under the runner
  confirming run   green (re-run with these stripped: $differing)
  covers           ${cov:-NOTHING DECLARED — this suite has no \`# covers:\` line}
  fingerprint      $fp

The dedupe ref is runner-env:$s — identical mismatches bump recurrence on this bead.

--- runner output -------------------------------------------------------------------
$(printf '%s\n' "$out" | tail -c 6000)
PAYLOAD
    )"; rc_inc=$?
    if [ "$rc_inc" -ne 0 ]; then
        log "suites: the intake could not file $s env mismatch — it stays spooled and drain will retry"
        return 1
    fi
    id="$(printf '%s\n' "$out_inc" | tail -n 1 | tr -d '[:space:]')"
    case "${id:-}" in
        ''|*[!A-Za-z0-9-]*|-*|*-) log "suites: the intake returned no id for $s env mismatch"; return 1 ;;
    esac
    printf '%s' "$id"
}

# --------------------------------------------------------------------------------------
# FILING A FIXTURE FAULT. One bead for the whole pass, naming every suite that could not
# start because the shared fixture collapsed. These are NOT red suites — running any of
# them individually against a healthy fixture will pass (which is the claim this bead
# rests on). The bead is distinct from a suite-red bead: it names the fixture rather than
# a suite, carries TESTDB_NAME as the dedupe key, and its reproduce line is about restoring
# the fixture rather than fixing a suite.
#
# WHY ONE BEAD AND NOT N. A shared fixture collapse is one event. Each borrower exiting
# 75 is a symptom of that one event. Filing N beads assigns N workers to a problem that
# has one cause, and the worker who claims test-hold.sh will run it by hand, watch it
# pass, and close the bead as unreproducible — which is not false, but it is not useful
# either. The operator's view should show one finding so that one person looks for one
# cause (law-count-things-not-log-lines).
# --------------------------------------------------------------------------------------
file_fixture_fault() {  # file_fixture_fault <n> <suite-list> <fixture-name>
    local n="$1" suites="$2" fixture="${3:-unknown}"
    if [ ! -r "$INC" ]; then
        log "suites: no intake at $INC — fixture fault ($fixture) reaches nobody"
        return 1
    fi
    local out_inc rc_inc id=""
    out_inc="$(SPIRA_INCIDENT_TYPE=bug \
          SPIRA_INCIDENT_PRIORITY="$PRIORITY" \
          SPIRA_INCIDENT_ACTOR=suites \
          SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
          SPIRA_INCIDENT_REPO="$SPIRA_HOME_REPO" \
          SPIRA_INCIDENT_REF="fixture-fault:${fixture}" \
          SPIRA_INCIDENT_CAUSE=fixture-fault \
          SPIRA_INCIDENT_PATH="$HERE/testdb.sh" \
          SPIRA_DB="$SPIRA_DB" \
          bash "$INC" file "shared fixture collapsed — ${n} suite(s) could not start" - <<PAYLOAD
The shared fixture failed to reset during the timed suite run. Every suite listed below
exited before running a single assertion. These are not red suites: running any of them
individually against a healthy fixture will pass.

  fixture name     $fixture
  suites affected  $suites
  count            $n

The pass builds one shared fixture and resets it for each borrower (testdb_reset inside
testdb_up). When the reset fails, the borrower exits with the fixture-fault code
(TESTDB_FAULT_EXIT=75) so the pass can file one bead here rather than one per borrower.

Investigate: why did testdb_reset fail? Likely causes: the fixture baseline directory was
deleted mid-pass, the fixture directory was removed, or a concurrent process corrupted it.

The suite names above provide the reproduce line once the fixture is healthy:
  bash spira/<suite>
PAYLOAD
    )"; rc_inc=$?
    if [ "$rc_inc" -ne 0 ]; then
        log "suites: the intake could not file fixture fault for $fixture"
        return 1
    fi
    id="$(printf '%s\n' "$out_inc" | tail -n 1 | tr -d '[:space:]')"
    case "${id:-}" in
        ''|*[!A-Za-z0-9-]*|-*|*-) log "suites: the intake returned no id for fixture fault"; return 1 ;;
    esac
    printf '%s' "$id"
}

# --------------------------------------------------------------------------------------
# FILING A SETUP FAULT. Called when a suite exits non-zero but emitted ASSERTIONS 0 —
# meaning it reached no assertion before it exited. The setup failed; the code under
# # covers: is not the problem, and a worker sent there will find it correct.
#
# THE DISCRIMINANT IS THE ASSERTIONS TRAILER, not the exit code. A missing trailer means
# the suite has not yet adopted the counter; suites.sh treats that as red (as today) so
# that an un-migrated suite cannot silently become a setup-fault. This is law-absence-
# needs-a-positive-control applied to the counter itself: absence must never read as zero.
#
# THE REF IS setup-fault:<suite>. Repeated failures on the same suite bump recurrence on
# one bead rather than filing a fresh one each pass. A change in the suite's output opens
# a new investigation (same dedup policy as suite reds, narrowed to this suite only).
#
# THE BEAD NAMES THE SUITE, NOT THE COVERS FILE. Filing against the covers file sent
# workers to code that never ran; this bead's reproduce line is the suite itself, and the
# body says plainly that the covered file is not at fault.
# --------------------------------------------------------------------------------------
file_setup_fault() {  # file_setup_fault <basename> <rc> <seconds> <output>
    local s="$1" rc="$2" secs="$3" out="$4" id=""
    if [ ! -r "$INC" ]; then
        log "suites: no intake at $INC — $s setup-fault reaches nobody"
        return 1
    fi
    local out_inc rc_inc
    out_inc="$(SPIRA_INCIDENT_TYPE=bug \
          SPIRA_INCIDENT_PRIORITY="$(priority_of "$s")" \
          SPIRA_INCIDENT_ACTOR=suites \
          SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
          SPIRA_INCIDENT_REPO="$SPIRA_HOME_REPO" \
          SPIRA_INCIDENT_REF="setup-fault:$s" \
          SPIRA_INCIDENT_PATH="$HERE/$s" \
          SPIRA_INCIDENT_CAUSE=setup-fault \
          SPIRA_DB="$SPIRA_DB" \
          bash "$INC" file "$s is setup-fault in the timed suite run" - <<PAYLOAD
The timed full run found this suite exiting non-zero with zero assertions reached. The
suite's setup failed before any assertion ran — the code under \`# covers:\` is NOT at
fault, and a worker sent there will find it correct.

  suite            $s
  status           setup-fault (rc=$rc) after ${secs}s
  ran assertions   0
  reproduce        bash spira/$s

Fix: find why the suite's setup fails before its first assertion. Common causes: a fixture
component is missing (repo-map, config file, required binary); a dependency exits during
source; a gate refuses the run before the test body executes. The output below shows
where the suite stopped.

The dedupe ref is setup-fault:$s — repeated failures on the same suite bump recurrence on
this bead rather than filing another. A change in the suite's output opens a new one.

--- output -------------------------------------------------------------------------
$(printf '%s\n' "$out" | tail -c 6000)
PAYLOAD
    )"; rc_inc=$?
    if [ "$rc_inc" -ne 0 ]; then
        log "suites: the intake could not file setup-fault for $s — it stays spooled"
        return 1
    fi
    id="$(printf '%s\n' "$out_inc" | tail -n 1 | tr -d '[:space:]')"
    case "${id:-}" in
        ''|*[!A-Za-z0-9-]*|-*|*-) log "suites: the intake returned no id for $s setup-fault"; return 1 ;;
    esac
    printf '%s' "$id"
}

# --------------------------------------------------------------------------------------
# FILING A SAME-CAUSE CLUSTER. Called after the suite loop when N>=2 confirmed reds share
# the same normalised first FAIL line. Files one bead naming every member suite.
#
# WHY ONE BEAD AND NOT N. N beads for one cause means N workers sent to one problem. The
# worker who claims any one suite reproduces the root; the others find it green and close
# as unreproducible, producing N bead-closings that fix nothing. One bead, one worker,
# one fix. (Same argument as file_fixture_fault, same law: law-count-things-not-log-lines.)
#
# THE REF IS cause-cluster:<cfp>. The cause fingerprint is the normalised first FAIL
# line, stable across passes: the same shared failure next cycle bumps recurrence on this
# bead rather than opening a new one. A change in the first FAIL line is new information.
# --------------------------------------------------------------------------------------
file_cluster() {  # file_cluster <n> <suite-list> <cfp> <repr-suite> <repr-rc> <repr-secs> <repr-out> <prio>
    local n="$1" suites="$2" cfp="$3" repr_suite="$4" repr_rc="$5" repr_secs="$6"
    local repr_out="$7" prio="$8" cov id="" s
    cov="$(suite_covers_of "$HERE/$repr_suite")"
    if [ ! -r "$INC" ]; then
        log "suites: no intake at $INC — cause-cluster of $n ($suites) reaches nobody"
        return 1
    fi
    local reproduce_lines=""
    for s in $suites; do reproduce_lines="${reproduce_lines}  bash spira/${s}"$'\n'; done
    local out_inc rc_inc
    out_inc="$(SPIRA_INCIDENT_TYPE=bug \
          SPIRA_INCIDENT_PRIORITY="$prio" \
          SPIRA_INCIDENT_ACTOR=suites \
          SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
          SPIRA_INCIDENT_REPO="$SPIRA_HOME_REPO" \
          SPIRA_INCIDENT_REF="cause-cluster:$cfp" \
          SPIRA_INCIDENT_PATH="$HERE/$repr_suite" \
          SPIRA_INCIDENT_CAUSE=suite-red \
          SPIRA_DB="$SPIRA_DB" \
          bash "$INC" file "${n} suite(s) failed on the same cause in the timed run" - <<PAYLOAD
The timed full run found ${n} suites failing on the same normalised first FAIL line. A shared
cause warrants one bead and one investigation, not ${n}: the worker who reproduces any of the
suites below reaches the root, and fixing the root fixes all ${n} of them.

  cause fingerprint  $cfp
  suites affected    $n
  covers             ${cov:-NOTHING DECLARED — representative suite has no \`# covers:\` line}

Suite list and reproduce lines:
$(printf '%s' "$reproduce_lines")
The cause fingerprint is over the first FAIL line with scratch paths and numbers normalised
out. An identical shared failure next cycle bumps recurrence on this bead rather than filing
another; a change in the first FAIL line opens a new one.

--- output of first suite in cluster (${repr_suite}, rc=${repr_rc}, ${repr_secs}s) ---
$(printf '%s\n' "$repr_out" | tail -c 6000)
PAYLOAD
    )"; rc_inc=$?
    if [ "$rc_inc" -ne 0 ]; then
        log "suites: the intake could not file cause-cluster ($suites)"
        return 1
    fi
    id="$(printf '%s\n' "$out_inc" | tail -n 1 | tr -d '[:space:]')"
    case "${id:-}" in
        ''|*[!A-Za-z0-9-]*|-*|*-) log "suites: the intake returned no id for cause-cluster"; return 1 ;;
    esac
    printf '%s' "$id"
}

# --------------------------------------------------------------------------------------
# THE PASS.
#
# THE BUDGET IS A WALL, NOT A HOPE. This runs inside an Ops session that systemd kills at
# FAYTH_TIMEOUT_SECONDS, so a pass that overran would be killed mid-suite and would record
# nothing at all — the worst of the outcomes, because a killed pass and a clean one are the
# same silence afterwards. So each suite is given the SMALLER of its own timeout and what is
# left of the budget, and a suite that will not fit is not started.
#
# AND THE CURSOR IS WHAT MAKES THAT SOUND. Stopping at the budget every pass from the same
# starting point would run the first few suites forever and the last few never — the exact
# defect this program exists to end, rebuilt inside it. The cursor names the suite the next
# pass begins with, so the set is covered by rotation over cycles however long it grows.
# --------------------------------------------------------------------------------------
cmd_run() {
    local started deadline s out rc secs fp t0 left slice status id next_cursor="" _td_shared=0 _td_fixture_name=""
    local env_mismatch=0
    # Cluster workspace: confirmed reds are deferred here; after the loop they are grouped
    # by cause_fp and filed as one bead per group (law-count-things-not-log-lines).
    local _cfp_dir="" _deferred_red_count=0 _cluster_suppressed=0
    local _setup_fault_count=0
    started="$(date +%s)"; deadline=$(( started + BUDGET ))
    mkdir -p "$STATE" 2>/dev/null

    if ! GATED="$(gated_suites)"; then
        # NOT A DEGRADED PASS THAT RUNS EVERYTHING. Without the gate's list this cannot know
        # which suites are already covered, and guessing wrong in the generous direction runs
        # the gate's own set a second time inside an eight-minute budget it does not fit in.
        log "suites: $GATE_LIST is unreadable — refusing to guess which suites the gate runs"
        return 1
    fi
    _cfp_dir="$(mktemp -d)" || return 1
    mkdir -p "$_cfp_dir/suite" "$_cfp_dir/cause"
    GATED=" $(echo $GATED) "

    local timed="" undeclared=""
    for s in $(all_suites); do
        is_gated "$s" && continue
        timed="$timed $s"
        [ -n "$(suite_covers_of "$HERE/$s")" ] || undeclared="$undeclared $s"
    done
    timed="$(echo $timed)"
    if [ -z "$timed" ]; then
        log "suites: every suite in the tree is gated — nothing to run on a timer"
        return 0
    fi

    # Rotate so the pass begins where the last one stopped. An unreadable or stale cursor
    # simply starts at the beginning, which is correct rather than merely safe.
    local start_at order=""
    start_at="$(cat "$CURSOR" 2>/dev/null | tr -d '[:space:]')"
    case " $timed " in *" $start_at "*) ;; *) start_at="" ;; esac
    if [ -n "$start_at" ]; then
        local seen=0
        for s in $timed; do [ "$s" = "$start_at" ] && seen=1; [ "$seen" = 1 ] && order="$order $s"; done
        for s in $timed; do [ "$s" = "$start_at" ] && break; order="$order $s"; done
    else
        order="$timed"
    fi

    printf 'timed suite run — %s suite(s) the gate does not run, %ss budget\n' \
        "$(set -- $timed; echo $#)" "$BUDGET"
    # WHAT WAS SKIPPED AND WHY, NAMED. A pass that quietly ran a subset and reported only
    # what it ran is the shape of the defect this program exists to end: the reader cannot
    # tell "the gate has this one" from "this one fell out of the world". Gated suites are
    # skipped because running them again would be paying twice inside an eight-minute budget
    # for a verdict the landing gate already produced on every branch.
    local gated_here=""
    for s in $(all_suites); do is_gated "$s" && gated_here="$gated_here $s"; done
    [ -n "$gated_here" ] && printf 'skipped — the landing gate already runs these on every branch:%s\n' "$gated_here"
    [ -n "$undeclared" ] && printf 'no `# covers:` declaration (run anyway, selection cannot see them):%s\n' "$undeclared"

    # --------------------------------------------------------------------------------------
    # SHARED TESTDB FIXTURE. Build once; suites that call testdb_up get the fast reset
    # path (~6ms via directory swap) rather than each building their own fresh database
    # (~7s). Without this, 56+ non-gated suites each pay the init cost every pass —
    # over 6 minutes of a 7-minute budget, and the last suites in the rotation are
    # cut short or killed. A suite killed mid-run by a tight slice fires its EXIT trap
    # (rm -rf its temp dir), which cascades failures in subsequent assertions.
    #
    # Vars are exported so every setsid'd suite subprocess inherits them. suites.sh's
    # own SPIRA_DB and PATH are restored immediately after the build so incident.sh and
    # bd calls here continue to reach the production store.
    #
    # BORROWERS DO NOT DROP. testdb_drop inside each suite's EXIT trap is a no-op when
    # TESTDB_SHARED=1 — only this shell drops at the end of cmd_run.
    # testdb_reset inside testdb_up clears the fixture to a clean baseline at the top
    # of every suite's testdb_up call, so each suite starts with an empty store.
    #
    # SPIRA_SUITES_SKIP_TESTDB=1 bypasses this block entirely. The embedded check runs
    # bd init in a temp dir to probe availability (~6s per call), and testdb_up runs a
    # second bd init for the fixture itself (~6s). A test harness that calls suites.sh
    # repeatedly with fixture suites that do not use testdb would pay ~12s per call for
    # a fixture nobody borrows. Set this flag in that context to skip the build; fixture
    # suites that need testdb must build their own.
    # --------------------------------------------------------------------------------------
    if [ -z "${SPIRA_SUITES_SKIP_TESTDB:-}" ] && \
       . "$HERE/testdb.sh" 2>/dev/null && testdb_available 2>/dev/null; then
        local _td_real_db="$SPIRA_DB"
        local _td_real_bd="${SPIRA_BD:-}"
        local _td_had_bd; [ -n "${SPIRA_BD+x}" ] && _td_had_bd=1 || _td_had_bd=0
        local _td_real_path="$PATH"
        local _td_real_spath="${SPIRA_PATH:-}"
        local _td_had_spath; [ -n "${SPIRA_PATH+x}" ] && _td_had_spath=1 || _td_had_spath=0
        if testdb_up suites 2>/dev/null; then
            export TESTDB_SHARED=1 TESTDB_NAME TESTDB_DIR TESTDB_BASELINE \
                   TESTDB_BIN TESTDB_MODE TESTDB_BD TESTDB_STARTED_SERVICE
            # Restore production vars — the fixture is for suite subprocesses, not us.
            SPIRA_DB="$_td_real_db"; export SPIRA_DB
            PATH="$_td_real_path"; export PATH
            if [ "$_td_had_bd" = 1 ]; then SPIRA_BD="$_td_real_bd"; export SPIRA_BD
            else unset SPIRA_BD 2>/dev/null || true; fi
            if [ "$_td_had_spath" = 1 ]; then SPIRA_PATH="$_td_real_spath"; export SPIRA_PATH
            else unset SPIRA_PATH 2>/dev/null || true; fi
            # TESTDB_FAULT_EXIT: the exit code testdb_up uses when a shared fixture reset
            # fails inside a borrower suite. Exported so suites inherit it; the value is
            # declared here (next to the shared fixture build) so both ends of the protocol
            # are in the same diff. 75 = EX_TEMPFAIL; it is not automake-skip (77) and not
            # a normal suite failure, so suites.sh can classify it distinctly.
            export TESTDB_FAULT_EXIT=75
            _td_fixture_name="${TESTDB_NAME:-unknown}"
            _td_shared=1
        else
            # testdb_up failed: restore any vars it may have modified before returning.
            # Without this, a failure partway through the fresh-fixture path (after PATH
            # was modified but before SPIRA_DB was set) leaves the production vars in a
            # corrupt state, and incident.sh subsequently targets the wrong database.
            SPIRA_DB="$_td_real_db"; export SPIRA_DB
            PATH="$_td_real_path"; export PATH
            if [ "$_td_had_bd" = 1 ]; then SPIRA_BD="$_td_real_bd"; export SPIRA_BD
            else unset SPIRA_BD 2>/dev/null || true; fi
            if [ "$_td_had_spath" = 1 ]; then SPIRA_PATH="$_td_real_spath"; export SPIRA_PATH
            else unset SPIRA_PATH 2>/dev/null || true; fi
        fi
    fi

    # Strip runner-injected variables from each suite's environment so a suite that is
    # sensitive to them (e.g. SPIRA_SUITES_MAXSEC changing a budget calc) fails the same
    # way in an aeon as it does under the systemd runner.  Mirrors the confirming-run logic
    # that already does this for red suites, but applied to the primary launch so the
    # failure is caught before it matters.
    local _suite_env="" _senv_rv
    for _senv_rv in $RUNNER_VARS; do
        [ -n "${!_senv_rv+x}" ] && _suite_env="$_suite_env -u $_senv_rv"
    done
    unset _senv_rv

    local ran=0 red=0 skipped=0 unreached=""
    local _fixture_fault_count=0 _fixture_faulted=""
    # Instrument: track which suites got results, and write unreached for any that didn't.
    local suites_with_results=""
    for s in $order; do
        left=$(( deadline - $(date +%s) ))
        if [ "$left" -le 5 ]; then
            unreached="$unreached $s"
            [ -n "${next_cursor:-}" ] || next_cursor="$s"
            continue
        fi
        # Skip before starting if the budget would be insufficient; starting anyway produces
        # rc=124 on a killed process, which reads as a test failure rather than a budget
        # constraint.
        #
        # Two thresholds, either of which triggers a skip:
        # 1. last_secs: the suite's last recorded runtime (self-calibrating after each run).
        # 2. declared_to: the suite's `# timeout: N` annotation, which sets both the minimum
        #    budget required to start and the watchdog ceiling for that run. A suite that runs
        #    the real harness lifecycle — aeon.sh, bd calls, git operations — must declare this
        #    explicitly, because its runtime under load can exceed last_secs by enough to
        #    produce a false timeout even when last_secs-based skipping would have allowed it.
        last_secs="$(record_read "$s" | awk '{print $3}' | grep -E '^[0-9]+$' || echo 0)"
        declared_to="$(timeout_of "$s")"
        if [ "$last_secs" -gt 30 ] && [ "$left" -lt "$last_secs" ]; then
            unreached="$unreached $s"
            next_cursor="${next_cursor:-$s}"
            continue
        fi
        if [ -n "$declared_to" ] && [ "$left" -lt "$declared_to" ]; then
            unreached="$unreached $s"
            next_cursor="${next_cursor:-$s}"
            continue
        fi
        slice="${declared_to:-$PER_SUITE}"; [ "$left" -lt "$slice" ] && slice="$left"
        t0="$(date +%s)"
        local tmp suite_pid killer watchdog_flag
        tmp="$(mktemp)" || return 1
        # The watchdog flag: the killer writes it before sending SIGTERM, giving the runner
        # an authoritative record that the watchdog fired regardless of the suite's exit code.
        # A suite that traps TERM runs its own cleanup and exits 1, not 143 — so the rc>=128
        # check alone misses the kill and files a false red (sp-prhs2).
        watchdog_flag="$(mktemp)"; rm -f "$watchdog_flag"
        # PROCESS GROUP ISOLATION. setsid makes the suite the leader of its own process
        # group (PGID = suite_pid), so kill -- -suite_pid reaches every descendant it leaves
        # running. Without this, a suite that hangs before its own cleanup lines keeps
        # orphaned children alive past the harness timeout (sp-a8c5).
        # shellcheck disable=SC2086
        setsid${_suite_env:+ env${_suite_env}} bash "$HERE/$s" > "$tmp" 2>&1 &
        suite_pid=$!
        # Watchdog: send SIGTERM to the whole process group if the suite overruns its slice.
        # KILLER IN ITS OWN PROCESS GROUP so that `kill -- -$killer` sweeps both the bash
        # and the `sleep` child in one shot.  Without setsid the `sleep` orphans in the
        # caller's PGID — the harness detects it as a background job left after exit (sp-pdwve).
        # The flag is written before kill so it is set even if kill returns non-zero.
        setsid bash -c "sleep ${slice} && printf '1' > '$watchdog_flag' && kill -- -${suite_pid} 2>/dev/null" &
        killer=$!
        wait "$suite_pid" 2>/dev/null; rc=$?
        kill -- -"$killer" 2>/dev/null; wait "$killer" 2>/dev/null || true
        # Classify as timeout when the watchdog fired, regardless of exit code.
        # Belt-and-suspenders: also remap rc>=128 (SIGTERM without a trap → 143).
        if [ -f "$watchdog_flag" ] || [ "$rc" -ge 128 ]; then rc=124; fi
        rm -f "$watchdog_flag"
        # SWEEP SURVIVORS. If any process remains in the suite's process group after it
        # exited, the suite has a cleanup defect. Kill them and, if the suite otherwise
        # passed, mark it red so the defect surfaces rather than being silently absorbed.
        # The kernel briefly holds a process group table entry accessible via kill -0 after
        # the last member exits; a 50 ms re-check lets the table settle before we declare a
        # defect.  Real survivors persist well beyond 50 ms; the transient race clears
        # within that window, so the two cases are distinguishable.
        if kill -0 -- -"$suite_pid" 2>/dev/null; then
            sleep 0.05
            if kill -0 -- -"$suite_pid" 2>/dev/null; then
                kill -- -"$suite_pid" 2>/dev/null || true
                if [ "$rc" -eq 0 ]; then
                    printf 'FAIL: %s left background jobs after exit — killed by harness\n' "$s" >> "$tmp"
                    rc=1
                fi
            fi
        fi
        out="$(cat "$tmp")" || true
        rm -f "$tmp"
        secs=$(( $(date +%s) - t0 ))
        ran=$(( ran + 1 ))
        suites_with_results="$suites_with_results $s"
        unreached_clear "$s"
        # FIXTURE FAULT. When the shared fixture collapsed and the suite exited before
        # running a single assertion, classify as fixture-fault rather than red. Only
        # applicable when this pass built a shared fixture (_td_shared=1): a suite that
        # exits 75 for an unrelated reason with no shared fixture in play is still red.
        if [ "$rc" -eq "${TESTDB_FAULT_EXIT:-75}" ] && [ "$_td_shared" = 1 ]; then
            record_write "$s" fixture-fault "$secs" -
            _fixture_fault_count=$(( _fixture_fault_count + 1 ))
            _fixture_faulted="${_fixture_faulted:+$_fixture_faulted }$s"
            printf '  %-26s FIXTURE-FAULT  shared fixture collapsed\n' "$s"
            continue
        fi
        # SILENT-FAILURE GUARD. A suite that exits non-zero with no combined output has
        # redirected its failure stream away from stdout/stderr. Without this guard the
        # filed bead carries no evidence and the failure is undiagnosable: the output
        # section in the bead body would be empty, with nothing to reason from. Append a
        # sentinel so the body is never empty on a failure path. Skip rc=77 (intentional
        # skip, not a failure) and rc=0 (pass). rc=124 (watchdog timeout) may also produce
        # empty output when a suite is killed before printing anything, and the diagnostic
        # is equally useful there.
        if [ -z "$out" ] && [ "$rc" -ne 0 ] && [ "$rc" -ne 77 ]; then
            out="[no output — suite exited rc=$rc with nothing on stdout/stderr; check for exec redirects discarding output]"
        fi
        case "$rc" in
            0)  status=ok
                record_write "$s" ok "$secs" -
                printf '  %-26s ok       %ss\n' "$s" "$secs" ;;
            # 77 is the automake convention and the one this tree already uses for "the box
            # cannot host this check". It is not a pass and it is not a failure: recorded as
            # its own status so `status` can report it, and never filed, because a bead
            # saying "your box has no Dolt server" is not work anybody can do.
            77) status=skip
                record_write "$s" skip "$secs" -
                skipped=$(( skipped + 1 ))
                printf '  %-26s SKIPPED  %s\n' "$s" \
                    "$(printf '%s' "$out" | sed -n 's/.*SKIP *//p' | head -1)" ;;
            124) status=timeout
                # Fingerprint over the suite name, not the slice. The slice is a property of
                # the budget (it varies when left < PER_SUITE), not a property of the failure —
                # fingerprinting it filed a new bead per pass for the same timing-out suite (sp-u1g).
                fp="$(fingerprint "$rc" "timeout:$s")"
                record_write "$s" timeout "$secs" "$fp"
                red=$(( red + 1 ))
                # A truncated slice timeout is a budget artefact: the suite was started with
                # less than its full allotment and killed for it. Use the default priority rather
                # than the suite's declared priority — a suite that declares P0 for genuine reds
                # must not manufacture P0s when it only timed out because the budget was short.
                local _full_to="${declared_to:-$PER_SUITE}"
                local _to_prio; _to_prio="$(priority_of "$s")"
                [ "$slice" -lt "$_full_to" ] && _to_prio="$PRIORITY"
                id="$(file_red "$s" timeout "$rc" "$secs" "$fp" "$out" "$_to_prio" || true)"
                printf '  %-26s TIMEOUT  killed at %ss  %s\n' "$s" "$slice" "${id:-not filed}" ;;
            *)  status=red
                fp="$(fingerprint "$rc" "$out")"
                # SETUP-FAULT GUARD. A suite that emitted ASSERTIONS 0 ran no assertions
                # before exiting — its setup failed. The code under # covers: is not the
                # problem; a worker sent there will find it correct. The discriminant is the
                # ASSERTIONS trailer (present and zero), never the exit code alone. A missing
                # trailer is treated as red (as today) so that un-migrated suites cannot
                # silently become setup-faults — the same rule suites.sh already applies to an
                # unreadable gate-suites list: absence never reads as all-clear.
                if grep -qxF 'ASSERTIONS 0' <<< "$out" 2>/dev/null; then
                    record_write "$s" setup-fault "$secs" -
                    _setup_fault_count=$(( _setup_fault_count + 1 ))
                    id="$(file_setup_fault "$s" "$rc" "$secs" "$out" || true)"
                    printf '  %-26s SETUP-FAULT  ran no assertions (rc=%s)  %s\n' "$s" "$rc" "${id:-not filed}"
                    continue
                fi
                red=$(( red + 1 ))
                # CONFIRMING RUN. Before filing, re-run the suite without runner-injected
                # variables — the environment an aeon's reproduce line runs in. If the suite
                # passes there, the red is an environment artefact, not a suite defect; an
                # aeon that claims the resulting bead finds it green and closes it truthfully,
                # committing nothing, while the next pass re-files it unchanged. Filing an
                # environment finding instead stops that loop without sending an aeon on a
                # fruitless reproduction. If budget is exhausted, file nothing — an unconfirmed
                # red is the current behaviour and it is the defect (sp-ezs7o).
                left=$(( deadline - $(date +%s) ))
                local _rv _rv_set confirm_env confirm_differing _cfp
                confirm_env="env"; confirm_differing=""
                for _rv in $RUNNER_VARS; do
                    _rv_set="${!_rv+x}"
                    if [ -n "$_rv_set" ]; then
                        confirm_env="$confirm_env -u $_rv"
                        confirm_differing="${confirm_differing:+$confirm_differing }$_rv"
                    fi
                done
                if [ -z "$confirm_differing" ]; then
                    # No runner variables are set — defer for post-loop cause clustering.
                    record_write "$s" red "$secs" "$fp"
                    _cfp="$(cause_fp "$rc" "$out")"
                    printf '%s' "$rc"   > "$_cfp_dir/suite/$s.rc"
                    printf '%s' "$secs" > "$_cfp_dir/suite/$s.secs"
                    printf '%s' "$fp"   > "$_cfp_dir/suite/$s.fp"
                    printf '%s' "$(priority_of "$s")" > "$_cfp_dir/suite/$s.prio"
                    printf '%s\n' "$out" > "$_cfp_dir/suite/$s.out"
                    printf '%s\n' "$s" >> "$_cfp_dir/cause/$_cfp"
                    _deferred_red_count=$(( _deferred_red_count + 1 ))
                    printf '  %-26s RED      rc=%s after %ss  (pending cluster)\n' "$s" "$rc" "$secs"
                elif [ "$left" -le 5 ]; then
                    # Budget exhausted: cannot confirm. Record the result but do not file.
                    # An unconfirmed red that reaches a bead an aeon cannot reproduce is the
                    # defect this mechanism exists to prevent.
                    record_write "$s" red-unconfirmed "$secs" "$fp"
                    printf '  %-26s RED-UNCONFIRMED rc=%s after %ss (no budget for confirming run)\n' \
                        "$s" "$rc" "$secs"
                else
                    local confirm_tmp confirm_rc confirm_out confirm_slice
                    confirm_slice="$PER_SUITE"; [ "$left" -lt "$confirm_slice" ] && confirm_slice="$left"
                    confirm_tmp="$(mktemp)"
                    # SYNCHRONOUS: `timeout` handles killing cleanly without background jobs
                    # that could interfere with the main loop's watchdog logic. Runs in the
                    # same process group as suites.sh, which is safe — the main loop's
                    # watchdog targets the SUITE's process group (PGID = suite_pid), not ours.
                    timeout "$confirm_slice" $confirm_env bash "$HERE/$s" > "$confirm_tmp" 2>&1
                    confirm_rc=$?
                    # Treat a timed-out confirming run as confirmed red: we cannot assert green.
                    [ "$confirm_rc" -eq 124 ] && confirm_rc=1
                    confirm_out="$(cat "$confirm_tmp")"; rm -f "$confirm_tmp"
                    if [ "$confirm_rc" -eq 0 ]; then
                        # Green in aeon's environment: env mismatch, not a suite defect.
                        record_write "$s" red "$secs" "$fp"
                        env_mismatch=$(( env_mismatch + 1 ))
                        id="$(file_env_red "$s" "$rc" "$secs" "$fp" "$out" "$confirm_differing" || true)"
                        printf '  %-26s ENV-MISMATCH rc=%s after %ss  vars: %s  %s\n' \
                            "$s" "$rc" "$secs" "$confirm_differing" "${id:-not filed}"
                    else
                        # Confirmed red in aeon's environment too — defer for clustering.
                        record_write "$s" red "$secs" "$fp"
                        _cfp="$(cause_fp "$rc" "$out")"
                        printf '%s' "$rc"   > "$_cfp_dir/suite/$s.rc"
                        printf '%s' "$secs" > "$_cfp_dir/suite/$s.secs"
                        printf '%s' "$fp"   > "$_cfp_dir/suite/$s.fp"
                        printf '%s' "$(priority_of "$s")" > "$_cfp_dir/suite/$s.prio"
                        printf '%s\n' "$out" > "$_cfp_dir/suite/$s.out"
                        printf '%s\n' "$s" >> "$_cfp_dir/cause/$_cfp"
                        _deferred_red_count=$(( _deferred_red_count + 1 ))
                        printf '  %-26s RED      rc=%s after %ss  (pending cluster)\n' "$s" "$rc" "$secs"
                    fi
                fi ;;
        esac
    done

    # Drop the shared fixture this shell owns; borrowers (suites) already no-op'd their drop.
    [ "$_td_shared" = 1 ] && { TESTDB_SHARED=0 testdb_drop 2>/dev/null || true; }

    # INSTRUMENT THE PASS: write an unreached record for every suite in $timed that did not
    # get a result file written. This makes visible which suites were skipped due to budget,
    # so the invariant "every suite in the tree is in exactly one column (gated or timed with
    # a result)" can be verified.
    suites_with_results=" $suites_with_results "
    for s in $timed; do
        case "$suites_with_results" in
            *" $s "*) ;; # Already ran this pass — unreached_clear was called during the run
            # Suite was not reached this pass. Write the .unreached flag without touching
            # .result: the previous verdict and runtime are preserved for the red count and
            # for the budget skip guard's last_secs read.
            *) unreached_write "$s" ;;
        esac
    done

    # The next pass starts at the first suite this one could not reach, or at the beginning
    # when it reached them all.
    mkdir -p "$STATE" 2>/dev/null
    printf '%s\n' "${next_cursor:-}" > "$CURSOR" 2>/dev/null || true

    if [ -n "$unreached" ]; then
        printf 'budget spent after %ss — not reached this pass, and first next pass:%s\n' \
            "$(( $(date +%s) - started ))" "$unreached"
    fi
    # FILE ONE BEAD FOR THE FIXTURE COLLAPSE. Every suite that exited with TESTDB_FAULT_EXIT
    # is a borrower that could not start — not a red suite. One bead names them all. This
    # runs here rather than inside the loop so there is exactly one bead per collapse rather
    # than one per affected suite; the dedupe ref (fixture-fault:<TESTDB_NAME>) is the second
    # guard against duplicates across passes.
    if [ "$_fixture_fault_count" -gt 0 ]; then
        local _ff_id
        _ff_id="$(file_fixture_fault "$_fixture_fault_count" "$_fixture_faulted" "${_td_fixture_name:-unknown}" || true)"
        printf 'fixture fault — %s suite(s) could not start (shared fixture collapsed): %s  %s\n' \
            "$_fixture_fault_count" "$_fixture_faulted" "${_ff_id:-not filed}"
    fi
    # CLUSTER CONFIRMED REDS AND FILE. Every confirmed red deferred above is grouped by its
    # cause_fp (normalised first FAIL line). Any group of two or more files one bead naming
    # all member suites; a singleton files as a normal red bead, as before. The suppression
    # count measures how many beads the clustering saved (law-dedup-must-be-measured).
    local _cfp_f _cfp_val _cfp_suites _cfp_n _best_prio _cs _cp
    local _repr _repr_rc _repr_secs _repr_fp _repr_out _suite_list
    for _cfp_f in "$_cfp_dir/cause/"*; do
        [ -f "$_cfp_f" ] || continue
        _cfp_val="$(basename "$_cfp_f")"
        _cfp_suites="$(cat "$_cfp_f")"
        _cfp_n="$(printf '%s\n' "$_cfp_suites" | grep -c . || true)"
        # Best priority: lowest number (most severe) across all members in this cluster.
        _best_prio="$PRIORITY"
        for _cs in $_cfp_suites; do
            _cp="$(priority_of "$_cs")"
            { [ "$_cp" -lt "$_best_prio" ] 2>/dev/null && _best_prio="$_cp"; } || true
        done
        _repr="$(printf '%s\n' "$_cfp_suites" | head -1)"
        _repr_rc="$(cat "$_cfp_dir/suite/$_repr.rc" 2>/dev/null || echo 1)"
        _repr_secs="$(cat "$_cfp_dir/suite/$_repr.secs" 2>/dev/null || echo 0)"
        _repr_fp="$(cat "$_cfp_dir/suite/$_repr.fp" 2>/dev/null || echo -)"
        _repr_out="$(cat "$_cfp_dir/suite/$_repr.out" 2>/dev/null || true)"
        if [ "$_cfp_n" -le 1 ]; then
            # Singleton: file as a normal individual red bead, same as before.
            id="$(file_red "$_repr" red "$_repr_rc" "$_repr_secs" "$_repr_fp" "$_repr_out" || true)"
            printf '  %-26s RED      rc=%s after %ss  %s\n' \
                "$_repr" "$_repr_rc" "$_repr_secs" "${id:-not filed}"
        else
            # Cluster of N>=2: one bead names every member suite.
            _cluster_suppressed=$(( _cluster_suppressed + _cfp_n - 1 ))
            _suite_list="$(printf '%s\n' "$_cfp_suites" | tr '\n' ' ' | sed 's/ $//')"
            id="$(file_cluster "$_cfp_n" "$_suite_list" "$_cfp_val" \
                "$_repr" "$_repr_rc" "$_repr_secs" "$_repr_out" "$_best_prio" || true)"
            printf 'cause-cluster (%s suites: %s)  %s\n' \
                "$_cfp_n" "$_suite_list" "${id:-not filed}"
        fi
    done
    rm -rf "$_cfp_dir" 2>/dev/null || true
    # THE ENV-MISMATCH COUNT IS EMITTED HERE. A nonzero count is the meter that tracks how bad
    # the runner-environment divergence is. When this reaches zero and stays there, a full
    # environment scrub (runner using env -i) becomes landable as a deliberate act rather than
    # a hopeful one. Print it even when zero so the line is parseable on every pass.
    printf '%s ran, %s red (%s env-mismatch, %s suppressed by clustering), %s skipped, %s fixture-fault, %s setup-fault, %ss\n' \
        "$ran" "$red" "$env_mismatch" "$_cluster_suppressed" "$skipped" "$_fixture_fault_count" "$_setup_fault_count" "$(( $(date +%s) - started ))"
    # Exit 2 when suites are red, a fixture fault, or a setup-fault was filed: incidents were
    # filed, the pass completed normally. Exit 1 is reserved for errors that abort before any
    # suite runs (gate-suites unreadable). The unit carries SuccessExitStatus=2 so systemd
    # does not mark it failed on a routine red day.
    [ "$red" -eq 0 ] && [ "$_fixture_fault_count" -eq 0 ] && [ "$_setup_fault_count" -eq 0 ] || return 2
}

# --------------------------------------------------------------------------------------
# WHAT RUNS WHERE, AND WHAT IT LAST SAID. This is the answer to the question nobody could
# ask before: which suites in this tree are executed by nothing.
# --------------------------------------------------------------------------------------
cmd_list() {
    local s where rec st at secs age reach gated_ok=1
    GATED="$(gated_suites)" || gated_ok=0
    GATED=" $(echo ${GATED:-}) "
    printf '%-26s %-7s %-9s %-8s %-7s %s\n' SUITE RUNS LAST AGE REACH COVERS
    for s in $(all_suites); do
        if [ "$gated_ok" = 0 ]; then where='?'
        elif is_gated "$s"; then where=gate
        else where=timed; fi
        rec="$(record_read "$s" || true)"
        if [ -n "$rec" ]; then
            read -r st at secs _ <<< "$rec"
            age="$(( ( $(date +%s) - at ) / 60 ))m"
        else
            # NEVER RUN AND RUN-BUT-UNRECORDED ARE THE SAME THING HERE, and both print `-`
            # rather than a status: this file is the only evidence either way, and inventing
            # a friendlier word for "no evidence" is how the original defect read as fine.
            st=-; age=-
        fi
        if [ "$where" = gate ]; then
            st=-; age=-; reach=-
        elif [ -f "$STATE/$s.unreached" ]; then
            # Not reached in the most recent pass that scheduled this suite. The previous
            # verdict is preserved in .result; show it alongside the "not reached" indicator
            # so both facts are visible and "red, not reached" cannot render as green.
            reach="—"
        elif [ -n "$rec" ]; then
            reach=ok
        else
            reach=-
        fi
        printf '%-26s %-7s %-9s %-8s %-7s %s\n' "$s" "$where" "$st" "$age" "$reach" \
            "$(suite_covers_of "$HERE/$s")"
    done
    [ "$gated_ok" = 1 ] || printf '\n%s is unreadable — which suites the gate runs is unknown\n' "$GATE_LIST"
}

# --------------------------------------------------------------------------------------
# THE BLOCK THE SWEEP CARRIES. Cheap by construction: a glob, a read per suite, no database
# and no subprocess that can hang, because the watchtower embeds it and the watchtower may
# not be another thing that is down during an outage.
#
# EVERY FIELD RENDERS `?` WHEN IT COULD NOT BE READ, never 0.
# --------------------------------------------------------------------------------------
cmd_status() {
    local s rec st at total=0 gate_n=0 timed_n=0 never=0 stale=0 red=0 skip=0 setup_fault=0 not_reached=0 oldest="" oldest_s="" now
    now="$(date +%s)"
    if ! GATED="$(gated_suites)"; then
        printf 'suites          ?   %s is unreadable — the gated set is unknown\n' "$GATE_LIST"
        return 0
    fi
    GATED=" $(echo $GATED) "
    for s in $(all_suites); do
        total=$(( total + 1 ))
        if is_gated "$s"; then gate_n=$(( gate_n + 1 )); continue; fi
        timed_n=$(( timed_n + 1 ))
        rec="$(record_read "$s" || true)"
        if [ -z "$rec" ]; then never=$(( never + 1 )); fi
        # A suite's last verdict survives an unreached pass — count red regardless of reach.
        if [ -n "$rec" ]; then
            read -r st at _ _ <<< "$rec"
            case "$st" in red|timeout|red-unconfirmed) red=$(( red + 1 )) ;; skip) skip=$(( skip + 1 )) ;; setup-fault|fixture-fault) setup_fault=$(( setup_fault + 1 )) ;; esac
            if [ "$(( now - at ))" -gt "$STALE" ]; then stale=$(( stale + 1 )); fi
            if [ -z "$oldest" ] || [ "$at" -lt "$oldest" ]; then oldest="$at"; oldest_s="$s"; fi
        fi
        # The .unreached file is distinct from a missing record (law-absence-needs-a-positive-control).
        [ -f "$STATE/$s.unreached" ] && not_reached=$(( not_reached + 1 ))
    done
    # Count suites without a # host-reason: or container calls — migration progress.
    # hermetic.sh --count-undeclared does the walk; ? when it is unreadable or the glob
    # matches nothing (indistinguishable from a wrong path), never a silent 0.
    local undeclared="?" copying="?"
    if [ -x "$HERE/hermetic.sh" ]; then
        undeclared="$(bash "$HERE/hermetic.sh" --count-undeclared 2>/dev/null)" || undeclared="?"
        [ -n "$undeclared" ] || undeclared="?"
        # Wave-2 migration backlog: suites still copying harness files or creating inline stubs.
        # Falls monotonically as sp-841s child beads close; ? if unreadable.
        copying="$(bash "$HERE/hermetic.sh" --count-copying 2>/dev/null)" || copying="?"
        [ -n "$copying" ] || copying="?"
    fi
    printf '  %-36s%s\n' "suites in the tree" "$total   ($gate_n gated, $timed_n timed)"
    printf '  %-36s%s\n' "host suites without # host-reason:" "$undeclared"
    printf '  %-36s%s\n' "suites still copying/stubbing (wave 2)" "$copying"
    printf '  %-36s%s\n' "timed suites with no result yet" "$never"
    printf '  %-36s%s\n' "timed results older than $(( STALE / 3600 ))h" "$stale"
    printf '  %-36s%s\n' "timed suites red at last run" "$red"
    printf '  %-36s%s\n' "timed suites setup/fixture fault at last run" "$setup_fault"
    printf '  %-36s%s\n' "timed suites skipped at last run" "$skip"
    printf '  %-36s%s\n' "timed suites not reached last pass" "$not_reached"
    if [ -n "$oldest" ]; then
        printf '  %-36s%sm   %s\n' "oldest timed result" "$(( (now - oldest) / 60 ))" "$oldest_s"
    else
        printf '  %-36s%s\n' "oldest timed result" "?   (nothing has run)"
    fi
}

case "${1:-list}" in
    run)    cmd_run ;;
    list)   cmd_list ;;
    status) cmd_status ;;
    *) printf 'usage: suites.sh [list|run|status]\n' >&2; exit 2 ;;
esac
