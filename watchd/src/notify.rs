//! `notify` — delivery that does not require a reader to exist (`cmd_notify`,
//! `_wd_notify_health`, `_wd_ask`, `_wd_escalate`). For a timer: a session hook fires at a
//! session boundary, which a headless watcher never has, so this is what asks "has anything
//! actionable been sitting here with nobody to take it" on its own schedule and escalates
//! through mail, which needs no session at all.
//!
//! Exit 0 nothing waiting long enough, 1 something escalated, 3 could not check or deliver.

use crate::context::Context;
use crate::filter::Filter;
use crate::fs_ops;
use crate::health::{self, Health};
use crate::iso8601;
use crate::manifest::{Kind, Row};
use crate::ops::Ops;
use crate::paths;
use crate::sop_run::{self, Verdict};

/// How many actionable lines of one watcher go into an ask — the evidence is read in a
/// pane, so a three-hundred-line backlog would bury the decision it is evidence for.
pub const NOTIFY_MAX: usize = 12;

/// The backlog ask (`escalate`): subject, default and why, as mailed.
pub const BACKLOG_STAMP: &str = "notify.escalated";
pub const HEALTH_STAMP: &str = "notify-health.escalated";
pub const BACKLOG_SUBJECT: &str = "Events a watcher produced have reached no reader";
pub const BACKLOG_DEFAULT: &str = "read them below and act on them here — nothing has been marked read, so the next session to latch still gets them; if a line of this kind is never worth waking anyone for, narrow SPIRA_ACTIONABLE rather than lengthening SPIRA_NOTIFY_AGE";
pub const BACKLOG_WHY: &str = "delivery of a watcher's events otherwise depends on a session existing to drain them, and a session hook fires at a session boundary — so an event produced while nothing is running waits for the next session to open, which for a headless agent never comes. Nothing else will surface these.";
/// The health ask (`notify_health`): subject, default and why, as mailed.
pub const HEALTH_SUBJECT: &str = "A watcher has stopped producing events";
pub const HEALTH_DEFAULT: &str = "restart it with `watchd restart <name>`, then read the last line above. A unit that restarts without ever becoming active is usually a second copy started by hand holding its lock — retiring that copy is the fix, and `Restart=always` respawns on a clean exit too, so the loop never ends on its own. If the watcher is meant to be stopped, take its row out of the manifest instead of leaving a unit systemd will respawn forever.";
pub const HEALTH_WHY: &str = "a watcher that is not running produces no events, and a watcher producing no events is indistinguishable from one with nothing to say. Nothing else escalates this: the other half of `notify` reports events that WERE produced, so the failure that stops production is exactly the one it cannot see.";

/// The `X-Spira-Condition` of an operator alarm, by its subject: the escalation stamp that
/// stands while the condition does.
pub fn condition_of(subject: &str) -> Option<String> {
    let name = match subject {
        BACKLOG_SUBJECT => BACKLOG_STAMP,
        HEALTH_SUBJECT => HEALTH_STAMP,
        _ => return None,
    };
    Some(format!("watchd:{}", name.trim_end_matches(".escalated")))
}

/// Exit status of `watchd condition <name>` once the alarm's condition has ended.
pub const CONDITION_ENDED: i32 = 10;

/// `watchd condition <name>`: 0 while the escalation stamp `<name>.escalated` stands, otherwise
/// [`CONDITION_ENDED`]; 2 for a name that is not an alarm.
pub fn condition_exit(run: &str, name: &str) -> i32 {
    let stamp = format!("{name}.escalated");
    if ![BACKLOG_STAMP, HEALTH_STAMP].contains(&stamp.as_str()) {
        return 2;
    }
    if paths::watchd_dir(run).join(stamp).exists() {
        0
    } else {
        CONDITION_ENDED
    }
}

/// FNV-1a 64-bit, hex-encoded — a stable, dependency-free fingerprint for the escalation
/// dedupe stamps (`_wd_ask`'s `cksum`, which is not reproduced bit-for-bit here because
/// nothing outside this binary ever reads the stamp file's content; only its own next run
/// does, so any stable hash of the same key values is equivalent).
pub fn fingerprint(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Suppressed while `key`'s fingerprint is unchanged, so a standing condition asks once
/// rather than once per pass. The fingerprint is written only after the ask was accepted —
/// stamping first would mark a finding delivered when a broken escalation path never sent
/// it.
///
/// Eight parameters, not a struct: every one is a distinct piece of the bash's own
/// `_wd_ask <stamp> <key> <title> <default> <why> <evidence>` call, kept as separate,
/// independently-named arguments at its two call sites (`escalate`, `notify_health`) rather
/// than bundled into a struct only this function would ever construct.
#[allow(clippy::too_many_arguments)]
fn ask(ops: &dyn Ops, run: &str, stamp_name: &str, key: &str, subject: &str, default: &str, why: &str, evidence: &str) -> Result<bool, String> {
    let stamp = paths::watchd_dir(run).join(stamp_name);
    let fp = fingerprint(key);
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(fp.as_str()) {
        return Ok(false);
    }
    if ops.mail_ask(subject, default, why, evidence).is_err() {
        eprintln!("watchd: the escalation path refused the ask ({subject}); not stamped, the next pass asks again");
        return Ok(false);
    }
    let _ = std::fs::create_dir_all(paths::watchd_dir(run));
    let _ = std::fs::write(&stamp, &fp);
    Ok(true)
}

fn escalate(ops: &dyn Ops, run: &str, key: &str, report: &str) -> Result<bool, String> {
    ask(
        ops,
        run,
        BACKLOG_STAMP,
        key,
        BACKLOG_SUBJECT,
        BACKLOG_DEFAULT,
        BACKLOG_WHY,
        report,
    )
}

fn world_halted(run: &str) -> bool {
    std::path::Path::new(run).join("world.halted").exists()
}

pub struct NotifyOutcome {
    pub report: String,
    pub code: i32,
}

pub fn cmd_notify(rows: &[Row], ops: &dyn Ops, ctx: &Context) -> Result<NotifyOutcome, String> {
    let notify_age = ctx.notify_age()?;
    let filter = Filter::compile(&ctx.actionable)?;
    let now = ctx.now;

    let mut report = String::new();
    let mut key = String::new();
    let mut stale = 0usize;

    for r in rows {
        if r.kind == Kind::Off {
            continue;
        }
        let Some(lf) = paths::logfile(&ctx.run, &r.name, r.kind, &r.target) else { continue };
        let total = fs_ops::total_lines(&lf);
        let cf = paths::cursorfile(&ctx.run, &r.name);
        let pos = fs_ops::read_pos(&cf, total);
        let pend = paths::pendfile(&ctx.run, &r.name);

        if total <= pos {
            fs_ops::remove(&pend);
            continue;
        }
        let chunk = fs_ops::read_range(&lf, pos, total);
        let hit = chunk.iter().enumerate().find(|(_, l)| filter.matches(l));
        let Some((first_idx, _)) = hit else {
            fs_ops::remove(&pend);
            continue;
        };
        let shown: Vec<&String> = chunk.iter().filter(|l| filter.matches(l)).collect();
        let apos = pos + first_idx as u64 + 1;

        let stamp_age = shown
            .iter()
            .filter_map(|l| l.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(ts, _)| ts))
            .filter_map(iso8601::parse_utc)
            .min()
            .map(|oldest| now - oldest);

        let (prev_pos, prev_at) = fs_ops::read_pending(&pend).map(|(p, a)| (Some(p), Some(a))).unwrap_or((None, None));
        let first_sighting = prev_at.is_none() || prev_pos != Some(apos);
        if first_sighting {
            let _ = fs_ops::write_pending(&pend, apos, now);
            if stamp_age.is_none() {
                continue;
            }
        }

        let age = stamp_age.unwrap_or_else(|| now - prev_at.unwrap_or(now)).max(0);
        if age < notify_age {
            continue;
        }

        let k = shown.len();
        let row_key = format!("{}|{}|{}\n", r.name, apos, chunk.get(first_idx).cloned().unwrap_or_default());
        let payload = shown.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n");
        let tried = match sop_run::attempt(ops, ctx, &r.name, &row_key, &payload) {
            Verdict::Handled => continue,
            Verdict::Escalate(note) => note,
        };
        stale += 1;
        key.push_str(&row_key);
        report.push('\n');
        report.push_str(&format!("{} — {k} actionable event(s) with no reader, the oldest for {}\n", r.name, crate::cursor::format_age(age)));
        report.push_str(&format!("  {}\n", lf.display()));
        for l in shown.iter().take(NOTIFY_MAX) {
            report.push_str("    ");
            report.push_str(l);
            report.push('\n');
        }
        if k > NOTIFY_MAX {
            report.push_str(&format!("    ... and {} more — all of them: watchd drain {}\n", k - NOTIFY_MAX, r.name));
        }
        if let Some(t) = tried {
            report.push_str(&format!("  {t}\n"));
        }
    }

    let mut found = false;
    if stale == 0 {
        fs_ops::remove(&paths::watchd_dir(&ctx.run).join("notify.escalated"));
    } else {
        escalate(ops, &ctx.run, &key, &report)?;
        found = true;
    }

    let health_outcome = notify_health(rows, ops, ctx)?;
    report.push_str(&health_outcome.report);
    if health_outcome.code == 1 {
        found = true;
    }

    let mh = ops.mail_health();
    if mh == 3 {
        return Err("watchd: mail-health could not check".to_string());
    }
    if mh == 1 {
        found = true;
    }

    Ok(NotifyOutcome { report, code: if found { 1 } else { 0 } })
}

fn notify_health(rows: &[Row], ops: &dyn Ops, ctx: &Context) -> Result<NotifyOutcome, String> {
    let now = ctx.now;
    let halted = world_halted(&ctx.run);
    let mut units = Vec::new();
    let mut idxs = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if r.kind == Kind::Daemon {
            units.push(paths::watch_unit_name(&r.name, &ctx.instance));
            idxs.push(i);
        } else if r.kind == Kind::Extern {
            units.push(ops.spira_unit(&r.target, "service", &ctx.instance));
            idxs.push(i);
        }
    }
    if units.is_empty() {
        return Ok(NotifyOutcome { report: String::new(), code: 0 });
    }
    let show = ops.show(&units);

    let mut report = String::new();
    let mut key = String::new();
    let mut stale = 0usize;
    for (pos, &i) in idxs.iter().enumerate() {
        let r = &rows[i];
        let unit = &units[pos];
        let Some((state, nrestarts)) = show.get(unit) else { continue };
        let uf = paths::unhealthyfile(&ctx.run, &r.name);

        let (hstate_degraded, hwhy) = if state != "active" {
            if halted {
                fs_ops::remove(&uf);
                continue;
            }
            (true, format!("unit is {state} — no writer"))
        } else {
            match health::probe(&r.health, ctx.health_timeout()) {
                Health::Degraded(why) => (true, why),
                _ => (false, String::new()),
            }
        };
        if !hstate_degraded {
            fs_ops::remove(&uf);
            continue;
        }

        let prev_at = fs_ops::read_u64_file(&uf);
        let Some(prev_at) = prev_at else {
            let _ = fs_ops::write_u64_file(&uf, now.max(0) as u64);
            continue;
        };
        let age = (now - prev_at as i64).max(0);
        // ONE SOURCE (per Ryan 2026-10-05): `notify_age()` already refuses rather than
        // defaulting on a malformed `SPIRA_NOTIFY_AGE` — `?` here, not `.unwrap_or(1800)`,
        // so that refusal actually reaches the caller instead of being discarded.
        if age < ctx.notify_age()? {
            continue;
        }

        let lf = paths::logfile(&ctx.run, &r.name, r.kind, &r.target);
        let last = lf.as_deref().and_then(fs_ops::last_line).unwrap_or_else(|| "(nothing)".to_string());
        let payload = format!("{} — DEGRADED: {hwhy}\nunit {unit} is {state}\nlast line written: {last}", r.name);
        let tried = match sop_run::attempt(ops, ctx, &r.name, &format!("{}|{}\n", r.name, hwhy), &payload) {
            Verdict::Handled => continue,
            Verdict::Escalate(note) => note,
        };
        stale += 1;
        let lock_line = if state != "active" { ops.orphan_lock(&r.target) } else { None };

        key.push_str(&format!("{}|{}\n", r.name, hwhy));
        report.push('\n');
        report.push_str(&format!("{} — DEGRADED for {}: {hwhy}\n", r.name, crate::cursor::format_age(age)));
        report.push_str(&format!("  {unit}, restarted {nrestarts} time(s) by systemd\n"));
        if let Some(ll) = &lock_line {
            report.push_str(&format!("  {ll}\n"));
        }
        report.push_str(&format!("  last line written: {last}\n"));
        if let Some(lf) = &lf {
            report.push_str(&format!("  {}\n", lf.display()));
        }
        if let Some(t) = tried {
            report.push_str(&format!("  {t}\n"));
        }
    }

    if stale == 0 {
        fs_ops::remove(&paths::watchd_dir(&ctx.run).join(HEALTH_STAMP));
        return Ok(NotifyOutcome { report: String::new(), code: 0 });
    }

    ask(
        ops,
        &ctx.run,
        HEALTH_STAMP,
        &key,
        HEALTH_SUBJECT,
        HEALTH_DEFAULT,
        HEALTH_WHY,
        &report,
    )?;
    Ok(NotifyOutcome { report, code: 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::fake::Fake;
    use testkit::TempDir;

    fn ctx(run: &str) -> Context {
        Context {
            run: run.to_string(),
            watchers: String::new(),
            watchers_overlay: String::new(),
            conf_file: "spira.conf".into(),
            actionable: crate::filter::DEFAULT.to_string(),
            health_timeout: "10".into(),
            notify_age: "1800".into(),
            id_prefix: String::new(),
            placeholders: Default::default(),
            systemctl: "systemctl".into(),
            systemctl_override: None,
            instance: "prod".into(),
            now: 1_790_726_400,
        }
    }

    fn row(name: &str, kind: Kind, target: &str, health: &str) -> Row {
        Row { name: name.into(), kind, target: target.into(), health: health.into() }
    }

    #[test_id(triukfa)]
    #[test]
    fn an_operator_alarms_condition_stands_while_its_escalation_stamp_does() {
        let run = testkit::TempDir::new("watchd-condition");
        let run_s = run.path().to_str().unwrap();
        assert_eq!(condition_of(BACKLOG_SUBJECT).as_deref(), Some("watchd:notify"));
        assert_eq!(condition_of(HEALTH_SUBJECT).as_deref(), Some("watchd:notify-health"));
        assert_eq!(condition_of("something else"), None);
        assert_eq!(condition_exit(run_s, "notify"), CONDITION_ENDED);
        std::fs::create_dir_all(paths::watchd_dir(run_s)).unwrap();
        std::fs::write(paths::watchd_dir(run_s).join(BACKLOG_STAMP), "fp").unwrap();
        assert_eq!(condition_exit(run_s, "notify"), 0, "positive control: the stamp stands");
        assert_eq!(condition_exit(run_s, "notify-health"), CONDITION_ENDED);
        assert_eq!(condition_exit(run_s, "../x"), 2);
    }

    #[test_id(t7en8w4)]
    #[test]
    fn fingerprints_are_stable_and_distinguish_different_keys() {
        assert_eq!(fingerprint("a"), fingerprint("a"));
        assert_ne!(fingerprint("a"), fingerprint("b"));
    }

    #[test_id(tbpqcgw)]
    #[test]
    fn a_fresh_actionable_backlog_does_not_escalate_before_the_age_threshold() {
        let d = TempDir::new("watchd-notify");
        let mut c = ctx(d.path().to_str().unwrap());
        c.notify_age = "1800".into();
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let lf = paths::logfile(&c.run, "pool", Kind::Daemon, "pool.sh").unwrap();
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(&lf, "pool: FAIL suite x\n").unwrap();
        let ops = Fake::default();
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out.code, 0, "{}", out.report);
        assert_eq!(ops.asks.borrow().len(), 0);
    }

    #[test_id(thxob33)]
    #[test]
    fn an_old_stamped_backlog_escalates_immediately_using_its_own_clock() {
        let d = TempDir::new("watchd-notify");
        let mut c = ctx(d.path().to_str().unwrap());
        c.notify_age = "60".into();
        c.now = iso8601::parse_utc("2026-09-30T01:00:00Z").unwrap();
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let lf = paths::logfile(&c.run, "pool", Kind::Daemon, "pool.sh").unwrap();
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(&lf, "[2026-09-30T00:00:00Z] pool: FAIL suite x\n").unwrap();
        let ops = Fake::default();
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out.code, 1, "{}", out.report);
        assert_eq!(ops.asks.borrow().len(), 1);
    }

    #[test_id(t4dqsx6)]
    #[test]
    fn an_unchanged_backlog_does_not_re_escalate_on_the_next_pass() {
        let d = TempDir::new("watchd-notify");
        let mut c = ctx(d.path().to_str().unwrap());
        c.notify_age = "60".into();
        c.now = iso8601::parse_utc("2026-09-30T01:00:00Z").unwrap();
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let lf = paths::logfile(&c.run, "pool", Kind::Daemon, "pool.sh").unwrap();
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(&lf, "[2026-09-30T00:00:00Z] pool: FAIL suite x\n").unwrap();
        let ops = Fake::default();
        cmd_notify(&rows, &ops, &c).unwrap();
        let out2 = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out2.code, 1, "notify still reports the standing condition");
        assert_eq!(ops.asks.borrow().len(), 1, "but does not mail a second time");
    }

    #[test_id(t54lq6m)]
    #[test]
    fn a_refused_ask_still_reports_the_finding_and_is_not_stamped() {
        let d = TempDir::new("watchd-notify");
        let mut c = ctx(d.path().to_str().unwrap());
        c.notify_age = "60".into();
        c.now = iso8601::parse_utc("2026-09-30T01:00:00Z").unwrap();
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let lf = paths::logfile(&c.run, "pool", Kind::Daemon, "pool.sh").unwrap();
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(&lf, "[2026-09-30T00:00:00Z] pool: FAIL suite x\n").unwrap();
        let out = cmd_notify(&rows, &Fake { refuse_asks: true, ..Fake::default() }, &c).unwrap();
        assert_eq!(out.code, 1, "{}", out.report);
        assert!(!paths::watchd_dir(&c.run).join("notify.escalated").exists());
    }

    #[test_id(t6t8xyn)]
    #[test]
    fn a_dead_daemon_escalates_through_notify_health() {
        let d = TempDir::new("watchd-notify");
        let mut c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let mut ops = Fake::default();
        let unit = paths::watch_unit_name("pool", "prod");
        ops.shown.insert(unit, ("failed".into(), "3".into()));
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out.code, 0, "first sighting starts the clock, does not fire yet: {}", out.report);
        // second pass, past the notify_age threshold.
        c.now += 3600;
        let out2 = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out2.code, 1, "{}", out2.report);
        assert_eq!(ops.asks.borrow().len(), 1);
    }

    #[test_id(tjv3g48)]
    #[test]
    fn a_halted_world_clears_the_unhealthy_clock_instead_of_escalating() {
        let d = TempDir::new("watchd-notify");
        let c = ctx(d.path().to_str().unwrap());
        std::fs::write(std::path::Path::new(&c.run).join("world.halted"), "").unwrap();
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let mut ops = Fake::default();
        ops.shown.insert(paths::watch_unit_name("pool", "prod"), ("inactive".into(), "0".into()));
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out.code, 0);
        assert_eq!(ops.asks.borrow().len(), 0);
    }

    #[test_id(t0ygavc)]
    #[test]
    fn an_unread_backlog_and_a_dead_watcher_escalate_as_two_asks_from_two_stamps() {
        let d = TempDir::new("watchd-notify");
        let mut c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", ""), row("gamma", Kind::Daemon, "gamma.sh", "")];
        let lf = paths::logfile(&c.run, "pool", Kind::Daemon, "pool.sh").unwrap();
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(&lf, "pool: FAIL suite x\n").unwrap();
        let mut ops = Fake::default();
        ops.shown.insert(paths::watch_unit_name("gamma", "prod"), ("failed".into(), "3".into()));
        assert_eq!(cmd_notify(&rows, &ops, &c).unwrap().code, 0);
        c.now += 3600;
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out.code, 1, "{}", out.report);
        assert_eq!(ops.asks.borrow().len(), 2);
        let wd = paths::watchd_dir(&c.run);
        assert!(wd.join("notify.escalated").exists() && wd.join("notify-health.escalated").exists());
        cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(ops.asks.borrow().len(), 2, "a pass at the same instant asks nothing new");
    }

    fn stale_pool(d: &TempDir) -> (Context, Vec<Row>) {
        let mut c = ctx(d.path().to_str().unwrap());
        c.notify_age = "60".into();
        c.now = iso8601::parse_utc("2026-09-30T01:00:00Z").unwrap();
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let lf = paths::logfile(&c.run, "pool", Kind::Daemon, "pool.sh").unwrap();
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(&lf, "[2026-09-30T00:00:00Z] pool: FAIL round batch=b42 ended with no verdict\n").unwrap();
        (c, rows)
    }

    fn recert_sop() -> Vec<(String, String)> {
        vec![("sop-recert".into(), "MATCH: FAIL round\nSYMPTOM: s\nCHECK: c\nFIX: f\nRUN: recert --batch {batch}\nDEADLINE: 60".into())]
    }

    #[test_id(tk62fsw)]
    #[test]
    fn a_stale_row_matching_a_runnable_sop_runs_the_tool_and_tells_the_concierge_instead_of_asking() {
        let d = TempDir::new("watchd-notify");
        let (c, rows) = stale_pool(&d);
        let ops = Fake { sops: recert_sop(), tool_result: Some(crate::ops::fake::ToolRunFake::Exit(0, "recertified b42\n".into())), ..Fake::default() };
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out.code, 0, "{}", out.report);
        assert_eq!(ops.asks.borrow().len(), 0);
        assert_eq!(ops.tool_calls.borrow().len(), 1);
        assert_eq!(ops.applied.borrow()[0].0, "sop-recert");
        assert!(ops.concierge_mail.borrow()[0].2.contains("recertified b42"));
    }

    #[test_id(ti3bua0)]
    #[test]
    fn a_failing_tool_still_escalates_and_the_ask_names_the_sop_and_its_exit_code() {
        let d = TempDir::new("watchd-notify");
        let (c, rows) = stale_pool(&d);
        let ops = Fake { sops: recert_sop(), tool_result: Some(crate::ops::fake::ToolRunFake::Exit(7, String::new())), ..Fake::default() };
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out.code, 1, "{}", out.report);
        let asks = ops.asks.borrow();
        assert_eq!(asks.len(), 1);
        assert!(asks[0].3.contains("sop-recert") && asks[0].3.contains("exit code 7"), "{}", asks[0].3);
    }

    #[test_id(tqjgml7)]
    #[test]
    fn a_failing_tool_on_a_stale_row_is_reported_with_one_tried_prefix_and_a_lone_runbook_with_none() {
        let d = TempDir::new("watchd-notify");
        let (c, rows) = stale_pool(&d);
        let ops = Fake { sops: recert_sop(), tool_result: Some(crate::ops::fake::ToolRunFake::Exit(7, String::new())), ..Fake::default() };
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert!(out.report.contains("  tried before escalating: ") && !out.report.contains("tried before escalating: tried"), "{}", out.report);

        let d = TempDir::new("watchd-notify");
        let (c, rows) = stale_pool(&d);
        let plain = recert_sop().into_iter().map(|(k, t)| (k, t.lines().filter(|l| !l.starts_with("RUN") && !l.starts_with("DEADLINE")).collect::<Vec<_>>().join("\n"))).collect();
        let ops = Fake { sops: plain, ..Fake::default() };
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert!(out.report.contains("  runbooks that matched: sop-recert") && !out.report.contains("tried"), "{}", out.report);
    }

    #[test_id(twc08ub)]
    #[test]
    fn the_third_pass_over_one_standing_incident_escalates_instead_of_running_a_third_time() {
        let d = TempDir::new("watchd-notify");
        let (c, rows) = stale_pool(&d);
        let ops = Fake { sops: recert_sop(), ..Fake::default() };
        for _ in 0..2 {
            assert_eq!(cmd_notify(&rows, &ops, &c).unwrap().code, 0);
        }
        assert_eq!(cmd_notify(&rows, &ops, &c).unwrap().code, 1);
        assert_eq!(ops.tool_calls.borrow().len(), 2);
        assert_eq!(ops.asks.borrow().len(), 1);
    }

    #[test_id(t6m2z7p)]
    #[test]
    fn a_dead_watcher_matching_a_runnable_sop_runs_the_tool_instead_of_asking() {
        let d = TempDir::new("watchd-notify");
        let mut c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let mut ops = Fake { sops: vec![("sop-dead".into(), "MATCH: no writer\nSYMPTOM: s\nCHECK: c\nFIX: f\nRUN: revive\nDEADLINE: 30".into())], ..Fake::default() };
        ops.shown.insert(paths::watch_unit_name("pool", "prod"), ("failed".into(), "3".into()));
        cmd_notify(&rows, &ops, &c).unwrap();
        c.now += 3600;
        let out = cmd_notify(&rows, &ops, &c).unwrap();
        assert_eq!(out.code, 0, "{}", out.report);
        assert_eq!(ops.asks.borrow().len(), 0);
        assert_eq!(ops.tool_calls.borrow()[0].0, "revive");
    }
}
