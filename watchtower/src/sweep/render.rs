//! The sweep's report — one field per pipeline vital sign, in the bash's own section order.
//! Every number here was gathered in `collect.rs`; this function is pure text assembly so
//! it can be unit-tested against a hand-built `SweepData` fixture, the same reason the bash
//! hoisted its own renderers above `main()`.

use super::collect::{Drain, SweepData};
use crate::log::now_iso;

fn secs(v: &str) -> String {
    if v == "?" {
        "?".to_string()
    } else {
        format!("{v}s")
    }
}

fn halt_section(d: &SweepData) -> String {
    match &d.halt {
        None => String::new(),
        Some(h) => {
            let mut s = format!("!! HALTED since {}", h.since);
            if let Some(why) = &h.why {
                s.push_str(&format!("\n   why: {why}"));
            }
            s.push_str("\n   No incidents are filed while the halt is in force.\n");
            s
        }
    }
}

fn drain_section(d: &SweepData) -> String {
    match &d.drain {
        Drain::NotDraining => String::new(),
        Drain::Draining { since, mins } => {
            let mins_disp = mins.map(|m| m.to_string()).unwrap_or_else(|| "?".into());
            format!("!! DRAINING since {since} ({mins_disp}m)\n   Summons gated; loop, landing and reaping continue. Lift with: world.sh resume\n")
        }
    }
}

fn throttle_section(d: &SweepData) -> String {
    if let Some(since) = &d.throttle.since {
        format!(
            "!! THROTTLED since {since}\n   Builder admission held (depth={}, drain was {} ago). Lifts automatically.\n   Override: SPIRA_QUEUE_THROTTLE_OVERRIDE=off in spira.conf\n",
            d.throttle.depth.as_deref().unwrap_or("?"),
            d.throttle.drain.as_deref().unwrap_or("?")
        )
    } else if d.throttle.override_off {
        "   throttle: OVERRIDE OFF (SPIRA_QUEUE_THROTTLE_OVERRIDE=off)\n".to_string()
    } else {
        String::new()
    }
}

pub fn is_nominal(d: &SweepData, snap_stale_s: i64) -> bool {
    let lapsed_zero = matches!(d.lapsed.count, crate::lapsed::Count::Known(0));
    lapsed_zero
        && d.snap_age.map(|a| a < snap_stale_s).unwrap_or(false)
        && d.nv_worst == 0
        && matches!(d.drain, Drain::NotDraining)
        && d.throttle.since.is_none()
        && !d.disk.disk_breach
        && !d.disk.mem_breach
        && d.failed_units.as_ref().map(|v| v.is_empty()).unwrap_or(false)
}

pub fn render(d: &SweepData) -> String {
    let nv_worst_key = d.nv_worst_key.as_deref().unwrap_or("none");
    let last_land_disp = d.last_land_id.as_deref().unwrap_or("none recorded");
    let throttle_since_disp = d.throttle.since.as_deref().unwrap_or("clear");
    let throttle_depth_disp = d.throttle.depth.as_deref().unwrap_or("\u{2014}");
    let drain_mins_disp = match &d.drain {
        Drain::NotDraining => "0".to_string(),
        Drain::Draining { mins, .. } => mins.map(|m| m.to_string()).unwrap_or_else(|| "?".into()),
    };

    format!(
        r#"## Spira pipeline, {now}
{halt}{drain}{throttle}
N workers pull from a DAG into a merge queue. These are that queue's vital signs. A field
reading `?` is one this pass COULD NOT READ — never treat it as a zero.

### The far end — is anything coming out?

  minutes since the last landing      {since_land}      (last: {last_land})
  branches finished but not landed    {sp_unlanded}
  branches done and waiting           {sp_branch_done}
  {gate_wait_label:<36}{gate_wait_disp}   {oldest_br}
  worst no-verdict streak             {nv_worst}       {nv_worst_key}

### The Sending — are finished branches leaving?

  SP_UNSENT is the total; the two rows below break it into work in flight (open bead,
  may still land) vs stranded (closed bead, sending.sh has not reaped it yet).
  A no-bead branch splits into two kinds: one whose commits are already on the base
  (SP_UNADOPTED — safe to delete) and one whose commits are absent (SP_ORPHAN_WORK —
  unlanded work; deletion would destroy commits). Only SP_UNADOPTED triggers the reap
  escalation.

  unsent branches (total)             {sp_unsent}
    in-flight (open bead)             {unsent_inflight}
    stranded (closed bead)            {sp_closed_stranded}
  oldest in-flight (hours)            {sp_unsent_oldest_h}
  oldest stranded (hours)             {sp_closed_stranded_oldest_h}
  BATCHED with no open batch          {sp_batched_stranded}
  BATCHED longer than one batch pass  {sp_batched_too_long}
  strays (no bead, commits on base)   {sp_unadopted}
  orphan work (no bead, has commits)  {sp_orphan_work}
  fiends (FAILED deletes, came back)  {sp_sent_failed}

### The gate — is it buying anything?

  UNKNOWN is never folded into either column. A run of them means this measurement has
  itself stopped working, which is the one thing a yield figure must not hide.

  {gate_reds_label:<36}{yield_reds}{yield_note}
  {defect_label:<36}{yield_defect}      ({yield_defect_inferred} inferred from a later pass, not stated)
  {fault_label:<36}{yield_fault}      worst: {yield_top_fault}
  {unk_label:<36}{yield_unknown}
  {solo_label:<36}{solo_med} median, {solo_max} worst (n={solo_n})
  {conc_label:<36}{conc_med} median, {conc_max} worst (n={conc_n})

### The workers

  /tmp used (? = cannot read)         {tmp_pct}
  / used (? = cannot read)            {disk_disp}
  memory available (? = cannot read)  {mem_disp}
  throttle                            {throttle_since}      (stamp: queue-throttled; depth at engage: {throttle_depth})
  draining since (? = cannot read)    {drain_mins}      minutes   (stamp: world.draining)
  aeons alive                         {aeons_live}      (counted now, not from the snapshot)
  FAILED UNITS                        {sp_failed_units}      {fu_names}   (>{fu_warn_mins}m failing escalates; ? = systemctl unreachable)
  beads in progress                   {sp_inprog}
  ready to claim                      {sp_ready}
  poisoned                            {sp_poison}
  stranded (claimed, nobody home)     {sp_strand_ghost}
  strand ledger, other classes        {sp_strand_other}
  account capacity paused             {sp_capacity_paused}

### The czar — trigger outcome

  A `?` means this probe could not read the store. A `no` means the condition
  returned after the czar closed its bead — file an investigation bead.

{czar_block}

### Lapsed aeons — killed by the liveness lease since the previous sweep

  An aeon whose trace was silent for the full lease is killed and its work preserved.
  Classify each into one of four outcomes (sop-lapsed-aeon-postmortem) — a lapse is
  never closed as "noted". ? = directory could not be read, not zero lapses.

{lapsed_section}

### The graph

  open {sp_open} {sp_mid} closed {sp_closed} {sp_mid} landed {sp_landed} {sp_mid} {ask_label} {sp_needsop}
  repo: unmapped {sp_repo_unmapped} {sp_mid} absent {sp_repo_absent}
  parked on CI {sp_awaiting_n}, oldest {sp_awaiting_age}, stuck {sp_awaiting_stuck}
  duplicate incident refs             {sp_dup_refs}      (surplus beads: {sp_dup_beads})

### The menu — run these scans, then look for what they do not cover

A sweep is not only a set of numbers to read. These are the scans that are worth sampling
before anything else, because each answers a question the numbers above cannot.

{suites_block}

### The shared checkout — are base branches clean?

  An aeon commit on a base branch bypasses the gate and every landing instrument. A checkout
  ahead of its remote means subsequent worktrees base on a ref nobody else has seen.

{guard_block}

### Can this snapshot be believed?

  collector snapshot age              {snap_age_disp}
  sentinel timer                      {sp_sentinel_timer}   last pass {sp_sentinel_age}s ago

## Your task

Read the vital signs above and decide whether anything needs attention.

If the pipeline is nominal: print one line saying so and exit. No bead is needed. Silence is
not acceptable — a session that found nothing must still say so, because silence and a greeting
are indistinguishable in the log.

If something needs attention: file one bead per finding with its evidence. Name what is wrong,
the number that was anomalous, and what it means. A '?' field means this sweep could not read
it — investigate why before filing a bead on the absence alone.

Run the scans named in the menu above if you have wall time remaining.
"#,
        sp_mid = '\u{b7}',
        now = now_iso(),
        halt = halt_section(d),
        drain = drain_section(d),
        throttle = throttle_section(d),
        since_land = d.since_land_disp,
        last_land = last_land_disp,
        sp_unlanded = d.env.g("SP_UNLANDED_N"),
        sp_branch_done = d.env.g("SP_BRANCH_DONE"),
        gate_wait_label = format!("longest gate wait, {}", d.gate_win_label),
        gate_wait_disp = gate_wait_disp(d),
        oldest_br = d.gate_wait.oldest_branch,
        nv_worst = d.nv_worst,
        nv_worst_key = nv_worst_key,
        sp_unsent = d.env.g("SP_UNSENT"),
        unsent_inflight = d.unsent_inflight_disp,
        sp_closed_stranded = d.env.g("SP_CLOSED_STRANDED"),
        sp_unsent_oldest_h = d.env.g("SP_UNSENT_OLDEST_H"),
        sp_closed_stranded_oldest_h = d.env.g("SP_CLOSED_STRANDED_OLDEST_H"),
        sp_batched_stranded = d.env.g("SP_BATCHED_STRANDED"),
        sp_batched_too_long = d.env.g("SP_BATCHED_TOO_LONG"),
        sp_unadopted = d.env.g("SP_UNADOPTED"),
        sp_orphan_work = d.env.g("SP_ORPHAN_WORK"),
        sp_sent_failed = d.env.g("SP_SENT_FAILED"),
        gate_reds_label = format!("gate reds, {}", d.yield_win_label),
        yield_reds = d.env.g("YIELD_REDS"),
        yield_note = d.yield_note,
        defect_label = "  the branch really was wrong".to_string(),
        yield_defect = d.env.g("YIELD_DEFECT"),
        yield_defect_inferred = d.env.g("YIELD_DEFECT_INFERRED"),
        fault_label = "  the gate's own fault".to_string(),
        yield_fault = d.env.g("YIELD_FAULT"),
        yield_top_fault = d.env.g("YIELD_TOP_FAULT"),
        unk_label = "  never classified".to_string(),
        yield_unknown = d.env.g("YIELD_UNKNOWN"),
        solo_label = "gate cost, solo".to_string(),
        solo_med = secs(d.env.g("YIELD_SOLO_MED")),
        solo_max = secs(d.env.g("YIELD_SOLO_MAX")),
        solo_n = d.env.g("YIELD_SOLO_N"),
        conc_label = "gate cost, another gate overlapping".to_string(),
        conc_med = secs(d.env.g("YIELD_CONC_MED")),
        conc_max = secs(d.env.g("YIELD_CONC_MAX")),
        conc_n = d.env.g("YIELD_CONC_N"),
        tmp_pct = d.tmp_pct,
        disk_disp = d.disk.disk_disp,
        mem_disp = d.disk.mem_disp,
        throttle_since = throttle_since_disp,
        throttle_depth = throttle_depth_disp,
        drain_mins = drain_mins_disp,
        aeons_live = d.aeons_live_disp,
        sp_failed_units = failed_units_disp(d),
        fu_names = d.failed_unit_names,
        fu_warn_mins = d.failed_units_warn_mins,
        sp_inprog = d.env.g("SP_INPROG"),
        sp_ready = d.env.g("SP_READY"),
        sp_poison = d.env.g("SP_POISON"),
        sp_strand_ghost = d.env.g("SP_STRAND_GHOST"),
        sp_strand_other = d.env.g("SP_STRAND_OTHER"),
        sp_capacity_paused = d.env.g("SP_CAPACITY_PAUSED"),
        czar_block = d.czar_block.trim_end(),
        lapsed_section = d.lapsed.section,
        sp_open = d.env.g("SP_OPEN"),
        sp_closed = d.env.g("SP_CLOSED"),
        sp_landed = d.env.g("SP_LANDED"),
        ask_label = d.ask_label,
        sp_needsop = d.env.g("SP_NEEDSOP"),
        sp_repo_unmapped = d.env.g("SP_REPO_UNMAPPED"),
        sp_repo_absent = d.env.g("SP_REPO_ABSENT"),
        sp_awaiting_n = d.env.g("SP_AWAITING_N"),
        sp_awaiting_age = d.env.g("SP_AWAITING_AGE"),
        sp_awaiting_stuck = d.env.g("SP_AWAITING_STUCK"),
        sp_dup_refs = d.env.g("SP_DUP_REFS"),
        sp_dup_beads = d.env.g("SP_DUP_BEADS"),
        suites_block = d.suites_block.trim_end(),
        guard_block = d.guard_block.trim_end(),
        snap_age_disp = d.snap_age_disp,
        sp_sentinel_timer = d.env.g("SP_SENTINEL_TIMER"),
        sp_sentinel_age = d.env.g("SP_SENTINEL_AGE"),
    )
}

fn gate_wait_disp(d: &SweepData) -> String {
    crate::gate_wait::disp(d.gate_wait.oldest_wait)
}

fn failed_units_disp(d: &SweepData) -> String {
    match &d.failed_units {
        Some(v) => v.len().to_string(),
        None => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_green_fixture_is_nominal() {
        let d = SweepData::fixture_nominal(1_700_000_000);
        assert!(is_nominal(&d, 60));
    }

    #[test]
    fn a_lapse_since_the_last_sweep_makes_it_not_nominal() {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.lapsed.count = crate::lapsed::Count::Known(1);
        assert!(!is_nominal(&d, 60));
    }

    #[test]
    fn a_stale_snapshot_makes_it_not_nominal() {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.snap_age = Some(120);
        assert!(!is_nominal(&d, 60));
    }

    #[test]
    fn snapshot_age_is_judged_against_the_given_threshold() {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.snap_age = Some(20);
        assert!(!is_nominal(&d, 7));
        d.snap_age = Some(0);
        assert!(is_nominal(&d, 7));
    }

    #[test]
    fn an_unreadable_snapshot_age_makes_it_not_nominal() {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.snap_age = None;
        assert!(!is_nominal(&d, 60));
    }

    #[test]
    fn a_nonzero_no_verdict_streak_makes_it_not_nominal() {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.nv_worst = 3;
        assert!(!is_nominal(&d, 60));
    }

    #[test]
    fn draining_throttled_disk_and_mem_breach_and_failed_units_each_make_it_not_nominal() {
        let base = SweepData::fixture_nominal(1_700_000_000);

        let mut d = SweepData::fixture_nominal(base.now);
        d.drain = Drain::Draining { since: "x".into(), mins: Some(1) };
        assert!(!is_nominal(&d, 60));

        let mut d = SweepData::fixture_nominal(base.now);
        d.throttle.since = Some("x".into());
        assert!(!is_nominal(&d, 60));

        let mut d = SweepData::fixture_nominal(base.now);
        d.disk.disk_breach = true;
        assert!(!is_nominal(&d, 60));

        let mut d = SweepData::fixture_nominal(base.now);
        d.disk.mem_breach = true;
        assert!(!is_nominal(&d, 60));

        let mut d = SweepData::fixture_nominal(base.now);
        d.failed_units = Some(vec!["spira-x.service".into()]);
        assert!(!is_nominal(&d, 60));

        let mut d = SweepData::fixture_nominal(base.now);
        d.failed_units = None;
        assert!(!is_nominal(&d, 60));
    }

    #[test]
    fn render_includes_every_section_header_and_the_halt_banner_when_halted() {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.halt = Some(super::super::collect::Halt {
            since: "2026-09-30T00:00:00Z".into(),
            why: Some("maintenance".into()),
        });
        let out = render(&d);
        assert!(out.contains("!! HALTED since 2026-09-30T00:00:00Z"));
        assert!(out.contains("why: maintenance"));
        assert!(out.contains("### The far end"));
        assert!(out.contains("### The Sending"));
        assert!(out.contains("### The gate"));
        assert!(out.contains("### The workers"));
        assert!(out.contains("### The czar"));
        assert!(out.contains("### Lapsed aeons"));
        assert!(out.contains("### The graph"));
        assert!(out.contains("### The menu"));
        assert!(out.contains("### The shared checkout"));
        assert!(out.contains("### Can this snapshot be believed?"));
        assert!(out.contains("## Your task"));
    }

    #[test]
    fn render_never_folds_an_unknown_field_into_a_zero() {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.env.set("SP_UNLANDED_N", "");
        let out = render(&d);
        assert!(out.contains("branches finished but not landed    ?"));
    }
}
