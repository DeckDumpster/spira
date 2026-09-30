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
use std::process::Command;

fn num(env_val: &str) -> Option<i64> {
    if env_val == "?" {
        None
    } else {
        env_val.parse().ok()
    }
}

pub fn run(d: &SweepData, cfg: &Cfg) {
    failed_units_escalation(d, cfg);
    drain_escalation(d, cfg);
    sending_oldest_unsent(d, cfg);
    unadopted_refs(d, cfg);
    hotfix(d, cfg);
    batched_stranded(d, cfg);
    batched_too_long(d, cfg);
    closed_stranded(d, cfg);
    dedup_meter(d, cfg);
    idle_while_ready(d, cfg);
    moot_sweep(cfg);
}

fn usable_inc(cfg: &Cfg) -> Option<&str> {
    incident::is_usable(&cfg.incident_sh).then_some(cfg.incident_sh.as_str())
}

fn failed_units_escalation(d: &SweepData, cfg: &Cfg) {
    let Some(units) = &d.failed_units else { return };
    let state_path = cfg.failed_units_state();
    let prev_rows = failed_units::read_state_file(&state_path);
    let inc = usable_inc(cfg);
    let mut new_rows = Vec::new();
    for unit in units {
        let prev = prev_rows.iter().find(|r| &r.unit == unit);
        let (row, should_escalate) = failed_units::decide(d.now, unit, prev, cfg.failed_units_warn_mins);
        if should_escalate {
            if let Some(inc) = inc {
                let logs = Command::new(&cfg.journalctl)
                    .args(["--user", "-u", unit, "-n", "3", "--no-pager"])
                    .output()
                    .map(|o| {
                        String::from_utf8_lossy(&o.stdout).into_owned()
                            + &String::from_utf8_lossy(&o.stderr)
                    })
                    .unwrap_or_default();
                let first_disp = crate::log::fmt_iso(row.first_seen);
                let age_mins = (d.now - row.first_seen) / 60;
                let body = format!(
                    "{unit} has been failing for {age_mins}m (first seen failing {first_disp})\n\nLast 3 log lines:\n{logs}\n\nCheck: journalctl --user -u {unit} -n 50\n"
                );
                let f = Finding::new(
                    &cfg.db,
                    &cfg.home_repo,
                    &format!("FAILED UNIT: {unit} failing for {age_mins}m"),
                    &body,
                )
                .priority(1)
                .reference(format!("incident:failed-unit-{unit}"))
                .cause("failed-unit");
                incident::file(inc, &f);
                log(&format!("watchtower: failed-unit escalation filed ({unit}, {age_mins}m)"));
            } else {
                log(&format!("watchtower: {} is missing — failed-unit escalation not filed", cfg.incident_sh));
            }
        }
        new_rows.push(row);
    }
    failed_units::write_state_file(&state_path, &new_rows);
}

fn drain_escalation(d: &SweepData, cfg: &Cfg) {
    let super::collect::Drain::Draining { since, mins } = &d.drain else {
        return;
    };
    let Some(mins) = mins else { return };
    if *mins < cfg.drain_warn_mins {
        return;
    }
    let Some(inc) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — drain escalation not filed", cfg.incident_sh));
        return;
    };
    let body = format!(
        "DRAINING for {mins}m — summons gated since {since}\n\nNew aeons cannot be summoned while world.draining exists. Loop, landing and reaping continue.\n\nLift with: world.sh resume\n"
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "DRAINING: world.sh summons gated", &body).priority(1);
    incident::file(inc, &f);
    log(&format!("watchtower: drain escalation filed ({mins}m >= {}m threshold)", cfg.drain_warn_mins));
}

fn sending_oldest_unsent(d: &SweepData, cfg: &Cfg) {
    let Some(oldest) = num(d.env.g("SP_UNSENT_OLDEST_H")) else { return };
    if oldest < cfg.unsent_warn_h {
        return;
    }
    let Some(inc) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — sending escalation not filed", cfg.incident_sh));
        return;
    };
    let body = format!(
        "Oldest unsent branch: {oldest}h — threshold is {}h\n\nA branch this old without a landing means the Sending rite has not run or cannot delete it.\nBranches owned by live in_progress beads are work in flight; confirm the branch has no holder before acting.\n\nCheck the sending binary (`sending --dry-run`) and the rite logs. Reap manually if the owning bead is already closed.\n",
        cfg.unsent_warn_h
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "SENDING: oldest unsent branch above threshold", &body)
        .priority(1)
        .reference("incident:sending-oldest-unsent")
        .cause("oldest-unsent");
    incident::file(inc, &f);
    log(&format!("watchtower: sending escalation filed (oldest unsent {oldest}h >= {}h threshold)", cfg.unsent_warn_h));
}

fn unadopted_refs(d: &SweepData, cfg: &Cfg) {
    let Some(n) = num(d.env.g("SP_UNADOPTED")) else { return };
    if n <= 0 {
        return;
    }
    let Some(inc) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — unadopted escalation not filed", cfg.incident_sh));
        return;
    };
    let names = d.env.raw("SP_UNADOPTED_NAMES").unwrap_or("(unavailable)");
    let body = format!(
        "Unadopted refs: {n}\n\nA spira/* branch whose suffix resolves to no bead can never be reaped by any rite.\nEach one is a permanent +1 on SP_UNADOPTED until removed by hand.\n\nBranches (spira/ prefix omitted): {names}\nDelete safely: git -C {} branch -D spira/<id> (no bead, no aeon holds it)\n",
        cfg.spira_run.display()
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "SENDING: unadopted refs cannot be reaped", &body)
        .priority(2)
        .reference("incident:sending-unadopted-refs")
        .cause("unadopted-refs")
        .delivers_action();
    incident::file(inc, &f);
    log(&format!("watchtower: unadopted escalation filed ({n} unadopted refs)"));
}

fn hotfix(d: &SweepData, cfg: &Cfg) {
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
    let Some(inc) = usable_inc(cfg) else {
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
    incident::file(inc, &f);
    log(&format!("watchtower: hotfix escalation filed ({sha} past threshold)"));
}

fn batched_stranded(d: &SweepData, cfg: &Cfg) {
    let Some(n) = num(d.env.g("SP_BATCHED_STRANDED")) else { return };
    if n <= 0 {
        return;
    }
    let Some(inc) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — batched-stranded escalation not filed", cfg.incident_sh));
        return;
    };
    let names = d.env.raw("SP_BATCHED_STRANDED_NAMES").unwrap_or("(unavailable)");
    let body = format!(
        "Stranded BATCHED branches: {n}\n\nThe branch(es) below have BATCHED landstate but their ID is absent from every open batch members= line. The Sending refuses to reap BATCHED branches, so these are permanently stuck until the landstate is corrected.\n\nBranch IDs (spira/ prefix omitted): {names}\n\nCheck: for each id, read $SPIRA_RUN/landstate/<id> (first field = BATCHED) and confirm the id does not appear in $SPIRA_QUEUE_DIR/*/open members= lines.\nFix: if the branch still points to the BATCHED tip, recertify: land_mark <id> CERTIFIED <tip>. If the tip moved, escalate — the branch has diverged from what was batched.\n"
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "SENDING: BATCHED branch absent from open batch", &body)
        .priority(1)
        .reference("incident:sending-batched-stranded")
        .cause("batched-stranded");
    incident::file(inc, &f);
    log(&format!("watchtower: batched-stranded escalation filed ({n} stranded)"));
}

fn batched_too_long(d: &SweepData, cfg: &Cfg) {
    let Some(n) = num(d.env.g("SP_BATCHED_TOO_LONG")) else { return };
    if n <= 0 {
        return;
    }
    let Some(inc) = usable_inc(cfg) else {
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
    incident::file(inc, &f);
    log(&format!("watchtower: batched-too-long escalation filed ({n} branches)"));
}

fn closed_stranded(d: &SweepData, cfg: &Cfg) {
    let Some(oldest) = num(d.env.g("SP_CLOSED_STRANDED_OLDEST_H")) else { return };
    if oldest < cfg.closed_stranded_warn_h {
        return;
    }
    let Some(inc) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — closed-stranded escalation not filed", cfg.incident_sh));
        return;
    };
    let body = format!(
        "Closed-bead branches not reaped: oldest {oldest}h (threshold {}h)\n\nThese branches belong to CLOSED beads but the Sending has kept them every pass because none of its reap rules matched. Each pass makes a GitHub API call per branch and logs a KEEP line.\n\nRun: sending --dry-run  to see each branch and the rule it failed.\n\nCommon causes:\n  - work landed via a batch PR whose commit names the bead (check: git log --grep=<id> origin/main)\n  - a superseded bead with an empty branch (check n>0 guard)\n  - a non-code deliverable bead with no delivers: label\n\nReap by hand if confirmed safe: spira_destroy_branch / spira_destroy_worktree via `sending <bead-id>` (one bead).\n",
        cfg.closed_stranded_warn_h
    );
    let f = Finding::new(&cfg.db, &cfg.home_repo, "SENDING: closed-bead branch not reaped above threshold", &body)
        .priority(2)
        .reference("incident:sending-closed-stranded")
        .cause("closed-stranded")
        .delivers_action();
    incident::file(inc, &f);
    log(&format!("watchtower: closed-stranded escalation filed (oldest {oldest}h >= {}h)", cfg.closed_stranded_warn_h));
}

fn dedup_meter(d: &SweepData, cfg: &Cfg) {
    let Some(refs) = num(d.env.g("SP_DUP_REFS")) else { return };
    if refs <= 0 {
        return;
    }
    let beads = d.env.g("SP_DUP_BEADS");
    let Some(inc) = usable_inc(cfg) else {
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
    incident::file(inc, &f);
    log(&format!("watchtower: dedup escalation filed ({refs} dup refs, {beads} surplus beads)"));
}

fn idle_while_ready(d: &SweepData, cfg: &Cfg) {
    if d.idle_while_ready.is_empty() {
        return;
    }
    let Some(inc) = usable_inc(cfg) else {
        log(&format!("watchtower: {} is missing — idle-while-ready escalation not filed", cfg.incident_sh));
        return;
    };
    for (fayth, ready, reason) in &d.idle_while_ready {
        let body = format!(
            "{fayth} has {ready} ready bead(s), but its last {} summons all came back idle.\n\nLast idle reason: {reason}\n\nA rank/lookup failure (epic_parent_lookup/epic_rank_rows in lib.sh) can render exactly this way: an empty ranked list reads as \"nothing ready to claim\" even though bd ready is non-empty. Check the ledger for claim-error lines before assuming the queue really is empty.\n",
            cfg.idle_while_ready_n
        );
        let f = Finding::new(
            &cfg.db,
            &cfg.home_repo,
            &format!("IDLE-WHILE-READY: {fayth} has ready work but keeps reporting idle"),
            &body,
        )
        .priority(1)
        .reference(format!("incident:idle-while-ready:{fayth}"))
        .cause("idle-while-ready");
        incident::file(inc, &f);
        log(&format!(
            "watchtower: idle-while-ready escalation filed for {fayth} (ready={ready}, last {} summons idle)",
            cfg.idle_while_ready_n
        ));
    }
}

fn moot_sweep(cfg: &Cfg) {
    let sh = cfg.moot_sh();
    if !incident::is_usable(&sh) {
        log(&format!("watchtower: moot-sweep skipped — {sh} is missing or unreadable"));
        return;
    }
    let ok = Command::new("bash")
        .arg(&sh)
        .arg("--apply")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        log("watchtower: moot-sweep ran");
    } else {
        log(&format!("watchtower: moot-sweep exited non-zero — check {sh}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::collect::SweepData;
    use crate::sweep::cfg::Cfg;
    use std::path::PathBuf;

    fn fake_incident(d: &std::path::Path) -> String {
        let inc = d.join("inc.sh");
        let capture = d.join("captured.txt");
        std::fs::write(
            &inc,
            format!(
                "#!/usr/bin/env bash\n{{ echo \"REF=$SPIRA_INCIDENT_REF\"; echo \"TITLE=$2\"; cat; echo '---'; }} >> {}\n",
                capture.display()
            ),
        )
        .unwrap();
        inc.to_str().unwrap().to_string()
    }

    fn cfg_with(d: &std::path::Path, incident_sh: String) -> Cfg {
        Cfg {
            spira_run: d.to_path_buf(),
            lib_sh_dir: String::new(),
            db: "db".into(),
            home_repo: "spira".into(),
            ask_label: "needs-operator".into(),
            snap_stale_s: 60,
            gate_window_s: 21600,
            gate_log: None,
            yield_window_s: 86400,
            yield_sh: None,
            disk_warn_pct: 90,
            mem_warn_mb: 1500,
            meminfo_path: PathBuf::from("/proc/meminfo"),
            failed_units_warn_mins: 15,
            unsent_warn_h: 24,
            closed_stranded_warn_h: 48,
            drain_warn_mins: 15,
            idle_while_ready_n: 5,
            systemctl: "systemctl".into(),
            journalctl: "journalctl".into(),
            prompt_file: None,
            lapsed_dir: None,
            lapsed_marker: None,
            failed_units_state: Some(d.join("failed-units.state")),
            throttle_stamp: None,
            queue_throttle_override: String::new(),
            incident_sh,
            suites_sh: None,
            moot_sh: Some("/does/not/exist/moot-sweep.sh".into()),
            branch_guard_sh: None,
        }
    }

    #[test]
    fn sending_oldest_unsent_fires_at_and_above_the_threshold_not_below() {
        let d = testkit::TempDir::new("wt-escalate-unsent");
        let inc = fake_incident(&d);
        let cfg = cfg_with(&d, inc);

        let mut data = SweepData::fixture_nominal(1_700_000_000);
        data.env.set("SP_UNSENT_OLDEST_H", "23");
        sending_oldest_unsent(&data, &cfg);
        assert!(!d.join("captured.txt").exists(), "below threshold must not file");

        let mut data2 = SweepData::fixture_nominal(1_700_000_000);
        data2.env.set("SP_UNSENT_OLDEST_H", "24");
        sending_oldest_unsent(&data2, &cfg);
        let captured = std::fs::read_to_string(d.join("captured.txt")).unwrap();
        assert!(captured.contains("REF=incident:sending-oldest-unsent"));
    }

    #[test]
    fn a_question_mark_never_fires_the_unsent_escalation() {
        let d = testkit::TempDir::new("wt-escalate-unsent-unknown");
        let inc = fake_incident(&d);
        let cfg = cfg_with(&d, inc);
        let data = SweepData::fixture_nominal(1_700_000_000); // SP_UNSENT_OLDEST_H unset -> "?"
        sending_oldest_unsent(&data, &cfg);
        assert!(!d.join("captured.txt").exists());
    }

    #[test]
    fn unadopted_refs_fires_on_any_positive_count() {
        let d = testkit::TempDir::new("wt-escalate-unadopted");
        let inc = fake_incident(&d);
        let cfg = cfg_with(&d, inc);
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        data.env.set("SP_UNADOPTED", "1");
        data.env.set("SP_UNADOPTED_NAMES", "sp-stray");
        unadopted_refs(&data, &cfg);
        let captured = std::fs::read_to_string(d.join("captured.txt")).unwrap();
        assert!(captured.contains("REF=incident:sending-unadopted-refs"));
        assert!(captured.contains("sp-stray"));
    }

    #[test]
    fn dedup_meter_includes_the_worst_offender_rows() {
        let d = testkit::TempDir::new("wt-escalate-dedup");
        let inc = fake_incident(&d);
        let cfg = cfg_with(&d, inc);
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        data.env.set("SP_DUP_REFS", "2");
        data.env.set("SP_DUP_BEADS", "3");
        data.env.set("SP_DUP_ROW0", "incident:x 3 beads");
        dedup_meter(&data, &cfg);
        let captured = std::fs::read_to_string(d.join("captured.txt")).unwrap();
        assert!(captured.contains("REF=incident:dedup-meter-nonzero"));
        assert!(captured.contains("incident:x 3 beads"));
    }

    #[test]
    fn a_missing_incident_sh_never_panics_it_just_skips() {
        let d = testkit::TempDir::new("wt-escalate-missing-inc");
        let cfg = cfg_with(&d, "/does/not/exist/incident.sh".into());
        let mut data = SweepData::fixture_nominal(1_700_000_000);
        data.env.set("SP_UNADOPTED", "5");
        unadopted_refs(&data, &cfg); // must not panic, and files nothing
        assert!(!d.join("captured.txt").exists());
    }
}
