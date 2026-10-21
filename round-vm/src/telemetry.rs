pub const COMPONENT: &str = "round-vm";
pub const OPERATIONS: &[&str] = &["acquire", "release", "run", "status", "reap", "warm", "drain", "_provision-bg", "template", "refresh", "teardown", "recipe-sha", "ec2-probe"];

pub fn endpoint() -> Option<String> {
    spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok()
}

pub fn operation(args: &[String]) -> &'static str {
    spira_otel::metrics::run::operation_label(args.first().map(String::as_str), OPERATIONS)
}

pub fn metered_code(args: &[String], f: impl FnOnce() -> i32) -> i32 {
    spira_otel::metrics::run::metered_code(endpoint, COMPONENT, operation(args), f)
}

/// One placed pass: where it ran, on what instance type, how long it waited for capacity and booted.
/// The spill rate is the `ec2` share of `spira_round_vm_placements`.
pub fn push_placement(place: &str, instance_type: &str, wait_s: u64, boot_s: u64) {
    push_placement_to(endpoint().as_deref(), place, instance_type, wait_s, boot_s);
}

/// The pass's verdict, cause and phase durations: the series the rounds dashboard reads.
pub fn push_pass(rec: &tsd::pass::PassRecord) {
    push_pass_to(endpoint().as_deref(), rec);
}

fn push_pass_to(endpoint: Option<&str>, rec: &tsd::pass::PassRecord) {
    rec.push_metrics(&spira_otel::metrics::MetricsSink::new(endpoint, "spira-round-vm", spira_otel::metrics::run::DEADLINE));
}

fn push_placement_to(endpoint: Option<&str>, place: &str, instance_type: &str, wait_s: u64, boot_s: u64) {
    use spira_otel::metrics::Point;
    spira_otel::metrics::MetricsSink::new(endpoint, "spira-round-vm", spira_otel::metrics::run::DEADLINE).push(
        &[
            Point::Count { name: "spira_round_vm_placements", value: 1 },
            Point::Seconds { name: "spira_round_vm_capacity_wait_seconds", value: wait_s as f64 },
            Point::Seconds { name: "spira_round_vm_boot_seconds", value: boot_s as f64 },
        ],
        &[("place", place), ("instance_type", instance_type)],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_verb_pushes_its_count_duration_and_error_outcome() {
        let (addr, h) = spira_otel::metrics::run::fixture_sink();
        let args = vec!["acquire".to_string()];
        let code = spira_otel::metrics::run::metered_code(|| Some(addr), COMPONENT, operation(&args), || 1);
        assert_eq!(code, 1);
        let body = h.join().unwrap();
        for want in ["spira_component_invocations", "spira_component_duration_seconds", "round-vm", "acquire", "error"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    #[test]
    fn a_placed_pass_pushes_its_place_instance_type_wait_and_boot() {
        let (addr, h) = spira_otel::metrics::run::fixture_sink();
        push_placement_to(Some(&addr), "ec2", "c7a.4xlarge", 30, 90);
        let body = h.join().unwrap();
        for want in ["spira_round_vm_placements", "spira_round_vm_capacity_wait_seconds", "spira_round_vm_boot_seconds", "c7a.4xlarge", "ec2"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    #[test]
    fn a_judged_pass_pushes_its_verdict_cause_place_and_phases() {
        use tsd::pass::{Cause, PassRecord, Place};
        for (cause, verdict) in [(Cause::Green, "green"), (Cause::RedAttributed, "red")] {
            let (addr, h) = spira_otel::metrics::run::fixture_sink();
            let rec = PassRecord {
                repo: "r".into(),
                batch: "b".into(),
                pass: "1".into(),
                members: vec![],
                instance_type: "16vcpu".into(),
                vcpus: Some(16),
                place: Place::Proxmox,
                capacity_wait_s: 1,
                boot_s: 2,
                build_s: 3,
                suites_s: 4,
                cause,
                detail: String::new(),
            };
            push_pass_to(Some(&addr), &rec);
            let body = h.join().unwrap();
            for want in ["spira_cert_pass_verdicts", "spira_cert_pass_causes", "spira_cert_pass_total_seconds", "spira_cert_pass_boot_seconds", "place", "proxmox", "16vcpu", verdict] {
                assert!(body.contains(want), "{want} missing from the push");
            }
        }
    }

    #[test]
    fn an_unlisted_verb_is_labelled_unknown() {
        assert_eq!(operation(&["bogus".into()]), "unknown");
    }
}
