//! `dwarves.sh usage`: what each dwarf uses, live and per bead.
//!
//! Live, two reads of the same sampler `watch.dwarf_usage` pushes from
//! (`runtime::usage::sample`), a second apart, give each dwarf's totals and its current CPU%
//! and MB/s. Per bead, the totals are read back from the metrics store: a session is one unit,
//! and a counter's session total is its last (largest) value there, so a bead's total is the sum
//! over its units.

use std::collections::BTreeMap;

use runtime::usage::{metric, Sample};
use spira_otel::query::Exported;

/// One sample's movement over `dt_secs`: CPU% (100 is one core) and MB/s read and written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rate {
    pub cpu_pct: f64,
    pub read_mb_s: f64,
    pub write_mb_s: f64,
}

/// `after`'s movement since `before` (the same cgroup), over `dt_secs`; a cgroup new in `after`
/// moves from zero.
pub fn rate(before: Option<&Sample>, after: &Sample, dt_secs: f64) -> Rate {
    if dt_secs <= 0.0 {
        return Rate::default();
    }
    let io = |s: &Sample| s.io.iter().fold((0u64, 0u64), |(r, w), d| (r + d.read_bytes, w + d.write_bytes));
    let (cpu0, (r0, w0)) = before.map_or((0, (0, 0)), |b| (b.cpu_usec, io(b)));
    let (r1, w1) = io(after);
    Rate {
        cpu_pct: after.cpu_usec.saturating_sub(cpu0) as f64 / 1e6 / dt_secs * 100.0,
        read_mb_s: r1.saturating_sub(r0) as f64 / 1e6 / dt_secs,
        write_mb_s: w1.saturating_sub(w0) as f64 / 1e6 / dt_secs,
    }
}

fn gib(b: u64) -> String {
    format!("{:.2}G", b as f64 / (1u64 << 30) as f64)
}

fn opt_gib(b: Option<u64>) -> String {
    b.map_or("-".into(), gib)
}

fn wall(started_ms: Option<u64>, now_ms: u64) -> String {
    started_ms.map_or("-".into(), |s| {
        let secs = now_ms.saturating_sub(s) / 1000;
        format!("{}m{:02}s", secs / 60, secs % 60)
    })
}

/// The live table: one block per dwarf (and per hand scope) in `after`.
pub fn render_live(before: &[Sample], after: &[Sample], dt_secs: f64, now_ms: u64) -> String {
    if after.is_empty() {
        return "no live dwarf and no hand build\n".into();
    }
    let mut out = String::new();
    for s in after {
        let r = rate(before.iter().find(|b| b.cgroup == s.cgroup), s, dt_secs);
        let who = if s.name.is_empty() { s.rune.clone() } else { format!("{}/{}", s.rune, s.name) };
        out.push_str(&format!(
            "{} {who}  bead {}  repo {}  wall {}\n  unit {}\n  cpu {:.1}s ({:.0}%)  mem {} (peak {})  worktree {}  build {}\n",
            s.kind.word(),
            if s.bead.is_empty() { "?" } else { &s.bead },
            if s.repo.is_empty() { "?" } else { &s.repo },
            wall(s.started_ms, now_ms),
            s.unit,
            s.cpu_seconds(),
            r.cpu_pct,
            gib(s.memory_current),
            gib(s.memory_peak),
            opt_gib(s.worktree_bytes),
            opt_gib(s.build_dir_bytes),
        ));
        for d in s.io.iter().filter(|d| d.read_bytes + d.write_bytes > 0) {
            let prev = before.iter().find(|b| b.cgroup == s.cgroup).and_then(|b| b.io.iter().find(|x| x.device == d.device));
            let (rr, ww) = prev.map_or((d.read_bytes, d.write_bytes), |p| (d.read_bytes.saturating_sub(p.read_bytes), d.write_bytes.saturating_sub(p.write_bytes)));
            let at = if d.mount.is_empty() { String::new() } else { format!(" {}", d.mount) };
            out.push_str(&format!(
                "  io {} ({}{at})  read {}  write {}  now {:.1}/{:.1} MB/s\n",
                d.device,
                d.role,
                gib(d.read_bytes),
                gib(d.write_bytes),
                rr as f64 / 1e6 / dt_secs.max(f64::MIN_POSITIVE),
                ww as f64 / 1e6 / dt_secs.max(f64::MIN_POSITIVE)
            ));
        }
        if !s.worktree.is_empty() || !s.build_dir.is_empty() {
            out.push_str(&format!("  dirs {}  {}\n", if s.worktree.is_empty() { "-" } else { &s.worktree }, if s.build_dir.is_empty() { "-" } else { &s.build_dir }));
        }
    }
    out
}

/// One session's totals: a unit's last value of each counter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub unit: String,
    pub kind: String,
    pub name: String,
    pub cpu_seconds: f64,
    /// Bytes written and read per disk role.
    pub write_bytes: BTreeMap<String, f64>,
    pub read_bytes: BTreeMap<String, f64>,
    pub memory_peak_bytes: f64,
    pub worktree_peak_bytes: f64,
    pub build_dir_peak_bytes: f64,
    pub wall_seconds: f64,
}

/// A bead's sessions and their sum. The peaks are the largest of any session, not a sum.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeadTotals {
    pub sessions: Vec<Session>,
    pub total: Session,
}

fn last(samples: &[(i64, f64)]) -> f64 {
    samples.iter().map(|(_, v)| *v).fold(0.0, f64::max)
}

/// The totals of every session the exported series hold, by unit.
pub fn bead_totals(series: &[Exported]) -> BeadTotals {
    let mut by_unit: BTreeMap<String, Session> = BTreeMap::new();
    for e in series {
        let l = |k: &str| e.labels.get(k).cloned().unwrap_or_default();
        let s = by_unit.entry(l("unit")).or_insert_with(|| Session { unit: l("unit"), kind: l("kind"), name: l("name"), ..Session::default() });
        let v = last(&e.samples);
        let role = || {
            let r = l("role");
            if r.is_empty() { l("device") } else { r }
        };
        match l("__name__").as_str() {
            metric::CPU_SECONDS => s.cpu_seconds = s.cpu_seconds.max(v),
            metric::IO_WRITE_BYTES => *s.write_bytes.entry(role()).or_default() += v,
            metric::IO_READ_BYTES => *s.read_bytes.entry(role()).or_default() += v,
            metric::MEMORY_PEAK_BYTES => s.memory_peak_bytes = s.memory_peak_bytes.max(v),
            metric::WORKTREE_BYTES => s.worktree_peak_bytes = s.worktree_peak_bytes.max(v),
            metric::BUILD_DIR_BYTES => s.build_dir_peak_bytes = s.build_dir_peak_bytes.max(v),
            metric::WALL_SECONDS => s.wall_seconds = s.wall_seconds.max(v),
            _ => {}
        }
    }
    let sessions: Vec<Session> = by_unit.into_values().collect();
    let mut total = Session { unit: format!("{} session(s)", sessions.len()), ..Session::default() };
    for s in &sessions {
        total.cpu_seconds += s.cpu_seconds;
        total.wall_seconds += s.wall_seconds;
        for (k, v) in &s.write_bytes {
            *total.write_bytes.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &s.read_bytes {
            *total.read_bytes.entry(k.clone()).or_default() += v;
        }
        total.memory_peak_bytes = total.memory_peak_bytes.max(s.memory_peak_bytes);
        total.worktree_peak_bytes = total.worktree_peak_bytes.max(s.worktree_peak_bytes);
        total.build_dir_peak_bytes = total.build_dir_peak_bytes.max(s.build_dir_peak_bytes);
    }
    BeadTotals { sessions, total }
}

/// The selector for every usage series of `bead`; a hand row's bead label may list several.
pub fn bead_selector(bead: &str) -> String {
    format!("{{__name__=~\"{}.*\",bead=~\"(.*,)?{bead}(,.*)?\"}}", metric::PREFIX)
}

fn bytes_line(m: &BTreeMap<String, f64>) -> String {
    if m.is_empty() {
        return "-".into();
    }
    m.iter().map(|(k, v)| format!("{k} {}", gib(*v as u64))).collect::<Vec<_>>().join(", ")
}

pub fn render_bead(bead: &str, t: &BeadTotals) -> String {
    let mut out = String::new();
    let line = |s: &Session| {
        format!(
            "  cpu {:.0}s  wall {:.0}s  written {}  read {}  mem peak {}  worktree peak {}  build peak {}\n",
            s.cpu_seconds,
            s.wall_seconds,
            bytes_line(&s.write_bytes),
            bytes_line(&s.read_bytes),
            gib(s.memory_peak_bytes as u64),
            gib(s.worktree_peak_bytes as u64),
            gib(s.build_dir_peak_bytes as u64)
        )
    };
    for s in &t.sessions {
        out.push_str(&format!("{} {} {}\n", s.kind, if s.name.is_empty() { "?" } else { &s.name }, s.unit));
        out.push_str(&line(s));
    }
    out.push_str(&format!("{bead}: {}\n", t.total.unit));
    out.push_str(&line(&t.total));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::usage::{DeviceIo, Kind};

    fn series(name: &str, unit: &str, role: &str, values: &[f64]) -> Exported {
        let mut labels = BTreeMap::from([("__name__".to_string(), name.to_string()), ("unit".to_string(), unit.to_string()), ("kind".to_string(), "dwarf".to_string()), ("name".to_string(), "fjalar".to_string()), ("bead".to_string(), "sp-abc".to_string())]);
        if !role.is_empty() {
            labels.insert("role".into(), role.into());
            labels.insert("device".into(), "sdx".into());
        }
        Exported { labels, samples: values.iter().enumerate().map(|(i, v)| (i as i64 * 60_000, *v)).collect() }
    }

    #[test]
    fn a_beads_total_is_the_sum_of_each_sessions_last_values() {
        let got = bead_totals(&[
            series(metric::CPU_SECONDS, "u1", "", &[10.0, 40.0, 100.0]),
            series(metric::CPU_SECONDS, "u2", "", &[5.0, 50.0]),
            series(metric::IO_WRITE_BYTES, "u1", "ssd-b", &[1e9, 3e9]),
            series(metric::IO_WRITE_BYTES, "u2", "ssd-b", &[2e9]),
            series(metric::IO_WRITE_BYTES, "u2", "ssd-c", &[7e8]),
            series(metric::IO_READ_BYTES, "u1", "ssd-b", &[4e6]),
            series(metric::WORKTREE_BYTES, "u1", "", &[9e9, 4e9]),
            series(metric::WORKTREE_BYTES, "u2", "", &[6e9]),
            series(metric::BUILD_DIR_BYTES, "u2", "", &[2e10]),
            series(metric::WALL_SECONDS, "u1", "", &[60.0, 600.0]),
            series(metric::MEMORY_BYTES, "u1", "", &[1e12]),
        ]);
        assert_eq!(got.sessions.iter().map(|s| (s.unit.as_str(), s.cpu_seconds)).collect::<Vec<_>>(), [("u1", 100.0), ("u2", 50.0)], "one session per unit, each its counter's last value");
        assert_eq!(got.total.cpu_seconds, 150.0);
        assert_eq!(got.total.write_bytes, BTreeMap::from([("ssd-b".to_string(), 5e9), ("ssd-c".to_string(), 7e8)]), "bytes written per disk, summed over sessions");
        assert_eq!(got.total.read_bytes, BTreeMap::from([("ssd-b".to_string(), 4e6)]));
        assert_eq!(got.total.worktree_peak_bytes, 9e9, "a peak is the largest of any session, not a sum");
        assert_eq!(got.total.build_dir_peak_bytes, 2e10);
        assert_eq!(got.total.wall_seconds, 600.0);
        assert_eq!(got.total.memory_peak_bytes, 0.0, "memory now is not a peak");
        assert_eq!(bead_totals(&[]), BeadTotals { sessions: vec![], total: Session { unit: "0 session(s)".into(), ..Session::default() } });
    }

    #[test]
    fn the_selector_matches_the_bead_alone_or_in_a_hand_rows_list() {
        assert_eq!(bead_selector("sp-abc"), "{__name__=~\"spira_dwarf_.*\",bead=~\"(.*,)?sp-abc(,.*)?\"}");
    }

    fn at(cpu_usec: u64, read: u64, write: u64) -> Sample {
        Sample { kind: Kind::Dwarf, cgroup: "c".into(), unit: "u".into(), cpu_usec, io: vec![DeviceIo { device: "sdc".into(), mount: "/srv/ssd-b".into(), role: "ssd-b".into(), read_bytes: read, write_bytes: write }], ..Sample::default() }
    }

    #[test]
    fn the_live_rate_is_the_movement_between_two_reads() {
        let r = rate(Some(&at(1_000_000, 0, 10_000_000)), &at(3_000_000, 5_000_000, 54_000_000), 1.0);
        assert_eq!(r, Rate { cpu_pct: 200.0, read_mb_s: 5.0, write_mb_s: 44.0 });
        assert_eq!(rate(None, &at(500_000, 0, 0), 0.5).cpu_pct, 100.0, "a new cgroup moves from zero");
        let table = render_live(&[at(1_000_000, 0, 10_000_000)], &[at(3_000_000, 5_000_000, 54_000_000)], 1.0, 0);
        assert!(table.contains("cpu 3.0s (200%)") && table.contains("io sdc (ssd-b /srv/ssd-b)") && table.contains("now 5.0/44.0 MB/s"), "{table}");
        assert_eq!(render_live(&[], &[], 1.0, 0), "no live dwarf and no hand build\n");
    }
}
