//! The spill rule (DESIGN.md §2.6): a pass goes to EC2 when a local round VM is already
//! leased (the hypervisor holds one round VM), when the local pool cannot lease a VM within a
//! short wait, or when this box's io pressure is above a threshold. Pure: the caller supplies
//! the lease, the wait and the reading.

use std::time::Duration;

pub const PSI_IO: &str = system_paths::PSI_IO;

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

/// A pressure reading: the kernel's own `avg300`, its five-minute average.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pressure {
    pub value: f64,
}

impl std::fmt::Display for Pressure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "avg300 {:.1}", self.value)
    }
}

/// The host's io `full` and cpu `some` pressure as the placement judges them.
pub fn host_load() -> HostLoad {
    host_load_with(read_psi(PSI_IO, "full", "avg300"), read_psi(PSI_CPU, "some", "avg300"))
}

pub fn host_load_with(io_avg300: Option<f64>, cpu_avg300: Option<f64>) -> HostLoad {
    HostLoad {
        io_full: io_avg300.map(|value| Pressure { value }),
        cpu_some: cpu_avg300.map(|value| Pressure { value }),
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

pub const PSI_CPU: &str = system_paths::PSI_CPU;

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
        Pressure { value }
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
            matches!(decide(&RULE, false, None, Some(tm(41.25))), Place::Ec2(r) if r.contains("avg300 41.2"))
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
    const CALM: HostLoad = HostLoad { io_full: Some(Pressure { value: 1.0 }), cpu_some: Some(Pressure { value: 2.0 }) };

    #[test]
    fn a_free_host_slot_under_both_thresholds_runs_the_round_on_the_host() {
        assert!(matches!(decide_host(&HOST, true, CALM), HostPlace::Host(_)));
        assert!(matches!(decide_host(&HOST, true, HostLoad::default()), HostPlace::Host(_)));
        let at_limit = HostLoad { io_full: Some(tm(50.0)), cpu_some: Some(tm(60.0)) };
        assert!(matches!(decide_host(&HOST, true, at_limit), HostPlace::Host(_)));
    }

    #[test]
    fn the_host_line_names_the_kernels_five_minute_average() {
        assert_eq!(
            decide_host(&HOST, true, host_load_with(Some(70.0), Some(1.0))),
            HostPlace::Ec2("host io pressure full avg300 70.0 above 50.0".into())
        );
        assert_eq!(
            decide_host(&HOST, true, host_load_with(Some(37.25), Some(1.0))),
            HostPlace::Host("host io pressure full avg300 37.2 at or under 50.0 — on the host".into())
        );
    }

    #[test]
    fn a_busy_host_slot_spills_whatever_the_pressure() {
        assert!(matches!(decide_host(&HOST, false, CALM), HostPlace::Ec2(r) if r.contains("slot")));
        assert!(matches!(decide_host(&HOST, false, HostLoad::default()), HostPlace::Ec2(_)));
    }

    #[test]
    fn host_pressure_over_either_threshold_spills() {
        let io = HostLoad { io_full: Some(tm(61.25)), ..CALM };
        assert!(matches!(decide_host(&HOST, true, io), HostPlace::Ec2(r) if r.contains("io") && r.contains("61.2") && r.contains("avg300")));
        let cpu = HostLoad { cpu_some: Some(tm(75.0)), ..CALM };
        assert!(matches!(decide_host(&HOST, true, cpu), HostPlace::Ec2(r) if r.contains("cpu") && r.contains("75.0")));
    }
}
