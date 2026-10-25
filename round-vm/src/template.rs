//! `round-vm template` (DESIGN.md §2.2b, sp-dvfea): a new round template whose test image
//! is BUILT on the template, so the template keeps podman's layer cache and a later
//! closure change rebuilds only the layers it touches. A finished build is recorded
//! (`template.json`) and `pve.env` repointed at it by both verbs, so no person types a VMID.
//! Placement is decided by the `round-vm` that runs the pass, so a stale record is detected there.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::procs::command;
use crate::provider::{boot, destroy_fenced, DestroyError, Provider, ProvisionSpec};
use crate::run::{shell_quote, SshRemote};

pub const TEMPLATE_USAGE: &str =
    "round-vm template: usage: round-vm template|refresh <tree-dir> [--ref <rev>] [--toolchain <ver>] | template --adopt <vmid> <tree-dir>";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TemplateArgs {
    pub tree_dir: PathBuf,
    /// The revision of `tree_dir` to build from; HEAD when absent.
    pub rev: Option<String>,
    /// sp-xjnzl: the toolchain `rustup` must have installed on the template so a round can
    /// pin to it (`run --toolchain`) and match the host's own rustc byte-for-byte — sccache
    /// hashes the compiler's version string, so an unpinned "stable" drifting out of step
    /// with the host is a silent, permanent cache miss, not an error.
    pub toolchain: Option<String>,
    /// `--adopt <vmid>`: record and repoint this existing template instead of building one.
    pub adopt: Option<String>,
}

pub fn parse_template_args(args: &[String]) -> Result<TemplateArgs, String> {
    let mut it = args.iter();
    let tree = it.next().filter(|a| !a.starts_with("--")).ok_or(TEMPLATE_USAGE)?;
    let mut r = TemplateArgs { tree_dir: PathBuf::from(tree), rev: None, toolchain: None, adopt: None };
    while let Some(a) = it.next() {
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        match key {
            // THE HYPERVISOR ASSIGNS EVERY VM ID (/cluster/nextid), never the caller: a chosen
            // id collided with an existing VM and was refused, and the next was a guess
            // (per Ryan 2026-10-04: "stop choosing a VM id and let the hypervisor do it").
            "--vmid" => {
                return Err("round-vm template: --vmid is not accepted — the hypervisor assigns every VM id (/cluster/nextid)".into());
            }
            "--ref" => {
                let v = inline.or_else(|| it.next().cloned()).ok_or("round-vm template: --ref needs a value")?;
                r.rev = Some(v).filter(|v| !v.is_empty());
            }
            "--adopt" => {
                let v = inline.or_else(|| it.next().cloned()).ok_or("round-vm template: --adopt needs a VM id")?;
                r.adopt = Some(v).filter(|v| !v.is_empty());
            }
            "--toolchain" => {
                let v = inline.or_else(|| it.next().cloned()).ok_or("round-vm template: --toolchain needs a value")?;
                r.toolchain = Some(v).filter(|v| !v.is_empty());
            }
            other => return Err(format!("round-vm template: unknown option: {other}")),
        }
    }
    Ok(r)
}

/// What the current template holds, written only by a finished build: `<state>/template.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub vmid: String,
    pub image: String,
    /// The recipe the template was built from; absent on a record older than the field.
    #[serde(default)]
    pub recipe_sha: Option<String>,
}

impl Record {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("template.json")
    }

    pub fn read(state_dir: &Path) -> Option<Record> {
        serde_json::from_str(&std::fs::read_to_string(Self::path(state_dir)).ok()?).ok()
    }

    pub fn write(&self, state_dir: &Path) -> Result<(), String> {
        let path = Self::path(state_dir);
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// True when the template was built from the recipe `sha` names.
    pub fn built_from(&self, sha: &str) -> bool {
        self.recipe_sha.as_deref() == Some(sha)
    }

    /// True when the image this template holds is the one named by `tag`.
    pub fn holds(&self, tag: &str) -> bool {
        self.image.rsplit(':').next() == Some(tag)
    }
}

/// Why the recorded template is not the current recipe's, or None when it is (or none is recorded).
pub fn staleness(rec: Option<&Record>, current: &str) -> Option<String> {
    let r = rec?;
    match r.recipe_sha.as_deref() {
        Some(built) if built == current => None,
        Some(built) => Some(format!("local round template {} is stale: built from recipe {built}, current recipe is {current}; run `round-vm refresh <tree-dir>`", r.vmid)),
        None => Some(format!("local round template {} records no recipe hash, current recipe is {current}; run `round-vm refresh <tree-dir>`", r.vmid)),
    }
}

/// `text` with `key=value` set: the first assignment of `key` replaced in place, else appended.
pub fn set_env_key(text: &str, key: &str, value: &str) -> String {
    let mut done = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            let bare = l.trim().strip_prefix("export ").map(str::trim_start).unwrap_or(l.trim());
            if !done && bare.split_once('=').map(|(k, _)| k.trim()) == Some(key) {
                done = true;
                format!("{key}={value}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !done {
        out.push(format!("{key}={value}"));
    }
    out.join("\n") + "\n"
}

/// Points `pve.env`'s PVE_TEMPLATE_VMID at `vmid`, atomically and keeping the file's mode.
pub fn repoint(pve_env: &Path, vmid: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let text = std::fs::read_to_string(pve_env).map_err(|e| format!("{}: {e}", pve_env.display()))?;
    let mode = std::fs::metadata(pve_env).map_err(|e| e.to_string())?.permissions().mode();
    let tmp = pve_env.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, set_env_key(&text, "PVE_TEMPLATE_VMID", vmid)).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, pve_env).map_err(|e| format!("{}: {e}", pve_env.display()))
}

/// Puts an existing template in force without building: it must be a template on the
/// hypervisor; the record and `pve.env` change together or the record is left as it was.
pub fn adopt(p: &dyn Provider, state_dir: &Path, pve_env: &Path, vmid: &str, image: &str, recipe_sha: Option<String>) -> Result<Record, String> {
    if !p.is_template(vmid)? {
        return Err(format!("VM {vmid} is not a template on the hypervisor — refusing to adopt it"));
    }
    let old = Record::read(state_dir);
    let rec = Record { vmid: vmid.to_string(), image: image.to_string(), recipe_sha };
    rec.write(state_dir)?;
    if let Err(e) = repoint(pve_env, vmid) {
        let _ = match &old {
            Some(o) => o.write(state_dir),
            None => std::fs::remove_file(Record::path(state_dir)).map_err(|e| e.to_string()),
        };
        return Err(e);
    }
    Ok(rec)
}

/// The name a template build gives its clone. Never `round-<vmid>`, so no acquire, release or
/// reap can destroy the finished template (G4).
pub fn template_name(commit: &str) -> String {
    format!("round-template-{}", &commit[..commit.len().min(12)])
}

/// Where the tree is unpacked on the template VM; removed before shutdown.
pub const TEMPLATE_WORK: &str = "/root/template-work";

/// Runs on the template VM, in `$1` (the unpacked tree), `$2` (this box's own LAN address,
/// empty to skip the cache), `$3` (the toolchain to install, empty to leave whatever
/// `cargo`/`rustc` already resolve to on the template's own PATH) and `$4` (this binary's
/// own resolved `CARGO_HOME` — `cli.rs`'s `template()` refuses before this script is ever
/// sent if it is unset, sp-xjnzl-2: never a literal in this source, which would tie it to
/// one operator's box, and never empty here in practice). The image is BUILT here
/// — podman's default `--layers` keeps every step's cache in the store the template
/// carries. Every other spira-testenv tag goes (a loaded image has no cache and only costs
/// disk). The cargo registry is warmed from the tree's lockfile. The key round-vm delivered
/// is removed: every clone gets its own through the guest agent. The last stdout line is
/// `image=<ref>`.
///
/// sp-xjnzl: the template also gets the box's shared compilation cache wired up — sccache
/// built with the `webdav` backend, installed at the exact `CARGO_HOME` the host itself
/// uses (see REMOTE_SCRIPT's own comment: that path is part of every dependency's cache
/// key). Installing it HERE, once, means every round VM cloned from this template inherits
/// a working `sccache` on its disk without reinstalling it per round; REMOTE_SCRIPT only
/// has to point it at the store and set `CARGO_HOME` to match.
///
/// The toolchain comes from the distro's `rustup` package, never a downloaded tarball or
/// installer script: every dependency is installed by a package manager.
pub const TEMPLATE_SCRIPT: &str = include_str!("template.sh");

/// The AMI guest setup, the one script both providers run; it feeds TEMPLATE_SCRIPT itself.
pub const AMI_SETUP: &str = include_str!("../ami-setup.sh");

/// The command the VM runs from the unpacked tree: the AMI guest setup, with this provider's
/// only differences (the cache host and the recipe to record) passed in the environment.
pub fn setup_command(dir: &str, host_addr: &str, toolchain: &str, cache_home: &str, recipe_sha: &str) -> String {
    format!(
        "SPIRA_TEMPLATE_HOST_ADDR={} SPIRA_RECIPE_SHA={} bash {} {} {} {}",
        shell_quote(host_addr),
        shell_quote(recipe_sha),
        shell_quote(&format!("{dir}/round-vm/ami-setup.sh")),
        shell_quote(dir),
        shell_quote(toolchain),
        shell_quote(cache_home)
    )
}

/// The image ref TEMPLATE_SCRIPT reported, if it did.
pub fn reported_image(stdout: &str) -> Option<String> {
    stdout.lines().rev().find_map(|l| l.strip_prefix("image=")).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The template VM's side, reached only by address.
pub trait Guest {
    fn reachable(&self, addr: &str) -> bool;
    /// Whether `cargo` resolves on the VM's own PATH, for a build that names no toolchain.
    fn has_cargo(&self, addr: &str, cache_home: &str) -> bool;
    /// Unpacks `commit` of the repository at `tree` into `dir` on the VM.
    fn upload(&self, addr: &str, tree: &Path, commit: &str, dir: &str) -> Result<(), String>;
    /// Runs TEMPLATE_SCRIPT in `dir` on the VM at `addr`, pointing its cache at
    /// `host_addr` (this box's own LAN address; empty skips the cache), installing
    /// `toolchain` (empty: leave rustup's default alone) at `cache_home` (empty: the
    /// template's own ambient `CARGO_HOME`): (exit code, stdout). 255 is ssh itself.
    fn prepare(&self, addr: &str, dir: &str, host_addr: &str, toolchain: &str, cache_home: &str, recipe_sha: &str) -> Result<(i32, String), String>;
}

impl Guest for SshRemote {
    fn reachable(&self, addr: &str) -> bool {
        crate::run::Remote::reachable(self, addr)
    }

    fn has_cargo(&self, addr: &str, cache_home: &str) -> bool {
        command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(format!(
                "[ -n {h} ] && PATH={h}/bin:$PATH; command -v cargo >/dev/null 2>&1",
                h = shell_quote(cache_home)
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false)
    }

    fn upload(&self, addr: &str, tree: &Path, commit: &str, dir: &str) -> Result<(), String> {
        let mut archive = command("git")
            .args(["-C", &tree.to_string_lossy(), "archive", "--format=tar", commit])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("git archive: {e}"))?;
        let out = archive.stdout.take().ok_or("git archive: no stdout")?;
        let q = shell_quote(dir);
        let st = command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(format!("rm -rf {q} && mkdir -p {q} && tar -x -C {q}"))
            .stdin(out)
            .status()
            .map_err(|e| format!("ssh: {e}"))?;
        let ast = archive.wait().map_err(|e| format!("git archive: {e}"))?;
        if !ast.success() {
            return Err(format!("git archive {commit} exited {}", ast.code().unwrap_or(-1)));
        }
        if !st.success() {
            return Err(format!("unpacking the tree on {addr} exited {}", st.code().unwrap_or(-1)));
        }
        Ok(())
    }

    fn prepare(&self, addr: &str, dir: &str, host_addr: &str, toolchain: &str, cache_home: &str, recipe_sha: &str) -> Result<(i32, String), String> {
        let child = command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(setup_command(dir, host_addr, toolchain, cache_home, recipe_sha))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ssh: {e}"))?;
        let out = child.wait_with_output().map_err(|e| format!("ssh: {e}"))?;
        Ok((out.status.code().unwrap_or(255), String::from_utf8_lossy(&out.stdout).into_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub vmid: String,
    pub image: String,
}

/// Clone (linked) → boot → key → unpack → build the image → shut down → convert → verify.
/// Any failure after the clone destroys the VM, fenced on the name this run gave it.
pub fn build(
    p: &dyn Provider,
    g: &dyn Guest,
    spec: &ProvisionSpec,
    ssh_tries: u32,
    tree: &Path,
    commit: &str,
    vmid: Option<String>,
    host_addr: &str,
    toolchain: &str,
    cache_home: &str,
    recipe_sha: &str,
) -> Result<Built, String> {
    let vmid = match vmid {
        Some(v) => v,
        None => p.next_id().map_err(|e| format!("API unreachable (nextid): {e}"))?,
    };
    let name = template_name(commit);
    let t = spec.timing;
    eprintln!("round-vm template: linked clone of the round template to {vmid} ({name})");
    let result = p
        .clone_to(&vmid, &name)
        .map_err(|e| format!("clone refused: {e}"))
        .and_then(|()| prepare_vm(p, g, spec, ssh_tries, tree, commit, &vmid, host_addr, toolchain, cache_home, recipe_sha));
    match result {
        Ok(image) => Ok(Built { vmid, image }),
        Err(reason) => Err(match destroy_fenced(p, &vmid, &name, t) {
            Ok(()) => format!("{reason}; VM {vmid} destroyed"),
            Err(DestroyError::NotOurs(e)) => format!("{reason}; {e}"),
            Err(DestroyError::Failed(e)) => format!("{reason}; and {e} — destroy VM {vmid} ({name}) by hand"),
        }),
    }
}

fn prepare_vm(
    p: &dyn Provider,
    g: &dyn Guest,
    spec: &ProvisionSpec,
    ssh_tries: u32,
    tree: &Path,
    commit: &str,
    vmid: &str,
    host_addr: &str,
    toolchain: &str,
    cache_home: &str,
    recipe_sha: &str,
) -> Result<String, String> {
    let t = spec.timing;
    let addr = boot(p, vmid, spec)?;
    let reachable = (0..ssh_tries.max(1)).any(|i| {
        if i > 0 {
            std::thread::sleep(t.poll);
        }
        g.reachable(&addr)
    });
    if !reachable {
        return Err(format!("VM {vmid} at {addr} never became reachable over ssh"));
    }
    if toolchain.is_empty() && !g.has_cargo(&addr, cache_home) {
        return Err(format!(
            "the base has no cargo and neither --toolchain nor the tree's rust-toolchain.toml names a toolchain to install; \
             pass --toolchain <ver> or pin a channel in {}/rust-toolchain.toml",
            tree.display()
        ));
    }
    eprintln!("round-vm template: {vmid} at {addr}: unpacking {commit}");
    g.upload(&addr, tree, commit, TEMPLATE_WORK)?;
    eprintln!("round-vm template: building the test image on {vmid} (a cold build: its heartbeat follows)");
    let (rc, out) = g.prepare(&addr, TEMPLATE_WORK, host_addr, toolchain, cache_home, recipe_sha)?;
    if rc != 0 {
        return Err(format!("preparing the template on {vmid} exited {rc}"));
    }
    let image = reported_image(&out).ok_or_else(|| format!("preparing the template on {vmid} reported no image"))?;
    eprintln!("round-vm template: {image} built; shutting {vmid} down");
    p.shutdown(vmid).map_err(|e| format!("shutdown of {vmid} failed: {e}"))?;
    let stopped = (0..t.boot_tries.max(1)).any(|i| {
        if i > 0 {
            std::thread::sleep(t.poll);
        }
        matches!(p.alive(vmid), Ok(false))
    });
    if !stopped {
        return Err(format!("VM {vmid} still running after shutdown"));
    }
    p.make_template(vmid).map_err(|e| format!("converting {vmid} to a template failed: {e}"))?;
    if !p.is_template(vmid).map_err(|e| format!("reading {vmid}'s config: {e}"))? {
        return Err(format!("VM {vmid} is not a template after conversion"));
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Timing;
    use crate::testutil::{FakeProvider, Step};
    use std::sync::Mutex;
    use std::time::Duration;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn spec() -> ProvisionSpec<'static> {
        ProvisionSpec {
            iface: "ens18",
            ssh_user: "root",
            pubkey: "ssh-ed25519 AAAA host",
            timing: Timing { boot_tries: 3, poll: Duration::ZERO, gone_tries: 2 },
        }
    }

    struct FakeGuest {
        rc: i32,
        out: String,
        reachable: bool,
        cargo: bool,
        calls: Mutex<Vec<String>>,
    }

    impl FakeGuest {
        fn ok() -> FakeGuest {
            FakeGuest { rc: 0, out: "noise\nimage=localhost/spira-testenv:abc123\n".into(), reachable: true, cargo: true, calls: Mutex::new(vec![]) }
        }
    }

    impl Guest for FakeGuest {
        fn reachable(&self, _: &str) -> bool {
            self.reachable
        }
        fn has_cargo(&self, _: &str, _: &str) -> bool {
            self.cargo
        }
        fn upload(&self, addr: &str, _: &Path, commit: &str, dir: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("upload {addr} {commit} {dir}"));
            Ok(())
        }
        fn prepare(&self, addr: &str, dir: &str, host_addr: &str, toolchain: &str, cache_home: &str, recipe_sha: &str) -> Result<(i32, String), String> {
            self.calls.lock().unwrap().push(format!("prepare {addr} {dir} {host_addr} {toolchain} {cache_home} {recipe_sha}"));
            Ok((self.rc, self.out.clone()))
        }
    }

    const SHA: &str = "0123456789abcdef0123";

    #[test]
    fn args_take_a_tree_and_a_toolchain_and_refuse_a_chosen_vmid() {
        assert_eq!(
            parse_template_args(&s(&["/t"])).unwrap(),
            TemplateArgs { tree_dir: "/t".into(), rev: None, toolchain: None, adopt: None }
        );
        assert!(parse_template_args(&s(&["/t", "--vmid", "9120"])).unwrap_err().contains("hypervisor assigns"));
        assert!(parse_template_args(&s(&["/t", "--vmid=9121"])).unwrap_err().contains("hypervisor assigns"));
        assert_eq!(
            parse_template_args(&s(&["/t", "--toolchain", "1.98.1"])).unwrap().toolchain.as_deref(),
            Some("1.98.1")
        );
        assert_eq!(
            parse_template_args(&s(&["/t", "--toolchain=fixture-chan"])).unwrap().toolchain.as_deref(),
            Some("fixture-chan")
        );
        assert!(parse_template_args(&s(&[])).is_err());
        assert!(parse_template_args(&s(&["/t", "--bogus"])).unwrap_err().contains("--bogus"));
        assert!(parse_template_args(&s(&["/t", "--toolchain"])).unwrap_err().contains("needs a value"));
    }

    #[test]
    fn the_template_is_never_named_like_a_round_vm() {
        let n = template_name(SHA);
        assert_eq!(n, "round-template-0123456789ab");
        assert_ne!(n, crate::provider::vm_name("9120"));
    }

    #[test]
    fn a_build_clones_builds_the_image_then_converts_a_stopped_vm() {
        let p = FakeProvider::new();
        let g = FakeGuest::ok();
        let b = build(&p, &g, &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "192.168.1.56", "1.98.1", "/opt/spira/cargo", "feedface").unwrap();
        assert_eq!(b, Built { vmid: "9120".into(), image: "localhost/spira-testenv:abc123".into() });
        assert!(p.is_template("9120").unwrap());
        assert_eq!(p.name("9120").as_deref(), Some("round-template-0123456789ab"));
        let calls = p.calls();
        assert!(calls.contains(&"clone 9120 round-template-0123456789ab".to_string()), "{calls:?}");
        assert!(p.file("9120", "/root/.ssh/authorized_keys").is_some(), "the key went in through the agent");
        let order: Vec<&String> = calls.iter().filter(|c| c.starts_with("shutdown") || c.starts_with("template")).collect();
        assert_eq!(order, ["shutdown 9120", "template 9120"]);
        assert_eq!(
            *g.calls.lock().unwrap(),
            [
                format!("upload 10.0.0.9120 {SHA} {TEMPLATE_WORK}"),
                // sp-xjnzl: the host's own address, the pinned toolchain and the operator's
                // own CARGO_HOME all reach the VM side of `prepare`, not just the tree and
                // the work dir.
                format!("prepare 10.0.0.9120 {TEMPLATE_WORK} 192.168.1.56 1.98.1 /opt/spira/cargo feedface")
            ]
        );
    }

    #[test]
    fn no_toolchain_and_no_cargo_on_the_base_refuses_before_the_tree_is_unpacked() {
        let p = FakeProvider::new();
        let g = FakeGuest { cargo: false, ..FakeGuest::ok() };
        let e = build(&p, &g, &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "", "feedface").unwrap_err();
        assert!(e.contains("no cargo") && e.contains("toolchain"), "{e}");
        assert!(g.calls.lock().unwrap().is_empty(), "nothing was unpacked or built: {:?}", g.calls.lock().unwrap());
        assert!(p.all_vms().is_empty());
        let g = FakeGuest { cargo: false, ..FakeGuest::ok() };
        build(&FakeProvider::new(), &g, &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "1.99.1", "", "feedface").unwrap();
        assert_eq!(g.calls.lock().unwrap().len(), 2, "a pinned toolchain installs, so a base without cargo proceeds");
    }

    #[test]
    fn without_a_vmid_the_hypervisor_allocates_one() {
        let p = FakeProvider::new();
        let b = build(&p, &FakeGuest::ok(), &spec(), 3, Path::new("/tree"), SHA, None, "", "", "", "feedface").unwrap();
        assert_eq!(b.vmid, "100");
        assert_eq!(p.count("nextid"), 1);
    }

    #[test]
    fn a_failed_image_build_destroys_the_half_built_vm_and_converts_nothing() {
        let p = FakeProvider::new();
        let g = FakeGuest { rc: 1, ..FakeGuest::ok() };
        let e = build(&p, &g, &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "", "feedface").unwrap_err();
        assert!(e.contains("exited 1") && e.contains("destroyed"), "{e}");
        assert!(p.all_vms().is_empty(), "{:?}", p.all_vms());
        assert_eq!(p.count("template "), 0);
    }

    #[test]
    fn no_reported_image_is_a_failure_even_on_exit_0() {
        let p = FakeProvider::new();
        let g = FakeGuest { out: "built something\n".into(), ..FakeGuest::ok() };
        let e = build(&p, &g, &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "", "feedface").unwrap_err();
        assert!(e.contains("reported no image"), "{e}");
        assert!(p.all_vms().is_empty());
    }

    #[test]
    fn every_failure_after_the_clone_leaves_no_vm() {
        for step in [Step::Start, Step::Addr, Step::Exec, Step::Shutdown, Step::MakeTemplate] {
            let p = FakeProvider::new();
            p.fail(step);
            let e = build(&p, &FakeGuest::ok(), &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "", "feedface").unwrap_err();
            assert!(p.all_vms().is_empty(), "{step:?} left {:?}: {e}", p.all_vms());
        }
        let p = FakeProvider::new();
        let g = FakeGuest { reachable: false, ..FakeGuest::ok() };
        let e = build(&p, &g, &spec(), 2, Path::new("/tree"), SHA, Some("9120".into()), "", "", "", "feedface").unwrap_err();
        assert!(e.contains("never became reachable"), "{e}");
        assert!(p.all_vms().is_empty());
    }

    #[test]
    fn a_vmid_that_is_taken_is_refused_and_the_vm_there_is_never_touched() {
        let p = FakeProvider::new();
        p.plant("108", "round-template-old");
        let e = build(&p, &FakeGuest::ok(), &spec(), 3, Path::new("/tree"), SHA, Some("108".into()), "", "", "", "feedface").unwrap_err();
        assert!(e.contains("clone refused") && e.contains("refusing to destroy"), "{e}");
        assert_eq!(p.name("108").as_deref(), Some("round-template-old"));
        assert_eq!(p.count("destroy"), 0);
    }

    #[test]
    fn a_vm_that_will_not_die_is_named_for_the_operator() {
        let p = FakeProvider::new();
        p.fail(Step::Shutdown);
        p.fail(Step::Destroy);
        let e = build(&p, &FakeGuest::ok(), &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "", "feedface").unwrap_err();
        assert!(e.contains("by hand") && e.contains("9120"), "{e}");
    }

    #[test]
    fn the_script_survives_an_environment_with_no_home() {
        let dir = crate::testutil::TempDir::new();
        let script = dir.path().join("t.sh");
        let head: String = TEMPLATE_SCRIPT.lines().take_while(|l| !l.starts_with("work=")).collect::<Vec<_>>().join("\n");
        std::fs::write(&script, format!("{head}\n[ -n \"$HOME\" ] && [ -n \"$USER\" ] && echo ok:$HOME\n")).unwrap();
        let out = std::process::Command::new("/bin/bash").arg(&script).env_clear().output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("ok:/"), "{out:?}");
        assert!(TEMPLATE_SCRIPT.find("export HOME USER").unwrap() < TEMPLATE_SCRIPT.find("installing nix").unwrap());
    }

    #[test]
    fn the_script_names_the_harness_home_before_testenv_runs() {
        let home = TEMPLATE_SCRIPT.find("export SPIRA_HOME=\"$work/spira\"").expect("testenv refuses without SPIRA_HOME");
        assert!(home < TEMPLATE_SCRIPT.find("-p testenv -- container image").unwrap());
    }

    #[test]
    fn the_script_names_the_one_config_source_before_testenv_runs() {
        let toml = TEMPLATE_SCRIPT.find("export SPIRA_TOML=\"$work/spira-config/tests/fixtures/complete.toml:$work/template-config.toml\"").expect("testenv refuses without SPIRA_TOML");
        assert!(toml < TEMPLATE_SCRIPT.find("-p testenv -- container image").unwrap());
        assert!(TEMPLATE_SCRIPT.find("cat > \"$work/template-config.toml\"").unwrap() < toml);
    }

    #[test]
    fn the_script_builds_rather_than_loads_and_reports_the_image_last() {
        assert!(TEMPLATE_SCRIPT.contains("-p testenv -- container image"));
        assert!(!TEMPLATE_SCRIPT.contains("podman load"));
        assert!(!TEMPLATE_SCRIPT.contains("--layers=false") && !TEMPLATE_SCRIPT.contains("--no-cache"));
        assert!(TEMPLATE_SCRIPT.contains("cargo fetch --locked"));
        assert!(TEMPLATE_SCRIPT.contains("/root/.ssh/authorized_keys"));
        assert_eq!(reported_image("a\nimage=x:1\n"), Some("x:1".into()));
        assert_eq!(reported_image("image=\n"), None);
        assert_eq!(reported_image(""), None);
    }

    #[test]
    fn the_template_installs_webdav_sccache_and_the_toolchain_before_building_the_image() {
        // sp-xjnzl: the template carries its OWN sccache (so every round VM clones a working
        // one), pointed at the host's shared store, matching the host's own CARGO_HOME —
        // see REMOTE_SCRIPT's own test and sccache-dav/DESIGN.md for why that path must be
        // byte-identical.
        let cargo_home = TEMPLATE_SCRIPT.find("export CARGO_HOME=\"${cache_home:-$HOME/.cargo}\"").expect("CARGO_HOME comes from the operator's own env (cache_home), never a hardcoded literal — a literal names one operator's box");
        let toolchain = TEMPLATE_SCRIPT.find("rustup toolchain install \"$toolchain\"").expect("the toolchain comes from the rustup package, not a downloaded tarball");
        let install = TEMPLATE_SCRIPT.find("build \"path:$work#spira-env\"").expect("sccache comes from spira-env, built from the tree's own flake.lock");
        let endpoint = TEMPLATE_SCRIPT.find("export SCCACHE_WEBDAV_ENDPOINT=\"http://${host_addr}:9431\"").expect("the cache endpoint is this box's own address, empty host_addr skips it");
        let spira_config_build = TEMPLATE_SCRIPT.find("cargo build -q --locked --release -p testenv -p spira-config").expect("spira-config must be built explicitly: `cargo run -p testenv` alone never builds it, and the image's doctor-check step needs the sibling binary sp-xjnzl's container.rs stages");
        let image = TEMPLATE_SCRIPT.find("-p testenv -- container image").unwrap();
        assert!(
            cargo_home < toolchain && toolchain < install && install < endpoint && endpoint < spira_config_build && spira_config_build < image,
            "the cache, the matching toolchain and the spira-config sibling must all be in place before the image build they are meant to warm or unblock"
        );
        assert!(!TEMPLATE_SCRIPT.contains("static.rust-lang.org"), "no downloaded toolchain tarball");
        assert!(TEMPLATE_SCRIPT.contains("sccache\" --stop-server"), "the template never ships a running sccache server baked into its disk image");
        assert!(!TEMPLATE_SCRIPT.contains("/home/"), "no literal home directory — cache_home is an operator-supplied argument, not a hardcoded path");
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use crate::testutil::{FakeProvider, TempDir};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn staleness_names_both_hashes_when_the_recipe_differs() {
        let rec = |sha: Option<&str>| Record { vmid: "102".into(), image: "r:t".into(), recipe_sha: sha.map(String::from) };
        assert_eq!(staleness(Some(&rec(Some("aaa"))), "aaa"), None);
        assert_eq!(staleness(None, "aaa"), None);
        let m = staleness(Some(&rec(Some("aaa"))), "bbb").unwrap();
        assert!(m.contains("aaa") && m.contains("bbb") && m.contains("102"), "{m}");
        assert!(staleness(Some(&rec(None)), "bbb").unwrap().contains("bbb"));
    }

    #[test]
    fn set_env_key_replaces_in_place_and_appends_when_absent() {
        let t = "A=1\n# c\nexport PVE_TEMPLATE_VMID=101\nB=2\n";
        assert_eq!(set_env_key(t, "PVE_TEMPLATE_VMID", "130"), "A=1\n# c\nPVE_TEMPLATE_VMID=130\nB=2\n");
        assert_eq!(set_env_key("A=1\n", "PVE_TEMPLATE_VMID", "130"), "A=1\nPVE_TEMPLATE_VMID=130\n");
        assert_eq!(set_env_key("PVE_TEMPLATE_VMID_X=1\n", "PVE_TEMPLATE_VMID", "9"), "PVE_TEMPLATE_VMID_X=1\nPVE_TEMPLATE_VMID=9\n");
    }

    #[test]
    fn repoint_rewrites_the_file_keeping_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new();
        let f = d.path().join("pve.env");
        std::fs::write(&f, "PVE_TOKEN_SECRET=s\nPVE_TEMPLATE_VMID=101\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        repoint(&f, "130").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "PVE_TOKEN_SECRET=s\nPVE_TEMPLATE_VMID=130\n");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_record_round_trips_and_names_the_tag_it_holds() {
        let d = TempDir::new();
        assert!(Record::read(d.path()).is_none());
        let r = Record { vmid: "130".into(), image: "localhost/spira-testenv:6b90b8194ee4".into(), recipe_sha: Some("feedface".into()) };
        r.write(d.path()).unwrap();
        assert_eq!(Record::read(d.path()), Some(r.clone()));
        assert!(r.holds("6b90b8194ee4"));
        assert!(!r.holds("deadbeef0000"));
    }

    #[test]
    fn the_template_runs_the_ami_guest_setup_and_records_the_recipe() {
        let c = setup_command("/w", "10.0.0.1", "1.99.1", "/c", "abc");
        assert!(c.contains("round-vm/ami-setup.sh' '/w' '1.99.1' '/c'"), "{c}");
        assert!(c.contains("SPIRA_RECIPE_SHA='abc'") && c.contains("SPIRA_TEMPLATE_HOST_ADDR='10.0.0.1'"), "{c}");
        assert!(AMI_SETUP.contains("build-essential"), "the guest setup installs the C toolchain");
        assert!(AMI_SETUP.contains("src/template.sh") && AMI_SETUP.contains("/etc/spira-round-recipe-sha"));
    }

    #[test]
    fn template_args_take_a_ref() {
        let a = parse_template_args(&["/t".to_string(), "--ref".into(), "local/main".into()]).unwrap();
        assert_eq!(a.rev.as_deref(), Some("local/main"));
    }

    #[test]
    fn adopt_args_take_a_vmid_and_a_tree() {
        let a = parse_template_args(&s(&["/t", "--adopt", "101"])).unwrap();
        assert_eq!(a.adopt.as_deref(), Some("101"));
        assert!(parse_template_args(&s(&["/t", "--adopt"])).is_err());
    }

    #[test]
    fn adopt_writes_the_record_and_repoints_in_one_step() {
        let d = crate::testutil::TempDir::new();
        let pve = d.path().join("pve.env");
        std::fs::write(&pve, "PVE_TEMPLATE_VMID=9\nOTHER=1\n").unwrap();
        let p = FakeProvider::new();
        p.plant("101", "round-template-x");
        p.stop("101").unwrap();
        p.make_template("101").unwrap();
        let r = adopt(&p, d.path(), &pve, "101", "localhost/spira-testenv:1dd2b64c36ca", None).unwrap();
        assert_eq!(Record::read(d.path()), Some(r));
        let rec = Record::read(d.path()).unwrap();
        assert!(rec.vmid == "101" && rec.holds("1dd2b64c36ca") && rec.recipe_sha.is_none());
        assert_eq!(std::fs::read_to_string(&pve).unwrap(), "PVE_TEMPLATE_VMID=101\nOTHER=1\n");
    }

    #[test]
    fn adopt_refuses_a_vm_that_is_not_a_template_and_changes_nothing() {
        let d = crate::testutil::TempDir::new();
        let pve = d.path().join("pve.env");
        std::fs::write(&pve, "PVE_TEMPLATE_VMID=9\n").unwrap();
        let p = FakeProvider::new();
        p.plant("101", "round-1");
        assert!(adopt(&p, d.path(), &pve, "101", "localhost/spira-testenv:t", None).unwrap_err().contains("not a template"));
        assert!(Record::read(d.path()).is_none());
        assert_eq!(std::fs::read_to_string(&pve).unwrap(), "PVE_TEMPLATE_VMID=9\n");
    }

    #[test]
    fn adopt_restores_the_old_record_when_the_repoint_fails() {
        let d = crate::testutil::TempDir::new();
        let missing = d.path().join("no-such-pve.env");
        let old = Record { vmid: "9".into(), image: "localhost/spira-testenv:old".into(), recipe_sha: None };
        old.write(d.path()).unwrap();
        let p = FakeProvider::new();
        p.plant("101", "round-template-x");
        p.stop("101").unwrap();
        p.make_template("101").unwrap();
        assert!(adopt(&p, d.path(), &missing, "101", "localhost/spira-testenv:new", None).is_err());
        assert_eq!(Record::read(d.path()), Some(old));
    }
}
