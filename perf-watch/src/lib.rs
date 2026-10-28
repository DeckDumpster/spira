//! One pass over the hot read paths: run each probe, time it end to end, and print one alarm
//! line for every probe past the limit. The clock and the runner are traits so a test drives
//! both and never waits on a real one.

use std::collections::BTreeMap;

pub mod happy;

pub struct Probe {
    pub name: String,
    pub argv: Vec<String>,
}

pub trait Clock {
    fn now_ms(&self) -> u64;
    fn epoch_secs(&self) -> u64;
}

pub trait Runner {
    fn run(&mut self, argv: &[String]) -> Result<String, String>;
    /// Time the last run measured itself where the caller's own throttle cannot reach.
    fn measured_ms(&self) -> Option<u64> {
        None
    }
    /// Where the last run says its own time went, as `name=ms,...`.
    fn phases(&self) -> Option<String> {
        None
    }
}

pub fn parse_probes(text: &str) -> Result<Vec<Probe>, String> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let Some((name, rest)) = l.split_once('\t') else {
            return Err(format!("line {}: expected <name><TAB><command>", i + 1));
        };
        let argv: Vec<String> = rest.split_whitespace().map(String::from).collect();
        if name.trim().is_empty() || argv.is_empty() {
            return Err(format!("line {}: a probe needs a name and a command", i + 1));
        }
        out.push(Probe { name: name.trim().to_string(), argv });
    }
    if out.is_empty() {
        return Err("no probes".to_string());
    }
    Ok(out)
}

/// Cumulative `caller/verb -> count` from `spira-lc stats`; empty when the answer is unreadable.
pub fn caller_counts(stats: &str) -> BTreeMap<String, u64> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stats) else { return BTreeMap::new() };
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for c in v["callers"].as_array().into_iter().flatten() {
        let (Some(who), Some(verb), Some(k)) = (c["caller"].as_str(), c["verb"].as_str(), c["count"].as_u64()) else { continue };
        *by.entry(format!("{who}/{verb}")).or_default() += k;
    }
    by
}

/// `caller/verb=N/min` over the window between two samples, busiest first. A counter that went
/// down means the server restarted, so its whole current count belongs to the window.
pub fn top_callers(prev: &BTreeMap<String, u64>, prev_at: u64, cur: &BTreeMap<String, u64>, now: u64, n: usize) -> Vec<String> {
    let secs = now.saturating_sub(prev_at).max(1) as f64;
    let mut v: Vec<(String, f64)> = cur
        .iter()
        .map(|(k, &c)| {
            let p = prev.get(k).copied().unwrap_or(0);
            (k.clone(), (if c >= p { c - p } else { c }) as f64 * 60.0 / secs)
        })
        .filter(|(_, r)| *r > 0.0)
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().take(n).map(|(k, r)| format!("{k}={r:.1}/min")).collect()
}

pub struct Timing {
    pub name: String,
    pub millis: u64,
    pub error: Option<String>,
    pub phases: Option<String>,
}

pub fn time_probes(probes: &[Probe], clock: &dyn Clock, runner: &mut dyn Runner) -> Vec<Timing> {
    probes
        .iter()
        .map(|p| {
            let t0 = clock.now_ms();
            let r = runner.run(&p.argv);
            let millis = runner.measured_ms().unwrap_or_else(|| clock.now_ms().saturating_sub(t0));
            Timing { name: p.name.clone(), millis, error: r.err(), phases: runner.phases() }
        })
        .collect()
}

pub fn alarms(timings: &[Timing], limit_ms: u64, callers: &[String]) -> Vec<String> {
    let top = if callers.is_empty() { "unknown".to_string() } else { callers.join(", ") };
    timings
        .iter()
        .filter_map(|t| match (&t.error, t.millis > limit_ms) {
            (Some(e), _) => Some(format!("PERF UNRUNNABLE {}: {e}", t.name)),
            (None, true) => Some(format!("PERF SLOW {} {}ms (limit {limit_ms}ms) top callers: {top}", t.name, t.millis)),
            _ => None,
        })
        .collect()
}

fn phase_sum(phases: &str) -> u64 {
    phases.split(',').filter_map(|p| p.split_once('=')?.1.parse::<u64>().ok()).sum()
}

/// One log row per over-budget probe: `<epoch>\t<probe>\t<ms>\t<phases>\trest=<ms>\t<top callers>`.
/// `rest` is the time no phase claims: process start, request building, and the wait before the
/// server saw the request.
pub fn slow_samples(epoch: u64, timings: &[Timing], limit_ms: u64, callers: &[String]) -> Vec<String> {
    let top = if callers.is_empty() { "unknown".to_string() } else { callers.join(", ") };
    timings
        .iter()
        .filter(|t| t.error.is_none() && t.millis > limit_ms)
        .map(|t| {
            let phases = t.phases.as_deref().unwrap_or("unreported");
            format!("{epoch}\t{}\t{}\t{phases}\trest={}\t{top}\n", t.name, t.millis, t.millis.saturating_sub(phase_sum(phases)))
        })
        .collect()
}

/// Every alarm line for one pass, and the stats sample to diff the next pass against (`None`
/// when stats were unreadable, so the older baseline stays).
#[allow(clippy::type_complexity)]
pub fn pass(
    probes: &[Probe],
    limit_ms: u64,
    stats_argv: &[String],
    prior: &Health,
    clock: &dyn Clock,
    runner: &mut dyn Runner,
) -> (Vec<Timing>, Vec<String>, Option<BTreeMap<String, u64>>, Vec<String>) {
    let timings = time_probes(probes, clock, runner);
    let cur = caller_counts(&runner.run(stats_argv).unwrap_or_default());
    let cur = (!cur.is_empty()).then_some(cur);
    let callers = match (&cur, prior.sample_at) {
        (Some(c), Some(at)) => top_callers(&prior.sample, at, c, clock.epoch_secs(), 3),
        _ => vec![],
    };
    let lines = alarms(&timings, limit_ms, &callers);
    (timings, lines, cur, callers)
}

pub struct Settings {
    pub budget_ms: u64,
    pub interval_secs: u64,
    pub remind_secs: u64,
}

/// What the health file says: when the last pass ran and when each standing alarm was last printed.
#[derive(Default, PartialEq, Debug)]
pub struct Health {
    pub last_pass: Option<u64>,
    pub alarms: BTreeMap<String, u64>,
    pub sample_at: Option<u64>,
    pub sample: BTreeMap<String, u64>,
}

impl Health {
    pub fn parse(text: &str) -> Health {
        let mut h = Health::default();
        for l in text.lines() {
            if let Some(n) = l.strip_prefix("pass ") {
                h.last_pass = n.trim().parse().ok();
            } else if let Some(n) = l.strip_prefix("sample ") {
                h.sample_at = n.trim().parse().ok();
            } else if let Some(r) = l.strip_prefix("count ") {
                if let Some((n, key)) = r.split_once('\t') {
                    if let Ok(n) = n.trim().parse() {
                        h.sample.insert(key.to_string(), n);
                    }
                }
            } else if let Some(r) = l.strip_prefix("alarm ") {
                if let Some((n, key)) = r.split_once('\t') {
                    if let Ok(n) = n.trim().parse() {
                        h.alarms.insert(key.to_string(), n);
                    }
                }
            }
        }
        h
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(n) = self.last_pass {
            out.push_str(&format!("pass {n}\n"));
        }
        if let Some(n) = self.sample_at {
            out.push_str(&format!("sample {n}\n"));
        }
        for (k, n) in &self.sample {
            out.push_str(&format!("count {n}\t{k}\n"));
        }
        for (k, n) in &self.alarms {
            out.push_str(&format!("alarm {n}\t{k}\n"));
        }
        out
    }
}

fn alarm_key(line: &str) -> String {
    line.split_whitespace().take(3).collect::<Vec<_>>().join(" ").trim_end_matches(':').to_string()
}

/// The Concierge inbox line for one alarm: `<UTC ISO time> [watch:perf-watch] <alarm>`.
pub fn inbox_line(epoch: u64, alarm: &str) -> String {
    let (days, rem) = (epoch / 86400, epoch % 86400);
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z [watch:perf-watch] {alarm}\n", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn append_inbox(path: &str, line: &str) {
    if path.is_empty() {
        return;
    }
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = std::io::Write::write_all(&mut f, line.as_bytes());
    }
}

pub struct Pass {
    pub lines: Vec<String>,
    pub slow: Vec<String>,
    pub timings: Vec<Timing>,
    pub health: Health,
}

/// A pass younger than the interval is skipped (`None`). Otherwise the alarms to print — a
/// standing one only once per remind interval — and the health to write back.
pub fn run_pass(
    cfg: &Settings,
    probes: &[Probe],
    stats_argv: &[String],
    prior: &Health,
    clock: &dyn Clock,
    runner: &mut dyn Runner,
) -> Option<Pass> {
    let now = clock.epoch_secs();
    if let Some(last) = prior.last_pass {
        if now.saturating_sub(last) < cfg.interval_secs {
            return None;
        }
    }
    let (timings, all, cur, callers) = pass(probes, cfg.budget_ms, stats_argv, prior, clock, runner);
    let mut health = Health { last_pass: Some(now), ..Health::default() };
    match cur {
        Some(c) => (health.sample_at, health.sample) = (Some(now), c),
        None => (health.sample_at, health.sample) = (prior.sample_at, prior.sample.clone()),
    }
    let mut lines = Vec::new();
    for l in all {
        let key = alarm_key(&l);
        let printed = match prior.alarms.get(&key) {
            Some(&at) if now.saturating_sub(at) < cfg.remind_secs => at,
            _ => {
                lines.push(l);
                now
            }
        };
        health.alarms.insert(key, printed);
    }
    let slow = slow_samples(now, &timings, cfg.budget_ms, &callers);
    Some(Pass { lines, slow, timings, health })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Stub {
        now: Cell<u64>,
        epoch: Cell<u64>,
    }
    impl Clock for Stub {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn epoch_secs(&self) -> u64 {
            self.epoch.get()
        }
    }

    struct Scripted<'a> {
        clock: &'a Stub,
        costs: BTreeMap<String, u64>,
        stats: String,
        asked_stats: bool,
    }
    impl Runner for Scripted<'_> {
        fn run(&mut self, argv: &[String]) -> Result<String, String> {
            if argv[0] == "stats" {
                self.asked_stats = true;
                return Ok(self.stats.clone());
            }
            match self.costs.get(&argv[0]) {
                Some(ms) => {
                    self.clock.now.set(self.clock.now.get() + ms);
                    Ok(String::new())
                }
                None => Err("not found".to_string()),
            }
        }
    }

    const PROBES: &str = "# hot paths\nlist\tlist --live\nshow\tshow sp-1\nmissing\tgone\n";
    const STATS: &str = r#"{"callers":[{"verb":"list","caller":"watcher","count":28},{"verb":"list","caller":"cockpit","count":5},{"verb":"show","caller":"watcher","count":2},{"verb":"event","caller":"sentinel","count":1}]}"#;

    fn run(costs: &[(&str, u64)], limit: u64) -> (Vec<String>, bool) {
        let clock = Stub { now: Cell::new(0), epoch: Cell::new(60) };
        let mut r = Scripted { clock: &clock, costs: costs.iter().map(|(k, v)| (k.to_string(), *v)).collect(), stats: STATS.into(), asked_stats: false };
        let prior = Health { sample_at: Some(0), ..Health::default() };
        let (_, lines, _, _) = pass(&parse_probes(PROBES).unwrap(), limit, &["stats".to_string()], &prior, &clock, &mut r);
        (lines, r.asked_stats)
    }

    #[test]
    fn inbox_line_stamps_utc_and_tags_the_watcher() {
        assert_eq!(inbox_line(1_791_748_794, "PERF SLOW x 9ms"), "2026-10-11T19:59:54Z [watch:perf-watch] PERF SLOW x 9ms\n");
        assert_eq!(inbox_line(0, "a"), "1970-01-01T00:00:00Z [watch:perf-watch] a\n");
    }

    #[test]
    fn a_standing_alarm_is_one_inbox_row_and_returns_after_the_remind_interval() {
        let clock = Stub { now: Cell::new(0), epoch: Cell::new(1_000_000) };
        let probes = parse_probes(PROBES).unwrap();
        let cfg = Settings { budget_ms: 500, interval_secs: 0, remind_secs: 1800 };
        let dir = testkit::TempDir::new("perf-inbox");
        let inbox = dir.path().join("inbox");
        let mut health = Health::default();
        let pass_at = |epoch: u64, health: &mut Health| {
            clock.epoch.set(epoch);
            let mut r = Scripted { clock: &clock, costs: [("list".to_string(), 730), ("show".to_string(), 1), ("gone".to_string(), 0)].into(), stats: STATS.into(), asked_stats: false };
            let out = run_pass(&cfg, &probes, &["stats".to_string()], health, &clock, &mut r).unwrap();
            for l in &out.lines {
                append_inbox(&inbox.to_string_lossy(), &inbox_line(epoch, l));
            }
            *health = out.health;
        };
        let rows = || std::fs::read_to_string(&inbox).unwrap_or_default().lines().count();
        pass_at(1_000_000, &mut health);
        pass_at(1_000_300, &mut health);
        assert_eq!(rows(), 1, "a standing alarm is one row, not one per pass");
        pass_at(1_001_900, &mut health);
        assert_eq!(rows(), 2, "still over the limit 30 minutes on, it is raised again");
    }

    #[test]
    fn a_probe_past_the_limit_alarms_with_its_time_and_the_top_callers() {
        let (lines, asked) = run(&[("list", 730), ("show", 120), ("gone", 0)], 500);
        assert!(asked);
        assert!(lines.contains(&"PERF SLOW list 730ms (limit 500ms) top callers: watcher/list=28.0/min, cockpit/list=5.0/min, watcher/show=2.0/min".to_string()), "{lines:?}");
        assert!(lines.iter().all(|l| !l.contains(" show ")), "{lines:?}");
    }

    struct SelfTimed<'a> {
        clock: &'a Stub,
        inside: u64,
    }
    impl Runner for SelfTimed<'_> {
        fn run(&mut self, _: &[String]) -> Result<String, String> {
            self.clock.now.set(self.clock.now.get() + 610);
            Ok(String::new())
        }
        fn measured_ms(&self) -> Option<u64> {
            Some(self.inside)
        }
    }

    #[test]
    fn a_probe_that_timed_itself_is_charged_its_own_time_not_the_throttled_wall_clock() {
        let probes = parse_probes("list\tlist --live\n").unwrap();
        let clock = Stub { now: Cell::new(0), epoch: Cell::new(0) };
        let mut r = SelfTimed { clock: &clock, inside: 16 };
        let t = time_probes(&probes, &clock, &mut r);
        assert_eq!(t[0].millis, 16);
        assert!(alarms(&t, 500, &[]).is_empty());
        r.inside = 700;
        let t = time_probes(&probes, &clock, &mut r);
        assert_eq!(alarms(&t, 500, &[]).len(), 1);
    }

    struct Phased<'a>(&'a Stub);
    impl Runner for Phased<'_> {
        fn run(&mut self, _: &[String]) -> Result<String, String> {
            self.0.now.set(self.0.now.get() + 2100);
            Ok(String::new())
        }
        fn phases(&self) -> Option<String> {
            Some("reply=40,push=2000".to_string())
        }
    }

    #[test]
    fn an_over_budget_sample_names_where_its_time_went_and_a_quick_one_is_not_logged() {
        let clock = Stub { now: Cell::new(0), epoch: Cell::new(77) };
        let probes = parse_probes("list\tlist --live\n").unwrap();
        let t = time_probes(&probes, &clock, &mut Phased(&clock));
        let rows = slow_samples(77, &t, 500, &["watcher/list=3.0/min".to_string()]);
        assert_eq!(rows, vec!["77\tlist\t2100\treply=40,push=2000\trest=60\twatcher/list=3.0/min\n".to_string()]);
        assert!(slow_samples(77, &t, 2100, &[]).is_empty(), "at the limit is not slow");
    }

    #[test]
    fn exactly_the_limit_is_not_slow() {
        let (lines, _) = run(&[("list", 500), ("show", 1), ("gone", 0)], 500);
        assert!(lines.is_empty(), "{lines:?}");
    }

    fn counts(rows: &[(&str, u64)]) -> BTreeMap<String, u64> {
        rows.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn a_steady_caller_outranks_a_historical_burst_whose_live_rate_is_zero() {
        let prev = counts(&[("gh-intake/show", 12_430), ("batcher/rounds", 100), ("idle/x", 5)]);
        let cur = counts(&[("gh-intake/show", 12_430), ("batcher/rounds", 126), ("idle/x", 5)]);
        assert_eq!(top_callers(&prev, 1000, &cur, 1060, 3), vec!["batcher/rounds=26.0/min".to_string()]);
    }

    #[test]
    fn a_restarted_counter_counts_from_zero_and_a_window_scales_to_minutes() {
        let prev = counts(&[("a/x", 900)]);
        let cur = counts(&[("a/x", 30), ("b/y", 60)]);
        assert_eq!(top_callers(&prev, 0, &cur, 300, 3), vec!["b/y=12.0/min".to_string(), "a/x=6.0/min".to_string()]);
    }

    #[test]
    fn with_no_baseline_the_callers_are_unknown_and_the_pass_stores_a_baseline() {
        let first = stepped(1000, &Health::default(), 900).unwrap();
        assert!(first.lines[0].ends_with("top callers: unknown"), "{:?}", first.lines);
        assert_eq!(first.health.sample_at, Some(1000));
        assert_eq!(first.health.sample.get("watcher/list"), Some(&28));
    }

    #[test]
    fn a_probe_that_cannot_run_is_its_own_alarm_never_a_fast_pass() {
        let (lines, _) = run(&[("list", 1), ("show", 1)], 500);
        assert_eq!(lines, vec!["PERF UNRUNNABLE missing: not found".to_string()]);
    }

    #[test]
    fn a_malformed_probe_file_is_refused() {
        assert!(parse_probes("no tab here").is_err());
        assert!(parse_probes("# only a comment\n").is_err());
    }

    fn stepped(epoch: u64, prior: &Health, cost: u64) -> Option<Pass> {
        let clock = Stub { now: Cell::new(0), epoch: Cell::new(epoch) };
        let mut r = Scripted { clock: &clock, costs: [("list", cost), ("show", 1), ("gone", 0)].iter().map(|(k, v)| (k.to_string(), *v)).collect(), stats: STATS.into(), asked_stats: false };
        let cfg = Settings { budget_ms: 500, interval_secs: 60, remind_secs: 600 };
        run_pass(&cfg, &parse_probes(PROBES).unwrap(), &["stats".to_string()], prior, &clock, &mut r)
    }

    #[test]
    fn a_pass_younger_than_the_interval_does_not_probe() {
        let h = Health { last_pass: Some(1000), ..Health::default() };
        assert!(stepped(1059, &h, 900).is_none());
        assert_eq!(stepped(1060, &h, 900).unwrap().health.last_pass, Some(1060));
    }

    #[test]
    fn a_standing_alarm_repeats_only_after_the_remind_interval_and_a_new_one_is_immediate() {
        let first = stepped(1000, &Health::default(), 900).unwrap();
        assert_eq!(first.lines.len(), 1, "{:?}", first.lines);
        let quiet = stepped(1100, &first.health, 900).unwrap();
        assert!(quiet.lines.is_empty(), "{:?}", quiet.lines);
        assert_eq!(quiet.health.alarms, first.health.alarms);
        let again = stepped(1000 + 600, &first.health, 900).unwrap();
        assert_eq!(again.lines.len(), 1);
    }

    #[test]
    fn a_cleared_alarm_is_forgotten_so_its_return_prints_at_once() {
        let first = stepped(1000, &Health::default(), 900).unwrap();
        let cleared = stepped(1100, &first.health, 1).unwrap();
        assert!(cleared.lines.is_empty(), "{:?}", cleared.lines);
        assert!(cleared.health.alarms.is_empty());
        assert!(!cleared.health.alarms.keys().any(|k| k.contains("SLOW")));
        let back = stepped(1200, &cleared.health, 900).unwrap();
        assert!(back.lines.iter().any(|l| l.contains("PERF SLOW list")), "{:?}", back.lines);
    }

    #[test]
    fn health_round_trips_through_its_file() {
        let first = stepped(1000, &Health::default(), 900).unwrap();
        assert_eq!(Health::parse(&first.health.render()), first.health);
        assert_eq!(Health::parse("garbage"), Health::default());
    }
}
