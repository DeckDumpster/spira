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
    Shutdown,
    MakeTemplate,
}

#[derive(Debug, Clone)]
struct FakeVm {
    name: String,
    running: bool,
    files: BTreeMap<String, String>,
    template: bool,
    tags: Vec<String>,
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
            FakeVm {
                name: name.into(),
                running: true,
                files: BTreeMap::new(),
                template: false,
                tags: vec![crate::provider::OWNER_TAG.into()],
            },
        );
    }
    pub fn plant_tagged(&self, vmid: &str, name: &str, tag: &str) {
        self.plant(vmid, name);
        self.0.lock().unwrap().vms.get_mut(vmid).unwrap().tags = Some(tag).filter(|t| !t.is_empty()).map(String::from).into_iter().collect();
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
        g.vms.insert(vmid.into(), FakeVm { name: name.into(), running: false, files: BTreeMap::new(), template: false, tags: vec![crate::provider::OWNER_TAG.into()] });
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
    fn tags_of(&self, vmid: &str) -> Result<Vec<String>, String> {
        Ok(self.0.lock().unwrap().vms.get(vmid).map(|v| v.tags.clone()).unwrap_or_default())
    }
    fn name_of(&self, vmid: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().vms.get(vmid).map(|v| v.name.clone()))
    }
    fn shutdown(&self, vmid: &str) -> Result<(), String> {
        self.step(Step::Shutdown, format!("shutdown {vmid}"))?;
        let mut g = self.0.lock().unwrap();
        g.vms.get_mut(vmid).ok_or("no such VM")?.running = false;
        Ok(())
    }
    fn make_template(&self, vmid: &str) -> Result<(), String> {
        self.step(Step::MakeTemplate, format!("template {vmid}"))?;
        let mut g = self.0.lock().unwrap();
        let v = g.vms.get_mut(vmid).ok_or("no such VM")?;
        if v.running {
            return Err("cannot convert a running VM".into());
        }
        v.template = true;
        Ok(())
    }
    fn is_template(&self, vmid: &str) -> Result<bool, String> {
        Ok(self.0.lock().unwrap().vms.get(vmid).map(|v| v.template).unwrap_or(false))
    }
}

impl FakeProvider {
    /// Every VM that exists, by id, with its name.
    pub fn all_vms(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().vms.iter().map(|(id, v)| (id.clone(), v.name.clone())).collect()
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

#[derive(Debug, Clone)]
pub struct FakeEc2Inst {
    pub id: String,
    pub tag: Option<String>,
    pub name: String,
    pub launched_at: u64,
    pub instance_type: String,
}

#[derive(Default)]
struct Ec2Inner {
    n: u32,
    insts: Vec<FakeEc2Inst>,
    scripts: Vec<String>,
    user_data: Vec<String>,
    never_joins: bool,
    unregistered_calls: u32,
    fail_run: bool,
    refused_types: Vec<String>,
    vanishing: Vec<(String, String)>,
    ended: std::collections::HashMap<String, String>,
    launched_types: Vec<String>,
    images: Vec<crate::ec2::Image>,
    launched_amis: Vec<String>,
    stores: Vec<crate::ec2::ImageStore>,
    in_use: Vec<String>,
    removed: Vec<String>,
}

/// An EC2 account in memory: instances, the SSM scripts run on them, the tailnet.
#[derive(Clone, Default)]
pub struct FakeEc2(Arc<Mutex<Ec2Inner>>);

impl FakeEc2 {
    pub fn plant(&self, id: &str, tag: Option<&str>, name: &str, launched_at: u64) {
        self.0.lock().unwrap().insts.push(FakeEc2Inst { id: id.into(), tag: tag.map(String::from), name: name.into(), launched_at, instance_type: String::new() });
    }
    pub fn plant_image(&self, id: &str, recipe_sha: &str, created: &str) {
        self.0.lock().unwrap().images.push(crate::ec2::Image { id: id.into(), recipe_sha: recipe_sha.into(), created: created.into() });
    }
    pub fn plant_store(&self, s: crate::ec2::ImageStore) {
        self.0.lock().unwrap().stores.push(s);
    }
    pub fn plant_in_use(&self, ami: &str) {
        self.0.lock().unwrap().in_use.push(ami.to_string());
    }
    pub fn removed(&self) -> Vec<String> {
        self.0.lock().unwrap().removed.clone()
    }
    pub fn launched_amis(&self) -> Vec<String> {
        self.0.lock().unwrap().launched_amis.clone()
    }
    pub fn ssm_unregistered_for(&self, calls: u32) {
        self.0.lock().unwrap().unregistered_calls = calls;
    }
    pub fn never_joins_tailnet(&self) {
        self.0.lock().unwrap().never_joins = true;
    }
    pub fn refuse_type(&self, t: &str) {
        self.0.lock().unwrap().refused_types.push(t.to_string());
    }
    pub fn vanish_type(&self, t: &str, reason: &str) {
        self.0.lock().unwrap().vanishing.push((t.to_string(), reason.to_string()));
    }
    pub fn launched_types(&self) -> Vec<String> {
        self.0.lock().unwrap().launched_types.clone()
    }
    pub fn fail_run(&self) {
        self.0.lock().unwrap().fail_run = true;
    }
    pub fn ids(&self) -> Vec<String> {
        self.0.lock().unwrap().insts.iter().map(|i| i.id.clone()).collect()
    }
    pub fn scripts(&self) -> Vec<String> {
        self.0.lock().unwrap().scripts.clone()
    }
    pub fn user_data(&self) -> Vec<String> {
        self.0.lock().unwrap().user_data.clone()
    }
}

impl crate::ec2::Ec2Api for FakeEc2 {
    fn images(&self) -> Result<Vec<crate::ec2::Image>, String> {
        Ok(self.0.lock().unwrap().images.clone())
    }
    fn image_stores(&self) -> Result<Vec<crate::ec2::ImageStore>, String> {
        Ok(self.0.lock().unwrap().stores.clone())
    }
    fn images_in_use(&self) -> Result<Vec<String>, String> {
        Ok(self.0.lock().unwrap().in_use.clone())
    }
    fn deregister_image(&self, id: &str) -> Result<(), String> {
        self.0.lock().unwrap().removed.push(format!("deregister {id}"));
        Ok(())
    }
    fn delete_snapshot(&self, id: &str) -> Result<(), String> {
        self.0.lock().unwrap().removed.push(format!("snapshot {id}"));
        Ok(())
    }
    fn run(&self, ami: &str, instance_type: &str, handle: &str, name: &str, user_data: &str) -> Result<String, String> {
        let mut g = self.0.lock().unwrap();
        g.launched_amis.push(ami.to_string());
        if g.fail_run {
            return Err("fake run-instances refused".into());
        }
        if g.refused_types.iter().any(|t| t == instance_type) {
            return Err(format!("aws ec2: An error occurred (VcpuLimitExceeded) when calling the RunInstances operation: {instance_type}"));
        }
        g.launched_types.push(instance_type.to_string());
        g.n += 1;
        let id = format!("i-{:04}", g.n);
        g.user_data.push(user_data.to_string());
        if let Some((_, reason)) = g.vanishing.iter().find(|(t, _)| t == instance_type).cloned() {
            g.ended.insert(handle.to_string(), format!("state terminated, reason {reason} (fake), transition \"\""));
            return Ok(id);
        }
        g.insts.push(FakeEc2Inst { id: id.clone(), tag: Some(handle.into()), name: name.into(), launched_at: crate::schema::now(), instance_type: instance_type.to_string() });
        Ok(id)
    }
    fn run_dry(&self, ami: &str, _instance_type: &str, _handle: &str, _name: &str, _user_data: &str) -> Result<(), String> {
        let mut g = self.0.lock().unwrap();
        g.launched_amis.push(format!("dry:{ami}"));
        if g.fail_run {
            return Err("fake run-instances refused".into());
        }
        Ok(())
    }
    fn find(&self, handle: &str) -> Result<Option<crate::ec2::Inst>, String> {
        Ok(self.list_tagged()?.into_iter().find(|i| i.handle == handle))
    }
    fn list_tagged(&self) -> Result<Vec<crate::ec2::Inst>, String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .insts
            .iter()
            .filter_map(|i| {
                i.tag.as_ref().map(|t| crate::ec2::Inst { id: i.id.clone(), handle: t.clone(), name: i.name.clone(), running: true, launched_at: i.launched_at, instance_type: i.instance_type.clone() })
            })
            .collect())
    }
    fn end_reason(&self, handle: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().ended.get(handle).cloned())
    }
    fn terminate(&self, id: &str) -> Result<(), String> {
        self.0.lock().unwrap().insts.retain(|i| i.id != id);
        Ok(())
    }
    fn ssm(&self, id: &str, script: &str) -> Result<(i32, String), String> {
        let mut g = self.0.lock().unwrap();
        if !g.insts.iter().any(|i| i.id == id) {
            return Err(format!("{id} is not a managed instance"));
        }
        if g.unregistered_calls > 0 {
            g.unregistered_calls -= 1;
            return Err(format!("aws ssm SendCommand: InvalidInstanceId: {id}"));
        }
        g.scripts.push(script.to_string());
        Ok((0, "diagnostic output".into()))
    }
    fn tailnet_addr(&self, hostname: &str) -> Result<Option<String>, String> {
        let g = self.0.lock().unwrap();
        Ok(g.insts.iter().find(|i| i.name == hostname && !g.never_joins).map(|i| format!("100.64.0.{}", i.id.trim_start_matches("i-").trim_start_matches('0'))))
    }
}
