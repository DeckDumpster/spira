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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Yield {
    Proceed,
    Hold,
}

fn is_ci(vm: &VmInfo) -> bool {
    CI_PREFIXES.iter().any(|p| vm.name.starts_with(p))
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

#[cfg(test)]
mod tests {
    use super::*;
    fn vm(n: &str, s: &str, l: Option<&str>, r: bool) -> VmInfo {
        VmInfo { name: n.into(), status: s.into(), lock: l.map(String::from), registered: r }
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
}
