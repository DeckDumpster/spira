//! The spill rule (DESIGN.md §2.6): a pass goes to EC2 when a local round VM is already
//! leased (the hypervisor holds one round VM), when the local pool cannot lease a VM within a
//! short wait, or when this box's io pressure is above a threshold. Pure: the caller supplies
//! the lease, the wait and the reading.

use std::time::Duration;

pub const PSI_IO: &str = "/proc/pressure/io";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    pub wait: Duration,
    pub io_full_avg60: f64,
    pub always: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Local,
    Ec2(String),
}

/// `full avg60=` of a `/proc/pressure/io` body.
pub fn parse_io_full_avg60(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("full "))?;
    line.split_whitespace()
        .find_map(|f| f.strip_prefix("avg60="))
        .and_then(|v| v.parse().ok())
}

pub fn read_io_full_avg60(path: &str) -> Option<f64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| parse_io_full_avg60(&t))
}

/// `local_leased` says a local round VM is already out; `no_lease_for` is how long the pass
/// has gone without a local lease (None: it is being leased). An unreadable pressure never
/// spills; `always` places every pass on EC2.
pub fn decide(
    rule: &Rule,
    local_leased: bool,
    no_lease_for: Option<Duration>,
    pressure: Option<f64>,
) -> Place {
    if rule.always {
        return Place::Ec2("placement ec2-always".into());
    }
    if local_leased {
        return Place::Ec2("a local round VM is already leased (one per hypervisor)".into());
    }
    if let Some(p) = pressure.filter(|p| *p > rule.io_full_avg60) {
        return Place::Ec2(format!(
            "io pressure full avg60 {p:.1} above {:.1}",
            rule.io_full_avg60
        ));
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
    pub io_full_avg60: f64,
    pub cpu_some_avg60: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HostLoad {
    pub io_full_avg60: Option<f64>,
    pub cpu_some_avg60: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPlace {
    Host,
    Ec2(String),
}

/// `some avg60=` of a `/proc/pressure/cpu` body.
pub fn parse_cpu_some_avg60(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("some "))?;
    line.split_whitespace().find_map(|f| f.strip_prefix("avg60=")).and_then(|v| v.parse().ok())
}

pub fn read_cpu_some_avg60(path: &str) -> Option<f64> {
    std::fs::read_to_string(path).ok().and_then(|t| parse_cpu_some_avg60(&t))
}

/// A round runs on the host only when the host slot is free and neither pressure is over its
/// threshold; an unreadable pressure never spills (the same rule as [`decide`]).
pub fn decide_host(rule: &HostRule, slot_free: bool, load: HostLoad) -> HostPlace {
    if !slot_free {
        return HostPlace::Ec2("the host round slot is taken".into());
    }
    if let Some(p) = load.io_full_avg60.filter(|p| *p > rule.io_full_avg60) {
        return HostPlace::Ec2(format!("host io pressure full avg60 {p:.1} above {:.1}", rule.io_full_avg60));
    }
    if let Some(p) = load.cpu_some_avg60.filter(|p| *p > rule.cpu_some_avg60) {
        return HostPlace::Ec2(format!("host cpu pressure some avg60 {p:.1} above {:.1}", rule.cpu_some_avg60));
    }
    HostPlace::Host
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: Rule = Rule {
        wait: Duration::from_secs(30),
        io_full_avg60: 30.0,
        always: false,
    };
    const ALWAYS: Rule = Rule {
        always: true,
        ..RULE
    };
    const PSI: &str = "some avg10=1.00 avg60=2.00 avg300=3.00 total=10\nfull avg10=0.00 avg60=41.25 avg300=9.00 total=5\n";

    #[test]
    fn full_avg60_is_read_not_some() {
        assert_eq!(parse_io_full_avg60(PSI), Some(41.25));
        assert_eq!(parse_io_full_avg60("nonsense"), None);
    }

    #[test]
    fn pressure_above_the_threshold_spills_and_below_it_stays_local() {
        assert!(
            matches!(decide(&RULE, false, None, Some(41.25)), Place::Ec2(r) if r.contains("41.2"))
        );
        assert_eq!(decide(&RULE, false, None, Some(12.0)), Place::Local);
        assert_eq!(decide(&RULE, false, None, Some(30.0)), Place::Local);
        assert_eq!(decide(&RULE, false, None, None), Place::Local);
    }

    #[test]
    fn a_local_pool_that_cannot_lease_within_the_wait_spills() {
        assert!(
            matches!(decide(&RULE, false, Some(Duration::from_secs(30)), Some(1.0)), Place::Ec2(r) if r.contains("30s"))
        );
        assert_eq!(
            decide(&RULE, false, Some(Duration::from_secs(29)), Some(1.0)),
            Place::Local
        );
    }

    #[test]
    fn a_second_pass_while_a_local_vm_is_leased_spills_at_once() {
        assert!(
            matches!(decide(&RULE, true, None, Some(0.0)), Place::Ec2(r) if r.contains("already leased"))
        );
        assert!(matches!(decide(&RULE, true, None, None), Place::Ec2(_)));
        assert_eq!(decide(&RULE, false, None, Some(0.0)), Place::Local);
    }
    #[test]
    fn ec2_always_places_on_ec2_with_a_free_local_vm_and_unreadable_pressure() {
        assert!(
            matches!(decide(&ALWAYS, false, None, Some(0.0)), Place::Ec2(r) if r.contains("ec2-always"))
        );
        assert!(matches!(decide(&ALWAYS, false, None, None), Place::Ec2(_)));
        assert_eq!(decide(&RULE, false, None, None), Place::Local);
    }

    const HOST: HostRule = HostRule { io_full_avg60: 30.0, cpu_some_avg60: 60.0 };
    const CALM: HostLoad = HostLoad { io_full_avg60: Some(1.0), cpu_some_avg60: Some(2.0) };

    #[test]
    fn a_free_host_slot_under_both_thresholds_runs_the_round_on_the_host() {
        assert_eq!(decide_host(&HOST, true, CALM), HostPlace::Host);
        assert_eq!(decide_host(&HOST, true, HostLoad::default()), HostPlace::Host);
        let at_limit = HostLoad { io_full_avg60: Some(30.0), cpu_some_avg60: Some(60.0) };
        assert_eq!(decide_host(&HOST, true, at_limit), HostPlace::Host);
    }

    #[test]
    fn a_busy_host_slot_spills_whatever_the_pressure() {
        assert!(matches!(decide_host(&HOST, false, CALM), HostPlace::Ec2(r) if r.contains("slot")));
        assert!(matches!(decide_host(&HOST, false, HostLoad::default()), HostPlace::Ec2(_)));
    }

    #[test]
    fn host_pressure_over_either_threshold_spills() {
        let io = HostLoad { io_full_avg60: Some(41.25), ..CALM };
        assert!(matches!(decide_host(&HOST, true, io), HostPlace::Ec2(r) if r.contains("io") && r.contains("41.2")));
        let cpu = HostLoad { cpu_some_avg60: Some(75.0), ..CALM };
        assert!(matches!(decide_host(&HOST, true, cpu), HostPlace::Ec2(r) if r.contains("cpu") && r.contains("75.0")));
    }

    #[test]
    fn cpu_some_is_read_not_full() {
        let psi = "some avg10=1.00 avg60=12.50 avg300=3.00 total=10\nfull avg10=0.00 avg60=99.00 avg300=9.00 total=5\n";
        assert_eq!(parse_cpu_some_avg60(psi), Some(12.5));
        assert_eq!(parse_cpu_some_avg60("nonsense"), None);
    }
}
