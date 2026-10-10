//! Sweep-yield decision (sp-55ni6): sweep VMs must not boot or keep running
//! while an ephemeral CI VM is provisioning. Pure decision function; the
//! Proxmox listing and `qm suspend`/resume wiring is a follow-up.

pub const CI_PREFIXES: [&str; 2] = ["ci-", "gh-runner-"];
pub const MAX_WAIT_SECS: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmInfo {
    pub name: String,
    pub status: String,
    pub lock: Option<String>,
    pub registered: bool,
    /// A Proxmox template: never a runner, so never "provisioning".
    pub template: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Yield {
    Proceed,
    Hold,
}

fn is_ci(vm: &VmInfo) -> bool {
    // A template (ci-runner-template-resealed) is stopped and unregistered forever; counting it
    // held every cold provision for the full MAX_WAIT_SECS (r-auto-110, 2026-10-10).
    !vm.template && CI_PREFIXES.iter().any(|p| vm.name.starts_with(p))
}

/// A CI VM is provisioning if it is locked (clone in progress) or not yet
/// a registered running runner.
pub fn ci_provisioning(vms: &[VmInfo]) -> bool {
    vms.iter()
        .filter(|v| is_ci(v))
        .any(|v| v.lock.is_some() || !(v.status == "running" && v.registered))
}

/// Hold while CI provisions, but never longer than MAX_WAIT_SECS.
pub fn decide(vms: &[VmInfo], waited_secs: u64) -> Yield {
    if waited_secs >= MAX_WAIT_SECS || !ci_provisioning(vms) {
        Yield::Proceed
    } else {
        Yield::Hold
    }
}

/// Block while CI provisions, polling `list` every `poll` via `sleep`,
/// for at most MAX_WAIT_SECS. Returns seconds waited. A listing error
/// proceeds (never block the sweep on a broken probe).
pub fn wait_for_ci(
    list: &dyn Fn() -> Result<Vec<VmInfo>, String>,
    sleep: &dyn Fn(u64),
    poll_secs: u64,
) -> u64 {
    let mut waited = 0;
    loop {
        match list() {
            Ok(v) if decide(&v, waited) == Yield::Hold => {
                sleep(poll_secs);
                waited += poll_secs;
            }
            _ => return waited,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn vm(n: &str, s: &str, l: Option<&str>, r: bool) -> VmInfo {
        VmInfo { name: n.into(), status: s.into(), lock: l.map(String::from), registered: r, template: false }
    }
    #[test]
    fn a_ci_named_template_never_holds_a_provision() {
        let t = VmInfo { name: "ci-runner-template-resealed".into(), status: "stopped".into(), lock: None, registered: false, template: true };
        assert_eq!(decide(&[t.clone()], 0), Yield::Proceed);
        let live = VmInfo { template: false, ..t };
        assert_eq!(decide(&[live], 0), Yield::Hold, "positive control: a stopped, unregistered ci VM still holds");
    }

    #[test]
    fn sweep_only_proceeds() {
        assert_eq!(decide(&[vm("round-102", "running", None, false)], 0), Yield::Proceed);
    }
    #[test]
    fn cloning_holds() {
        assert_eq!(decide(&[vm("ci-1", "stopped", Some("clone"), false)], 0), Yield::Hold);
    }
    #[test]
    fn unregistered_holds() {
        assert_eq!(decide(&[vm("gh-runner-7", "running", None, false)], 5), Yield::Hold);
    }
    #[test]
    fn registered_proceeds() {
        assert_eq!(decide(&[vm("gh-runner-7", "running", None, true)], 5), Yield::Proceed);
    }
    #[test]
    fn bounded_wait() {
        assert_eq!(decide(&[vm("ci-1", "stopped", Some("clone"), false)], 600), Yield::Proceed);
    }
    #[test]
    fn wait_is_bounded_and_returns_when_clear() {
        let hold = || Ok(vec![vm("ci-1", "stopped", Some("clone"), false)]);
        assert_eq!(wait_for_ci(&hold, &|_| {}, 60), 600);
        let n = std::cell::Cell::new(0);
        let l = || {
            n.set(n.get() + 1);
            Ok(if n.get() < 3 { vec![vm("ci-1", "running", None, false)] } else { vec![] })
        };
        assert_eq!(wait_for_ci(&l, &|_| {}, 30), 60);
        assert_eq!(wait_for_ci(&|| Err("x".into()), &|_| {}, 30), 0);
    }
}
