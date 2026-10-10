//! The EC2 provider behind the provider seam (DESIGN.md §3.3, §3.5). One tagged instance per
//! pass; every operation resolves the instance through its tag, so an instance without the
//! tag cannot be addressed at all (G4), and `destroy` re-checks the name before it terminates.
//! The instance joins the tailnet from user-data; SSM is the control path for key delivery and
//! diagnosis. The cloud is reached only through [`Ec2Api`].

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::provider::{vm_name, Provider};
use crate::schema::now;

pub const TAG_KEY: &str = "spira-round-vm";
pub const HANDLE_PREFIX: &str = "ec2-";

pub fn is_ec2_handle(handle: &str) -> bool {
    handle.starts_with(HANDLE_PREFIX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inst {
    pub id: String,
    pub handle: String,
    pub name: String,
    pub running: bool,
    pub launched_at: u64,
}

pub trait Ec2Api {
    /// Launches one instance tagged `TAG_KEY=handle`, `Name=name`; its instance id.
    fn run(&self, handle: &str, name: &str, user_data: &str) -> Result<String, String>;
    /// The live instance tagged with `handle`; None once it is terminated or never existed.
    fn find(&self, handle: &str) -> Result<Option<Inst>, String>;
    /// Every live instance carrying `TAG_KEY`; an untagged instance is never listed.
    fn list_tagged(&self) -> Result<Vec<Inst>, String>;
    fn terminate(&self, id: &str) -> Result<(), String>;
    /// Runs `script` on the instance through SSM: (exit code, output).
    fn ssm(&self, id: &str, script: &str) -> Result<(i32, String), String>;
    /// The tailnet IPv4 address of the online peer `hostname`.
    fn tailnet_addr(&self, hostname: &str) -> Result<Option<String>, String>;
}

#[derive(Debug, Clone)]
pub struct Ec2Config {
    pub profile: String,
    pub region: String,
    pub ami: String,
    pub instance_type: String,
    pub subnet: String,
    pub security_group: String,
    pub instance_profile: String,
    pub tailscale_key_file: String,
    pub run_deadline: Duration,
    pub host_addr: String,
}

impl Ec2Config {
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Ec2Config {
        let s = |k: &str| get(k).map(|v| v.trim().to_string()).unwrap_or_default();
        let deadline = s("SPIRA_ROUND_VM_EC2_RUN_DEADLINE").parse().unwrap_or(7200);
        Ec2Config {
            profile: s("SPIRA_ROUND_VM_EC2_PROFILE"),
            region: s("SPIRA_ROUND_VM_EC2_REGION"),
            ami: s("SPIRA_ROUND_VM_EC2_AMI"),
            instance_type: Some(s("SPIRA_ROUND_VM_EC2_INSTANCE_TYPE")).filter(|v| !v.is_empty()).unwrap_or_else(|| "c6i.8xlarge".into()),
            subnet: s("SPIRA_ROUND_VM_EC2_SUBNET"),
            security_group: s("SPIRA_ROUND_VM_EC2_SECURITY_GROUP"),
            instance_profile: s("SPIRA_ROUND_VM_EC2_INSTANCE_PROFILE"),
            tailscale_key_file: s("SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE"),
            run_deadline: Duration::from_secs(deadline),
            host_addr: s("SPIRA_ROUND_VM_EC2_HOST_ADDR"),
        }
    }

    /// What the provider cannot start without, by key name.
    pub fn missing(&self) -> Vec<String> {
        let mut m: Vec<String> = [
            ("SPIRA_ROUND_VM_EC2_PROFILE (the AWS profile)", &self.profile),
            ("SPIRA_ROUND_VM_EC2_REGION", &self.region),
            ("SPIRA_ROUND_VM_EC2_AMI (the pinned round AMI)", &self.ami),
            ("SPIRA_ROUND_VM_EC2_SUBNET", &self.subnet),
            ("SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE (the tailscale auth key file)", &self.tailscale_key_file),
            ("SPIRA_ROUND_VM_EC2_HOST_ADDR (this host's tailnet address)", &self.host_addr),
        ]
        .iter()
        .filter(|(_, v)| v.is_empty())
        .map(|(k, _)| k.to_string())
        .collect();
        if !self.tailscale_key_file.is_empty() {
            match std::fs::read_to_string(&self.tailscale_key_file) {
                Ok(k) if !k.trim().is_empty() => {}
                _ => m.push(format!("the tailscale auth key file {} (unreadable or empty)", self.tailscale_key_file)),
            }
        }
        m
    }
}

pub struct Ec2<A: Ec2Api> {
    pub api: A,
    tailscale_key: String,
    run_deadline: Duration,
}

impl<A: Ec2Api> Ec2<A> {
    /// Refuses, naming everything missing, rather than starting half-configured.
    pub fn new(api: A, cfg: &Ec2Config) -> Result<Ec2<A>, String> {
        let missing = cfg.missing();
        if !missing.is_empty() {
            return Err(format!("round-vm: the EC2 provider cannot start; missing: {}", missing.join("; ")));
        }
        let key = std::fs::read_to_string(&cfg.tailscale_key_file).map_err(|e| format!("{}: {e}", cfg.tailscale_key_file))?;
        Ok(Ec2 { api, tailscale_key: key.trim().to_string(), run_deadline: cfg.run_deadline })
    }

    fn inst(&self, handle: &str) -> Result<Inst, String> {
        self.api.find(handle)?.ok_or_else(|| format!("no EC2 instance tagged {TAG_KEY}={handle}"))
    }

    fn script(&self, handle: &str, script: &str) -> Result<(i32, String), String> {
        let i = self.inst(handle)?;
        self.api.ssm(&i.id, script)
    }
}

/// Instance user-data: join the tailnet as `hostname`, ephemeral and tagged. The key is never echoed.
pub fn user_data(auth_key: &str, hostname: &str) -> String {
    format!(
        "#!/bin/bash\nset +x\ntailscale up --auth-key={} --hostname={} --advertise-tags=tag:round-vm --accept-dns=false\n",
        sh_quote(auth_key),
        sh_quote(hostname)
    )
}

pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn counter() -> u32 {
    static N: AtomicU32 = AtomicU32::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

impl<A: Ec2Api> Provider for Ec2<A> {
    fn next_id(&self) -> Result<String, String> {
        Ok(format!("{HANDLE_PREFIX}{:x}-{:x}-{:x}", now(), std::process::id(), counter()))
    }

    fn clone_to(&self, vmid: &str, name: &str) -> Result<(), String> {
        if self.api.find(vmid)?.is_some() {
            return Err(format!("EC2 instance {vmid} already exists"));
        }
        self.api.run(vmid, name, &user_data(&self.tailscale_key, name)).map(|_| ())
    }

    fn start(&self, vmid: &str) -> Result<(), String> {
        self.inst(vmid).map(|_| ())
    }

    fn guest_addr(&self, vmid: &str, _iface: &str) -> Result<Option<String>, String> {
        let i = self.inst(vmid)?;
        if !i.running {
            return Ok(None);
        }
        self.api.tailnet_addr(&i.name)
    }

    fn guest_exec(&self, vmid: &str, argv: &[&str]) -> Result<i32, String> {
        let cmd: Vec<String> = argv.iter().map(|a| sh_quote(a)).collect();
        self.script(vmid, &cmd.join(" ")).map(|(rc, _)| rc)
    }

    fn guest_file_write(&self, vmid: &str, path: &str, content: &str) -> Result<(), String> {
        let (rc, out) = self.script(vmid, &format!("printf %s {} > {}", sh_quote(content), sh_quote(path)))?;
        if rc == 0 {
            Ok(())
        } else {
            Err(format!("writing {path} exited {rc}: {}", out.trim()))
        }
    }

    fn alive(&self, vmid: &str) -> Result<bool, String> {
        Ok(self.api.find(vmid)?.map(|i| i.running).unwrap_or(false))
    }

    fn stop(&self, _vmid: &str) -> Result<(), String> {
        Ok(())
    }

    fn destroy(&self, vmid: &str) -> Result<(), String> {
        let Some(i) = self.api.find(vmid)? else { return Ok(()) };
        if i.name != vm_name(vmid) {
            return Err(format!("refusing to terminate EC2 instance {}: it is named {:?}, not {:?}", i.id, i.name, vm_name(vmid)));
        }
        self.api.terminate(&i.id)
    }

    fn name_of(&self, vmid: &str) -> Result<Option<String>, String> {
        Ok(self.api.find(vmid)?.map(|i| i.name))
    }

    fn shutdown(&self, _vmid: &str) -> Result<(), String> {
        Err("EC2 instances are not templates".into())
    }

    fn make_template(&self, _vmid: &str) -> Result<(), String> {
        Err("EC2 instances are not templates".into())
    }

    fn is_template(&self, _vmid: &str) -> Result<bool, String> {
        Ok(false)
    }

    fn stale(&self) -> Vec<String> {
        let t = now();
        match self.api.list_tagged() {
            Ok(v) => v
                .into_iter()
                .filter(|i| i.launched_at + self.run_deadline.as_secs() < t)
                .map(|i| i.handle)
                .collect(),
            Err(e) => {
                eprintln!("round-vm: EC2 leak reaper cannot list tagged instances: {e}");
                Vec::new()
            }
        }
    }

    fn diagnose(&self, vmid: &str) -> String {
        match self.script(vmid, "tailscale status 2>&1 | head -20; journalctl -u tailscaled --no-pager -n 20 2>&1; tail -n 20 /var/log/cloud-init-output.log 2>&1") {
            Ok((rc, out)) => format!("SSM diagnosis (exit {rc}): {}", out.trim()),
            Err(e) => format!("SSM diagnosis failed: {e}"),
        }
    }
}

/// The real API: the `aws` CLI under a named profile, and `tailscale status --json`.
pub struct AwsCli {
    pub profile: String,
    pub region: String,
    pub ami: String,
    pub instance_type: String,
    pub subnet: String,
    pub security_group: String,
    pub instance_profile: String,
    pub scratch: std::path::PathBuf,
}

impl AwsCli {
    pub fn new(cfg: &Ec2Config, scratch: std::path::PathBuf) -> AwsCli {
        AwsCli {
            profile: cfg.profile.clone(),
            region: cfg.region.clone(),
            ami: cfg.ami.clone(),
            instance_type: cfg.instance_type.clone(),
            subnet: cfg.subnet.clone(),
            security_group: cfg.security_group.clone(),
            instance_profile: cfg.instance_profile.clone(),
            scratch,
        }
    }

    fn base(&self) -> Vec<String> {
        ["--profile", &self.profile, "--region", &self.region].iter().map(|s| s.to_string()).collect()
    }

    fn aws(&self, args: Vec<String>) -> Result<String, String> {
        let mut argv = self.base();
        argv.extend(args);
        let out = crate::procs::command("aws").args(&argv).output().map_err(|e| format!("aws: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!("aws {}: {}", argv.get(4).map(String::as_str).unwrap_or(""), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    pub fn run_args(&self, handle: &str, name: &str, user_data_file: &str) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "ec2".into(), "run-instances".into(),
            "--image-id".into(), self.ami.clone(),
            "--instance-type".into(), self.instance_type.clone(),
            "--subnet-id".into(), self.subnet.clone(),
            "--count".into(), "1".into(),
            "--instance-initiated-shutdown-behavior".into(), "terminate".into(),
            "--user-data".into(), format!("file://{user_data_file}"),
            "--tag-specifications".into(),
            format!("ResourceType=instance,Tags=[{{Key={TAG_KEY},Value={handle}}},{{Key=Name,Value={name}}}]"),
            "--query".into(), "Instances[0].InstanceId".into(),
            "--output".into(), "text".into(),
        ];
        if !self.security_group.is_empty() {
            a.extend(["--security-group-ids".into(), self.security_group.clone()]);
        }
        if !self.instance_profile.is_empty() {
            a.extend(["--iam-instance-profile".into(), format!("Name={}", self.instance_profile)]);
        }
        a
    }

    fn describe(&self, handle: Option<&str>) -> Result<Vec<Inst>, String> {
        let tag = match handle {
            Some(h) => format!("Name=tag:{TAG_KEY},Values={h}"),
            None => format!("Name=tag-key,Values={TAG_KEY}"),
        };
        let out = self.aws(vec![
            "ec2".into(), "describe-instances".into(),
            "--filters".into(), tag,
            "Name=instance-state-name,Values=pending,running,stopping,stopped,shutting-down".into(),
            "--output".into(), "json".into(),
        ])?;
        parse_instances(&out)
    }
}

impl Ec2Api for AwsCli {
    fn run(&self, handle: &str, name: &str, user_data: &str) -> Result<String, String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(&self.scratch).map_err(|e| format!("{}: {e}", self.scratch.display()))?;
        let file = self.scratch.join(format!("user-data.{handle}"));
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        f.write_all(user_data.as_bytes()).map_err(|e| e.to_string())?;
        let r = self.aws(self.run_args(handle, name, &file.to_string_lossy()));
        let _ = std::fs::remove_file(&file);
        r.map(|s| s.trim().to_string())
    }

    fn find(&self, handle: &str) -> Result<Option<Inst>, String> {
        Ok(self.describe(Some(handle))?.into_iter().find(|i| i.handle == handle))
    }

    fn list_tagged(&self) -> Result<Vec<Inst>, String> {
        self.describe(None)
    }

    fn terminate(&self, id: &str) -> Result<(), String> {
        self.aws(vec!["ec2".into(), "terminate-instances".into(), "--instance-ids".into(), id.into()]).map(|_| ())
    }

    fn ssm(&self, id: &str, script: &str) -> Result<(i32, String), String> {
        let params = serde_json::json!({"commands": [script]}).to_string();
        let cmd = self
            .aws(vec![
                "ssm".into(), "send-command".into(), "--instance-ids".into(), id.into(),
                "--document-name".into(), "AWS-RunShellScript".into(),
                "--parameters".into(), params,
                "--query".into(), "Command.CommandId".into(), "--output".into(), "text".into(),
            ])?
            .trim()
            .to_string();
        // batch-job: SSM diagnose command polled until its own deadline
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let out = self.aws(vec![
                "ssm".into(), "get-command-invocation".into(), "--command-id".into(), cmd.clone(),
                "--instance-id".into(), id.into(), "--output".into(), "json".into(),
            ]);
            if let Ok(out) = out {
                let v: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
                match v.get("Status").and_then(Value::as_str) {
                    Some("Pending" | "InProgress" | "Delayed") | None => {}
                    Some(_) => {
                        let rc = v.get("ResponseCode").and_then(Value::as_i64).unwrap_or(-1) as i32;
                        let text = v.get("StandardOutputContent").and_then(Value::as_str).unwrap_or("");
                        let err = v.get("StandardErrorContent").and_then(Value::as_str).unwrap_or("");
                        return Ok((rc, format!("{text}{err}")));
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!("SSM command {cmd} on {id} timed out"));
            }
        }
    }

    fn tailnet_addr(&self, hostname: &str) -> Result<Option<String>, String> {
        let out = crate::procs::command("tailscale").args(["status", "--json"]).output().map_err(|e| format!("tailscale: {e}"))?;
        if !out.status.success() {
            return Err(format!("tailscale status: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        peer_addr(&String::from_utf8_lossy(&out.stdout), hostname)
    }
}

/// The IPv4 of the online peer `hostname` in `tailscale status --json`.
pub fn peer_addr(json: &str, hostname: &str) -> Result<Option<String>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("tailscale status: {e}"))?;
    let Some(peers) = v.get("Peer").and_then(Value::as_object) else { return Ok(None) };
    Ok(peers
        .values()
        .filter(|p| p.get("HostName").and_then(Value::as_str) == Some(hostname) && p.get("Online").and_then(Value::as_bool) == Some(true))
        .find_map(|p| {
            p.get("TailscaleIPs")
                .and_then(Value::as_array)
                .and_then(|ips| ips.iter().filter_map(Value::as_str).find(|ip| !ip.contains(':')))
                .map(String::from)
        }))
}

pub fn parse_instances(json: &str) -> Result<Vec<Inst>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("describe-instances: {e}"))?;
    let mut out = Vec::new();
    for r in v.get("Reservations").and_then(Value::as_array).into_iter().flatten() {
        for i in r.get("Instances").and_then(Value::as_array).into_iter().flatten() {
            let tag = |k: &str| {
                i.get("Tags")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .find(|t| t.get("Key").and_then(Value::as_str) == Some(k))
                    .and_then(|t| t.get("Value").and_then(Value::as_str))
                    .map(String::from)
            };
            let Some(handle) = tag(TAG_KEY) else { continue };
            let state = i.pointer("/State/Name").and_then(Value::as_str).unwrap_or("");
            if state == "terminated" {
                continue;
            }
            out.push(Inst {
                id: i.get("InstanceId").and_then(Value::as_str).unwrap_or("").to_string(),
                handle,
                name: tag("Name").unwrap_or_default(),
                running: state == "running",
                launched_at: i.get("LaunchTime").and_then(Value::as_str).and_then(parse_iso).unwrap_or(0),
            });
        }
    }
    Ok(out)
}

/// Seconds since the epoch of `YYYY-MM-DDTHH:MM:SS[.fff](Z|+00:00)`.
pub fn parse_iso(s: &str) -> Option<u64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|p| p.get(..2).and_then(|x| x.parse::<i64>().ok()));
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + hh * 3600 + mm * 60 + ss).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_times_parse_to_epoch_seconds() {
        assert_eq!(parse_iso("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(parse_iso("2026-10-10T01:02:03.000Z"), Some(1791594123));
        assert_eq!(parse_iso("garbage"), None);
    }

    #[test]
    fn describe_output_keeps_tagged_live_instances_only() {
        let json = r#"{"Reservations":[{"Instances":[
          {"InstanceId":"i-1","State":{"Name":"running"},"LaunchTime":"2026-10-10T01:02:03+00:00","Tags":[{"Key":"spira-round-vm","Value":"ec2-a"},{"Key":"Name","Value":"round-ec2-a"}]},
          {"InstanceId":"i-2","State":{"Name":"running"},"Tags":[{"Key":"Name","Value":"someone-elses"}]},
          {"InstanceId":"i-3","State":{"Name":"terminated"},"Tags":[{"Key":"spira-round-vm","Value":"ec2-b"}]}]}]}"#;
        let v = parse_instances(json).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].id.as_str(), v[0].handle.as_str(), v[0].name.as_str(), v[0].running), ("i-1", "ec2-a", "round-ec2-a", true));
    }

    #[test]
    fn tailnet_peer_is_found_by_hostname_only_when_online() {
        let json = r#"{"Peer":{"k1":{"HostName":"round-ec2-a","Online":true,"TailscaleIPs":["100.64.0.9","fd7a::9"]},
                                "k2":{"HostName":"round-ec2-b","Online":false,"TailscaleIPs":["100.64.0.10"]}}}"#;
        assert_eq!(peer_addr(json, "round-ec2-a").unwrap().as_deref(), Some("100.64.0.9"));
        assert_eq!(peer_addr(json, "round-ec2-b").unwrap(), None);
        assert_eq!(peer_addr(json, "round-ec2-c").unwrap(), None);
    }

    #[test]
    fn run_args_tag_the_instance_and_terminate_on_shutdown() {
        let cli = AwsCli {
            profile: "p".into(), region: "r".into(), ami: "ami-1".into(), instance_type: "c6i.8xlarge".into(),
            subnet: "subnet-1".into(), security_group: "sg-1".into(), instance_profile: "ssm".into(), scratch: "/tmp".into(),
        };
        let a = cli.run_args("ec2-a", "round-ec2-a", "/tmp/ud");
        let j = a.join(" ");
        assert!(j.contains("Tags=[{Key=spira-round-vm,Value=ec2-a},{Key=Name,Value=round-ec2-a}]"), "{j}");
        assert!(j.contains("--instance-initiated-shutdown-behavior terminate"), "{j}");
        assert!(j.contains("--security-group-ids sg-1") && j.contains("--iam-instance-profile Name=ssm"), "{j}");
        assert!(!j.contains("tskey"), "the auth key stays in the user-data file, never in argv");
    }

    #[test]
    fn user_data_quotes_the_key_and_joins_as_the_instance_name() {
        let u = user_data("tskey-a'b", "round-ec2-a");
        assert!(u.contains("--auth-key='tskey-a'\\''b'") && u.contains("--hostname='round-ec2-a'") && u.contains("tag:round-vm"), "{u}");
    }

    use crate::provider::{destroy_verified, provision, DestroyError, ProvisionSpec, Timing};
    use crate::testutil::{FakeEc2, TempDir};

    const T: Timing = Timing { boot_tries: 2, poll: Duration::ZERO, gone_tries: 2 };

    fn config(d: &TempDir) -> Ec2Config {
        let key = d.path().join("ts.key");
        std::fs::write(&key, "tskey-auth-abc\n").unwrap();
        Ec2Config {
            profile: "p".into(), region: "r".into(), ami: "ami-1".into(), instance_type: "c6i.8xlarge".into(), subnet: "subnet-1".into(),
            security_group: String::new(), instance_profile: String::new(), tailscale_key_file: key.to_string_lossy().into(),
            run_deadline: Duration::from_secs(7200), host_addr: "100.64.0.1".into(),
        }
    }

    fn spec() -> ProvisionSpec<'static> {
        ProvisionSpec { iface: "ens18", ssh_user: "root", pubkey: "ssh-ed25519 AAAA host", timing: T }
    }

    #[test]
    fn provision_run_and_verified_destroy() {
        let d = TempDir::new();
        let fake = FakeEc2::default();
        let p = Ec2::new(fake.clone(), &config(&d)).unwrap();
        let vm = provision(&p, &spec(), &mut |_| {}).unwrap();
        assert!(is_ec2_handle(&vm.handle) && vm.addr.starts_with("100.64.0."), "{vm:?}");
        assert_eq!(p.name_of(&vm.handle).unwrap(), Some(vm_name(&vm.handle)));
        let scripts = fake.scripts().join("\n");
        assert!(scripts.contains("/root/.ssh/authorized_keys") && scripts.contains("ssh-ed25519 AAAA host"), "{scripts}");
        assert!(fake.user_data()[0].contains("--auth-key='tskey-auth-abc'"), "the tailscale key reaches the instance in user-data");
        destroy_verified(&p, &vm.handle, T).unwrap();
        assert!(fake.ids().is_empty(), "terminated: {:?}", fake.ids());
        destroy_verified(&p, &vm.handle, T).unwrap();
    }

    #[test]
    fn an_instance_that_never_reaches_the_tailnet_is_diagnosed_then_destroyed() {
        let d = TempDir::new();
        let fake = FakeEc2::default();
        fake.never_joins_tailnet();
        let p = Ec2::new(fake.clone(), &config(&d)).unwrap();
        let e = provision(&p, &spec(), &mut |_| {}).unwrap_err();
        assert!(e.reason.contains("did not come up") && e.reason.contains("SSM diagnosis") && e.reason.contains("diagnostic output"), "{}", e.reason);
        assert!(fake.scripts().iter().any(|s| s.contains("tailscale status")));
        assert!(fake.ids().is_empty(), "leaked {:?}", fake.ids());
        assert!(e.doomed.is_none());
    }

    #[test]
    fn a_failed_launch_leaves_nothing_behind() {
        let d = TempDir::new();
        let fake = FakeEc2::default();
        fake.fail_run();
        let p = Ec2::new(fake.clone(), &config(&d)).unwrap();
        let e = provision(&p, &spec(), &mut |_| {}).unwrap_err();
        assert!(e.reason.contains("clone refused"), "{}", e.reason);
        assert!(fake.ids().is_empty());
    }

    #[test]
    fn destroy_of_a_wrongly_named_instance_is_refused() {
        let d = TempDir::new();
        let fake = FakeEc2::default();
        fake.plant("i-9", Some("ec2-x"), "somebody-elses", 0);
        let p = Ec2::new(fake.clone(), &config(&d)).unwrap();
        let e = destroy_verified(&p, "ec2-x", T).unwrap_err();
        assert!(matches!(e, DestroyError::NotOurs(_)), "{e:?}");
        assert!(p.destroy("ec2-x").unwrap_err().contains("refusing"));
        assert_eq!(fake.ids(), ["i-9"]);
    }

    #[test]
    fn an_untagged_instance_cannot_be_addressed_by_any_handle() {
        let d = TempDir::new();
        let fake = FakeEc2::default();
        fake.plant("i-7", None, "round-ec2-y", 0);
        let p = Ec2::new(fake.clone(), &config(&d)).unwrap();
        assert_eq!(p.name_of("ec2-y").unwrap(), None);
        destroy_verified(&p, "ec2-y", T).unwrap();
        assert_eq!(fake.ids(), ["i-7"]);
    }

    #[test]
    fn the_reaper_names_stale_tagged_instances_and_never_an_untagged_one() {
        let d = TempDir::new();
        let fake = FakeEc2::default();
        fake.plant("i-old", Some("ec2-old"), "round-ec2-old", 1);
        fake.plant("i-untagged", None, "round-ec2-untagged", 1);
        fake.plant("i-new", Some("ec2-new"), "round-ec2-new", crate::schema::now());
        let p = Ec2::new(fake.clone(), &config(&d)).unwrap();
        assert_eq!(p.stale(), ["ec2-old"]);
    }

    #[test]
    fn the_provider_refuses_to_start_naming_everything_missing() {
        let d = TempDir::new();
        let mut c = config(&d);
        c.profile.clear();
        c.tailscale_key_file = d.path().join("absent").to_string_lossy().into();
        let e = Ec2::new(FakeEc2::default(), &c).err().expect("refused");
        assert!(e.contains("SPIRA_ROUND_VM_EC2_PROFILE") && e.contains("tailscale auth key file") && e.contains("absent"), "{e}");
        let none = Ec2Config::from_lookup(&|_| None);
        assert!(none.missing().len() >= 5);
    }
}
