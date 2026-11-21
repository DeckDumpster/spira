//! The nine threshold escalations filed after a non-nominal sweep writes its prompt file,
//! plus the failed-units state update and the moot-sweep call. Each mirrors one bash
//! section 1:1 (DESIGN.md §5); every one guards on `incident::is_usable` and on its number
//! being numeric — a `?` means the probe failed, and filing on a failed probe sounds the
//! alarm without evidence (law-absence-needs-a-positive-control).

use super::collect::SweepData;
use super::Cfg;
use crate::failed_units;
use crate::incident::{self, Finding};
use crate::log::log;

fn num(env_val: &str) -> Option<i64> {
    if env_val == "?" {
        None
    } else {
        env_val.parse().ok()
    }
}

/// What a threshold escalation does with its finding: today `incident alarm`, in a host that
/// acts later (the scheduler's `watch.tower`) a collector of the alarms to request.
pub type Raise<'a> = &'a mut dyn FnMut(&Finding) -> incident::AlarmOutcome;

pub fn run(d: &SweepData, cfg: &Cfg) {
    failed_units_escalation(d, cfg);
    drain_check(d);
    let inc = cfg.incident_sh.clone();
    thresholds(d, cfg, &mut |f| incident::alarm_outcome(&inc, f));
    moot_sweep(cfg);
}

/// The threshold escalations over one sweep's data, each finding handed to `raise`: every one
/// but the failed-unit escalation (whose first-seen rows are state) and the drain check (an act).
pub fn thresholds(d: &SweepData, cfg: &Cfg, raise: Raise) {
    drain_escalation(d, cfg, raise);
    hel_oldest_unsent(d, cfg, raise);
    unadopted_refs(d, cfg, raise);
    hotfix(d, cfg, raise);
    batched_stranded(d, cfg, raise);
    batched_too_long(d, cfg, raise);
    closed_stranded(d, cfg, raise);
    dedup_meter(d, cfg, raise);
    gate_silent(d, cfg, raise);
    gate_slow(d, cfg, raise);
    idle_while_ready(d, cfg, raise);
}

/// The failed-unit alarm for `unit`, failing since `first_seen` (epoch seconds) at `now`.
pub fn failed_unit_finding(db: &str, home_repo: &str, unit: &str, invocation: &str, first_seen: i64, now: i64) -> Finding {
    let first_disp = crate::log::fmt_iso(first_seen);
    let age_mins = (now - first_seen) / 60;
    let body = format!("{unit} has one failed run (invocation {invocation}) standing for {age_mins}m (it failed at {first_disp}); it has not been re-run since\n\nCheck: systemctl --user status {unit}\n");
    Finding::new(db, home_repo, &format!("FAILED UNIT: {unit} failing for {age_mins}m"), &body).priority(1).reference(format!("incident:failed-unit-{unit}")).cause("failed-unit")
}

fn usable_inc(cfg: &Cfg) -> Option<&str> {
    incident::is_usable(&cfg.incident_sh).then_some(cfg.incident_sh.as_str())
}

/// True unless the store positively answers that no unfinished bead holds the unit. bd
/// names the beads carrying the reference; whether one is unfinished is its lifecycle row's
/// (sp-jgjvh: an incident bead is a work bead). A failed query — bd's or the machine's —
/// counts as tracked: re-filing on a probe that did not answer is noise.
fn tracked_by_open_bead(cfg: &Cfg, unit: &str) -> bool {
    let reference = format!("incident:failed-unit-{unit}");
    let out = crate::lc::content_list(&cfg.lc, &["--external-ref", &reference, "--all", "--json", "--limit", "0", "--brief"]);
    let ids: Vec<String> = match out {
        Some(o) => match serde_json::from_str::<Vec<serde_json::Value>>(&o) {
            Ok(v) => v.iter().filter_map(|r| r.get("id").and_then(|x| x.as_str()).map(String::from)).collect(),
            Err(_) => return true,
        },
        _ => return true,
    };
    crate::conditions::unfinished(&cfg.lc_bin, ids).is_none_or(|live| !live.is_empty())
}

fn failed_units_escalation(d: &SweepData, cfg: &Cfg) {
    let Some(units) = &d.failed_units else { return };
    let invocation_of = |u: &str| d.failed_invocations.get(u).cloned().unwrap_or_default();
    let state_path = cfg.failed_units_state();
    let prev_rows = failed_units::read_state_file(&state_path);
    let inc = usable_inc(cfg);
    let mut new_rows = Vec::new();
    for unit in units {
        let prev = prev_rows.iter().find(|r| &r.unit == unit);
        let (row, decided) = failed_units::decide(d.now, unit, &invocation_of(unit), prev, cfg.failed_units_warn_mins);
        let should_escalate = decided || (row.escalated && !tracked_by_open_bead(cfg, unit));
        if should_escalate {
            if let Some(inc) = inc {
                let age_mins = (d.now - row.first_seen) / 60;
                incident::alarm(inc, &failed_unit_finding(&cfg.db, &cfg.home_repo, unit, &row.invocation, row.first_seen, d.now));
                log(&format!("watchtower: failed-unit escalation filed ({unit}, {age_mins}m)"));
            } else {
                log(&format!("watchtower: {} is missing — failed-unit escalation not filed", cfg.incident_sh));
            }
        }
        new_rows.push(row);
    }
    failed_units::write_state_file(&state_path, &new_rows);
}

/// The drain's own cap and owner-liveness checks live in `world drain-check`, which pages
/// the Concierge once per condition and never lifts the drain.
fn drain_check(d: &SweepData) {
    if !matches!(d.drain, super::collect::Drain::Draining { .. }) {
        return;
    }
    // FORK-EDGE: needs spira_world::drain check-and-page as a library fn (world.rs drain_check_and_page is binary-private)
    match spira_config::bounded::bounded("world").arg("drain-check").output() {
        Ok(o) if o.status.success() => {}
        _ => log("watchtower: `world drain-check` failed — the drain's cap and owner were not checked"),
    }
}

fn drain_finding(cfg: &Cfg, since: &str, mins: i64) -> Finding {
    let body = format!(
        "DRAINING for {mins}m — calls gated since {since}\n\nNew dwarves cannot be called while world.draining exists. Loop, landing and reaping continue.\n\nLift with: world.sh resume\n"
    );
    Finding::new(&cfg.db, &cfg.home_repo, "DRAINING: world.sh calls gated", &body).priority(1)
}

fn drain_escalation(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let super::collect::Drain::Draining { since, mins } = &d.drain else {
        return;
    };
    let Some(mins) = mins else { return };
    if *mins < cfg.drain_warn_mins {
        return;
    }
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — drain escalation not filed", cfg.incident_sh));
        return;
    };
    raise(&drain_finding(cfg, since, *mins));
    log(&format!("watchtower: drain escalation filed ({mins}m >= {}m threshold)", cfg.drain_warn_mins));
}

fn hel_oldest_unsent(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(oldest) = num(d.env.g("SP_UNSENT_OLDEST_H")) else { return };
    if oldest < cfg.unsent_warn_h {
        return;
    }
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — sending escalation not filed", cfg.incident_sh));
        return;
    };
    let body = format!(
        "Oldest unsent branch: {oldest}h — threshold is {}h\n\nA branch this old without a landing means the Hel rite has not run or cannot delete it.\nBranches owned by live in_progress beads are work in flight; confirm the branch has no holder before acting.\n\nCheck the hel binary (`hel --dry-run`) and the rite logs. Reap manually if the owning bead is already closed.\n",
        cfg.unsent_warn_h
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "HEL: oldest unsent branch above threshold", &body)
        .priority(1)
        .reference("incident:hel-oldest-unsent")
        .cause("oldest-unsent");
    raise(&f);
    log(&format!("watchtower: sending escalation filed (oldest unsent {oldest}h >= {}h threshold)", cfg.unsent_warn_h));
}

fn unadopted_refs(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(n) = num(d.env.g("SP_UNADOPTED")) else { return };
    if n <= 0 {
        return;
    }
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — unadopted escalation not filed", cfg.incident_sh));
        return;
    };
    let names = d.env.raw("SP_UNADOPTED_NAMES").unwrap_or("(unavailable)");
    let body = format!(
        "Unadopted refs: {n}\n\nA spira/* branch whose suffix resolves to no bead can never be reaped by any rite.\nEach one is a permanent +1 on SP_UNADOPTED until removed by hand.\n\nBranches (spira/ prefix omitted): {names}\nDelete safely: git -C {} branch -D spira/<id> (no bead, no dwarf holds it)\n",
        cfg.spira_run.display()
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "HEL: unadopted refs cannot be reaped", &body)
        .priority(2)
        .reference("incident:hel-unadopted-refs")
        .cause("unadopted-refs")
        .delivers_action();
    raise(&f);
    log(&format!("watchtower: unadopted escalation filed ({n} unadopted refs)"));
}

fn hotfix(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(out) = &d.release_status else { return };
    let Some(alert) = out.lines().find(|l| l.starts_with("ALERT ")) else {
        return;
    };
    let Some(running_line) = out.lines().find(|l| l.starts_with("RUNNING UNLANDED ")) else {
        return;
    };
    // `sed -n 's/^RUNNING UNLANDED \([0-9a-f]\{40\}\):.*/\1/p'` — exactly 40 lowercase hex
    // digits immediately followed by a colon, or no match at all (no escalation).
    let rest = running_line.trim_start_matches("RUNNING UNLANDED ");
    let sha: String = rest
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        .collect();
    if sha.len() != 40 || rest.as_bytes().get(40) != Some(&b':') {
        return;
    }
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — hotfix escalation not filed", cfg.incident_sh));
        return;
    };
    let body = format!(
        "{running_line}\n\n{alert}\n\nThe running system is on a commit that has not landed. It will be superseded\nautomatically once that commit is an ancestor of local/main; until then land the fix\nor `release rollback`.\n"
    );
    let f = Finding::new(
        &cfg.db,
        &cfg.home_repo,
        &format!("HOTFIX: RUNNING UNLANDED {} past threshold", &sha[..12]),
        &body,
    )
    .priority(1)
    .reference(format!("incident:hotfix-{sha}"))
    .cause("hotfix-standing");
    raise(&f);
    log(&format!("watchtower: hotfix escalation filed ({sha} past threshold)"));
}

fn batched_stranded(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(n) = num(d.env.g("SP_BATCHED_STRANDED")) else { return };
    if n <= 0 {
        return;
    }
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — batched-stranded escalation not filed", cfg.incident_sh));
        return;
    };
    let candidates: Vec<&str> = d.env.raw("SP_BATCHED_STRANDED_NAMES").unwrap_or("").split_whitespace().collect();
    let Some(held) = crate::lc::beads_in_on(&cfg.lc, "IN_DELIVERY") else {
        log(&format!(
            "watchtower: batched-stranded — lifecycle did not answer; {} candidate(s) not re-confirmed, not filed",
            candidates.len()
        ));
        return;
    };
    let held: std::collections::HashSet<&str> = held.iter().map(|b| b.id.as_str()).collect();
    let (alive, gone): (Vec<&str>, Vec<&str>) = candidates.iter().partition(|id| held.contains(**id));
    if alive.is_empty() {
        log(&format!("watchtower: batched-stranded — no candidate still IN_DELIVERY, not filed (dropped: {})", gone.join(" ")));
        return;
    }
    let n = alive.len();
    let names = alive.join(" ");
    let ages = stranded_ages(&kept_ages(d.env.raw("SP_BATCHED_STRANDED_AGES").unwrap_or(""), &alive), &names);
    let body = format!(
        "Stranded IN_DELIVERY branches: {n}\n\nThe branch(es) below are IN_DELIVERY on the lifecycle record but their ID is absent from every open batch members= line. The Hel refuses to reap in-delivery branches, so these are permanently stuck until the record is corrected.\n\nBranch IDs (spira/ prefix omitted), with how long each has been outside every open batch:\n{ages}\n\nCheck: for each id, `spira-lc state <id>` (IN_DELIVERY) and confirm the id does not appear in $SPIRA_QUEUE_DIR/*/open members= lines.\nFix: settle or abandon the batch the row names on the lifecycle record (`spira-lc show <id>`). If the tip moved, escalate — the branch has diverged from what was batched.\n"
    );
    let title = format!("HEL: BATCHED branch absent from open batch: {names}");
    let f = Finding::new(&cfg.db, &cfg.home_repo, &title, &body)
        .priority(1)
        .reference("incident:hel-batched-stranded")
        .cause("batched-stranded");
    let outcome = match raise(&f) {
        incident::AlarmOutcome::Noted => "noted in the Concierge inbox",
        incident::AlarmOutcome::Deduped => "deduped (already noted within the reminder interval)",
        incident::AlarmOutcome::Failed => "NOT filed (incident alarm failed)",
    };
    log(&format!("watchtower: batched-stranded escalation {outcome} ({n} stranded: {names})"));
}

fn kept_ages(raw: &str, ids: &[&str]) -> String {
    raw.split_whitespace().filter(|p| p.split_once('=').is_some_and(|(id, _)| ids.contains(&id))).collect::<Vec<_>>().join(" ")
}

/// `id=secs` pairs as one line per bead: `id — 6m10s`.
fn stranded_ages(raw: &str, names: &str) -> String {
    if raw.trim().is_empty() {
        return names.split_whitespace().map(|id| format!("  {id} — (age unknown)")).collect::<Vec<_>>().join("\n");
    }
    raw.split_whitespace()
        .filter_map(|p| p.split_once('='))
        .map(|(id, s)| match s.parse::<i64>() {
            Ok(s) => format!("  {id} — {}m{:02}s", s / 60, s % 60),
            Err(_) => format!("  {id} — (age unknown)"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn batched_too_long(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(n) = num(d.env.g("SP_BATCHED_TOO_LONG")) else { return };
    if n <= 0 {
        return;
    }
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — batched-too-long escalation not filed", cfg.incident_sh));
        return;
    };
    let names = d.env.raw("SP_BATCHED_TOO_LONG_NAMES").unwrap_or("(unavailable)");
    let body = format!(
        "BATCHED branches not resolved after one batch interval: {n}\n\nThe branch(es) below have been in BATCHED state longer than expected:\n\n{names}\n\nCheck: is the open batch PR mergeable? Run: gh pr view <pr-number> --json mergeable,mergeStateStatus.\nFix: if DIRTY, abandon the batch: queue abandon <repo> --reason \"conflict\".\n"
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "QUEUE: BATCHED branch not resolved (too long)", &body)
        .priority(1)
        .reference("incident:queue-batched-too-long")
        .cause("batched-too-long");
    raise(&f);
    log(&format!("watchtower: batched-too-long escalation filed ({n} branches)"));
}

fn closed_stranded(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(oldest) = num(d.env.g("SP_CLOSED_STRANDED_OLDEST_H")) else { return };
    if oldest < cfg.closed_stranded_warn_h {
        return;
    }
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — closed-stranded escalation not filed", cfg.incident_sh));
        return;
    };
    let body = format!(
        "Closed-bead branches not reaped: oldest {oldest}h (threshold {}h)\n\nThese branches belong to CLOSED beads but the Hel has kept them every pass because none of its reap rules matched. Each pass makes a GitHub API call per branch and logs a KEEP line.\n\nRun: hel --dry-run <bead-id>  to judge one branch and see the rule it failed. A full `hel --dry-run` resumes from its cursor and does not judge every branch.\n\nCommon causes:\n  - work landed via a batch PR whose commit names the bead (check: git log --grep=<id> origin/main)\n  - a superseded bead with an empty branch (check n>0 guard)\n  - a non-code deliverable bead with no delivers: label\n\nReap by hand if confirmed safe: spira_destroy_branch / spira_destroy_worktree via `hel <bead-id>` (one bead). A manual hold on a live lifecycle row still refuses it; a terminal row's manual hold no longer does.\n",
        cfg.closed_stranded_warn_h
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "HEL: closed-bead branch not reaped above threshold", &body)
        .priority(2)
        .reference("incident:hel-closed-stranded")
        .cause("closed-stranded")
        .delivers_action();
    raise(&f);
    log(&format!("watchtower: closed-stranded escalation filed (oldest {oldest}h >= {}h)", cfg.closed_stranded_warn_h));
}

fn dedup_meter(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(refs) = num(d.env.g("SP_DUP_REFS")) else { return };
    if refs <= 0 {
        return;
    }
    let beads = d.env.g("SP_DUP_BEADS");
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — dedup escalation not filed", cfg.incident_sh));
        return;
    };
    let mut body = format!(
        "Duplicate incident refs: {refs} refs, {beads} surplus beads\n\nThe incident.sh dedup path is not deduplicating within its lookback window. Multiple beads exist for the same external_ref, which means each pass filed a fresh bead instead of bumping a recurrence. Collapse the surplus beads and investigate why open_incident() or recent_closed_incident() missed the existing one.\n\nWorst offenders:\n"
    );
    for i in 0..5 {
        if let Some(row) = d.env.raw(&format!("SP_DUP_ROW{i}")) {
            if !row.is_empty() {
                body.push_str(&format!("  {row}\n"));
            }
        }
    }
    let f = Finding::new(
        &cfg.db,
        &cfg.home_repo,
        &format!("DEDUP: duplicate incident refs detected ({refs} refs, {beads} surplus)"),
        &body,
    )
    .priority(1)
    .reference("incident:dedup-meter-nonzero")
    .cause("dedup-meter");
    raise(&f);
    log(&format!("watchtower: dedup escalation filed ({refs} dup refs, {beads} surplus beads)"));
}

fn gate_silent(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(s) = &d.gate_silence else { return };
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — gate-silent escalation not filed", cfg.incident_sh));
        return;
    };
    let last = s.last_gate.as_deref().unwrap_or("none (gate.log has no timestamped line)");
    let body = format!(
        "{} bead(s) reached SUBMITTED in the last {}m but gate.log gained no line in that time.\n\nLast gate.log timestamp: {last}\nSubmitted: {}\n\nA gate that stops running is otherwise silent: submitted branches reach rounds ungated. Check that the landing pass runs the gate for submitted branches before certifying them.\n",
        s.submitted.len(),
        cfg.gate_silence_window_s / 60,
        s.submitted.join(" ")
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "GATE SILENT: submissions with no gate.log activity", &body)
        .priority(1)
        .reference("incident:gate-silent")
        .cause("gate-silent");
    raise(&f);
    log(&format!("watchtower: gate-silent escalation filed (last gate {last})"));
}

fn gate_slow(d: &SweepData, cfg: &Cfg, raise: Raise) {
    let Some(s) = &d.gate_slow else { return };
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — gate-slow escalation not filed", cfg.incident_sh));
        return;
    };
    let body = format!(
        "The gate's p90 test_clock (gate.log, the seconds charged to SPIRA_GATE_DEADLINE) over the last 24h is {}s across {} gates; the deadline is {}s. Build time is not counted.\n\nSee the phases= field of the slowest gate.log rows for the test phase that overruns; per-phase caps are in gate/DESIGN.md.\n",
        s.p90, s.rows, cfg.gate_deadline_s
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "GATE SLOW: p90 gate test clock at the deadline", &body)
        .priority(1)
        .reference("incident:gate-slow")
        .cause("gate-slow");
    raise(&f);
    log(&format!("watchtower: gate-slow escalation filed (p90 {}s)", s.p90));
}

fn idle_while_ready(d: &SweepData, cfg: &Cfg, raise: Raise) {
    if d.idle_while_ready.is_empty() {
        return;
    }
    let Some(_) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — idle-while-ready escalation not filed", cfg.incident_sh));
        return;
    };
    for (rune, ready, reason) in &d.idle_while_ready {
        let body = format!(
            "{rune} has {ready} ready bead(s), but its last {} calls all came back idle.\n\nLast idle reason: {reason}\n\nA rank/lookup failure (epic_parent_lookup/epic_rank_rows in lib.sh) can render exactly this way: an empty ranked list reads as \"nothing ready to claim\" even though bd ready is non-empty. Check the ledger for claim-error lines before assuming the queue really is empty.\n",
            cfg.idle_while_ready_n
        );
        let f = Finding::new(
            &cfg.db,
            &cfg.home_repo,
            &format!("IDLE-WHILE-READY: {rune} has ready work but keeps reporting idle"),
            &body,
        )
        .priority(1)
        .reference(format!("incident:idle-while-ready:{rune}"))
        .cause("idle-while-ready");
        raise(&f);
        log(&format!(
            "watchtower: idle-while-ready escalation filed for {rune} (ready={ready}, last {} calls idle)",
            cfg.idle_while_ready_n
        ));
    }
}

fn moot_sweep(cfg: &Cfg) {
    super::moot::sweep(&cfg.moot(), true);
    log("watchtower: moot-sweep ran");
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::collect::SweepData;
    use crate::sweep::cfg::Cfg;

    /// The raise `run` gives the escalations: `incident alarm` on the cfg's incident.sh.
    fn alarm(cfg: &Cfg) -> impl FnMut(&Finding) -> incident::AlarmOutcome + '_ {
        move |f| incident::alarm_outcome(&cfg.incident_sh, f)
    }

    #[test_id(tpbw9tb)]
    #[test]
    fn the_stranded_finding_lists_each_bead_with_how_long_it_has_been_stranded() {
        assert_eq!(stranded_ages("sp-a=370 sp-b=61", ""), "  sp-a — 6m10s\n  sp-b — 1m01s");
        assert_eq!(stranded_ages("sp-a=?", ""), "  sp-a — (age unknown)");
        assert_eq!(stranded_ages("", "sp-a sp-b"), "  sp-a — (age unknown)\n  sp-b — (age unknown)", "a collector with no ages still names the beads");
    }

    fn fake_incident(d: &std::path::Path) -> String {
        let inc = d.join("inc.sh");
        let capture = d.join("captured.txt");
        std::fs::write(
            &inc,
            format!(
                "#!/usr/bin/env bash\n{{ echo \"REF=$SPIRA_INCIDENT_REF\"; echo \"TITLE=$2\"; echo \"FENRIR=$SPIRA_FENRIR_EXEMPT\"; cat; echo '---'; }} >> {}\n",
                capture.display()
            ),
        )
        .unwrap();
        inc.to_str().unwrap().to_string()
    }

    fn cfg_with(d: &std::path::Path, incident_sh: String) -> Cfg {
        Cfg::fixture(d, incident_sh)
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(tud1yaf)]
    #[test]
    fn a_question_mark_never_fires_the_unsent_escalation() {
        let d = testkit::TempDir::new("wt-escalate-unsent-unknown");
        let inc = fake_incident(&d);
        let cfg = cfg_with(&d, inc);
        let data = SweepData::fixture_nominal(1_700_000_000); // SP_UNSENT_OLDEST_H unset -> "?"
        hel_oldest_unsent(&data, &cfg, &mut alarm(&cfg));
        assert!(!d.join("captured.txt").exists());
    }

    #[test_id(tpcdzjw)]
    #[test]
    fn a_missing_incident_sh_never_panics_it_just_skips() {
        let d = testkit::TempDir::new("wt-escalate-missing-inc");
        let cfg = cfg_with(&d, "does/not/exist/incident.sh".into());
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        data.env.set("SP_UNADOPTED", "5");
        unadopted_refs(&data, &cfg, &mut alarm(&cfg)); // must not panic, and files nothing
        assert!(!d.join("captured.txt").exists());
    }

    fn captured(d: &std::path::Path) -> String {
        std::fs::read_to_string(d.join("captured.txt")).unwrap_or_default()
    }

    #[test_id(teyv1id)]
    #[test]
    fn a_failed_unit_is_tracked_only_when_the_content_door_names_a_live_bead() {
        let d = testkit::TempDir::new("wt-escalate-lc");
        let mut cfg = cfg_with(&d, "inc".into());
        let (t, lc) = crate::lc::scripted::scripted(0, "[]");
        cfg.lc = lc;
        assert!(!tracked_by_open_bead(&cfg, "spira-x.service"), "no bead carries the reference");
        assert_eq!(t.seen.lock().unwrap()[0][..4], ["content", "list", "--external-ref", "incident:failed-unit-spira-x.service"]);
        cfg.lc = crate::lc::scripted::scripted(2, "cannot tell").1;
        assert!(tracked_by_open_bead(&cfg, "spira-x.service"), "a store that did not answer counts as tracked");
    }

    fn drain(mins: Option<i64>) -> SweepData {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.drain = super::super::collect::Drain::Draining { since: "2026-09-08 20:02:00 UTC".into(), mins };
        d
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(t5xzi83)]
    #[test]
    fn drain_escalation_is_silent_when_not_draining_or_the_age_is_unreadable() {
        let d = testkit::TempDir::new("wt-esc-drain-quiet");
        let mut cfg = cfg_with(&d, fake_incident(&d));
        cfg.drain_warn_mins = 0;
        drain_escalation(&SweepData::fixture_nominal(1_700_000_000), &cfg, &mut alarm(&cfg));
        drain_escalation(&drain(None), &cfg, &mut alarm(&cfg));
        assert!(captured(&d).is_empty());
    }

    // covers: UC-ops-detection-remediation-07 UC-ops-detection-remediation-27
    #[test_id(tqz67af)]
    #[test]
    fn the_draining_escalation_is_filed_fenrir_exempt() {
        let d = testkit::TempDir::new("wt-esc-drain-exempt");
        let mut cfg = cfg_with(&d, fake_incident(&d));
        cfg.drain_warn_mins = 0;
        let mut raised: Vec<Finding> = Vec::new();
        drain_escalation(&drain(Some(600)), &cfg, &mut |f| {
            raised.push(f.clone());
            incident::AlarmOutcome::Noted
        });
        assert_eq!(raised.len(), 1);
        assert!(raised[0].fenrir_exempt);
    }

    fn file_one(tag: &str, f: fn(&SweepData, &Cfg, Raise), pairs: &[(&str, &str)]) -> String {
        let d = testkit::TempDir::new(tag);
        let cfg = cfg_with(&d, fake_incident(&d));
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        for (k, v) in pairs {
            data.env.set(*k, *v);
        }
        f(&data, &cfg, &mut alarm(&cfg));
        captured(&d)
    }

    #[test_id(tudp2ms)]
    #[test]
    fn batched_stranded_is_silent_at_zero_and_at_a_question_mark() {
        assert!(file_one("wt-esc-bs0", batched_stranded, &[("SP_BATCHED_STRANDED", "0")]).is_empty());
        assert!(file_one("wt-esc-bsq", batched_stranded, &[("SP_BATCHED_STRANDED", "?")]).is_empty());
    }

    fn stranded_raised(tag: &str, names: &str, ages: &str, lc: spira_store::lc_call::LcClient) -> Vec<Finding> {
        let d = testkit::TempDir::new(tag);
        let mut cfg = cfg_with(&d, fake_incident(&d));
        cfg.lc = lc;
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        data.env.set("SP_BATCHED_STRANDED", names.split_whitespace().count().to_string());
        data.env.set("SP_BATCHED_STRANDED_NAMES", names);
        data.env.set("SP_BATCHED_STRANDED_AGES", ages);
        let mut raised: Vec<Finding> = Vec::new();
        batched_stranded(&data, &cfg, &mut |f| {
            raised.push(f.clone());
            incident::AlarmOutcome::Noted
        });
        raised
    }

    #[test_id(t7gspqm)]
    #[test]
    fn a_bead_the_machine_no_longer_holds_in_delivery_is_not_alarmed() {
        let (_, lc) = crate::lc::scripted::scripted_by(&[("list --state IN_DELIVERY", 0, "[]")]);
        assert!(stranded_raised("wt-esc-bs-landed", "sp-landed", "sp-landed=400", lc).is_empty());
    }

    #[test_id(tfx6n95)]
    #[test]
    fn only_the_beads_still_in_delivery_are_named_in_the_title_and_body() {
        let (_, lc) = crate::lc::scripted::scripted_by(&[("list --state IN_DELIVERY", 0, r#"[{"bead_id":"sp-stuck","tip":"t","since":1}]"#)]);
        let raised = stranded_raised("wt-esc-bs-one", "sp-gone sp-stuck", "sp-gone=350 sp-stuck=410", lc);
        assert_eq!(raised.len(), 1);
        assert!(raised[0].title.contains("sp-stuck") && !raised[0].title.contains("sp-gone"), "{}", raised[0].title);
        assert!(raised[0].body.contains("sp-stuck — 6m50s"), "{}", raised[0].body);
        assert!(!raised[0].body.contains("sp-gone"), "{}", raised[0].body);
        assert!(raised[0].body.contains("IN_DELIVERY branches: 1"), "{}", raised[0].body);
    }

    #[test_id(txl7o09)]
    #[test]
    fn a_lifecycle_that_does_not_answer_files_no_stranded_alarm() {
        let (_, lc) = crate::lc::scripted::scripted(2, "cannot tell");
        assert!(stranded_raised("wt-esc-bs-fault", "sp-a", "sp-a=400", lc).is_empty());
    }

    #[test_id(tzks5lf)]
    #[test]
    fn batched_too_long_is_silent_at_zero_and_at_a_question_mark() {
        assert!(file_one("wt-esc-bl0", batched_too_long, &[("SP_BATCHED_TOO_LONG", "0")]).is_empty());
        assert!(file_one("wt-esc-blq", batched_too_long, &[("SP_BATCHED_TOO_LONG", "?")]).is_empty());
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(tmp48sh)]
    #[test]
    fn unadopted_refs_is_silent_at_zero_and_at_a_question_mark() {
        assert!(file_one("wt-esc-un0", unadopted_refs, &[("SP_UNADOPTED", "0")]).is_empty());
        assert!(file_one("wt-esc-unq", unadopted_refs, &[("SP_UNADOPTED", "?")]).is_empty());
    }

    #[test_id(t0fpne9)]
    #[test]
    fn gate_slow_files_the_deadline_it_was_configured_with() {
        let d = testkit::TempDir::new("wt-esc-gslow");
        let mut cfg = cfg_with(&d, fake_incident(&d));
        cfg.gate_deadline_s = 777;
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        let mut raised: Vec<Finding> = Vec::new();
        let mut keep = |f: &Finding| {
            raised.push(f.clone());
            incident::AlarmOutcome::Noted
        };
        gate_slow(&data, &cfg, &mut keep);
        data.gate_slow = Some(crate::gate_wait::Slow { p90: 780, rows: 25 });
        gate_slow(&data, &cfg, &mut keep);
        assert_eq!(raised.len(), 1, "silent until the measure says slow");
        assert!(raised[0].body.contains("the deadline is 777s"), "{}", raised[0].body);
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(txgo7zt)]
    #[test]
    fn an_unread_oldest_unsent_is_silent_even_at_a_zero_threshold() {
        let d = testkit::TempDir::new("wt-esc-unsent0");
        let mut cfg = cfg_with(&d, fake_incident(&d));
        cfg.unsent_warn_h = 0;
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        data.env.set("SP_UNSENT_OLDEST_H", "?");
        hel_oldest_unsent(&data, &cfg, &mut alarm(&cfg));
        assert!(captured(&d).is_empty());
    }

    fn raised(tag: &str, f: fn(&SweepData, &Cfg, Raise), mutate: &dyn Fn(&mut SweepData, &mut Cfg)) -> Vec<Finding> {
        let d = testkit::TempDir::new(tag);
        let mut cfg = cfg_with(&d, fake_incident(&d));
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        mutate(&mut data, &mut cfg);
        let mut out = Vec::new();
        f(&data, &cfg, &mut |x| {
            out.push(x.clone());
            incident::AlarmOutcome::Noted
        });
        out
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(ty4tm13)]
    #[test]
    fn the_hel_escalation_files_at_the_configured_threshold_under_one_stable_ref() {
        let at = |h: &str, warn: i64| raised("wt-esc-hel", hel_oldest_unsent, &|d, c| {
            d.env.set("SP_UNSENT_OLDEST_H", h);
            c.unsent_warn_h = warn;
        });
        assert!(at("23", 24).is_empty(), "below the threshold");
        let filed = at("24", 24);
        assert_eq!(filed.len(), 1, "at the threshold");
        assert_eq!(filed[0].reference.as_deref(), Some("incident:hel-oldest-unsent"));
        assert_eq!(at("24", 24)[0].reference, filed[0].reference, "the same ref on every pass");
        assert_eq!(at("5", 4).len(), 1, "the threshold is the configured one, not a literal");
        assert!(at("0", 24).is_empty() && at("?", 0).is_empty());
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(tyoathu)]
    #[test]
    fn the_closed_stranded_escalation_prescribes_the_per_bead_dry_run_not_the_cursor_resumed_one() {
        let filed = raised("wt-esc-stranded", closed_stranded, &|d, c| {
            d.env.set("SP_CLOSED_STRANDED_OLDEST_H", "441");
            c.closed_stranded_warn_h = 24;
        });
        assert_eq!(filed.len(), 1);
        let body = &filed[0].body;
        assert!(body.contains("hel --dry-run <bead-id>"), "{body}");
        assert!(!body.contains("Run: hel --dry-run  to see each branch"), "{body}");
        assert!(body.contains("resumes from its cursor"), "{body}");
        assert!(body.contains("manual hold on a live lifecycle row still refuses it"), "{body}");
        assert!(body.contains("terminal row's manual hold no longer does"), "{body}");
        assert_eq!(filed[0].reference.as_deref(), Some("incident:hel-closed-stranded"));
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(tcct3iq)]
    #[test]
    fn the_dedup_escalation_files_on_any_duplicate_ref_and_never_on_zero_or_a_question_mark() {
        let with = |refs: &str| raised("wt-esc-dedup", dedup_meter, &|d, _| {
            d.env.set("SP_DUP_REFS", refs);
            d.env.set("SP_DUP_BEADS", "5");
            d.env.set("SP_DUP_ROW0", "incident:x x3");
        });
        assert!(with("0").is_empty() && with("?").is_empty());
        let filed = with("3");
        assert_eq!(filed.len(), 1);
        assert_eq!(filed[0].reference.as_deref(), Some("incident:dedup-meter-nonzero"));
        assert!(filed[0].title.contains("3 refs, 5 surplus") && filed[0].body.contains("incident:x x3"), "{}", filed[0].body);
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(trfyis8)]
    #[test]
    fn the_drain_escalation_files_at_the_configured_threshold_and_not_below_it() {
        let at = |mins: i64, warn: i64| raised("wt-esc-drain-at", drain_escalation, &|d, c| {
            *d = drain(Some(mins));
            c.drain_warn_mins = warn;
        });
        assert!(at(14, 15).is_empty());
        let filed = at(15, 15);
        assert_eq!(filed.len(), 1);
        assert!(filed[0].title.starts_with("DRAINING"));
        assert_eq!(at(3, 2).len(), 1, "the threshold is the configured one");
    }
}
