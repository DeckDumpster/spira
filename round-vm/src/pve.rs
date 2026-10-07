//! The Proxmox provider: the HTTP API, spoken directly (DESIGN.md §3.3). TLS is verified
//! against `PVE_CACERT`; there is no insecure fallback.

use std::io::BufReader;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::PveEnv;
use crate::provider::Provider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Delete => "DELETE",
        }
    }
}

/// One API call: `params` go in the query string for GET/DELETE and as a form body for
/// POST. Returns the response's `data`.
pub trait Transport {
    fn call(&self, method: Method, path: &str, params: &[(&str, String)]) -> Result<Value, String>;
}

pub struct HttpTransport {
    agent: ureq::Agent,
    base: String,
    auth: String,
}

impl HttpTransport {
    pub fn new(env: &PveEnv) -> Result<HttpTransport, String> {
        let pem = std::fs::File::open(&env.cacert).map_err(|e| format!("PVE_CACERT {}: {e}", env.cacert.display()))?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut BufReader::new(pem)) {
            let cert = cert.map_err(|e| format!("PVE_CACERT {}: {e}", env.cacert.display()))?;
            roots.add(cert).map_err(|e| format!("PVE_CACERT {}: {e}", env.cacert.display()))?;
        }
        if roots.is_empty() {
            return Err(format!("PVE_CACERT {}: no certificate in it", env.cacert.display()));
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        // batch-job: Proxmox VM clone and start calls take as long as the hypervisor does
        let agent = ureq::AgentBuilder::new().tls_config(Arc::new(tls)).timeout(Duration::from_secs(60)).build();
        Ok(HttpTransport {
            agent,
            base: format!("https://{}:{}/api2/json", env.api_host, env.api_port),
            auth: format!("PVEAPIToken={}={}", env.token_id, env.token_secret),
        })
    }
}

impl Transport for HttpTransport {
    fn call(&self, method: Method, path: &str, params: &[(&str, String)]) -> Result<Value, String> {
        let mut req = self.agent.request(method.as_str(), &format!("{}{path}", self.base)).set("Authorization", &self.auth);
        let resp = match method {
            Method::Post => {
                let form: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
                req.send_form(&form)
            }
            Method::Get | Method::Delete => {
                for (k, v) in params {
                    req = req.query(k, v);
                }
                req.call()
            }
        };
        let body = match resp {
            Ok(r) => r.into_string().map_err(|e| format!("{} {path}: {e}", method.as_str()))?,
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(format!("HTTP {code} ({} {path}): {}", method.as_str(), body.trim()));
            }
            Err(e) => return Err(format!("{} {path}: {e}", method.as_str())),
        };
        let v: Value = serde_json::from_str(&body).map_err(|e| format!("{} {path}: not JSON: {e}", method.as_str()))?;
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    }
}

/// Percent-encodes one path segment (a task UPID is full of ':').
pub fn encode_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The form of `agent/file-write`: the content goes raw. `encode` defaults to true (the
/// API base64-encodes it); `encoding` is not a parameter at all (HTTP 400, live bug 2).
pub fn file_write_params(path: &str, content: &str) -> Vec<(&'static str, String)> {
    vec![("file", path.to_string()), ("content", content.to_string())]
}

/// The form of `agent/exec`: one `command` per argv element and nothing else —
/// `capture-output` is an unknown property on this Proxmox.
pub fn exec_params(argv: &[&str]) -> Vec<(&'static str, String)> {
    argv.iter().map(|a| ("command", a.to_string())).collect()
}

/// The first IPv4 address on `iface` in an `agent/network-get-interfaces` result.
pub fn ipv4_on(data: &Value, iface: &str) -> Option<String> {
    data.get("result")?
        .as_array()?
        .iter()
        .filter(|i| i.get("name").and_then(Value::as_str) == Some(iface))
        .flat_map(|i| i.get("ip-addresses").and_then(Value::as_array).cloned().unwrap_or_default())
        .find(|a| a.get("ip-address-type").and_then(Value::as_str) == Some("ipv4"))
        .and_then(|a| a.get("ip-address").and_then(Value::as_str).map(str::to_string))
        .filter(|a| !a.is_empty())
}

/// A round VM must never be ballooned down or out-weighed by the host's builders: balloon 0
/// fixes its memory at the template's size, and a high CPU weight wins contention for cores.
const ROUND_CPUUNITS: u32 = 10000;

/// The MAC a VMID always gets: the VMID is the pool slot, so a re-clone of a slot reuses
/// its DHCP lease instead of minting a new one. `02` marks it locally administered.
pub fn slot_mac(vmid: &str) -> Result<String, String> {
    let n: u32 = vmid.parse().map_err(|_| format!("VMID {vmid:?} is not a number"))?;
    if n >= 1 << 24 {
        return Err(format!("VMID {vmid} does not fit a 24-bit MAC suffix"));
    }
    Ok(format!("02:52:56:{:02x}:{:02x}:{:02x}", n >> 16, (n >> 8) & 0xff, n & 0xff))
}

/// `net0` with its `model=MAC` field's MAC replaced; None if it has no such field.
pub fn with_mac(net0: &str, mac: &str) -> Option<String> {
    let mut fields: Vec<String> = net0.split(',').map(str::to_string).collect();
    let first = fields.first_mut()?;
    let (model, _) = first.split_once('=')?;
    *first = format!("{model}={}", mac.to_uppercase());
    Some(fields.join(","))
}

fn as_id(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s == "1" || s == "true",
        _ => false,
    }
}

pub struct Pve<T: Transport> {
    pub t: T,
    pub env: PveEnv,
    pub task_timeout: Duration,
    pub exec_timeout: Duration,
    pub poll: Duration,
}

impl<T: Transport> Pve<T> {
    fn node(&self) -> String {
        format!("/nodes/{}", self.env.node)
    }

    fn qemu(&self, vmid: &str) -> String {
        format!("{}/qemu/{vmid}", self.node())
    }

    /// Lists VMs for the sweep-yield decision (sp-55ni6). `registered` is
    /// approximated by "running": PVE cannot see runner registration.
    pub fn list_vms(&self) -> Result<Vec<crate::ci_yield::VmInfo>, String> {
        let d = self.t.call(Method::Get, &format!("{}/qemu", self.node()), &[])?;
        let list = d.as_array().ok_or_else(|| format!("VM list is not a list: {d}"))?;
        Ok(list
            .iter()
            .map(|v| {
                let status = v.get("status").and_then(Value::as_str).unwrap_or("").to_string();
                crate::ci_yield::VmInfo {
                    name: v.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    registered: status == "running",
                    lock: v.get("lock").and_then(Value::as_str).map(String::from),
                    status,
                }
            })
            .collect())
    }

    /// POSTs/DELETEs something that returns a task, and waits for the task to finish OK.
    fn task(&self, method: Method, path: &str, params: &[(&str, String)]) -> Result<(), String> {
        let data = self.t.call(method, path, params)?;
        let Some(upid) = data.as_str().filter(|s| !s.is_empty()).map(str::to_string) else {
            return Ok(());
        };
        let status_path = format!("{}/tasks/{}/status", self.node(), encode_segment(&upid));
        let deadline = Instant::now() + self.task_timeout;
        loop {
            let st = self.t.call(Method::Get, &status_path, &[])?;
            if st.get("status").and_then(Value::as_str) == Some("stopped") {
                let exit = st.get("exitstatus").and_then(Value::as_str).unwrap_or("");
                return if exit == "OK" { Ok(()) } else { Err(format!("task {upid} failed: {exit}")) };
            }
            if Instant::now() >= deadline {
                return Err(format!("task {upid} timed out after {}s", self.task_timeout.as_secs()));
            }
            std::thread::sleep(self.poll);
        }
    }
}

impl<T: Transport> Provider for Pve<T> {
    fn next_id(&self) -> Result<String, String> {
        let d = self.t.call(Method::Get, "/cluster/nextid", &[])?;
        as_id(&d).ok_or_else(|| format!("nextid returned {d}"))
    }

    fn clone_to(&self, vmid: &str, name: &str) -> Result<(), String> {
        let mut params = vec![("newid", vmid.to_string()), ("name", name.to_string()), ("full", "0".to_string())];
        if let Some(pool) = &self.env.pool {
            params.push(("pool", pool.clone()));
        }
        self.task(Method::Post, &format!("{}/clone", self.qemu(&self.env.template_vmid)), &params)?;
        let mac = slot_mac(vmid)?;
        let cfg_path = format!("{}/config", self.qemu(vmid));
        let cfg = self.t.call(Method::Get, &cfg_path, &[])?;
        let mut reserve = vec![("balloon", "0".to_string()), ("cpuunits", ROUND_CPUUNITS.to_string())];
        if let Some(net0) = cfg.get("net0").and_then(Value::as_str) {
            let pinned = with_mac(net0, &mac).ok_or_else(|| format!("net0 {net0:?} has no model=MAC field"))?;
            reserve.insert(0, ("net0", pinned));
        }
        self.task(Method::Post, &cfg_path, &reserve)
    }

    fn hold_for_ci(&self) {
        crate::ci_yield::wait_for_ci(
            &|| self.list_vms(),
            &|s| std::thread::sleep(std::time::Duration::from_secs(s)),
            30,
        );
    }

    fn start(&self, vmid: &str) -> Result<(), String> {
        self.task(Method::Post, &format!("{}/status/start", self.qemu(vmid)), &[])
    }

    fn guest_addr(&self, vmid: &str, iface: &str) -> Result<Option<String>, String> {
        let d = self.t.call(Method::Get, &format!("{}/agent/network-get-interfaces", self.qemu(vmid)), &[])?;
        Ok(ipv4_on(&d, iface))
    }

    fn guest_exec(&self, vmid: &str, argv: &[&str]) -> Result<i32, String> {
        let d = self.t.call(Method::Post, &format!("{}/agent/exec", self.qemu(vmid)), &exec_params(argv))?;
        let pid = d.get("pid").and_then(as_id).ok_or_else(|| format!("guest-exec returned no pid: {d}"))?;
        let deadline = Instant::now() + self.exec_timeout;
        loop {
            let st = self.t.call(Method::Get, &format!("{}/agent/exec-status", self.qemu(vmid)), &[("pid", pid.clone())])?;
            if truthy(st.get("exited")) {
                return Ok(st.get("exitcode").and_then(Value::as_i64).unwrap_or(0) as i32);
            }
            if Instant::now() >= deadline {
                return Err(format!("guest-exec `{}` timed out (pid {pid})", argv.join(" ")));
            }
            std::thread::sleep(self.poll);
        }
    }

    fn guest_file_write(&self, vmid: &str, path: &str, content: &str) -> Result<(), String> {
        self.t
            .call(Method::Post, &format!("{}/agent/file-write", self.qemu(vmid)), &file_write_params(path, content))
            .map(|_| ())
    }

    fn alive(&self, vmid: &str) -> Result<bool, String> {
        let d = self.t.call(Method::Get, &format!("{}/status/current", self.qemu(vmid)), &[])?;
        Ok(d.get("status").and_then(Value::as_str) == Some("running"))
    }

    fn stop(&self, vmid: &str) -> Result<(), String> {
        self.task(Method::Post, &format!("{}/status/stop", self.qemu(vmid)), &[])
    }

    fn destroy(&self, vmid: &str) -> Result<(), String> {
        self.task(
            Method::Delete,
            &self.qemu(vmid),
            &[("purge", "1".to_string()), ("destroy-unreferenced-disks", "1".to_string())],
        )
    }

    fn shutdown(&self, vmid: &str) -> Result<(), String> {
        self.task(Method::Post, &format!("{}/status/shutdown", self.qemu(vmid)), &[])
    }

    fn make_template(&self, vmid: &str) -> Result<(), String> {
        self.task(Method::Post, &format!("{}/template", self.qemu(vmid)), &[])
    }

    fn is_template(&self, vmid: &str) -> Result<bool, String> {
        let d = self.t.call(Method::Get, &format!("{}/config", self.qemu(vmid)), &[])?;
        Ok(truthy(d.get("template")))
    }

    fn name_of(&self, vmid: &str) -> Result<Option<String>, String> {
        let d = self.t.call(Method::Get, &format!("{}/qemu", self.node()), &[])?;
        let list = d.as_array().ok_or_else(|| format!("VM list is not a list: {d}"))?;
        Ok(list
            .iter()
            .find(|v| v.get("vmid").and_then(as_id).as_deref() == Some(vmid))
            .map(|v| v.get("name").and_then(Value::as_str).unwrap_or("").to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Mutex;

    type Request = (Method, String, Vec<(String, String)>);

    /// A scripted Proxmox: answers by method + path, records every request.
    #[derive(Default)]
    struct FakeApi {
        log: Mutex<Vec<Request>>,
        vms: Mutex<Vec<Value>>,
        exec_polls: Mutex<u32>,
    }

    impl Transport for FakeApi {
        fn call(&self, method: Method, path: &str, params: &[(&str, String)]) -> Result<Value, String> {
            self.log.lock().unwrap().push((method, path.to_string(), params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()));
            let upid = json!("UPID:pve:0001:task:");
            Ok(match (method, path) {
                (Method::Get, "/cluster/nextid") => json!("123"),
                (Method::Get, "/nodes/pve/qemu") => Value::Array(self.vms.lock().unwrap().clone()),
                (Method::Get, p) if p.starts_with("/nodes/pve/tasks/") => json!({"status": "stopped", "exitstatus": "OK"}),
                (Method::Get, p) if p.ends_with("/agent/exec-status") => {
                    let mut n = self.exec_polls.lock().unwrap();
                    *n += 1;
                    if *n < 2 { json!({"exited": 0}) } else { json!({"exited": 1, "exitcode": 3}) }
                }
                (Method::Post, p) if p.ends_with("/agent/exec") => json!({"pid": 42}),
                (Method::Post, p) if p.ends_with("/agent/file-write") => Value::Null,
                (Method::Get, p) if p.ends_with("/status/current") => json!({"status": "running"}),
                (Method::Get, "/nodes/pve/qemu/124/config") => json!({"net0": "virtio=BC:24:11:AA:BB:CC,bridge=vmbr0,firewall=1"}),
                (Method::Get, "/nodes/pve/qemu/123/config") => json!({"template": 1, "name": "t"}),
                (Method::Get, p) if p.ends_with("/config") => json!({"name": "vm"}),
                (Method::Get, p) if p.ends_with("/agent/network-get-interfaces") => json!({"result": [
                    {"name": "lo", "ip-addresses": [{"ip-address-type": "ipv4", "ip-address": "127.0.0.1"}]},
                    {"name": "ens18", "ip-addresses": [
                        {"ip-address-type": "ipv6", "ip-address": "fe80::1"},
                        {"ip-address-type": "ipv4", "ip-address": "192.168.1.149"}]}]}),
                _ => upid,
            })
        }
    }

    fn pve() -> Pve<FakeApi> {
        Pve {
            t: FakeApi::default(),
            env: PveEnv {
                api_host: "h".into(),
                api_port: 8006,
                node: "pve".into(),
                token_id: "id".into(),
                token_secret: "s".into(),
                cacert: PathBuf::from("/ca"),
                template_vmid: "9000".into(),
                pool: Some("ci".into()),
            },
            task_timeout: Duration::from_secs(5),
            exec_timeout: Duration::from_secs(5),
            poll: Duration::ZERO,
        }
    }

    fn last(p: &Pve<FakeApi>, needle: &str) -> Request {
        p.t.log.lock().unwrap().iter().rev().find(|(_, path, _)| path.contains(needle)).cloned().expect(needle)
    }

    #[test]
    fn file_write_sends_raw_content_and_no_encoding_parameter() {
        let p = pve();
        p.guest_file_write("123", "/root/.ssh/authorized_keys", "ssh-ed25519 AAAA k\n").unwrap();
        let (m, path, params) = last(&p, "file-write");
        assert_eq!(m, Method::Post);
        assert_eq!(path, "/nodes/pve/qemu/123/agent/file-write");
        assert_eq!(
            params,
            vec![("file".into(), "/root/.ssh/authorized_keys".into()), ("content".into(), "ssh-ed25519 AAAA k\n".into())]
        );
    }

    #[test]
    fn exec_sends_only_command_params_and_returns_the_exit_code() {
        let p = pve();
        let rc = p.guest_exec("123", &["mkdir", "-p", "-m", "700", "/root/.ssh"]).unwrap();
        assert_eq!(rc, 3);
        let (_, _, params) = p.t.log.lock().unwrap().iter().find(|(_, path, _)| path.ends_with("/agent/exec")).cloned().unwrap();
        assert!(params.iter().all(|(k, _)| k == "command"), "{params:?}");
        assert_eq!(params.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(), ["mkdir", "-p", "-m", "700", "/root/.ssh"]);
        let (_, _, q) = last(&p, "exec-status");
        assert_eq!(q, vec![("pid".into(), "42".into())]);
    }

    #[test]
    fn clone_names_the_template_and_pool_and_waits_on_the_task() {
        let p = pve();
        p.clone_to("123", "round-123").unwrap();
        let log = p.t.log.lock().unwrap().clone();
        assert_eq!(log[0].1, "/nodes/pve/qemu/9000/clone");
        assert!(log[0].2.contains(&("newid".into(), "123".into())));
        assert!(log[0].2.contains(&("name".into(), "round-123".into())));
        assert!(log[0].2.contains(&("pool".into(), "ci".into())));
        assert_eq!(log[1].1, "/nodes/pve/tasks/UPID%3Apve%3A0001%3Atask%3A/status");
    }

    #[test]
    fn a_slot_always_gets_the_same_mac_and_slots_differ() {
        assert_eq!(slot_mac("124").unwrap(), slot_mac("124").unwrap());
        assert_ne!(slot_mac("124").unwrap(), slot_mac("125").unwrap());
        assert_eq!(slot_mac("124").unwrap(), "02:52:56:00:00:7c");
        assert!(slot_mac("x").is_err());
    }

    #[test]
    fn clone_pins_the_slot_mac_on_net0_keeping_the_rest() {
        let p = pve();
        p.clone_to("124", "round-124").unwrap();
        let (m, path, params) = last(&p, "/qemu/124/config");
        assert_eq!((m, path.as_str()), (Method::Post, "/nodes/pve/qemu/124/config"));
        assert_eq!(
            params,
            vec![
                ("net0".into(), "virtio=02:52:56:00:00:7C,bridge=vmbr0,firewall=1".into()),
                ("balloon".into(), "0".into()),
                ("cpuunits".into(), "10000".into()),
            ]
        );
        let first = pve();
        first.clone_to("124", "round-124").unwrap();
        assert_eq!(last(&first, "/qemu/124/config").2, params);
    }

    #[test]
    fn with_mac_refuses_a_net0_without_a_model_field() {
        assert_eq!(with_mac("e1000=AA:BB,bridge=b", "02:00:00:00:00:01").as_deref(), Some("e1000=02:00:00:00:00:01,bridge=b"));
        assert_eq!(with_mac("bridge", "02:00:00:00:00:01"), None);
    }

    #[test]
    fn a_template_build_shuts_down_gracefully_and_converts() {
        let p = pve();
        p.shutdown("123").unwrap();
        assert_eq!(last(&p, "/status/shutdown").0, Method::Post);
        p.make_template("123").unwrap();
        let (m, path, _) = last(&p, "/template");
        assert_eq!((m, path.as_str()), (Method::Post, "/nodes/pve/qemu/123/template"));
        assert!(p.is_template("123").unwrap());
        assert!(!p.is_template("124").unwrap());
    }

    #[test]
    fn destroy_purges_through_delete() {
        let p = pve();
        p.destroy("123").unwrap();
        let (m, path, params) = last(&p, "/qemu/123");
        assert_eq!((m, path.as_str()), (Method::Delete, "/nodes/pve/qemu/123"));
        assert!(params.contains(&("purge".into(), "1".into())));
    }

    #[test]
    fn a_failed_task_is_an_error() {
        struct Failing;
        impl Transport for Failing {
            fn call(&self, _: Method, path: &str, _: &[(&str, String)]) -> Result<Value, String> {
                Ok(if path.contains("/tasks/") { json!({"status": "stopped", "exitstatus": "clone failed"}) } else { json!("UPID:x") })
            }
        }
        let p = Pve { t: Failing, env: pve().env, task_timeout: Duration::from_secs(1), exec_timeout: Duration::from_secs(1), poll: Duration::ZERO };
        let e = p.start("1").unwrap_err();
        assert!(e.contains("clone failed"), "{e}");
    }

    #[test]
    fn reads_nextid_address_liveness_and_names() {
        let p = pve();
        assert_eq!(p.next_id().unwrap(), "123");
        assert_eq!(p.guest_addr("123", "ens18").unwrap().as_deref(), Some("192.168.1.149"));
        assert_eq!(p.guest_addr("123", "eth9").unwrap(), None);
        assert!(p.alive("123").unwrap());
        p.t.vms.lock().unwrap().push(json!({"vmid": 123, "name": "round-123"}));
        assert_eq!(p.name_of("123").unwrap().as_deref(), Some("round-123"));
        assert_eq!(p.name_of("124").unwrap(), None);
    }

    #[test]
    fn transport_refuses_a_cacert_with_no_certificate() {
        let d = crate::testutil::TempDir::new();
        let ca = d.path().join("ca.pem");
        std::fs::write(&ca, "not a pem").unwrap();
        let mut env = pve().env;
        env.cacert = ca;
        assert!(HttpTransport::new(&env).err().unwrap().contains("no certificate"));
    }
}
