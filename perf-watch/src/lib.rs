//! One pass over the hot read paths: run each probe, time it end to end, and print one alarm
//! line for every probe past the limit. The clock and the runner are traits so a test drives
//! both and never waits on a real one.

use std::collections::BTreeMap;

pub struct Probe {
    pub name: String,
    pub argv: Vec<String>,
}

pub trait Clock {
    fn now_ms(&self) -> u64;
}

pub trait Runner {
    fn run(&mut self, argv: &[String]) -> Result<String, String>;
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

/// `caller/verb=count`, busiest first, from `spira-lc stats`; empty when the answer is unreadable.
pub fn top_callers(stats: &str, n: usize) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stats) else { return vec![] };
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for c in v["callers"].as_array().into_iter().flatten() {
        let (Some(who), Some(verb), Some(k)) = (c["caller"].as_str(), c["verb"].as_str(), c["count"].as_u64()) else { continue };
        *by.entry(format!("{who}/{verb}")).or_default() += k;
    }
    let mut v: Vec<(String, u64)> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().take(n).map(|(k, c)| format!("{k}={c}")).collect()
}

pub struct Timing {
    pub name: String,
    pub millis: u64,
    pub error: Option<String>,
}

pub fn time_probes(probes: &[Probe], clock: &dyn Clock, runner: &mut dyn Runner) -> Vec<Timing> {
    probes
        .iter()
        .map(|p| {
            let t0 = clock.now_ms();
            let r = runner.run(&p.argv);
            Timing { name: p.name.clone(), millis: clock.now_ms().saturating_sub(t0), error: r.err() }
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

/// Every alarm line for one pass; `stats` is asked only when something is slow.
pub fn pass(probes: &[Probe], limit_ms: u64, stats_argv: &[String], clock: &dyn Clock, runner: &mut dyn Runner) -> (Vec<Timing>, Vec<String>) {
    let timings = time_probes(probes, clock, runner);
    let any = timings.iter().any(|t| t.error.is_none() && t.millis > limit_ms);
    let callers = if any { top_callers(&runner.run(stats_argv).unwrap_or_default(), 3) } else { vec![] };
    let lines = alarms(&timings, limit_ms, &callers);
    (timings, lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Stub {
        now: Cell<u64>,
    }
    impl Clock for Stub {
        fn now_ms(&self) -> u64 {
            self.now.get()
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
        let clock = Stub { now: Cell::new(0) };
        let mut r = Scripted { clock: &clock, costs: costs.iter().map(|(k, v)| (k.to_string(), *v)).collect(), stats: STATS.into(), asked_stats: false };
        let (_, lines) = pass(&parse_probes(PROBES).unwrap(), limit, &["stats".to_string()], &clock, &mut r);
        (lines, r.asked_stats)
    }

    #[test]
    fn a_probe_past_the_limit_alarms_with_its_time_and_the_top_callers() {
        let (lines, asked) = run(&[("list", 730), ("show", 120), ("gone", 0)], 500);
        assert!(asked);
        assert!(lines.contains(&"PERF SLOW list 730ms (limit 500ms) top callers: watcher/list=28, cockpit/list=5, watcher/show=2".to_string()), "{lines:?}");
        assert!(lines.iter().all(|l| !l.contains(" show ")), "{lines:?}");
    }

    #[test]
    fn exactly_the_limit_is_not_slow_and_a_quiet_pass_never_asks_for_callers() {
        let (lines, asked) = run(&[("list", 500), ("show", 1), ("gone", 0)], 500);
        assert!(lines.is_empty(), "{lines:?}");
        assert!(!asked);
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
}
