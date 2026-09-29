//! Test doubles: a temp dir, a fake provider with a VM table, and a fake alarm.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::provider::Provider;

pub struct TempDir(testkit::TempDir);

#[allow(clippy::new_without_default)]
impl TempDir {
    pub fn new() -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = testkit::TempDir::new(&format!("round-vm-test-{}", N.fetch_add(1, Ordering::SeqCst)));
        TempDir(p)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    NextId,
    Clone,
    Start,
    Addr,
    Exec,
    FileWrite,
    /// destroy returns an error and the VM stays.
    Destroy,
    /// destroy reports success and the VM stays.
    DestroySilently,
}

#[derive(Debug, Clone)]
struct FakeVm {
    name: String,
    running: bool,
    files: BTreeMap<String, String>,
}

#[derive(Default)]
struct Inner {
    next: u32,
    vms: BTreeMap<String, FakeVm>,
    fails: BTreeSet<Step>,
    calls: Vec<String>,
}

/// A hypervisor in memory. VMIDs count up from 100; a clone onto a taken VMID fails, as
/// the real one does.
#[derive(Clone)]
pub struct FakeProvider(Arc<Mutex<Inner>>);

#[allow(clippy::new_without_default)]
impl FakeProvider {
    pub fn new() -> FakeProvider {
        FakeProvider(Arc::new(Mutex::new(Inner { next: 100, ..Default::default() })))
    }
    pub fn fail(&self, s: Step) {
        self.0.lock().unwrap().fails.insert(s);
    }
    pub fn heal(&self) {
        self.0.lock().unwrap().fails.clear();
    }
    pub fn plant(&self, vmid: &str, name: &str) {
        self.0.lock().unwrap().vms.insert(
            vmid.into(),
            FakeVm { name: name.into(), running: true, files: BTreeMap::new() },
        );
    }
    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }
    pub fn count(&self, prefix: &str) -> usize {
        self.calls().iter().filter(|c| c.starts_with(prefix)).count()
    }
    pub fn name(&self, vmid: &str) -> Option<String> {
        self.0.lock().unwrap().vms.get(vmid).map(|v| v.name.clone())
    }
    pub fn file(&self, vmid: &str, path: &str) -> Option<String> {
        self.0.lock().unwrap().vms.get(vmid).and_then(|v| v.files.get(path).cloned())
    }
    /// VMs round-vm created that still exist.
    pub fn live_vms(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .vms
            .iter()
            .filter(|(id, v)| v.name == format!("round-{id}"))
            .map(|(id, _)| id.clone())
            .collect()
    }
    pub fn kill(&self, vmid: &str) {
        if let Some(v) = self.0.lock().unwrap().vms.get_mut(vmid) {
            v.running = false;
        }
    }

    fn step(&self, s: Step, call: String) -> Result<(), String> {
        let mut g = self.0.lock().unwrap();
        g.calls.push(call);
        if g.fails.contains(&s) {
            return Err(format!("fake {s:?} failure"));
        }
        Ok(())
    }
}

impl Provider for FakeProvider {
    fn next_id(&self) -> Result<String, String> {
        self.step(Step::NextId, "nextid".into())?;
        let mut g = self.0.lock().unwrap();
        let id = g.next;
        g.next += 1;
        Ok(id.to_string())
    }
    fn clone_to(&self, vmid: &str, name: &str) -> Result<(), String> {
        self.step(Step::Clone, format!("clone {vmid} {name}"))?;
        let mut g = self.0.lock().unwrap();
        if g.vms.contains_key(vmid) {
            return Err(format!("VM {vmid} already exists"));
        }
        g.vms.insert(vmid.into(), FakeVm { name: name.into(), running: false, files: BTreeMap::new() });
        Ok(())
    }
    fn start(&self, vmid: &str) -> Result<(), String> {
        self.step(Step::Start, format!("start {vmid}"))?;
        let mut g = self.0.lock().unwrap();
        g.vms.get_mut(vmid).ok_or("no such VM")?.running = true;
        Ok(())
    }
    fn guest_addr(&self, vmid: &str, _iface: &str) -> Result<Option<String>, String> {
        self.step(Step::Addr, format!("addr {vmid}"))?;
        let g = self.0.lock().unwrap();
        Ok(g.vms.get(vmid).filter(|v| v.running).map(|_| format!("10.0.0.{vmid}")))
    }
    fn guest_exec(&self, vmid: &str, argv: &[&str]) -> Result<i32, String> {
        self.step(Step::Exec, format!("exec {vmid} {}", argv.join(" ")))?;
        Ok(0)
    }
    fn guest_file_write(&self, vmid: &str, path: &str, content: &str) -> Result<(), String> {
        self.step(Step::FileWrite, format!("write {vmid} {path}"))?;
        let mut g = self.0.lock().unwrap();
        g.vms.get_mut(vmid).ok_or("no such VM")?.files.insert(path.into(), content.into());
        Ok(())
    }
    fn alive(&self, vmid: &str) -> Result<bool, String> {
        let g = self.0.lock().unwrap();
        Ok(g.vms.get(vmid).map(|v| v.running).unwrap_or(false))
    }
    fn stop(&self, vmid: &str) -> Result<(), String> {
        let mut g = self.0.lock().unwrap();
        g.calls.push(format!("stop {vmid}"));
        if let Some(v) = g.vms.get_mut(vmid) {
            v.running = false;
        }
        Ok(())
    }
    fn destroy(&self, vmid: &str) -> Result<(), String> {
        let mut g = self.0.lock().unwrap();
        g.calls.push(format!("destroy {vmid}"));
        if g.fails.contains(&Step::Destroy) {
            return Err("fake destroy failure".into());
        }
        if g.fails.contains(&Step::DestroySilently) {
            return Ok(());
        }
        g.vms.remove(vmid);
        Ok(())
    }
    fn name_of(&self, vmid: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().vms.get(vmid).map(|v| v.name.clone()))
    }
}

/// Records every alarm instead of mailing it.
#[derive(Clone, Default)]
pub struct FakeAlarm(pub Arc<Mutex<Vec<String>>>);

impl crate::alarm::Alarm for FakeAlarm {
    fn raise(&self, reason: &str) {
        self.0.lock().unwrap().push(reason.to_string());
    }
}

impl FakeAlarm {
    pub fn sent(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}
