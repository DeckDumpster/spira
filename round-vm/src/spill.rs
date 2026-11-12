//! The spill rule (DESIGN.md §2.6): a pass goes to EC2 when a local round VM is already
//! leased (the hypervisor holds one round VM), when the local pool cannot lease a VM within a
//! short wait, or when this box's io pressure is above a threshold. Pure: the caller supplies
//! the lease, the wait and the reading.

use std::time::Duration;

pub const PSI_IO: &str = "/proc/pressure/io";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    pub wait: Duration,
    pub io_full_5m_tm90: f64,
    pub always: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Local,
    Ec2(String),
}

/// The `field=` of the `kind` (`some` or `full`) line of a PSI body.
pub fn parse_psi(text: &str, kind: &str, field: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.split_whitespace().next() == Some(kind))?;
    let prefix = format!("{field}=");
    line.split_whitespace().find_map(|f| f.strip_prefix(prefix.as_str())).and_then(|v| v.parse().ok())
}

pub fn read_psi(path: &str, kind: &str, field: &str) -> Option<f64> {
    std::fs::read_to_string(path).ok().and_then(|t| parse_psi(&t, kind, field))
}

/// The seconds of history a pressure is judged on, and the cadence the sampler keeps.
pub const WINDOW_SECS: u64 = 300;
pub const SAMPLE_EVERY_SECS: u64 = 10;
/// The sampler's state file under the run directory.
pub const SAMPLES_FILE: &str = "psi-samples";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Basis {
    Tm90,
    Avg300,
}

/// A pressure reading and the measure it is: the five-minute tm90, or the kernel's `avg300`
/// when the sampler has not kept half the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pressure {
    pub value: f64,
    pub basis: Basis,
}

impl std::fmt::Display for Pressure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.basis {
            Basis::Tm90 => write!(f, "5m tm90 {:.1}", self.value),
            Basis::Avg300 => write!(f, "avg300 {:.1} (fewer than half the 5m samples)", self.value),
        }
    }
}

/// The mean of the samples with the top tenth dropped; `None` for no samples.
pub fn tm90(samples: &[f64]) -> Option<f64> {
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v.truncate(v.len() - v.len() / 10);
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// The tm90 of `samples` when at least half the window's samples are present, else `avg300`.
pub fn pressure(samples: &[f64], avg300: Option<f64>) -> Option<Pressure> {
    if samples.len() as u64 * 2 >= WINDOW_SECS / SAMPLE_EVERY_SECS {
        return tm90(samples).map(|value| Pressure { value, basis: Basis::Tm90 });
    }
    avg300.map(|value| Pressure { value, basis: Basis::Avg300 })
}

/// One sampler tick: the `avg10` of io `full` and cpu `some`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub at: u64,
    pub io_full: f64,
    pub cpu_some: f64,
}

pub fn read_samples(path: &std::path::Path, now: u64) -> Vec<Sample> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some(Sample { at: f.next()?.parse().ok()?, io_full: f.next()?.parse().ok()?, cpu_some: f.next()?.parse().ok()? })
        })
        .filter(|s| s.at > now.saturating_sub(WINDOW_SECS) && s.at <= now)
        .collect()
}

/// Appends `sample` to the file, keeping only the window; replaced whole so a reader never sees half a line.
pub fn record_sample(path: &std::path::Path, sample: Sample) -> std::io::Result<()> {
    let mut kept = read_samples(path, sample.at);
    kept.push(sample);
    let body: String = kept.iter().map(|s| format!("{} {} {}\n", s.at, s.io_full, s.cpu_some)).collect();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// Reads the kernel's `avg10` at `now` into the file; unreadable pressure records nothing.
pub fn sample_now(path: &std::path::Path, now: u64) -> Result<Sample, String> {
    let io_full = read_psi(PSI_IO, "full", "avg10").ok_or("io pressure is unreadable")?;
    let cpu_some = read_psi(PSI_CPU, "some", "avg10").ok_or("cpu pressure is unreadable")?;
    let sample = Sample { at: now, io_full, cpu_some };
    record_sample(path, sample).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(sample)
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The host's io `full` and cpu `some` pressure as the placement judges them.
pub fn host_load(run_dir: &std::path::Path, now: u64) -> HostLoad {
    let samples = read_samples(&run_dir.join(SAMPLES_FILE), now);
    let io: Vec<f64> = samples.iter().map(|s| s.io_full).collect();
    let cpu: Vec<f64> = samples.iter().map(|s| s.cpu_some).collect();
    HostLoad {
        io_full: pressure(&io, read_psi(PSI_IO, "full", "avg300")),
        cpu_some: pressure(&cpu, read_psi(PSI_CPU, "some", "avg300")),
    }
}

/// `local_leased` says a local round VM is already out; `no_lease_for` is how long the pass
/// has gone without a local lease (None: it is being leased). An unreadable pressure never
/// spills; `always` places every pass on EC2.
pub fn decide(
    rule: &Rule,
    local_leased: bool,
    no_lease_for: Option<Duration>,
    pressure: Option<Pressure>,
) -> Place {
    if rule.always {
        return Place::Ec2("placement ec2-always".into());
    }
    if local_leased {
        return Place::Ec2("a local round VM is already leased (one per hypervisor)".into());
    }
    if let Some(p) = pressure.filter(|p| p.value > rule.io_full_5m_tm90) {
        return Place::Ec2(format!("io pressure full {p} above {:.1}", rule.io_full_5m_tm90));
    }
    match no_lease_for {
        Some(w) if w >= rule.wait => {
            Place::Ec2(format!("no local VM within {}s", rule.wait.as_secs()))
        }
        _ => Place::Local,
    }
}

pub const PSI_CPU: &str = "/proc/pressure/cpu";

/// The host's own limits on taking a round: past either pressure the round goes to EC2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostRule {
    pub io_full_5m_tm90: f64,
    pub cpu_some_5m_tm90: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HostLoad {
    pub io_full: Option<Pressure>,
    pub cpu_some: Option<Pressure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPlace {
    Host(String),
    Ec2(String),
}

/// A round runs on the host only when the host slot is free and neither pressure is over its
/// threshold; an unreadable pressure never spills (the same rule as [`decide`]).
pub fn decide_host(rule: &HostRule, slot_free: bool, load: HostLoad) -> HostPlace {
    if !slot_free {
        return HostPlace::Ec2("the host round slot is taken".into());
    }
    if let Some(p) = load.io_full.filter(|p| p.value > rule.io_full_5m_tm90) {
        return HostPlace::Ec2(format!("host io pressure full {p} above {:.1}", rule.io_full_5m_tm90));
    }
    if let Some(p) = load.cpu_some.filter(|p| p.value > rule.cpu_some_5m_tm90) {
        return HostPlace::Ec2(format!("host cpu pressure some {p} above {:.1}", rule.cpu_some_5m_tm90));
    }
    let line = match load.io_full {
        Some(p) => format!("host io pressure full {p} at or under {:.1} — on the host", rule.io_full_5m_tm90),
        None => "host io pressure unreadable — on the host".to_string(),
    };
    HostPlace::Host(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: Rule = Rule {
        wait: Duration::from_secs(30),
        io_full_5m_tm90: 30.0,
        always: false,
    };
    const ALWAYS: Rule = Rule {
        always: true,
        ..RULE
    };
    const PSI: &str = "some avg10=1.00 avg60=2.00 avg300=3.00 total=10\nfull avg10=0.50 avg60=41.25 avg300=9.00 total=5\n";

    fn tm(value: f64) -> Pressure {
        Pressure { value, basis: Basis::Tm90 }
    }

    #[test]
    fn psi_reads_the_named_line_and_field() {
        assert_eq!(parse_psi(PSI, "full", "avg10"), Some(0.5));
        assert_eq!(parse_psi(PSI, "some", "avg300"), Some(3.0));
        assert_eq!(parse_psi("nonsense", "full", "avg10"), None);
    }

    #[test]
    fn pressure_above_the_threshold_spills_and_below_it_stays_local() {
        assert!(
            matches!(decide(&RULE, false, None, Some(tm(41.25))), Place::Ec2(r) if r.contains("5m tm90 41.2"))
        );
        assert_eq!(decide(&RULE, false, None, Some(tm(12.0))), Place::Local);
        assert_eq!(decide(&RULE, false, None, Some(tm(30.0))), Place::Local);
        assert_eq!(decide(&RULE, false, None, None), Place::Local);
    }

    #[test]
    fn a_local_pool_that_cannot_lease_within_the_wait_spills() {
        assert!(
            matches!(decide(&RULE, false, Some(Duration::from_secs(30)), Some(tm(1.0))), Place::Ec2(r) if r.contains("30s"))
        );
        assert_eq!(
            decide(&RULE, false, Some(Duration::from_secs(29)), Some(tm(1.0))),
            Place::Local
        );
    }

    #[test]
    fn a_second_pass_while_a_local_vm_is_leased_spills_at_once() {
        assert!(
            matches!(decide(&RULE, true, None, Some(tm(0.0))), Place::Ec2(r) if r.contains("already leased"))
        );
        assert!(matches!(decide(&RULE, true, None, None), Place::Ec2(_)));
        assert_eq!(decide(&RULE, false, None, Some(tm(0.0))), Place::Local);
    }
    #[test]
    fn ec2_always_places_on_ec2_with_a_free_local_vm_and_unreadable_pressure() {
        assert!(
            matches!(decide(&ALWAYS, false, None, Some(tm(0.0))), Place::Ec2(r) if r.contains("ec2-always"))
        );
        assert!(matches!(decide(&ALWAYS, false, None, None), Place::Ec2(_)));
        assert_eq!(decide(&RULE, false, None, None), Place::Local);
    }

    const HOST: HostRule = HostRule { io_full_5m_tm90: 50.0, cpu_some_5m_tm90: 60.0 };
    const CALM: HostLoad = HostLoad { io_full: Some(Pressure { value: 1.0, basis: Basis::Tm90 }), cpu_some: Some(Pressure { value: 2.0, basis: Basis::Tm90 }) };

    #[test]
    fn a_free_host_slot_under_both_thresholds_runs_the_round_on_the_host() {
        assert!(matches!(decide_host(&HOST, true, CALM), HostPlace::Host(_)));
        assert!(matches!(decide_host(&HOST, true, HostLoad::default()), HostPlace::Host(_)));
        let at_limit = HostLoad { io_full: Some(tm(50.0)), cpu_some: Some(tm(60.0)) };
        assert!(matches!(decide_host(&HOST, true, at_limit), HostPlace::Host(_)));
    }

    #[test]
    fn the_host_line_names_the_measure() {
        let load = HostLoad { io_full: Some(tm(37.25)), ..CALM };
        assert_eq!(decide_host(&HOST, true, load), HostPlace::Host("host io pressure full 5m tm90 37.2 at or under 50.0 — on the host".into()));
    }

    #[test]
    fn a_busy_host_slot_spills_whatever_the_pressure() {
        assert!(matches!(decide_host(&HOST, false, CALM), HostPlace::Ec2(r) if r.contains("slot")));
        assert!(matches!(decide_host(&HOST, false, HostLoad::default()), HostPlace::Ec2(_)));
    }

    #[test]
    fn host_pressure_over_either_threshold_spills() {
        let io = HostLoad { io_full: Some(tm(61.25)), ..CALM };
        assert!(matches!(decide_host(&HOST, true, io), HostPlace::Ec2(r) if r.contains("io") && r.contains("61.2") && r.contains("5m tm90")));
        let cpu = HostLoad { cpu_some: Some(tm(75.0)), ..CALM };
        assert!(matches!(decide_host(&HOST, true, cpu), HostPlace::Ec2(r) if r.contains("cpu") && r.contains("75.0")));
    }

    #[test]
    fn three_spikes_in_thirty_samples_do_not_move_the_round_off_the_host() {
        let mut s = vec![10.0; 27];
        s.extend([95.0; 3]);
        assert_eq!(tm90(&s), Some(10.0));
        let load = HostLoad { io_full: pressure(&s, Some(99.0)), ..CALM };
        assert!(matches!(decide_host(&HOST, true, load), HostPlace::Host(_)));
    }

    #[test]
    fn thirty_samples_of_sixty_spill() {
        let load = HostLoad { io_full: pressure(&[60.0; 30], Some(0.0)), ..CALM };
        assert!(matches!(decide_host(&HOST, true, load), HostPlace::Ec2(r) if r.contains("5m tm90 60.0")));
    }

    #[test]
    fn fewer_than_half_the_samples_judge_on_avg300_and_say_so() {
        let load = HostLoad { io_full: pressure(&[0.0; 14], Some(70.0)), ..CALM };
        assert!(matches!(decide_host(&HOST, true, load), HostPlace::Ec2(r) if r.contains("avg300 70.0") && r.contains("fewer than half")));
        let calm = HostLoad { io_full: pressure(&[90.0; 10], Some(5.0)), ..CALM };
        assert!(matches!(decide_host(&HOST, true, calm), HostPlace::Host(l) if l.contains("avg300 5.0")));
        assert_eq!(pressure(&[0.0; 15], Some(70.0)).map(|p| p.basis), Some(Basis::Tm90));
        assert_eq!(pressure(&[], None), None);
    }

    #[test]
    fn the_sampler_keeps_five_minutes_and_the_host_load_reads_them() {
        let tmp = testkit::TempDir::new("psi-samples");
        let dir = tmp.path().to_path_buf();
        let file = dir.join(SAMPLES_FILE);
        for i in 0..40u64 {
            record_sample(&file, Sample { at: 1000 + i * SAMPLE_EVERY_SECS, io_full: i as f64, cpu_some: 1.0 }).unwrap();
        }
        let now = 1000 + 39 * SAMPLE_EVERY_SECS;
        let kept = read_samples(&file, now);
        assert_eq!(kept.len(), 30);
        assert_eq!(kept[0].io_full, 10.0);
        assert_eq!(host_load(&dir, now).io_full.map(|p| p.basis), Some(Basis::Tm90));
        assert_eq!(host_load(&dir, now + 3600).io_full.map(|p| p.basis).filter(|b| *b == Basis::Tm90), None);
    }
}
