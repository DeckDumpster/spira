//! `round-vm template` (DESIGN.md §2.2b, sp-dvfea): a new round template whose test image
//! is BUILT on the template, so the template keeps podman's layer cache and a later
//! closure change rebuilds only the layers it touches. It never repoints `pve.env`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::procs::command;
use crate::provider::{boot, destroy_fenced, DestroyError, Provider, ProvisionSpec};
use crate::run::{shell_quote, SshRemote};

pub const TEMPLATE_USAGE: &str =
    "round-vm template: usage: round-vm template <tree-dir> [--toolchain <ver>]";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TemplateArgs {
    pub tree_dir: PathBuf,
    /// sp-xjnzl: the toolchain `rustup` must have installed on the template so a round can
    /// pin to it (`run --toolchain`) and match the host's own rustc byte-for-byte — sccache
    /// hashes the compiler's version string, so an unpinned "stable" drifting out of step
    /// with the host is a silent, permanent cache miss, not an error.
    pub toolchain: Option<String>,
}

pub fn parse_template_args(args: &[String]) -> Result<TemplateArgs, String> {
    let mut it = args.iter();
    let tree = it.next().filter(|a| !a.starts_with("--")).ok_or(TEMPLATE_USAGE)?;
    let mut r = TemplateArgs { tree_dir: PathBuf::from(tree), toolchain: None };
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
            "--toolchain" => {
                let v = inline.or_else(|| it.next().cloned()).ok_or("round-vm template: --toolchain needs a value")?;
                r.toolchain = Some(v).filter(|v| !v.is_empty());
            }
            other => return Err(format!("round-vm template: unknown option: {other}")),
        }
    }
    Ok(r)
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
/// The toolchain install is a STANDALONE tarball from static.rust-lang.org, not `rustup
/// toolchain install`: measured on this box's own template, `rustup toolchain install`
/// refuses with `rustup is not installed at '/root/.cargo'` — a pre-existing defect in how
/// this template's rustup was provisioned, reproduced even under rustup's own ambient
/// default `CARGO_HOME`, nothing to do with this script overriding it. A plain `rustc`/
/// `cargo` binary has no such self-location check (confirmed on the host, which runs the
/// identical rustup), so installing the exact release tarball directly avoids the defect
/// rather than working around it.
pub const TEMPLATE_SCRIPT: &str = r#"set -euo pipefail
work="$1" host_addr="$2" toolchain="$3" cache_home="$4"
cd "$work"
export CARGO_HOME="${cache_home:-$HOME/.cargo}"
mkdir -p "$CARGO_HOME/bin"
if [ -n "$toolchain" ] && [ ! -x "$CARGO_HOME/bin/rustc" ]; then
    echo "round-vm template: installing rust $toolchain standalone into $CARGO_HOME (must match the host's rustc byte-for-byte, sp-xjnzl)" >&2
    tmp="$(mktemp -d)"
    curl -fsSL "https://static.rust-lang.org/dist/rust-${toolchain}-x86_64-unknown-linux-gnu.tar.gz" -o "$tmp/rust.tar.gz"
    tar -xzf "$tmp/rust.tar.gz" -C "$tmp" --strip-components=1
    "$tmp/install.sh" --prefix="$CARGO_HOME" --destdir="" --disable-ldconfig >&2
    rm -rf "$tmp"
fi
export PATH="$CARGO_HOME/bin:$PATH"
if [ ! -x "$CARGO_HOME/bin/sccache" ]; then
    echo "round-vm template: installing sccache (webdav backend) into $CARGO_HOME/bin" >&2
    cargo install sccache --locked --no-default-features --features webdav --quiet
fi
export RUSTC_WRAPPER="$CARGO_HOME/bin/sccache"
export SCCACHE_IGNORE_SERVER_IO_ERROR=1
if [ -n "$host_addr" ]; then
    export SCCACHE_WEBDAV_ENDPOINT="http://${host_addr}:9431"
    export SCCACHE_WEBDAV_KEY_PREFIX="/"
fi
# testenv's own cold-build step stages a sibling `spira-config` binary into the image build
# context for the Containerfile's doctor-check (container.rs's stage_spira_config,
# testenv/Containerfile) — `cargo run -p testenv` alone never builds spira-config, since
# nothing else in the workspace pulls in ITS bin target. Built explicitly here so the
# sibling exists before testenv ever looks for it.
cargo build -q --locked --release -p testenv -p spira-config
img="$(cargo run -q --locked --release -p testenv -- container image)"
if [ -z "$img" ]; then echo "round-vm template: testenv container image printed no image ref" >&2; exit 1; fi
podman images --format '{{.Repository}}:{{.Tag}}' \
    | { grep '^localhost/spira-testenv:' || true; } | { grep -vxF "$img" || true; } \
    | while read -r old; do podman rmi "$old" >&2; done
cargo fetch --locked >&2
"$CARGO_HOME/bin/sccache" --stop-server >&2 || true
cd /
rm -rf "$work" /root/.ssh/authorized_keys
sync
printf 'image=%s\n' "$img"
"#;

/// The image ref TEMPLATE_SCRIPT reported, if it did.
pub fn reported_image(stdout: &str) -> Option<String> {
    stdout.lines().rev().find_map(|l| l.strip_prefix("image=")).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The template VM's side, reached only by address.
pub trait Guest {
    fn reachable(&self, addr: &str) -> bool;
    /// Unpacks `commit` of the repository at `tree` into `dir` on the VM.
    fn upload(&self, addr: &str, tree: &Path, commit: &str, dir: &str) -> Result<(), String>;
    /// Runs TEMPLATE_SCRIPT in `dir` on the VM at `addr`, pointing its cache at
    /// `host_addr` (this box's own LAN address; empty skips the cache), installing
    /// `toolchain` (empty: leave rustup's default alone) at `cache_home` (empty: the
    /// template's own ambient `CARGO_HOME`): (exit code, stdout). 255 is ssh itself.
    fn prepare(&self, addr: &str, dir: &str, host_addr: &str, toolchain: &str, cache_home: &str) -> Result<(i32, String), String>;
}

impl Guest for SshRemote {
    fn reachable(&self, addr: &str) -> bool {
        crate::run::Remote::reachable(self, addr)
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

    fn prepare(&self, addr: &str, dir: &str, host_addr: &str, toolchain: &str, cache_home: &str) -> Result<(i32, String), String> {
        let mut child = command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(format!(
                "bash -s -- {} {} {} {}",
                shell_quote(dir),
                shell_quote(host_addr),
                shell_quote(toolchain),
                shell_quote(cache_home)
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ssh: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            si.write_all(TEMPLATE_SCRIPT.as_bytes()).map_err(|e| format!("ssh stdin: {e}"))?;
        }
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
        .and_then(|()| prepare_vm(p, g, spec, ssh_tries, tree, commit, &vmid, host_addr, toolchain, cache_home));
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
    eprintln!("round-vm template: {vmid} at {addr}: unpacking {commit}");
    g.upload(&addr, tree, commit, TEMPLATE_WORK)?;
    eprintln!("round-vm template: building the test image on {vmid} (a cold build: its heartbeat follows)");
    let (rc, out) = g.prepare(&addr, TEMPLATE_WORK, host_addr, toolchain, cache_home)?;
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
        calls: Mutex<Vec<String>>,
    }

    impl FakeGuest {
        fn ok() -> FakeGuest {
            FakeGuest { rc: 0, out: "noise\nimage=localhost/spira-testenv:abc123\n".into(), reachable: true, calls: Mutex::new(vec![]) }
        }
    }

    impl Guest for FakeGuest {
        fn reachable(&self, _: &str) -> bool {
            self.reachable
        }
        fn upload(&self, addr: &str, _: &Path, commit: &str, dir: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("upload {addr} {commit} {dir}"));
            Ok(())
        }
        fn prepare(&self, addr: &str, dir: &str, host_addr: &str, toolchain: &str, cache_home: &str) -> Result<(i32, String), String> {
            self.calls.lock().unwrap().push(format!("prepare {addr} {dir} {host_addr} {toolchain} {cache_home}"));
            Ok((self.rc, self.out.clone()))
        }
    }

    const SHA: &str = "0123456789abcdef0123";

    #[test]
    fn args_take_a_tree_and_a_toolchain_and_refuse_a_chosen_vmid() {
        assert_eq!(
            parse_template_args(&s(&["/t"])).unwrap(),
            TemplateArgs { tree_dir: "/t".into(), toolchain: None }
        );
        assert!(parse_template_args(&s(&["/t", "--vmid", "9120"])).unwrap_err().contains("hypervisor assigns"));
        assert!(parse_template_args(&s(&["/t", "--vmid=9121"])).unwrap_err().contains("hypervisor assigns"));
        assert_eq!(
            parse_template_args(&s(&["/t", "--toolchain", "1.98.1"])).unwrap().toolchain.as_deref(),
            Some("1.98.1")
        );
        assert_eq!(
            parse_template_args(&s(&["/t", "--toolchain=1.98.1"])).unwrap().toolchain.as_deref(),
            Some("1.98.1")
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
        let b = build(&p, &g, &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "192.168.1.56", "1.98.1", "/opt/spira/cargo").unwrap();
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
                format!("prepare 10.0.0.9120 {TEMPLATE_WORK} 192.168.1.56 1.98.1 /opt/spira/cargo")
            ]
        );
    }

    #[test]
    fn without_a_vmid_the_hypervisor_allocates_one() {
        let p = FakeProvider::new();
        let b = build(&p, &FakeGuest::ok(), &spec(), 3, Path::new("/tree"), SHA, None, "", "", "").unwrap();
        assert_eq!(b.vmid, "100");
        assert_eq!(p.count("nextid"), 1);
    }

    #[test]
    fn a_failed_image_build_destroys_the_half_built_vm_and_converts_nothing() {
        let p = FakeProvider::new();
        let g = FakeGuest { rc: 1, ..FakeGuest::ok() };
        let e = build(&p, &g, &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "").unwrap_err();
        assert!(e.contains("exited 1") && e.contains("destroyed"), "{e}");
        assert!(p.all_vms().is_empty(), "{:?}", p.all_vms());
        assert_eq!(p.count("template "), 0);
    }

    #[test]
    fn no_reported_image_is_a_failure_even_on_exit_0() {
        let p = FakeProvider::new();
        let g = FakeGuest { out: "built something\n".into(), ..FakeGuest::ok() };
        let e = build(&p, &g, &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "").unwrap_err();
        assert!(e.contains("reported no image"), "{e}");
        assert!(p.all_vms().is_empty());
    }

    #[test]
    fn every_failure_after_the_clone_leaves_no_vm() {
        for step in [Step::Start, Step::Addr, Step::Exec, Step::Shutdown, Step::MakeTemplate] {
            let p = FakeProvider::new();
            p.fail(step);
            let e = build(&p, &FakeGuest::ok(), &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "").unwrap_err();
            assert!(p.all_vms().is_empty(), "{step:?} left {:?}: {e}", p.all_vms());
        }
        let p = FakeProvider::new();
        let g = FakeGuest { reachable: false, ..FakeGuest::ok() };
        let e = build(&p, &g, &spec(), 2, Path::new("/tree"), SHA, Some("9120".into()), "", "", "").unwrap_err();
        assert!(e.contains("never became reachable"), "{e}");
        assert!(p.all_vms().is_empty());
    }

    #[test]
    fn a_vmid_that_is_taken_is_refused_and_the_vm_there_is_never_touched() {
        let p = FakeProvider::new();
        p.plant("108", "round-template-old");
        let e = build(&p, &FakeGuest::ok(), &spec(), 3, Path::new("/tree"), SHA, Some("108".into()), "", "", "").unwrap_err();
        assert!(e.contains("clone refused") && e.contains("refusing to destroy"), "{e}");
        assert_eq!(p.name("108").as_deref(), Some("round-template-old"));
        assert_eq!(p.count("destroy"), 0);
    }

    #[test]
    fn a_vm_that_will_not_die_is_named_for_the_operator() {
        let p = FakeProvider::new();
        p.fail(Step::Shutdown);
        p.fail(Step::Destroy);
        let e = build(&p, &FakeGuest::ok(), &spec(), 3, Path::new("/tree"), SHA, Some("9120".into()), "", "", "").unwrap_err();
        assert!(e.contains("by hand") && e.contains("9120"), "{e}");
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
        let toolchain = TEMPLATE_SCRIPT.find("static.rust-lang.org/dist/rust-${toolchain}-").expect("a standalone toolchain, not rustup (its own toolchain-install is broken on this template)");
        let install = TEMPLATE_SCRIPT.find("cargo install sccache --locked --no-default-features --features webdav").expect("the exact install command, same as deps.toml's and doctor's");
        let endpoint = TEMPLATE_SCRIPT.find("export SCCACHE_WEBDAV_ENDPOINT=\"http://${host_addr}:9431\"").expect("the cache endpoint is this box's own address, empty host_addr skips it");
        let spira_config_build = TEMPLATE_SCRIPT.find("cargo build -q --locked --release -p testenv -p spira-config").expect("spira-config must be built explicitly: `cargo run -p testenv` alone never builds it, and the image's doctor-check step needs the sibling binary sp-xjnzl's container.rs stages");
        let image = TEMPLATE_SCRIPT.find("-p testenv -- container image").unwrap();
        assert!(
            cargo_home < toolchain && toolchain < install && install < endpoint && endpoint < spira_config_build && spira_config_build < image,
            "the cache, the matching toolchain and the spira-config sibling must all be in place before the image build they are meant to warm or unblock"
        );
        assert!(!TEMPLATE_SCRIPT.contains("rustup toolchain install"), "rustup's own toolchain-install refuses on this template (measured: \"rustup is not installed at ...\") — never reintroduce it here");
        assert!(TEMPLATE_SCRIPT.contains("sccache\" --stop-server"), "the template never ships a running sccache server baked into its disk image");
        assert!(!TEMPLATE_SCRIPT.contains("/home/"), "no literal home directory — cache_home is an operator-supplied argument, not a hardcoded path");
    }
}
