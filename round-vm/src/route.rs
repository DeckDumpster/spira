//! One [`Provider`] over the local hypervisor and EC2: a handle's shape says whose it is, so
//! every guarantee built on the seam (G2, G3, G4) holds for a spilled VM through the same calls.

use std::sync::Arc;

use crate::ec2::is_ec2_handle;
use crate::provider::Provider;

pub struct Routed {
    pub local: Box<dyn Provider>,
    pub ec2: Option<Arc<dyn Provider>>,
}

impl Routed {
    fn of(&self, vmid: &str) -> Result<&dyn Provider, String> {
        if !is_ec2_handle(vmid) {
            return Ok(self.local.as_ref());
        }
        self.ec2.as_deref().ok_or_else(|| format!("{vmid} is an EC2 VM and the EC2 provider is not configured"))
    }
}

macro_rules! to {
    ($s:ident, $id:ident, $m:ident $(, $a:expr)*) => {
        $s.of($id)?.$m($id $(, $a)*)
    };
}

impl Provider for Routed {
    fn next_id(&self) -> Result<String, String> {
        self.local.next_id()
    }
    fn clone_to(&self, vmid: &str, name: &str) -> Result<(), String> {
        to!(self, vmid, clone_to, name)
    }
    fn start(&self, vmid: &str) -> Result<(), String> {
        to!(self, vmid, start)
    }
    fn guest_addr(&self, vmid: &str, iface: &str) -> Result<Option<String>, String> {
        to!(self, vmid, guest_addr, iface)
    }
    fn guest_exec(&self, vmid: &str, argv: &[&str]) -> Result<i32, String> {
        to!(self, vmid, guest_exec, argv)
    }
    fn guest_file_write(&self, vmid: &str, path: &str, content: &str) -> Result<(), String> {
        to!(self, vmid, guest_file_write, path, content)
    }
    fn alive(&self, vmid: &str) -> Result<bool, String> {
        to!(self, vmid, alive)
    }
    fn stop(&self, vmid: &str) -> Result<(), String> {
        to!(self, vmid, stop)
    }
    fn destroy(&self, vmid: &str) -> Result<(), String> {
        to!(self, vmid, destroy)
    }
    fn name_of(&self, vmid: &str) -> Result<Option<String>, String> {
        to!(self, vmid, name_of)
    }
    fn shutdown(&self, vmid: &str) -> Result<(), String> {
        to!(self, vmid, shutdown)
    }
    fn make_template(&self, vmid: &str) -> Result<(), String> {
        to!(self, vmid, make_template)
    }
    fn is_template(&self, vmid: &str) -> Result<bool, String> {
        to!(self, vmid, is_template)
    }
    fn hold_for_ci(&self) {
        self.local.hold_for_ci()
    }
    fn stale(&self) -> Vec<String> {
        let mut v = self.local.stale();
        v.extend(self.ec2.iter().flat_map(|e| e.stale()));
        v
    }
    fn diagnose(&self, vmid: &str) -> String {
        self.of(vmid).map(|p| p.diagnose(vmid)).unwrap_or_default()
    }
}
