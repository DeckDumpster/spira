//! CPU-quota throttling of the units the loop's throughput rides on. A unit whose cgroup
//! `cpu.stat` shows `nr_throttled`/`nr_periods` high is being slowed by a quota, and nothing
//! else reports it: the symptom is timeouts elsewhere. The ratio is taken over the interval
//! since the previous pass, never over uptime — a lifetime ratio hides a throttle that
//! began an hour ago and shouts about one fixed yesterday. A unit with no quota never
//! advances `nr_periods`, so it can never read as throttled.

use std::collections::BTreeMap;
use std::path::Path;

pub struct Cfg {
    pub units: Vec<String>,
    pub warn_pct: i64,
    pub min_periods: i64,
    pub cgroup_root: String,
    pub systemctl: String,
}

pub const DEFAULT_UNITS: &str = "dolt-beads.service spira-landing-pass.service spira-sentinel.service spira-summon.service spira-verdict.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub periods: i64,
    pub throttled: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// cpu.stat unreadable or the unit has no cgroup: unknown, never clear.
    Unknown,
    /// First sight of this unit (or counters went backwards): no interval yet.
    Baseline,
    /// Too few periods advanced for a ratio to mean anything — the unit is idle or unfenced.
    Idle,
    Measured { periods: i64, throttled: i64 },
}

pub fn parse_cpu_stat(text: &str) -> Option<Sample> {
    let mut periods = None;
    let mut throttled = None;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match (it.next(), it.next().and_then(|v| v.parse::<i64>().ok())) {
            (Some("nr_periods"), Some(v)) => periods = Some(v),
            (Some("nr_throttled"), Some(v)) => throttled = Some(v),
            _ => {}
        }
    }
    Some(Sample { periods: periods?, throttled: throttled? })
}

pub fn read(prev: Option<Sample>, now: Option<Sample>, min_periods: i64) -> Reading {
    let Some(n) = now else { return Reading::Unknown };
    let Some(p) = prev else { return Reading::Baseline };
    let dp = n.periods - p.periods;
    let dt = n.throttled - p.throttled;
    if dp < 0 || dt < 0 {
        return Reading::Baseline;
    }
    if dp < min_periods {
        return Reading::Idle;
    }
    Reading::Measured { periods: dp, throttled: dt }
}

pub fn breached(r: &Reading, warn_pct: i64) -> bool {
    matches!(r, Reading::Measured { periods, throttled } if throttled * 100 > warn_pct * periods)
}

pub fn disp(r: &Reading, warn_pct: i64) -> String {
    match r {
        Reading::Unknown => "?".into(),
        Reading::Baseline => "baseline".into(),
        Reading::Idle => "idle".into(),
        Reading::Measured { periods, throttled } => {
            let pct = throttled * 100 / periods;
            if breached(r, warn_pct) {
                format!("FAULT ({pct}% of {periods} periods throttled, warn above {warn_pct}%)")
            } else {
                format!("{pct}%")
            }
        }
    }
}

pub fn parse_state(text: &str) -> BTreeMap<String, Sample> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if let [u, a, b] = p[..] {
            if let (Ok(periods), Ok(throttled)) = (a.parse(), b.parse()) {
                m.insert(u.to_string(), Sample { periods, throttled });
            }
        }
    }
    m
}

pub fn render_state(m: &BTreeMap<String, Sample>) -> String {
    m.iter().map(|(u, s)| format!("{u} {} {}\n", s.periods, s.throttled)).collect()
}

fn unit_sample(cfg: &Cfg, unit: &str) -> Option<Sample> {
    let out = std::process::Command::new(&cfg.systemctl)
        .args(["--user", "show", "-p", "ControlGroup", "--value", unit])
        .output()
        .ok()?;
    let cg = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || cg.is_empty() {
        return None;
    }
    let path = Path::new(&cfg.cgroup_root).join(cg.trim_start_matches('/')).join("cpu.stat");
    parse_cpu_stat(&std::fs::read_to_string(path).ok()?)
}

pub struct Row {
    pub unit: String,
    pub reading: Reading,
}

/// One pass: sample every unit, compare with the state file, persist the new samples.
/// Units that could not be read keep their previous sample so one failed read does not
/// forfeit the next interval.
pub fn gather(cfg: &Cfg, state_path: &Path) -> Vec<Row> {
    let mut prev = std::fs::read_to_string(state_path).map(|t| parse_state(&t)).unwrap_or_default();
    let mut rows = Vec::new();
    for unit in &cfg.units {
        let now = unit_sample(cfg, unit);
        rows.push(Row { unit: unit.clone(), reading: read(prev.get(unit).copied(), now, cfg.min_periods) });
        if let Some(s) = now {
            prev.insert(unit.clone(), s);
        }
    }
    let _ = std::fs::write(state_path, render_state(&prev));
    rows
}

pub fn summarize(rows: &[Row], warn_pct: i64) -> (bool, String) {
    let breach = rows.iter().any(|r| breached(&r.reading, warn_pct));
    let line = rows
        .iter()
        .map(|r| format!("{}={}", r.unit.trim_end_matches(".service"), disp(&r.reading, warn_pct)))
        .collect::<Vec<_>>()
        .join(" ");
    (breach, line)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "usage_usec 100\nnr_periods 1088059\nnr_throttled 657510\nthrottled_usec 5\n";

    fn s(p: i64, t: i64) -> Option<Sample> {
        Some(Sample { periods: p, throttled: t })
    }

    #[test]
    fn parses_a_real_shaped_cpu_stat() {
        assert_eq!(parse_cpu_stat(STAT), s(1088059, 657510));
    }

    #[test]
    fn a_cpu_stat_missing_the_throttle_counters_is_unreadable() {
        assert_eq!(parse_cpu_stat("usage_usec 1\n"), None);
    }

    #[test]
    fn the_production_incident_numbers_breach() {
        let r = read(s(0, 0), s(1088059, 657510), 100);
        assert!(breached(&r, 5));
        assert!(disp(&r, 5).starts_with("FAULT (60%"));
    }

    #[test]
    fn five_percent_exactly_is_not_a_breach_and_just_over_is() {
        assert!(!breached(&read(s(0, 0), s(1000, 50), 100), 5));
        assert!(breached(&read(s(0, 0), s(1000, 51), 100), 5));
    }

    #[test]
    fn the_ratio_is_the_interval_not_the_lifetime() {
        // Heavily throttled history, clean interval since.
        assert!(!breached(&read(s(1000, 900), s(2000, 900), 100), 5));
        // Clean history, throttled interval.
        assert!(breached(&read(s(1000, 0), s(2000, 500), 100), 5));
    }

    #[test]
    fn an_unfenced_unit_never_advances_periods_and_reads_idle() {
        assert_eq!(read(s(0, 0), s(0, 0), 100), Reading::Idle);
        assert!(!breached(&Reading::Idle, 5));
    }

    #[test]
    fn a_throttle_with_too_few_periods_is_not_load() {
        assert_eq!(read(s(0, 0), s(10, 10), 100), Reading::Idle);
    }

    #[test]
    fn unreadable_is_unknown_and_never_a_breach_or_a_clear() {
        let r = read(s(0, 0), None, 100);
        assert_eq!(r, Reading::Unknown);
        assert_eq!(disp(&r, 5), "?");
        assert!(!breached(&r, 5));
    }

    #[test]
    fn first_sight_and_counter_reset_are_a_baseline() {
        assert_eq!(read(None, s(5000, 5000), 100), Reading::Baseline);
        assert_eq!(read(s(5000, 10), s(100, 0), 100), Reading::Baseline);
    }

    #[test]
    fn state_round_trips_and_skips_junk() {
        let mut m = BTreeMap::new();
        m.insert("a.service".to_string(), Sample { periods: 3, throttled: 1 });
        let text = render_state(&m);
        assert_eq!(parse_state(&format!("{text}garbage\nx y z\n")), m);
    }

    fn fixture(root: &Path, cg: &str, stat: &str) {
        let d = root.join(cg);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("cpu.stat"), stat).unwrap();
    }

    fn fake_systemctl(dir: &Path) -> String {
        let p = dir.join("systemctl");
        std::fs::write(&p, "#!/bin/sh\ncase \"$6\" in dolt-beads.service) echo /app/dolt;; spira-sentinel.service) echo /app/sentinel;; *) exit 1;; esac\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn gather_flags_a_throttled_unit_across_two_passes_from_fixture_cpu_stat() {
        let d = testkit::TempDir::new("wt-cpu-throttle");
        let root = d.join("cg");
        fixture(&root, "app/dolt", "nr_periods 1000\nnr_throttled 0\n");
        fixture(&root, "app/sentinel", "nr_periods 1000\nnr_throttled 0\n");
        let cfg = Cfg {
            units: vec!["dolt-beads.service".into(), "spira-sentinel.service".into(), "spira-gone.service".into()],
            warn_pct: 5,
            min_periods: 100,
            cgroup_root: root.to_string_lossy().into_owned(),
            systemctl: fake_systemctl(&d.join("")),
        };
        let state = d.join("state");
        let first = gather(&cfg, &state);
        assert!(first.iter().take(2).all(|r| r.reading == Reading::Baseline));
        assert_eq!(first[2].reading, Reading::Unknown);

        fixture(&root, "app/dolt", "nr_periods 2000\nnr_throttled 600\n");
        fixture(&root, "app/sentinel", "nr_periods 2000\nnr_throttled 10\n");
        let second = gather(&cfg, &state);
        let (breach, line) = summarize(&second, 5);
        assert!(breach);
        assert!(line.contains("dolt-beads=FAULT (60%"), "{line}");
        assert!(line.contains("spira-sentinel=1%"), "{line}");
        assert!(line.contains("spira-gone=?"), "{line}");
    }
}
