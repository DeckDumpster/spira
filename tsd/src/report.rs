//! Certification throughput per repo and instance type, computed from `cert-pass` rows only.
//! Submission time is not in a pass record, so "submitted to landed" is measured from the start
//! of the first pass that carried the bead (its row's `ts` less the pass's own phases) to the
//! end of the green pass that landed it.

use crate::pass::{Cause, FAMILY};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub const SUMMARY_FAMILY: &str = "cert-throughput";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass {
    pub repo: String,
    pub cell: String,
    pub ec2: bool,
    pub members: Vec<String>,
    pub wait_s: u64,
    pub boot_s: u64,
    pub work_s: u64,
    pub cause: Cause,
    pub end: u64,
}

impl Pass {
    fn start(&self) -> u64 {
        self.end.saturating_sub(self.wait_s + self.boot_s + self.work_s)
    }
}

/// Rows of the family in `text`; a later row for the same `(repo, batch, pass)` replaces an
/// earlier one. The second value counts lines of the family that could not be read as a pass.
pub fn parse_passes(text: &str) -> (Vec<Pass>, usize) {
    let mut by_key: BTreeMap<(String, String, String), Pass> = BTreeMap::new();
    let mut unreadable = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        if row.get("family").and_then(Value::as_str) != Some(FAMILY) {
            continue;
        }
        let end = row.get("ts").and_then(Value::as_str).and_then(crate::parse_ts);
        let (Ok(r), Some(end)) = (crate::pass::PassRecord::from_row(&row), end) else {
            unreadable += 1;
            continue;
        };
        let cell = if r.instance_type.is_empty() { r.place.as_str().to_string() } else { r.instance_type.clone() };
        by_key.insert(
            (r.repo.clone(), r.batch, r.pass),
            Pass {
                repo: r.repo,
                cell,
                ec2: r.place == crate::pass::Place::Ec2,
                members: r.members,
                wait_s: r.capacity_wait_s,
                boot_s: r.boot_s,
                work_s: r.build_s + r.suites_s,
                cause: r.cause,
                end,
            },
        );
    }
    (by_key.into_values().collect(), unreadable)
}

/// `type=usd_per_hour,...`
pub fn parse_prices(spec: &str) -> Result<BTreeMap<String, f64>, String> {
    let mut m = BTreeMap::new();
    for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (t, p) = item.split_once('=').ok_or_else(|| format!("price {item:?} is not type=usd_per_hour"))?;
        let p: f64 = p.trim().parse().ok().filter(|p: &f64| p.is_finite() && *p >= 0.0).ok_or_else(|| format!("price {item:?} is not a non-negative number"))?;
        m.insert(t.trim().to_string(), p);
    }
    Ok(m)
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Cell {
    pub passes: u64,
    pub wait_s: u64,
    pub wall_s: u64,
    pub billed_s: u64,
    pub lost: BTreeMap<&'static str, u64>,
    pub landed: u64,
    pub latency_s: Vec<u64>,
    pub passes_per_landed: Vec<u64>,
    pub ec2: bool,
}

impl Cell {
    pub fn per_hour(&self, window_s: u64) -> f64 {
        self.landed as f64 * 3600.0 / window_s.max(1) as f64
    }

    pub fn wait_share(&self) -> Option<f64> {
        (self.wall_s > 0).then(|| self.wait_s as f64 / self.wall_s as f64)
    }

    pub fn lost_total(&self) -> u64 {
        self.lost.values().sum()
    }

    pub fn mean_passes(&self) -> Option<f64> {
        (!self.passes_per_landed.is_empty()).then(|| self.passes_per_landed.iter().sum::<u64>() as f64 / self.passes_per_landed.len() as f64)
    }

    pub fn latency_pct(&self, pct: u64) -> Option<u64> {
        let mut v = self.latency_s.clone();
        v.sort_unstable();
        let rank = (pct * v.len() as u64).div_ceil(100).max(1) as usize;
        v.get(rank.wrapping_sub(1)).copied()
    }
}

/// `None` when the cell is not EC2 or the type has no price; the cost is the cell's billed
/// seconds (boot, build and suites; the lease wait bills nothing) at the type's hourly rate.
pub fn cost_usd(cell: &str, c: &Cell, prices: &BTreeMap<String, f64>) -> Option<f64> {
    c.ec2.then_some(())?;
    prices.get(cell).map(|p| c.billed_s as f64 / 3600.0 * p)
}

#[derive(Debug)]
pub struct Report {
    pub from: u64,
    pub to: u64,
    pub cells: BTreeMap<(String, String), Cell>,
}

const LOST: [Cause; 4] = [Cause::OverCap, Cause::VmLost, Cause::Infra, Cause::ContentionFlake];

/// Passes ending in `[from, to)`. A bead lands at its first green pass, whenever that was, and is
/// counted in the cell of that pass if it falls in the window.
pub fn compute(passes: &[Pass], from: u64, to: u64) -> Report {
    let mut cells: BTreeMap<(String, String), Cell> = BTreeMap::new();
    let in_window = |p: &Pass| p.end >= from && p.end < to;
    for p in passes.iter().filter(|p| in_window(p)) {
        let c = cells.entry((p.repo.clone(), p.cell.clone())).or_default();
        c.ec2 = p.ec2;
        c.passes += 1;
        c.wait_s += p.wait_s;
        c.wall_s += p.wait_s + p.boot_s + p.work_s;
        if p.ec2 {
            c.billed_s += p.boot_s + p.work_s;
        }
        if LOST.contains(&p.cause) {
            *c.lost.entry(p.cause.as_str()).or_default() += 1;
        }
    }
    let mut carried: BTreeMap<(&str, &str), Vec<&Pass>> = BTreeMap::new();
    for p in passes {
        for m in &p.members {
            carried.entry((p.repo.as_str(), m.as_str())).or_default().push(p);
        }
    }
    let mut ordered: Vec<&Pass> = passes.iter().collect();
    ordered.sort_by_key(|p| p.end);
    let mut landed: HashSet<(&str, &str)> = HashSet::new();
    for p in ordered.into_iter().filter(|p| p.cause == Cause::Green) {
        for m in &p.members {
            if !landed.insert((p.repo.as_str(), m.as_str())) || !in_window(p) {
                continue;
            }
            let all = &carried[&(p.repo.as_str(), m.as_str())];
            let first = all.iter().map(|q| q.start()).min().unwrap_or(p.end);
            let c = cells.entry((p.repo.clone(), p.cell.clone())).or_default();
            c.ec2 = p.ec2;
            c.landed += 1;
            c.latency_s.push(p.end.saturating_sub(first));
            c.passes_per_landed.push(all.iter().filter(|q| q.end <= p.end).count() as u64);
        }
    }
    Report { from, to, cells }
}

fn dur(s: Option<u64>) -> String {
    match s {
        None => "-".into(),
        Some(s) if s >= 3600 => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
        Some(s) => format!("{}m{:02}s", s / 60, s % 60),
    }
}

fn usd(v: Option<f64>) -> String {
    v.map_or("-".into(), |v| format!("${v:.2}"))
}

fn per_bead(cell: &str, c: &Cell, prices: &BTreeMap<String, f64>) -> Option<f64> {
    (c.landed > 0).then(|| cost_usd(cell, c, prices).map(|t| t / c.landed as f64))?
}

impl Report {
    fn window_s(&self) -> u64 {
        self.to - self.from
    }

    pub fn text(&self, prices: &BTreeMap<String, f64>) -> String {
        let mut out = format!("certification throughput {} .. {} ({:.1}h)\n", crate::iso_utc(self.from), crate::iso_utc(self.to), self.window_s() as f64 / 3600.0);
        if self.cells.is_empty() {
            out.push_str("no passes in the window\n");
        }
        for ((repo, cell), c) in &self.cells {
            let lost: Vec<String> = c.lost.iter().map(|(k, v)| format!("{k}={v}")).collect();
            out.push_str(&format!(
                "\n{repo} / {cell}\n  passes             {} (n={})\n  landed per hour    {:.2} (n={} landed)\n  first pass->landed p50 {} p95 {} (n={})\n  passes per landed  {} (n={})\n  capacity wait      {} of wall (n={} passes)\n  lost passes        {} ({})\n  cost per landed    {} (n={}){}\n",
                c.passes,
                c.passes,
                c.per_hour(self.window_s()),
                c.landed,
                dur(c.latency_pct(50)),
                dur(c.latency_pct(95)),
                c.landed,
                c.mean_passes().map_or("-".into(), |m| format!("{m:.2}")),
                c.landed,
                c.wait_share().map_or("-".into(), |s| format!("{:.1}%", s * 100.0)),
                c.passes,
                c.lost_total(),
                if lost.is_empty() { "none".into() } else { lost.join(" ") },
                usd(per_bead(cell, c, prices)),
                c.landed,
                match (c.ec2, prices.contains_key(cell)) {
                    (false, _) => "  [not EC2]",
                    (true, false) => "  [no price for this type]",
                    _ => "",
                },
            ));
        }
        out
    }

    /// One line a briefing can read.
    pub fn line(&self, prices: &BTreeMap<String, f64>) -> String {
        let day = crate::iso_utc(self.from)[..10].to_string();
        if self.cells.is_empty() {
            return format!("cert {day}: no passes");
        }
        let parts: Vec<String> = self
            .cells
            .iter()
            .map(|((repo, cell), c)| {
                format!(
                    "{repo}/{cell} landed={} ({:.2}/h) p50={} p95={} passes/bead={} wait={} lost={} $/bead={} n={}",
                    c.landed,
                    c.per_hour(self.window_s()),
                    dur(c.latency_pct(50)),
                    dur(c.latency_pct(95)),
                    c.mean_passes().map_or("-".into(), |m| format!("{m:.2}")),
                    c.wait_share().map_or("-".into(), |s| format!("{:.0}%", s * 100.0)),
                    c.lost_total(),
                    usd(per_bead(cell, c, prices)),
                    c.passes,
                )
            })
            .collect();
        format!("cert {day}: {}", parts.join(" | "))
    }

    /// One summary row per cell, for the `cert-throughput` family.
    pub fn rows(&self, prices: &BTreeMap<String, f64>) -> Vec<Vec<(String, Value)>> {
        let f = |k: &str, v: Value| (k.to_string(), v);
        self.cells
            .iter()
            .map(|((repo, cell), c)| {
                vec![
                    f("window_from", crate::iso_utc(self.from).into()),
                    f("window_to", crate::iso_utc(self.to).into()),
                    f("repo", repo.clone().into()),
                    f("instance_type", cell.clone().into()),
                    f("passes", c.passes.into()),
                    f("landed", c.landed.into()),
                    f("landed_per_hour", Value::from(c.per_hour(self.window_s()))),
                    f("latency_p50_s", c.latency_pct(50).map_or(Value::Null, Value::from)),
                    f("latency_p95_s", c.latency_pct(95).map_or(Value::Null, Value::from)),
                    f("passes_per_landed", c.mean_passes().map_or(Value::Null, Value::from)),
                    f("capacity_wait_share", c.wait_share().map_or(Value::Null, Value::from)),
                    f("lost", c.lost_total().into()),
                    f("usd_per_landed", per_bead(cell, c, prices).map_or(Value::Null, Value::from)),
                ]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::{PassRecord, Place};

    fn rec(repo: &str, batch: &str, pass: &str, members: &[&str], it: &str, wait: u64, boot: u64, build: u64, suites: u64, cause: Cause) -> PassRecord {
        PassRecord {
            repo: repo.into(),
            batch: batch.into(),
            pass: pass.into(),
            members: members.iter().map(|s| s.to_string()).collect(),
            instance_type: it.into(),
            vcpus: Some(16),
            place: if it.is_empty() { Place::Proxmox } else { Place::Ec2 },
            capacity_wait_s: wait,
            boot_s: boot,
            build_s: build,
            suites_s: suites,
            cause,
            detail: String::new(),
        }
    }

    fn week() -> String {
        let dir = testkit::TempDir::new("tsd-report-week");
        let rows = [
            // sp / c7i.4xlarge: b1 red then green (a lands on pass 2), b2 green, b3 vm-lost
            ("2026-10-12T01:00:00Z", rec("sp", "b1", "1", &["a", "b"], "c7i.4xlarge", 60, 60, 240, 600, Cause::RedAttributed)),
            ("2026-10-12T02:00:00Z", rec("sp", "b1", "2", &["a", "b"], "c7i.4xlarge", 0, 60, 240, 600, Cause::Green)),
            ("2026-10-12T04:00:00Z", rec("sp", "b2", "1", &["c"], "c7i.4xlarge", 120, 60, 240, 600, Cause::Green)),
            ("2026-10-12T05:00:00Z", rec("sp", "b3", "1", &["d"], "c7i.4xlarge", 0, 60, 0, 0, Cause::VmLost)),
            // sp / c7i.2xlarge
            ("2026-10-12T06:00:00Z", rec("sp", "b4", "1", &["d"], "c7i.2xlarge", 0, 60, 300, 1140, Cause::Green)),
            // a second repo on a local VM
            ("2026-10-12T07:00:00Z", rec("pd", "p1", "1", &["x"], "", 0, 30, 100, 270, Cause::Green)),
            // outside the day
            ("2026-10-13T01:00:00Z", rec("sp", "b5", "1", &["e"], "c7i.4xlarge", 0, 60, 240, 600, Cause::Green)),
        ];
        for (ts, r) in rows {
            r.write(&dir, "h", ts).unwrap();
        }
        std::fs::read_to_string(crate::family_path(&dir, FAMILY)).unwrap()
    }

    fn day() -> (u64, u64) {
        (crate::parse_ts("2026-10-12").unwrap(), crate::parse_ts("2026-10-13").unwrap())
    }

    #[test]
    fn a_fixture_week_reproduces_the_hand_computed_numbers() {
        let (passes, bad) = parse_passes(&week());
        assert_eq!(bad, 0);
        let (from, to) = day();
        let r = compute(&passes, from, to);
        let prices = parse_prices("c7i.4xlarge=0.72,c7i.2xlarge=0.36").unwrap();

        let big = &r.cells[&("sp".to_string(), "c7i.4xlarge".to_string())];
        assert_eq!((big.passes, big.landed), (4, 3));
        // a,b land at 02:00 after a first pass that began 01:00-1h(wait60+boot60+work840=960s) = 00:44:00
        // c lands at 04:00 after a pass that began 04:00-1020s
        assert_eq!(big.latency_s, vec![4560, 4560, 1020]);
        assert_eq!(big.latency_pct(50), Some(4560));
        assert_eq!(big.latency_pct(95), Some(4560));
        assert_eq!(big.passes_per_landed, vec![2, 2, 1]);
        assert!((big.mean_passes().unwrap() - 5.0 / 3.0).abs() < 1e-9);
        // wait 60+0+120+0 = 180 of wall 960+900+1020+60 = 2940
        assert_eq!((big.wait_s, big.wall_s), (180, 2940));
        assert_eq!(big.lost.get("vm-lost"), Some(&1));
        assert_eq!(big.lost_total(), 1);
        // billed 900+900+960+60... boot+work per pass: 900, 900, 900, 60 = 2760s at 0.72/h
        assert_eq!(big.billed_s, 2760);
        let cost = cost_usd("c7i.4xlarge", big, &prices).unwrap();
        assert!((cost - 2760.0 / 3600.0 * 0.72).abs() < 1e-9);
        assert!((per_bead("c7i.4xlarge", big, &prices).unwrap() - cost / 3.0).abs() < 1e-9);
        assert!((big.per_hour(to - from) - 3.0 / 24.0).abs() < 1e-9);

        // d was lost on the 4xlarge and landed on the 2xlarge: its latency spans both cells
        let small = &r.cells[&("sp".to_string(), "c7i.2xlarge".to_string())];
        assert_eq!(small.landed, 1);
        assert_eq!((small.latency_s.clone(), small.passes_per_landed.clone()), (vec![3660], vec![2]));
        assert_eq!(cost_usd("c7i.2xlarge", small, &prices).map(|c| (c * 1000.0).round()), Some(150.0));

        let local = &r.cells[&("pd".to_string(), "proxmox".to_string())];
        assert_eq!((local.landed, local.latency_s.clone()), (1, vec![400]));
        assert_eq!(cost_usd("proxmox", local, &prices), None);
        assert_eq!(r.cells.len(), 3);

        let line = r.line(&prices);
        assert!(line.starts_with("cert 2026-10-12: pd/proxmox ") && line.contains("sp/c7i.4xlarge landed=3 (0.12/h)"), "{line}");
    }

    #[test]
    fn a_bead_lands_once_and_an_unreadable_row_is_counted_not_guessed() {
        let mut text = week();
        text.push_str("{\"family\":\"cert-pass\",\"ts\":\"2026-10-12T09:00:00Z\",\"repo\":\"sp\",\"cause\":\"flaky\"}\n");
        text.push_str("not json\n");
        let (passes, bad) = parse_passes(&text);
        assert_eq!(bad, 1);
        let again = rec("sp", "b9", "1", &["c"], "c7i.4xlarge", 0, 1, 1, 1, Cause::Green);
        let dir = testkit::TempDir::new("tsd-report-once");
        again.write(&dir, "h", "2026-10-12T10:00:00Z").unwrap();
        let mut all = passes;
        all.extend(parse_passes(&std::fs::read_to_string(crate::family_path(&dir, FAMILY)).unwrap()).0);
        let (from, to) = day();
        let r = compute(&all, from, to);
        assert_eq!(r.cells[&("sp".to_string(), "c7i.4xlarge".to_string())].landed, 3);
    }

    #[test]
    fn an_empty_window_says_so_and_a_malformed_price_is_refused() {
        let r = compute(&[], 0, 86_400);
        assert!(r.text(&BTreeMap::new()).contains("no passes in the window"));
        assert_eq!(r.line(&BTreeMap::new()), "cert 1970-01-01: no passes");
        assert!(parse_prices("c7i.4xlarge").is_err());
        assert!(parse_prices("c7i.4xlarge=cheap").is_err());
        assert!(parse_prices("c7i.4xlarge=-1").is_err());
        assert_eq!(parse_prices("").unwrap().len(), 0);
    }
}
