//! The provider seam (DESIGN.md §3.3) and the operations built once on top of it:
//! provision, key delivery and verified destroy. Every guarantee about a VM's lifecycle
//! (G2 no leak, G3 verified destroy, G4 the name fence) lives here, so it holds for every
//! provider, and the Proxmox provider only has to speak its API.

use std::time::Duration;

use crate::schema::Vm;

pub trait Provider {
    /// A VMID nobody holds right now (the hypervisor's own allocator).
    fn next_id(&self) -> Result<String, String>;
    /// Clones the round template to `vmid`, named `name`.
    fn clone_to(&self, vmid: &str, name: &str) -> Result<(), String>;
    fn start(&self, vmid: &str) -> Result<(), String>;
    /// The guest's IPv4 address on `iface`, once its agent reports one.
    fn guest_addr(&self, vmid: &str, iface: &str) -> Result<Option<String>, String>;
    /// Runs `argv` in the guest through its agent; the command's exit code.
    fn guest_exec(&self, vmid: &str, argv: &[&str]) -> Result<i32, String>;
    /// Writes `content` to the ABSOLUTE `path` in the guest through its agent.
    fn guest_file_write(&self, vmid: &str, path: &str, content: &str) -> Result<(), String>;
    fn alive(&self, vmid: &str) -> Result<bool, String>;
    fn stop(&self, vmid: &str) -> Result<(), String>;
    fn destroy(&self, vmid: &str) -> Result<(), String>;
    /// The VM's name, or None if the VMID does not exist.
    fn name_of(&self, vmid: &str) -> Result<Option<String>, String>;
}

/// The only name round-vm gives a VM, and the only name it will ever destroy (G4).
pub fn vm_name(vmid: &str) -> String {
    format!("round-{vmid}")
}

/// How long verification and boot waits poll.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub boot_tries: u32,
    pub poll: Duration,
    pub gone_tries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestroyError {
    /// The VMID exists but is not ours (G4): never touched.
    NotOurs(String),
    /// Ours, and not verifiably gone.
    Failed(String),
}

impl std::fmt::Display for DestroyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DestroyError::NotOurs(m) | DestroyError::Failed(m) => f.write_str(m),
        }
    }
}

/// Stop, destroy, then poll until the VMID is gone (G3). A VMID already gone is success.
pub fn destroy_verified(p: &dyn Provider, vmid: &str, t: Timing) -> Result<(), DestroyError> {
    let name = p
        .name_of(vmid)
        .map_err(|e| DestroyError::Failed(format!("round-vm: destroy {vmid}: cannot list VMs: {e}")))?;
    let Some(name) = name else { return Ok(()) };
    if name != vm_name(vmid) {
        return Err(DestroyError::NotOurs(format!(
            "round-vm: refusing to destroy VM {vmid}: it is named {name:?}, not {:?}",
            vm_name(vmid)
        )));
    }
    if p.alive(vmid).unwrap_or(true) {
        // A stop that fails is not final: destroy below reports the real refusal.
        let _ = p.stop(vmid);
    }
    let destroy_err = p.destroy(vmid).err();
    for i in 0..t.gone_tries.max(1) {
        if let Ok(None) = p.name_of(vmid) {
            return Ok(());
        }
        if i + 1 < t.gone_tries {
            std::thread::sleep(t.poll);
        }
    }
    Err(DestroyError::Failed(match destroy_err {
        Some(e) => format!("round-vm: destroy {vmid} failed: {e}"),
        None => format!("round-vm: destroy {vmid}: still present after destroy"),
    }))
}

/// The ssh user's home, absolute — the guest agent's working directory is not it, so a
/// relative path lands where sshd never reads (live bug 1).
pub fn user_home(user: &str) -> String {
    if user == "root" {
        "/root".to_string()
    } else {
        format!("/home/{user}")
    }
}

pub fn authorized_keys_path(user: &str) -> String {
    format!("{}/.ssh/authorized_keys", user_home(user))
}

fn exec_ok(p: &dyn Provider, vmid: &str, argv: &[&str]) -> Result<(), String> {
    match p.guest_exec(vmid, argv)? {
        0 => Ok(()),
        rc => Err(format!("`{}` exited {rc}", argv.join(" "))),
    }
}

/// Puts `pubkey` in `user`'s authorized_keys through the guest agent (DESIGN.md §2.4).
pub fn deliver_key(p: &dyn Provider, vmid: &str, user: &str, pubkey: &str) -> Result<(), String> {
    let ssh_dir = format!("{}/.ssh", user_home(user));
    let keys = authorized_keys_path(user);
    exec_ok(p, vmid, &["mkdir", "-p", "-m", "700", &ssh_dir])?;
    exec_ok(p, vmid, &["chmod", "700", &ssh_dir])?;
    let mut content = pubkey.trim_end().to_string();
    content.push('\n');
    p.guest_file_write(vmid, &keys, &content)?;
    exec_ok(p, vmid, &["chmod", "600", &keys])?;
    if user != "root" {
        let owner = format!("{user}:{user}");
        exec_ok(p, vmid, &["chown", "-R", &owner, &ssh_dir])?;
    }
    Ok(())
}

/// A provision that failed. `doomed` is a VM of ours this attempt could not verifiably
/// destroy; the caller records it so the next acquire retries the destroy (G2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionFailure {
    pub reason: String,
    pub doomed: Option<String>,
}

pub struct ProvisionSpec<'a> {
    pub iface: &'a str,
    pub ssh_user: &'a str,
    pub pubkey: &'a str,
    pub timing: Timing,
}

fn cleanup(p: &dyn Provider, vmid: &str, t: Timing, reason: String) -> ProvisionFailure {
    match destroy_verified(p, vmid, t) {
        Ok(()) | Err(DestroyError::NotOurs(_)) => ProvisionFailure { reason, doomed: None },
        Err(DestroyError::Failed(e)) => ProvisionFailure {
            reason: format!("{reason}; and {e}"),
            doomed: Some(vmid.to_string()),
        },
    }
}

/// Clone → start → address → key. `on_vmid` is told the VMID before the clone is asked
/// for, so a process that dies mid-provision leaves a record the next acquire can reap.
/// Every failure after that point destroys the VM before returning (G2).
pub fn provision(p: &dyn Provider, spec: &ProvisionSpec, on_vmid: &mut dyn FnMut(&str)) -> Result<Vm, ProvisionFailure> {
    let fail = |reason: String| ProvisionFailure { reason, doomed: None };
    let vmid = p.next_id().map_err(|e| fail(format!("API unreachable (nextid): {e}")))?;
    on_vmid(&vmid);
    let t = spec.timing;
    if let Err(e) = p.clone_to(&vmid, &vm_name(&vmid)) {
        return Err(cleanup(p, &vmid, t, format!("clone refused: {e}")));
    }
    if let Err(e) = p.start(&vmid) {
        return Err(cleanup(p, &vmid, t, format!("VM {vmid} did not start: {e}")));
    }
    let mut addr = None;
    for i in 0..t.boot_tries.max(1) {
        if let Ok(Some(a)) = p.guest_addr(&vmid, spec.iface) {
            addr = Some(a);
            break;
        }
        if i + 1 < t.boot_tries {
            std::thread::sleep(t.poll);
        }
    }
    let Some(addr) = addr else {
        return Err(cleanup(p, &vmid, t, format!("VM {vmid} did not come up on the network ({})", spec.iface)));
    };
    if let Err(e) = deliver_key(p, &vmid, spec.ssh_user, spec.pubkey) {
        return Err(cleanup(p, &vmid, t, format!("guest-agent key delivery failed on VM {vmid}: {e}")));
    }
    Ok(Vm { handle: vmid, addr })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeProvider, Step};

    fn spec(pubkey: &str) -> ProvisionSpec<'_> {
        ProvisionSpec {
            iface: "ens18",
            ssh_user: "root",
            pubkey,
            timing: Timing { boot_tries: 3, poll: Duration::ZERO, gone_tries: 3 },
        }
    }

    #[test]
    fn key_path_is_absolute_for_root_and_other_users() {
        assert_eq!(authorized_keys_path("root"), "/root/.ssh/authorized_keys");
        let ci = user_home("ci");
        assert!(ci.starts_with("/home") && ci.ends_with("/ci") && ci.matches('/').count() == 2, "{ci}");
        assert_eq!(authorized_keys_path("ci"), format!("{ci}/.ssh/authorized_keys"));
    }

    #[test]
    fn key_delivery_creates_ssh_dir_700_then_writes_the_absolute_path() {
        let p = FakeProvider::new();
        let vm = provision(&p, &spec("ssh-ed25519 AAAA host"), &mut |_| {}).unwrap();
        let calls = p.calls();
        let mkdir = calls.iter().position(|c| c == &format!("exec {} mkdir -p -m 700 /root/.ssh", vm.handle)).expect("mkdir");
        let write = calls
            .iter()
            .position(|c| c == &format!("write {} /root/.ssh/authorized_keys", vm.handle))
            .expect("write to /root/.ssh/authorized_keys");
        assert!(mkdir < write, "{calls:?}");
        assert!(calls.contains(&format!("exec {} chmod 600 /root/.ssh/authorized_keys", vm.handle)));
        assert_eq!(p.file(&vm.handle, "/root/.ssh/authorized_keys").as_deref(), Some("ssh-ed25519 AAAA host\n"));
    }

    #[test]
    fn provision_returns_the_guest_address_and_names_the_vm_round_id() {
        let p = FakeProvider::new();
        let mut seen = None;
        let vm = provision(&p, &spec("k"), &mut |id| seen = Some(id.to_string())).unwrap();
        assert_eq!(seen.as_deref(), Some(vm.handle.as_str()));
        assert_eq!(p.name(&vm.handle), Some(vm_name(&vm.handle)));
        assert_eq!(vm.addr, format!("10.0.0.{}", vm.handle));
    }

    #[test]
    fn every_failure_after_clone_destroys_the_vm() {
        for step in [Step::Clone, Step::Start, Step::Addr, Step::Exec, Step::FileWrite] {
            let p = FakeProvider::new();
            p.fail(step);
            let e = provision(&p, &spec("k"), &mut |_| {}).unwrap_err();
            assert!(e.doomed.is_none(), "{step:?}: {e:?}");
            assert!(p.live_vms().is_empty(), "{step:?} leaked {:?}", p.live_vms());
            assert!(!e.reason.is_empty());
        }
    }

    #[test]
    fn a_vm_that_will_not_die_is_reported_doomed_not_forgotten() {
        let p = FakeProvider::new();
        p.fail(Step::Start);
        p.fail(Step::Destroy);
        let e = provision(&p, &spec("k"), &mut |_| {}).unwrap_err();
        assert_eq!(e.doomed.as_deref(), Some("100"));
        assert!(e.reason.contains("did not start"), "{}", e.reason);
    }

    #[test]
    fn nextid_failure_names_the_api() {
        let p = FakeProvider::new();
        p.fail(Step::NextId);
        let e = provision(&p, &spec("k"), &mut |_| {}).unwrap_err();
        assert!(e.reason.contains("API unreachable"), "{}", e.reason);
    }

    #[test]
    fn destroy_is_verified_and_a_gone_vmid_is_success() {
        let p = FakeProvider::new();
        let vm = provision(&p, &spec("k"), &mut |_| {}).unwrap();
        let t = spec("k").timing;
        destroy_verified(&p, &vm.handle, t).unwrap();
        assert!(p.live_vms().is_empty());
        destroy_verified(&p, &vm.handle, t).unwrap();
    }

    #[test]
    fn destroy_that_leaves_the_vm_listed_is_an_error() {
        let p = FakeProvider::new();
        let vm = provision(&p, &spec("k"), &mut |_| {}).unwrap();
        p.fail(Step::DestroySilently);
        let e = destroy_verified(&p, &vm.handle, spec("k").timing).unwrap_err();
        assert!(matches!(e, DestroyError::Failed(ref m) if m.contains("still present")), "{e:?}");
    }

    #[test]
    fn destroy_refuses_a_vm_not_named_round_id() {
        let p = FakeProvider::new();
        p.plant("9000", "ci-template");
        let e = destroy_verified(&p, "9000", spec("k").timing).unwrap_err();
        assert!(matches!(e, DestroyError::NotOurs(_)), "{e:?}");
        assert_eq!(p.name("9000").as_deref(), Some("ci-template"));
    }

    #[test]
    fn a_clone_that_lost_the_nextid_race_never_destroys_the_winner() {
        let p = FakeProvider::new();
        p.plant("100", "someone-else");
        let e = provision(&p, &spec("k"), &mut |_| {}).unwrap_err();
        assert!(e.reason.contains("clone refused"), "{}", e.reason);
        assert_eq!(p.name("100").as_deref(), Some("someone-else"));
    }
}
