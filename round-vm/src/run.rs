//! `round-vm run` (DESIGN.md §2.2): mirror → acquire → batch on the VM → pull results,
//! telemetry and binaries → manifest → install by tree sha → release.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;
use std::time::Duration;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use serde_json::Value;

use crate::config::{Config, PveEnv};
use crate::template::Record;
use crate::pool::{Deps, Pool};
use crate::procs::{command, FileLock};
use crate::schema::{AcquireMode, Manifest, ProcId, Vm};
use crate::spool::{linger, stream_into, Server, Spool};

pub const RUN_USAGE: &str =
    "round-vm run: usage: round-vm run <tree-dir> [--suites <csv>] [--maxpar <n>] [--toolchain <ver>] [--results-dir <dir>] [--attr-spool <dir>]";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunArgs {
    pub tree_dir: PathBuf,
    pub suites: Option<String>,
    pub maxpar: Option<u32>,
    pub toolchain: Option<String>,
    pub results_dir: Option<PathBuf>,
    /// DESIGN.md §2.2a: stream results while the corpus runs, and serve attribution reruns.
    pub attr_spool: Option<PathBuf>,
}

/// Parses `run`'s arguments; `--opt value` and `--opt=value` both work.
pub fn parse_run_args(args: &[String]) -> Result<RunArgs, String> {
    let mut it = args.iter();
    let tree = it.next().filter(|a| !a.starts_with("--")).ok_or(RUN_USAGE)?;
    let mut r = RunArgs { tree_dir: PathBuf::from(tree), ..Default::default() };
    while let Some(a) = it.next() {
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut val = || inline.clone().or_else(|| it.next().cloned()).ok_or(format!("round-vm run: {key} needs a value"));
        match key {
            "--suites" => r.suites = Some(val()?).filter(|v| !v.is_empty()),
            "--maxpar" => {
                let v = val()?;
                r.maxpar = Some(v.parse().map_err(|_| format!("round-vm run: --maxpar not a number: {v:?}"))?);
            }
            "--toolchain" => r.toolchain = Some(val()?).filter(|v| !v.is_empty()),
            "--results-dir" => r.results_dir = Some(PathBuf::from(val()?)),
            "--attr-spool" => r.attr_spool = Some(PathBuf::from(val()?)),
            other => return Err(format!("round-vm run: unknown option: {other}")),
        }
    }
    Ok(r)
}

/// The host side: the tree's identity and the mirror the VM clones from.
pub trait Host: Sync {
    fn is_checkout(&self, tree: &Path) -> bool;
    /// (commit sha, tree sha) of `tree`'s HEAD.
    fn head(&self, tree: &Path) -> Result<(String, String), String>;
    /// Points the mirror at `tree`'s HEAD and makes sure the daemon serving it is up.
    fn prepare_mirror(&self, tree: &Path) -> Result<(), String>;
    /// The testenv image tag `commit` of `tree` needs, computed without a checkout.
    fn image_tag(&self, tree: &Path, commit: &str) -> Result<String, String>;
    /// Fetches `branch` of `tree`'s repository into the mirror as `refs/heads/<as_ref>`.
    fn mirror_ref(&self, tree: &Path, branch: &str, as_ref: &str) -> Result<(), String>;
}

pub struct BatchJob {
    pub host_addr: String,
    pub mirror_port: u16,
    pub suites: Option<String>,
    pub maxpar: u32,
    pub toolchain: Option<String>,
    /// sp-xjnzl: this operator's own `CARGO_HOME` (read from the caller's own environment,
    /// never hardcoded — a literal path names one operator's box, which this crate ships to
    /// everyone who clones it). Empty: the VM's own ambient default applies, no cache
    /// sharing with the host.
    pub cache_home: Option<String>,
    pub testenv_registry: Option<String>,
    /// Seconds of setup (script start to the suites launching) beyond which the run reports SETUP-SLOW.
    pub setup_alarm_secs: u64,
}

/// The VM side, reached only by address.
pub trait Remote: Sync {
    fn reachable(&self, addr: &str) -> bool;
    /// Runs the batch on the VM; the remote testenv runner's exit code (255: ssh itself).
    fn run_batch(&self, addr: &str, job: &BatchJob) -> Result<i32, String>;
    /// Copies the remote directory `remote_path` into `local`.
    fn pull(&self, addr: &str, remote_path: &str, local: &Path) -> Result<(), String>;
    /// Runs one attribution rerun (DESIGN.md §2.2a); its testenv exit code (255: ssh).
    fn run_attr(&self, addr: &str, job: &crate::spool::AttrJob) -> Result<i32, String>;
}

/// Single-quotes `s` for a POSIX shell, so an empty argument survives ssh's re-joining of
/// the remote command line (the bash lost an empty --suites and shifted every later one).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The VM side of a round. The VM is a LAUNCHER (design runtime-is-a-release, sp-dvfea): it
/// builds the round's tree, stages that build as a release (`release build --bin-dir`, the
/// same layout production and GitHub CI run, sp-6cbna) and sets `SPIRA_RELEASE` and `PATH`
/// outright — the release's `bin/` and `spira/`, then cargo and the system directories —
/// for everything after, so conf.sh finds `spira-config` and every harness script runs this
/// round's tools by name. [`LAUNCHER_ENV`] records the two for the attribution jobs.
/// Then it runs the round's own `testenv` (release profile — its binaries ship; its in-place
/// build is incremental on the one just made) and stages `target/release`'s executables
/// into `~/round-bins/` so the host pulls the binaries and not cargo's target directory.
pub const REMOTE_SCRIPT: &str = r#"set -euo pipefail
host_addr="$1" port="$2" suites="$3" maxpar="$4" toolchain="$5" cache_home="$6" registry="$7" setup_alarm="$8"
t_start=$(date +%s)
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin"
if ! command -v cargo >/dev/null 2>&1; then
    echo "round-vm: cargo not found on PATH ($PATH) — the template is missing a Rust toolchain" >&2
    exit 127
fi
rm -rf ~/round-work ~/round-bins ~/round-launcher.env
git clone --quiet "git://${host_addr}:${port}/mirror.git" ~/round-work
cd ~/round-work
# sp-xjnzl-2: warm conf.sh's own generated fragments (conf.d.keys.generated.sh,
# conf.d.defaults.generated.sh — gitignored, never present after a fresh clone) ONCE,
# sequentially, before the suite batch starts. Every suite's own container sources
# conf.sh (directly, or through a tool like watchd that shells into it), and conf.sh
# regenerates these itself when they are stale or missing (conf-gen.sh) — on the HOST's
# long-lived checkout that is already warm almost always, so the regeneration path is
# barely exercised; on a FRESH VM clone every single one of 400+ suites hits "missing"
# on its first conf.sh sourcing, all at once, at --maxpar. Warming it here, before any
# suite runs, means every one of them finds it already fresh and never regenerates at
# all — this is what test-install-migrate.sh's intermittent "the watcher manifest is
# malformed" (watchd's own conf.sh sourcing failing) traced back to.
bash spira/conf-gen.sh >&2 || true
if [ -n "$toolchain" ]; then export RUSTUP_TOOLCHAIN="$toolchain"; fi
# sp-xjnzl: ONE compilation cache shared with the host itself, not a VM-local one — the
# box's own address, which this VM already reaches for the mirror, is reused for the cache
# store too (sccache-dav/DESIGN.md). CARGO_HOME is overridden to $6, this binary's OWN
# resolved config (SPIRA_ROUND_VM_CACHE_HOME, or the operator's config file — Config::load's
# own doc comment names where) — never
# the caller's ambient CARGO_HOME, and never empty: run()'s own preflight already refused
# before this script was ever sent (sp-xjnzl-2, law-a-binary-resolves-the-config-it-reads).
# sccache hashes a dependency's registry source path into its cache key, so a hit across
# machines needs that path byte-identical, not merely consistent (spira-config/DESIGN-
# build-cache.md §2.5; re-verified against sccache 0.18.0's own generate_hash_key for this
# bead). The `${cache_home:-...}` below is belt-and-suspenders only — Source::get already
# filters out an empty value into None, which the Rust-side preflight refuses on — never a
# real fallback path in production. A read or write this box's store refuses degrades to a
# cache miss, never a build failure (sccache's own RemoteStorage tolerates both) — so an
# unreachable store costs speed, not a round.
export CARGO_HOME="${cache_home:-$HOME/.cargo}"
mkdir -p "$CARGO_HOME/bin"
export RUSTC_WRAPPER="$CARGO_HOME/bin/sccache"
export SCCACHE_IGNORE_SERVER_IO_ERROR=1
export SCCACHE_WEBDAV_ENDPOINT="http://${host_addr}:9431"
export SCCACHE_WEBDAV_KEY_PREFIX="/"
t0=$(date +%s)
if ! cargo build -q --profile release --workspace --config profile.release.incremental=false; then
    echo "round-vm: the round's workspace build failed" >&2
    exit 4
fi
echo "round-vm: built the round in $(( $(date +%s) - t0 ))s" >&2
# The round's one source of config: the tree's complete fixture, with this VM's own paths
# declared over it. Nothing here is searched for or defaulted.
mkdir -p "$HOME/round-work/.runtime/spira"
cat > "$HOME/round-config.toml" <<ROUNDCFG
[spira]
run = "$HOME/round-work/.runtime/spira"   # where REMOTE_RESULTS pulls batch-results from
releases = "$HOME/round-releases"
home_repo = "$HOME/round-work"
batch_maxpar = $maxpar
ROUNDCFG
export SPIRA_TOML="$HOME/round-work/spira-config/tests/fixtures/complete.toml:$HOME/round-config.toml"
rel_sha="$(SPIRA_HOME="$HOME/round-work/spira" target/release/release build "$(git rev-parse HEAD)" --repo "$HOME/round-work" --bin-dir "$HOME/round-work/target/release" --releases "$HOME/round-releases")"
export SPIRA_RELEASE="$HOME/round-releases/$rel_sha"
export SPIRA_REPO="$HOME/round-work"
export PATH="$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:$CARGO_HOME/bin:$HOME/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
got="$(command -v spira-config || true)"
if [ "$got" != "$SPIRA_RELEASE/bin/spira-config" ]; then
    echo "round-vm: spira-config resolves to ${got:-nothing}, not the staged release $SPIRA_RELEASE" >&2
    exit 2
fi
printf 'export SPIRA_RELEASE=%q\nexport SPIRA_REPO=%q\nexport PATH=%q\n' "$SPIRA_RELEASE" "$SPIRA_REPO" "$PATH" > ~/round-launcher.env
echo "round-vm: launcher: SPIRA_RELEASE=$SPIRA_RELEASE" >&2
tag="$(testenv container tag)" || tag=""
# A ROUND NEVER COLD-BUILDS ITS IMAGE (law-unexpected-image-builds-are-killed-then-fixed,
# sp-lktok). An absent image means this VM predates the template, or the template predates
# the tree: refuse at once, naming it, rather than compile toolchains for twenty minutes in
# the critical path. Exit 3 (cannot tell): no verdict was reached.
if [ -z "$tag" ]; then
    echo "round-vm: IMAGE-ABSENT: testenv could not compute the image tag for this tree — refusing to run (a round never builds its image)" >&2
    exit 3
elif podman image exists "localhost/spira-testenv:$tag" 2>/dev/null; then
    echo "round-vm: template image: localhost/spira-testenv:$tag present" >&2
else
    echo "round-vm: IMAGE-ABSENT: localhost/spira-testenv:$tag is not on this VM — refusing to cold-build it (law-unexpected-image-builds-are-killed-then-fixed); refresh the template with round-vm template and recycle the warm pool" >&2
    exit 3
fi
setup_secs=$(( $(date +%s) - t_start ))
if [ -n "$registry" ]; then export SPIRA_TESTENV_REGISTRY="$registry"; fi
set +e
# THE WORKSPACE'S OWN UNIT TESTS, once per round (per Ryan 2026-10-05: no suite invokes cargo).
# They run beside the suites, on the build above; a red here makes the round red.
# A SCRUBBED ENVIRONMENT: the launcher's SPIRA_RELEASE/SPIRA_REPO/PATH exported above leak
# into tests that resolve configuration (4 reds on 2026-10-05); the tests get HOME, cargo's own
# PATH, the build cache and a git identity (a round VM's root has none), nothing else.
env -i HOME="$HOME" PATH="$CARGO_HOME/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" \
    CARGO_HOME="$CARGO_HOME" RUSTC_WRAPPER="$RUSTC_WRAPPER" SCCACHE_IGNORE_SERVER_IO_ERROR=1 \
    SCCACHE_WEBDAV_ENDPOINT="$SCCACHE_WEBDAV_ENDPOINT" SCCACHE_WEBDAV_KEY_PREFIX="$SCCACHE_WEBDAV_KEY_PREFIX" \
    GIT_AUTHOR_NAME=round GIT_AUTHOR_EMAIL=round@spira GIT_COMMITTER_NAME=round GIT_COMMITTER_EMAIL=round@spira \
    cargo test -q --profile release --workspace --no-fail-fast --config profile.release.incremental=false > ~/round-unit-tests.log 2>&1 &
unit_pid=$!
# The suites run on the build above (--artifacts: testenv never runs cargo a second time).
if [ -n "$suites" ]; then
    testenv --mode parallel --artifacts "$HOME/round-work/target/release" --suites "$suites" round
else
    testenv --mode parallel --artifacts "$HOME/round-work/target/release" round
fi
rc=$?
wait "$unit_pid"; unit_rc=$?
if [ "$unit_rc" -eq 0 ]; then
    echo "round-vm: UNIT-TESTS: PASS ($(grep -c '^test result: ok' ~/round-unit-tests.log) test binaries)" >&2
else
    echo "round-vm: UNIT-TESTS: FAIL (cargo test rc=$unit_rc):" >&2
    grep -E -A3 '^(test .* FAILED|---- |error(\[|:))|panicked at' ~/round-unit-tests.log | head -120 >&2
    [ "$rc" -eq 0 ] && rc=1
fi
suites_secs=$(( $(date +%s) - t_start - setup_secs ))
echo "round-vm: setup ${setup_secs}s, suites ${suites_secs}s" >&2
if [ "$setup_secs" -gt "$setup_alarm" ]; then
    echo "round-vm: SETUP-SLOW: setup took ${setup_secs}s, over the ${setup_alarm}s limit — something is being built or fetched that the template should hold" >&2
fi
mkdir -p ~/round-bins
find target/release -maxdepth 1 -type f -executable -exec cp {} ~/round-bins/ \; 2>/dev/null
exit "$rc"
"#;

/// What REMOTE_SCRIPT leaves on the VM for the attribution jobs: the launcher's
/// `SPIRA_RELEASE` and `PATH`, as `export` lines.
pub const LAUNCHER_ENV: &str = "round-launcher.env";

/// Where REMOTE_SCRIPT stages the round's executables, relative to the VM user's home.
pub const REMOTE_BINS: &str = "round-bins/";

pub fn remote_command(job: &BatchJob) -> String {
    let args = [
        job.host_addr.clone(),
        job.mirror_port.to_string(),
        job.suites.clone().unwrap_or_default(),
        job.maxpar.to_string(),
        job.toolchain.clone().unwrap_or_default(),
        job.cache_home.clone().unwrap_or_default(),
        job.testenv_registry.clone().unwrap_or_default(),
        job.setup_alarm_secs.to_string(),
    ];
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    format!("bash -s -- {}", quoted.join(" "))
}

pub struct SshRemote {
    pub user: String,
    pub port: u16,
    pub key: PathBuf,
}

impl SshRemote {
    pub(crate) fn ssh_opts(&self) -> Vec<String> {
        [
            "-i", &self.key.to_string_lossy(), "-p", &self.port.to_string(),
            "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

impl Remote for SshRemote {
    fn reachable(&self, addr: &str) -> bool {
        command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run_batch(&self, addr: &str, job: &BatchJob) -> Result<i32, String> {
        let mut child = command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(remote_command(job))
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ssh: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            si.write_all(REMOTE_SCRIPT.as_bytes()).map_err(|e| format!("ssh stdin: {e}"))?;
        }
        let st = child.wait().map_err(|e| format!("ssh: {e}"))?;
        Ok(st.code().unwrap_or(255))
    }

    fn run_attr(&self, addr: &str, job: &crate::spool::AttrJob) -> Result<i32, String> {
        let mut child = command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(crate::spool::attr_command(job))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| format!("ssh: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            si.write_all(crate::spool::REMOTE_ATTR_SCRIPT.as_bytes()).map_err(|e| format!("ssh stdin: {e}"))?;
        }
        let st = child.wait().map_err(|e| format!("ssh: {e}"))?;
        Ok(st.code().unwrap_or(255))
    }

    fn pull(&self, addr: &str, remote_path: &str, local: &Path) -> Result<(), String> {
        fs::create_dir_all(local).map_err(|e| format!("{}: {e}", local.display()))?;
        let st = command("rsync")
            .arg("-a")
            .arg("-e")
            .arg(format!("ssh {}", self.ssh_opts().join(" ")))
            .arg(format!("{}@{addr}:{remote_path}", self.user))
            .arg(format!("{}/", local.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("rsync: {e}"))?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("rsync {remote_path} exited {}", st.code().unwrap_or(-1)))
        }
    }
}

pub struct GitHost {
    pub state_dir: PathBuf,
    pub mirror_port: u16,
    /// The LAN address the VMs reach; the daemon binds only there.
    pub listen: String,
}

pub(crate) fn git(args: &[&str]) -> Result<String, String> {
    let out = command("git").args(args).stdin(Stdio::null()).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The addresses on each line of `ip -br addr`, prefix lengths stripped.
fn host_addresses(ip_br_addr: &str) -> Vec<String> {
    ip_br_addr
        .lines()
        .flat_map(|l| l.split_whitespace().skip(2))
        .map(|a| a.split('/').next().unwrap_or(a).to_string())
        .collect()
}

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// Stops the mirror daemon `prepare_mirror` left running for `state_dir`, and waits for it
/// to be gone. Nothing running is success.
pub fn stop_mirror(state_dir: &Path) -> Result<(), String> {
    let pidfile = state_dir.join("git-daemon.pid");
    let Some(pid) = fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok()) else {
        return Ok(());
    };
    if pid_alive(pid) {
        // SAFETY: plain SIGTERM to the pid the daemon wrote for this state dir.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        for _ in 0..50 {
            if !pid_alive(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if pid_alive(pid) {
            return Err(format!("git daemon {pid} did not exit after SIGTERM"));
        }
    }
    let _ = fs::remove_file(&pidfile);
    Ok(())
}

impl Host for GitHost {
    fn is_checkout(&self, tree: &Path) -> bool {
        git(&["-C", &tree.to_string_lossy(), "rev-parse", "--git-dir"]).is_ok()
    }

    fn head(&self, tree: &Path) -> Result<(String, String), String> {
        let t = tree.to_string_lossy();
        Ok((git(&["-C", &t, "rev-parse", "HEAD"])?, git(&["-C", &t, "rev-parse", "HEAD^{tree}"])?))
    }

    fn image_tag(&self, tree: &Path, commit: &str) -> Result<String, String> {
        let scratch = self.state_dir.join(format!(".tag.{}", std::process::id()));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        let r = (|| {
            let mut archive = command("git")
                .args(["-C", &tree.to_string_lossy(), "archive", "--format=tar", commit, "spira/testenv/Containerfile", "spira/deps.toml"])
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|e| format!("git archive: {e}"))?;
            let out = archive.stdout.take().ok_or("git archive: no stdout")?;
            let st = command("tar").arg("-x").arg("-C").arg(&scratch).stdin(out).status().map_err(|e| format!("tar: {e}"))?;
            if !archive.wait().map_err(|e| e.to_string())?.success() || !st.success() {
                return Err(format!("cannot read the image closure of {commit}"));
            }
            let o = command("testenv")
                .args(["container", "tag"])
                .env("SPIRA_TESTENV_HARNESS", &scratch)
                .output()
                .map_err(|e| format!("testenv container tag: {e}"))?;
            let tag = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && !tag.is_empty() {
                Ok(tag)
            } else {
                Err(format!("testenv container tag exited {}: {}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).trim()))
            }
        })();
        let _ = fs::remove_dir_all(&scratch);
        r
    }

    fn prepare_mirror(&self, tree: &Path) -> Result<(), String> {
        if self.listen.is_empty() {
            return Err("no listen address for the mirror daemon (SPIRA_ROUND_VM_HOST_ADDR)".into());
        }
        let out = command("ip").args(["-br", "addr"]).stdin(Stdio::null()).output().map_err(|e| format!("ip -br addr: {e}"))?;
        let addrs = host_addresses(&String::from_utf8_lossy(&out.stdout));
        if !out.status.success() || !addrs.iter().any(|a| a == &self.listen) {
            return Err(format!(
                "{} is not an address of this host (ip -br addr: {}); the host's address has changed — \
                 set round_vm_host_addr (and sccache_dav_addr) to the current one with spira-config set, then re-activate the release",
                self.listen,
                addrs.join(" ")
            ));
        }
        fs::create_dir_all(&self.state_dir).map_err(|e| e.to_string())?;
        let mirror = self.state_dir.join("mirror.git");
        let m = mirror.to_string_lossy().to_string();
        if !mirror.is_dir() {
            git(&["init", "--quiet", "--bare", &m])?;
        }
        git(&["--git-dir", &m, "fetch", "--quiet", &tree.to_string_lossy(), "+HEAD:refs/heads/round"])?;
        git(&["--git-dir", &m, "symbolic-ref", "HEAD", "refs/heads/round"])?;

        let pidfile = self.state_dir.join("git-daemon.pid");
        let running = |p: &Path| fs::read_to_string(p).ok().and_then(|s| s.trim().parse().ok()).map(pid_alive).unwrap_or(false);
        let addrfile = self.state_dir.join("git-daemon.addr");
        let bound_to = fs::read_to_string(&addrfile).ok().map(|s| s.trim().to_string());
        if running(&pidfile) {
            if bound_to.as_deref() == Some(self.listen.as_str()) {
                return Ok(());
            }
            stop_mirror(&self.state_dir)?;
        }
        let _ = fs::remove_file(&pidfile);
        let _ = fs::remove_file(&addrfile);
        let st = command("git")
            .arg("daemon")
            .arg("--reuseaddr")
            .arg(format!("--listen={}", self.listen))
            .arg(format!("--port={}", self.mirror_port))
            .arg(format!("--base-path={}", self.state_dir.display()))
            .arg("--export-all")
            .arg(format!("--pid-file={}", pidfile.display()))
            .arg("--detach")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("git daemon: {e}"))?;
        for _ in 0..20 {
            if running(&pidfile) {
                return fs::write(&addrfile, &self.listen).map_err(|e| format!("{}: {e}", addrfile.display()));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(format!("git daemon did not start on port {} (exit {:?})", self.mirror_port, st.code()))
    }

    fn mirror_ref(&self, tree: &Path, branch: &str, as_ref: &str) -> Result<(), String> {
        let m = self.state_dir.join("mirror.git").to_string_lossy().to_string();
        git(&["--git-dir", &m, "fetch", "--quiet", &tree.to_string_lossy(), &format!("+refs/heads/{branch}:refs/heads/{as_ref}")]).map(|_| ())
    }
}

/// The directory that holds the pulled `.result` files: `dir` itself, or the one
/// BATCH_KEY subdirectory the testenv runner nests them under.
pub fn results_leaf(dir: &Path) -> Option<PathBuf> {
    let has_result = |d: &Path| {
        fs::read_dir(d)
            .map(|rd| rd.flatten().any(|e| e.path().extension().map(|x| x == "result").unwrap_or(false)))
            .unwrap_or(false)
    };
    if has_result(dir) {
        return Some(dir.to_path_buf());
    }
    let mut subs: Vec<PathBuf> = fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    subs.sort();
    subs.into_iter().find(|p| has_result(p))
}

/// Sum of every suite's own wall time: the third field of each `<suite>.result`.
pub fn suite_wall_sum(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter(|e| e.path().extension().map(|x| x == "result").unwrap_or(false))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| t.split_whitespace().nth(2).and_then(|s| s.parse::<u64>().ok()))
        .sum()
}

/// testenv's `build_wall_s` from `runner.meta`, present only when it built.
pub fn build_wall(dir: &Path) -> Option<u64> {
    let t = fs::read_to_string(dir.join("runner.meta")).ok()?;
    t.lines().find_map(|l| l.strip_prefix("build_wall_s=")).and_then(|v| v.trim().parse().ok())
}

/// The tree the VM's testenv run built and tested: `tree=` in the pulled `batch.meta`.
pub fn reported_tree(results: &Path) -> Option<String> {
    let t = fs::read_to_string(results.join("batch.meta")).ok()?;
    t.lines().find_map(|l| l.strip_prefix("tree=")).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The executables directly in `dir` (the pulled `round-bins/`).
fn executables(dir: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for e in fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?.flatten() {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_symlink() {
            let target = fs::read_link(&from).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&to);
            std::os::unix::fs::symlink(target, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        } else {
            fs::copy(&from, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
    }
    Ok(())
}

/// Installs the pulled executables into `<target>` — the round worktree's own
/// `target/release`, where `queue land-local --worktree` reads them (G8). The VM's testenv
/// must report (batch.meta `tree=`) exactly the tree the host sent; any other tree is a
/// refusal naming both, and no tree at all (the run died first) is a distinct error;
/// either installs nothing. No executables at all (the batch never
/// reached its build) is not a refusal.
pub fn install_bins(pulled: &Path, expected: &str, reported: Option<&str>, target: &Path) -> Result<(), String> {
    let exes = executables(pulled);
    if exes.is_empty() {
        return Ok(());
    }
    match reported {
        None => return Err("round-vm run: the VM reported no tested tree (batch did not complete) — nothing installed".into()),
        Some(r) if r != expected => {
            return Err(format!("round-vm run: refusing binaries: tree sha {r} does not match expected {expected} — nothing installed"))
        }
        Some(_) => {}
    }
    fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    for exe in exes {
        let to = target.join(exe.file_name().unwrap());
        let _ = fs::remove_file(&to);
        fs::copy(&exe, &to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

/// Appends the VM's tsd rows to the host's `<run>/tsd/<family>.jsonl`, each tagged with
/// `ran_on`, `vcpus` and `maxpar`, skipping a row already present, under `<file>.lock`.
pub fn merge_tsd(src: &Path, run_dir: &Path, vm: &str, vcpus: u32, maxpar: u32) -> Result<usize, String> {
    let Ok(rd) = fs::read_dir(src) else { return Ok(0) };
    let mut added = 0;
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false)).collect();
    files.sort();
    for f in files {
        let dest = run_dir.join("tsd").join(f.file_name().unwrap());
        let mut lock_path = dest.clone().into_os_string();
        lock_path.push(".lock");
        let _lock = FileLock::exclusive(Path::new(&lock_path))?;
        let mut seen: HashSet<String> = fs::read_to_string(&dest).unwrap_or_default().lines().map(str::to_string).collect();
        let mut out = fs::OpenOptions::new().create(true).append(true).open(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        for line in fs::read_to_string(&f).unwrap_or_default().lines() {
            let Ok(Value::Object(mut row)) = serde_json::from_str::<Value>(line) else { continue };
            row.insert("ran_on".into(), Value::from(vm));
            row.insert("vcpus".into(), Value::from(vcpus));
            row.insert("maxpar".into(), Value::from(maxpar));
            let s = Value::Object(row).to_string();
            if seen.insert(s.clone()) {
                writeln!(out, "{s}").map_err(|e| format!("{}: {e}", dest.display()))?;
                added += 1;
            }
        }
    }
    Ok(added)
}

pub struct RunEnv<'a> {
    pub cfg: &'a Config,
    pub pool: &'a Pool,
    pub deps: &'a Deps<'a>,
    pub host: &'a dyn Host,
    pub remote: &'a dyn Remote,
}

/// `round-vm run`. Returns the process exit code (DESIGN.md §2.2).
pub fn run(env: &RunEnv, args: &RunArgs) -> i32 {
    let cfg = env.cfg;
    if !env.host.is_checkout(&args.tree_dir) {
        eprintln!("round-vm run: not a git checkout: {}", args.tree_dir.display());
        return 2;
    }
    let Some(host_addr) = cfg.host_addr.clone() else {
        eprintln!("round-vm run: SPIRA_ROUND_VM_HOST_ADDR not set");
        return 2;
    };
    if fs::File::open(&cfg.host_key).is_err() {
        eprintln!("round-vm run: SPIRA_ROUND_VM_HOST_KEY not readable: {}", cfg.host_key.display());
        return 2;
    }
    if cfg.cache_home.is_none() {
        eprintln!(
            "round-vm run: SPIRA_ROUND_VM_CACHE_HOME is not set, by environment or in the \
             operator's own config file — cannot resolve the VM-side CARGO_HOME; the \
             template's own sccache lives wherever `round-vm template` was told to put it, \
             and this binary refuses to guess (law-a-binary-resolves-the-config-it-reads). \
             Set it to match."
        );
        return 2;
    }
    let (commit_sha, tree_sha) = match env.host.head(&args.tree_dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("round-vm run: {e}");
            return 2;
        }
    };
    if let Some(code) = stale_template(env, &args.tree_dir, &commit_sha) {
        return code;
    }
    if let Err(e) = env.host.prepare_mirror(&args.tree_dir) {
        eprintln!("round-vm run: cannot prepare the mirror from {}: {e}", args.tree_dir.display());
        return 2;
    }
    let (vm, mode) = match env.pool.acquire(env.deps, Some(ProcId::current())) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("round-vm run: acquire failed: {e}");
            return 2;
        }
    };
    eprintln!("round-vm run: {} {} {}", vm.handle, vm.addr, mode.as_str());
    let code = on_vm(env, args, &vm, mode, &host_addr, &commit_sha, &tree_sha);
    if let Err(e) = env.pool.release(&vm.handle, env.deps.factory) {
        eprintln!("round-vm run: warning: release of {} failed: {e}", vm.handle);
    }
    code
}

/// Exit 3 (no verdict) when the recorded template holds a different image than the tree
/// needs: refusing here costs seconds, where a VM that cannot find its image costs a boot.
/// A record that describes a template `pve.env` no longer names is not evidence either way.
fn stale_template(env: &RunEnv, tree: &Path, commit: &str) -> Option<i32> {
    let rec = Record::read(&env.cfg.state_dir)?;
    if let Ok(pe) = PveEnv::load(&env.cfg.pve_env_path, &|k| std::env::var(k).ok()) {
        if pe.template_vmid != rec.vmid {
            eprintln!("round-vm run: template.json describes template {}, pve.env names {} — not checking the image tag", rec.vmid, pe.template_vmid);
            return None;
        }
    }
    let tag = match env.host.image_tag(tree, commit) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("round-vm run: cannot compute the tree's image tag ({e}) — the VM will check it");
            return None;
        }
    };
    if rec.holds(&tag) {
        return None;
    }
    eprintln!(
        "round-vm run: TEMPLATE-STALE: template {} holds {}, this tree needs localhost/spira-testenv:{tag} — refusing to boot a VM that would cold-build it; run `round-vm refresh` (it records the new template and recycles the warm pool)",
        rec.vmid, rec.image
    );
    Some(3)
}

fn on_vm(env: &RunEnv, args: &RunArgs, vm: &Vm, mode: AcquireMode, host_addr: &str, commit_sha: &str, tree_sha: &str) -> i32 {
    let cfg = env.cfg;
    let maxpar = args.maxpar.unwrap_or(cfg.maxpar);
    let reachable = (0..cfg.ssh_tries.max(1)).any(|i| {
        if i > 0 {
            std::thread::sleep(cfg.boot_poll);
        }
        env.remote.reachable(&vm.addr)
    });
    if !reachable {
        eprintln!("round-vm run: VM {} at {} never became reachable over ssh", vm.handle, vm.addr);
        return 2;
    }
    let job = BatchJob {
        host_addr: host_addr.to_string(),
        mirror_port: cfg.mirror_port,
        suites: args.suites.clone(),
        maxpar,
        toolchain: args.toolchain.clone(),
        cache_home: cfg.cache_home.clone(),
        testenv_registry: cfg.testenv_registry.clone(),
        setup_alarm_secs: cfg.setup_alarm_secs,
    };
    let t0 = Instant::now();
    let Some(spool_dir) = args.attr_spool.clone() else {
        let remote_rc = match env.remote.run_batch(&vm.addr, &job) {
            Ok(255) => {
                eprintln!("round-vm run: ssh to {} failed (exit 255)", vm.addr);
                return 2;
            }
            Ok(rc) => rc,
            Err(e) => {
                eprintln!("round-vm run: {e}");
                return 2;
            }
        };
        return after_batch(env, args, vm, mode, commit_sha, tree_sha, maxpar, remote_rc, t0.elapsed().as_secs());
    };

    // DESIGN.md §2.2a: stream results while the corpus runs; serve attribution reruns on this
    // VM until the batcher closes the spool.
    let pid = std::process::id();
    let in_flight = AtomicUsize::new(0);
    let mirror_lock = Mutex::new(());
    let results_dir = args.results_dir.clone().unwrap_or_else(|| cfg.run_dir.join("batch-results"));
    let stream_scratch = cfg.state_dir.join(format!(".pulled-stream.{pid}"));
    std::thread::scope(|sc| {
        let mut server = Server {
            spool: Spool { dir: spool_dir },
            host: env.host,
            remote: env.remote,
            tree_dir: &args.tree_dir,
            addr: &vm.addr,
            host_addr,
            mirror_port: cfg.mirror_port,
            toolchain: args.toolchain.clone(),
            scratch: cfg.state_dir.join(format!(".spool.{pid}")),
            taken: BTreeSet::new(),
            in_flight: &in_flight,
            mirror_lock: &mirror_lock,
        };
        let batch = sc.spawn(|| env.remote.run_batch(&vm.addr, &job));
        let mut last: Option<Instant> = None;
        while !batch.is_finished() {
            if last.map_or(true, |l| l.elapsed() >= cfg.stream_every) {
                if env.remote.pull(&vm.addr, REMOTE_RESULTS, &stream_scratch).is_ok() {
                    if let Err(e) = stream_into(&stream_scratch, &results_dir) {
                        eprintln!("round-vm run: streaming results: {e}");
                    }
                }
                last = Some(Instant::now());
            }
            server.serve(sc, false);
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = fs::remove_dir_all(&stream_scratch);
        let (code, usable) = match batch.join() {
            Ok(Ok(255)) => {
                eprintln!("round-vm run: ssh to {} failed (exit 255)", vm.addr);
                (2, false)
            }
            Ok(Ok(rc)) => (after_batch(env, args, vm, mode, commit_sha, tree_sha, maxpar, rc, t0.elapsed().as_secs()), true),
            Ok(Err(e)) => {
                eprintln!("round-vm run: {e}");
                (2, false)
            }
            Err(_) => {
                eprintln!("round-vm run: the batch thread panicked");
                (2, false)
            }
        };
        if let Err(e) = server.spool.corpus_done(code) {
            eprintln!("round-vm run: {e}");
        }
        if usable {
            linger(&mut server, sc, cfg.attr_linger, Duration::from_millis(200));
        }
        code
    })
}

/// `run`'s exit code for a non-zero remote exit (DESIGN.md §2.2): testenv's own contract
/// (1 red, 2/3 its harness faults, 4 workspace build) passes through; anything else — cargo's
/// 101, git's 128, a signal — never reached a testenv verdict and is round-vm's own fault, 2,
/// named on stderr (sp-dp872).
pub fn exit_for_remote(rc: i32) -> i32 {
    match rc {
        1..=4 => rc,
        _ => {
            eprintln!("round-vm run: the remote runner exited {rc}, outside testenv's contract (0-4) — no verdict; see its output above");
            2
        }
    }
}

/// Where the VM's testenv writes the corpus's results.
pub const REMOTE_RESULTS: &str = "round-work/.runtime/spira/batch-results/";

/// Everything after the remote corpus returned `remote_rc`: pull results, tsd and binaries,
/// write the manifest, install by tree sha. Returns `run`'s exit code.
#[allow(clippy::too_many_arguments)]
fn after_batch(
    env: &RunEnv,
    args: &RunArgs,
    vm: &Vm,
    mode: AcquireMode,
    commit_sha: &str,
    tree_sha: &str,
    maxpar: u32,
    remote_rc: i32,
    wall: u64,
) -> i32 {
    let cfg = env.cfg;
    let pid = std::process::id();
    let scratch = |name: &str| cfg.state_dir.join(format!(".pulled-{name}.{pid}"));
    let results_dir = args.results_dir.clone().unwrap_or_else(|| cfg.run_dir.join("batch-results"));
    let mut own_fault = false;

    let pulled = scratch("results");
    let _ = fs::remove_dir_all(&pulled);
    let pulled_ok = env.remote.pull(&vm.addr, REMOTE_RESULTS, &pulled).is_ok();
    let leaf = results_leaf(&pulled);
    if let Some(leaf) = &leaf {
        if let Err(e) = copy_tree(leaf, &results_dir) {
            eprintln!("round-vm run: {e}");
            own_fault = true;
        }
    } else if !pulled_ok || remote_rc == 0 {
        eprintln!("round-vm run: no suite results came back from {}", vm.handle);
        own_fault = true;
    }
    let _ = fs::remove_dir_all(&pulled);
    let suite_sum = suite_wall_sum(&results_dir);
    let build = build_wall(&results_dir);

    let tsd = scratch("tsd");
    let _ = fs::remove_dir_all(&tsd);
    if env.remote.pull(&vm.addr, "round-work/.runtime/spira/tsd/", &tsd).is_ok() {
        if let Err(e) = merge_tsd(&tsd, &cfg.run_dir, &vm.handle, cfg.vcpus, maxpar) {
            eprintln!("round-vm run: tsd merge: {e}");
        }
    }
    let _ = fs::remove_dir_all(&tsd);

    let bins = scratch("bins");
    let _ = fs::remove_dir_all(&bins);
    let _ = env.remote.pull(&vm.addr, REMOTE_BINS, &bins);
    let reported = reported_tree(&results_dir);
    let manifest = Manifest {
        tree_sha: tree_sha.to_string(),
        tree_sha_found: reported.clone(),
        commit_sha: commit_sha.to_string(),
        vm: vm.handle.clone(),
        acquire: mode,
        vcpus: cfg.vcpus,
        maxpar,
        batch_wall_secs: wall,
        build_wall_secs: build,
        suite_wall_secs_sum: suite_sum,
    };
    let mdir = cfg.state_dir.join("manifests");
    let written = fs::create_dir_all(&mdir)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::to_string(&manifest).map_err(|e| e.to_string()))
        .and_then(|j| fs::write(mdir.join(format!("{tree_sha}.json")), j).map_err(|e| e.to_string()));
    if let Err(e) = written {
        eprintln!("round-vm run: manifest: {e}");
        own_fault = true;
    }
    let target = args.tree_dir.join("target").join("release");
    if let Err(e) = install_bins(&bins, tree_sha, reported.as_deref(), &target) {
        eprintln!("{e}");
        own_fault = true;
    }
    let _ = fs::remove_dir_all(&bins);

    if remote_rc != 0 {
        return exit_for_remote(remote_rc);
    }
    if own_fault {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Fields;
    use crate::pool::{Attempt, Spawner};
    use crate::provider::Timing;
    use crate::testutil::{FakeAlarm, FakeProvider, TempDir};
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_args_accept_both_spellings() {
        let r = parse_run_args(&args(&["/t", "--suites", "a,b", "--maxpar=24", "--toolchain", "1.82.0", "--results-dir=/r"])).unwrap();
        assert_eq!(r.tree_dir, PathBuf::from("/t"));
        assert_eq!(r.suites.as_deref(), Some("a,b"));
        assert_eq!(r.maxpar, Some(24));
        assert_eq!(r.toolchain.as_deref(), Some("1.82.0"));
        assert_eq!(r.results_dir, Some(PathBuf::from("/r")));
    }

    #[test]
    fn run_args_errors_name_the_problem() {
        assert_eq!(parse_run_args(&[]).unwrap_err(), RUN_USAGE);
        assert!(parse_run_args(&args(&["/t", "--bogus"])).unwrap_err().contains("unknown option: --bogus"));
        assert!(parse_run_args(&args(&["/t", "--maxpar", "x"])).unwrap_err().contains("--maxpar"));
        assert!(parse_run_args(&args(&["/t", "--suites"])).unwrap_err().contains("needs a value"));
    }

    #[test]
    fn the_remote_script_says_whether_the_template_holds_the_rounds_image() {
        let clone = REMOTE_SCRIPT.find("git clone").unwrap();
        let check = REMOTE_SCRIPT.find("podman image exists").unwrap();
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(clone < check && check < batch, "checked after the clone, before testenv starts");
        assert!(REMOTE_SCRIPT.contains("template image: localhost/spira-testenv:$tag present"));
        assert!(!REMOTE_SCRIPT.contains("this round builds it"), "a round never cold-builds its image");
        assert!(REMOTE_SCRIPT.contains("IMAGE-ABSENT: localhost/spira-testenv:$tag is not on this VM — refusing to cold-build it"));
        let absent = REMOTE_SCRIPT.find("refusing to cold-build it").unwrap();
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(absent < batch, "the refusal precedes the batch");
        assert!(REMOTE_SCRIPT[absent..batch].contains("exit 3"), "an absent image exits 3 before any batch runs");
    }

    #[test]
    fn the_round_builds_once_in_a_clean_tree_with_testenvs_own_flags() {
        assert!(!REMOTE_SCRIPT.contains("ln -sfn"), "a target symlink dirties the tree and dangles in the container");
        assert!(REMOTE_SCRIPT.contains(&format!("cargo build -q --profile release --workspace {}", spira_config::build::one_shot_words("release"))));
    }

    #[test]
    fn conf_gen_is_warmed_once_before_the_workspace_build_and_the_suite_batch() {
        // sp-xjnzl-2: a fresh clone never carries conf.sh's gitignored generated fragments,
        // so every suite's own conf.sh sourcing would otherwise regenerate them independently
        // the moment the batch goes parallel — warming once, here, means none of them do.
        let clone = REMOTE_SCRIPT.find("git clone").unwrap();
        let warm = REMOTE_SCRIPT.find("bash spira/conf-gen.sh").unwrap();
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(clone < warm && warm < build && build < batch, "conf-gen.sh must be warmed after the clone but before either the workspace build or the suite batch");
        assert!(REMOTE_SCRIPT.contains("bash spira/conf-gen.sh >&2 || true"), "non-fatal: conf.sh's own per-suite self-heal is still the fallback if this one warm attempt fails");
    }

    #[test]
    fn the_vm_is_a_launcher_release_path_set_outright_before_any_harness_script() {
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        let stage = REMOTE_SCRIPT.find("release build").unwrap();
        let path = REMOTE_SCRIPT.find("export PATH=\"$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:").unwrap();
        let first_script = REMOTE_SCRIPT.find("testenv container tag").unwrap();
        let tenv = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(build < stage && stage < path && path < first_script && path < tenv);
        assert!(REMOTE_SCRIPT.contains("export SPIRA_RELEASE=\"$HOME/round-releases/$rel_sha\""));
        // A release is not a checkout: testenv run from it names the repo by SPIRA_REPO, or
        // it cannot resolve the base ref (the first proof round: VERDICT FAULT reason=base-ref).
        assert!(REMOTE_SCRIPT.contains("export SPIRA_REPO=\"$HOME/round-work\""));
        assert!(REMOTE_SCRIPT.contains("export SPIRA_REPO=%q"));
        assert!(REMOTE_SCRIPT.contains("\"$SPIRA_RELEASE/bin/spira-config\""), "positive control on what PATH resolves");
        assert!(!REMOTE_SCRIPT.contains(":$PATH"), "PATH is set outright, never appended to");
        assert!(!REMOTE_SCRIPT.contains("cargo run"), "testenv runs from the staged release by name");
        assert!(REMOTE_SCRIPT.contains(LAUNCHER_ENV));
        assert!(REMOTE_SCRIPT.contains("exit 4"), "a failed workspace build is testenv's build-failure code");
    }

    #[test]
    fn the_round_s_build_shares_the_hosts_own_cache_not_a_vm_local_one() {
        // sp-xjnzl: CARGO_HOME, RUSTC_WRAPPER and the webdav endpoint must all be set
        // BEFORE the launcher's first `cargo build` — that build, not just testenv's
        // in-place one, is the one the original fault actually hit.
        let cache_home = REMOTE_SCRIPT.find("export CARGO_HOME=\"${cache_home:-$HOME/.cargo}\"").expect("CARGO_HOME comes from the operator's own env (cache_home), never a hardcoded literal — a literal names one operator's box");
        let wrapper = REMOTE_SCRIPT.find("export RUSTC_WRAPPER=\"$CARGO_HOME/bin/sccache\"").expect("RUSTC_WRAPPER set outright, by absolute path");
        let endpoint = REMOTE_SCRIPT.find("export SCCACHE_WEBDAV_ENDPOINT=\"http://${host_addr}:9431\"").expect("the webdav endpoint reuses the VM's own host_addr, the same address the mirror already uses");
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        assert!(cache_home < build && wrapper < build && endpoint < build, "the cache must be wired up before the first build, not just before testenv's");
        // The VM's PATH must still find cargo/rustc (the pre-existing $HOME/.cargo/bin,
        // rustup's own shims) AND the new CARGO_HOME/bin (where sccache now installs) —
        // neither replaces the other.
        assert!(REMOTE_SCRIPT.contains("$CARGO_HOME/bin:$HOME/.cargo/bin:"));
        assert!(!REMOTE_SCRIPT.contains("/home/"), "no literal home directory — cache_home is an operator-supplied argument, not a hardcoded path");
    }

    #[test]
    fn remote_command_keeps_empty_arguments_in_place() {
        let cmd = remote_command(&BatchJob {
            host_addr: "10.0.0.1".into(),
            mirror_port: 9430,
            suites: None,
            maxpar: 16,
            toolchain: Some("1.82.0".into()),
            cache_home: Some("/opt/spira/cargo".into()),
            testenv_registry: Some("registry.example/spira".into()),
            setup_alarm_secs: 45,
        });
        assert_eq!(cmd, "bash -s -- '10.0.0.1' '9430' '' '16' '1.82.0' '/opt/spira/cargo' 'registry.example/spira' '45'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn remote_script_checks_cargo_before_paying_for_the_clone() {
        let check = REMOTE_SCRIPT.find("command -v cargo").expect("checks for cargo");
        let clone = REMOTE_SCRIPT.find("git clone").expect("clones the mirror");
        assert!(check < clone, "cargo must be checked before the clone, not after it fails");
        let path_export = REMOTE_SCRIPT.find(r#"export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin""#).expect("puts cargo's install dirs on PATH");
        assert!(path_export < check, "PATH must be widened before the check that reads it");
    }

    /// Runs only REMOTE_SCRIPT's guard, with no `cargo` reachable — never reaching the git
    /// clone or the disk state a real round would leave behind.
    #[test]
    fn remote_script_fails_fast_and_named_with_no_cargo_on_path() {
        use std::process::Command;
        // REMOTE_SCRIPT resets PATH to fixed system directories; where one of them carries a
        // cargo (a round VM does), "no cargo on PATH" cannot be arranged here: nothing to judge.
        if ["/usr/local/bin", "/usr/local/sbin", "/usr/sbin", "/usr/bin", "/sbin", "/bin"]
            .iter()
            .any(|d| std::path::Path::new(d).join("cargo").exists())
        {
            return;
        }
        let out = Command::new("bash")
            .arg("-c")
            .arg(REMOTE_SCRIPT)
            .arg("round-vm-test")
            .args(["host", "9430", "", "16", "", "/nonexistent-cargo-home", "", "60"])
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/nonexistent-home")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(127));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("cargo not found"), "{stderr}");
    }

    #[test]
    fn measurement_reads_flat_nested_and_empty_results() {
        let d = TempDir::new();
        let flat = d.path().join("flat");
        fs::create_dir_all(&flat).unwrap();
        fs::write(flat.join("test-a.sh.result"), "ok 1 3 - serial explicit 0\n").unwrap();
        fs::write(flat.join("test-b.sh.result"), "ok 1 4 - serial explicit 0\n").unwrap();
        assert_eq!(results_leaf(&flat), Some(flat.clone()));
        assert_eq!(suite_wall_sum(&flat), 7);
        let nested = d.path().join("nested");
        fs::create_dir_all(nested.join("deadbeef")).unwrap();
        fs::write(nested.join("deadbeef/test-c.sh.result"), "ok 1 5 - serial explicit 0\n").unwrap();
        assert_eq!(results_leaf(&nested), Some(nested.join("deadbeef")));
        let empty = d.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(results_leaf(&empty), None);
        assert_eq!(suite_wall_sum(&empty), 0);
        fs::write(flat.join("runner.meta"), "nproc=8\nmaxpar=16\nbuild_wall_s=42\n").unwrap();
        assert_eq!(build_wall(&flat), Some(42));
        assert_eq!(build_wall(&empty), None);
    }

    fn exe(p: &Path) {
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        testkit::write_exe(p, "x");
    }

    #[test]
    fn binaries_of_another_tree_are_refused_and_nothing_installed() {
        let d = TempDir::new();
        let (pulled, target) = (d.path().join("pulled"), d.path().join("wt/target/release"));
        exe(&pulled.join("fakebin"));
        let e = install_bins(&pulled, "expectedsha", Some("wrongsha"), &target).unwrap_err();
        assert!(e.contains("wrongsha") && e.contains("expectedsha"), "{e}");
        assert!(e.contains("refusing binaries"), "{e}");
        let none = install_bins(&pulled, "expectedsha", None, &target).unwrap_err();
        assert!(none.contains("reported no tested tree") && !none.contains("refusing") && !none.contains("<none"), "{none}");
        assert!(!target.exists());
    }

    #[test]
    fn binaries_of_this_tree_land_where_land_local_reads_them() {
        let d = TempDir::new();
        let (pulled, target) = (d.path().join("pulled"), d.path().join("wt/target/release"));
        exe(&pulled.join("fakebin"));
        fs::write(pulled.join("not-executable"), "x").unwrap();
        install_bins(&pulled, "expectedsha", Some("expectedsha"), &target).unwrap();
        assert!(target.join("fakebin").is_file());
        assert!(!target.join("not-executable").exists());
        install_bins(&d.path().join("absent"), "expectedsha", None, &target).unwrap();
    }

    #[test]
    fn tsd_rows_are_tagged_and_deduplicated() {
        let d = TempDir::new();
        let src = d.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("suite.jsonl"), "{\"suite\":\"a\",\"secs\":3}\nnot json\n{\"suite\":\"b\"}\n").unwrap();
        let run = d.path().join("run");
        assert_eq!(merge_tsd(&src, &run, "123", 16, 24).unwrap(), 2);
        assert_eq!(merge_tsd(&src, &run, "123", 16, 24).unwrap(), 0, "a re-merge adds nothing");
        let rows: Vec<Value> = fs::read_to_string(run.join("tsd/suite.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["ran_on"], "123");
        assert_eq!(rows[0]["vcpus"], 16);
        assert_eq!(rows[0]["maxpar"], 24);
        assert_eq!(rows[0]["secs"], 3);
    }

    // ── run end to end against fakes ────────────────────────────────────────────────

    #[test]
    fn teardown_leaves_no_listener_and_nothing_running_is_success() {
        let d = TempDir::new();
        assert!(stop_mirror(d.path()).is_ok(), "no pidfile");
        let pidfile = d.path().join("git-daemon.pid");
        let st = std::process::Command::new("sh").arg("-c").arg(format!("sleep 300 </dev/null >/dev/null 2>&1 & echo $! > {}", pidfile.display())).status().unwrap();
        assert!(st.success());
        let pid: i32 = fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(pid_alive(pid));
        stop_mirror(d.path()).unwrap();
        assert!(!pid_alive(pid), "the daemon survived teardown");
        assert!(!pidfile.exists());
    }

    #[test]
    fn host_addresses_reads_ip_br_addr() {
        let out = "lo UNKNOWN 127.0.0.1/8 ::1/128\neth0 UP 192.168.15.174/22 fe80::1/64\n";
        let a = host_addresses(out);
        assert!(a.iter().any(|x| x == "192.168.15.174") && a.iter().any(|x| x == "127.0.0.1"));
        assert!(!a.iter().any(|x| x == "192.168.1.56"));
    }

    #[test]
    fn a_real_mirror_daemon_listens_where_told_and_teardown_closes_it() {
        let d = TempDir::new();
        let tree = d.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        for a in [vec!["init", "--quiet"], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "--allow-empty", "-m", "x"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&tree).args(&a).status().unwrap().success());
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let state = d.path().join("state");
        let no_addr = GitHost { state_dir: state.clone(), mirror_port: port, listen: String::new() };
        assert!(no_addr.prepare_mirror(&tree).is_err(), "no address means no daemon, never every interface");
        let host = GitHost { state_dir: state.clone(), mirror_port: port, listen: "127.0.0.1".into() };
        host.prepare_mirror(&tree).unwrap();
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok(), "daemon not listening");
        let old_pid = fs::read_to_string(state.join("git-daemon.pid")).unwrap();
        host.prepare_mirror(&tree).unwrap();
        assert_eq!(old_pid, fs::read_to_string(state.join("git-daemon.pid")).unwrap(), "a daemon on the right address is kept");
        fs::write(state.join("git-daemon.addr"), "192.0.2.1").unwrap();
        host.prepare_mirror(&tree).unwrap();
        assert_ne!(old_pid, fs::read_to_string(state.join("git-daemon.pid")).unwrap(), "a daemon recorded on another address is restarted");
        assert_eq!(fs::read_to_string(state.join("git-daemon.addr")).unwrap(), "127.0.0.1");
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        let foreign = GitHost { state_dir: state.clone(), mirror_port: port, listen: "192.0.2.1".into() };
        let e = foreign.prepare_mirror(&tree).unwrap_err();
        assert!(e.contains("not an address of this host") && e.contains("spira-config set"), "{e}");
        stop_mirror(&state).unwrap();
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "listener survived teardown");
    }

    struct FakeHost;
    impl Host for FakeHost {
        fn is_checkout(&self, tree: &Path) -> bool {
            !tree.ends_with("not-a-repo")
        }
        fn head(&self, _: &Path) -> Result<(String, String), String> {
            Ok(("commitsha".into(), "treesha".into()))
        }
        fn prepare_mirror(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn image_tag(&self, tree: &Path, _: &str) -> Result<String, String> {
            if tree.ends_with("tag-unreadable") { Err("no closure".into()) } else if tree.ends_with("newer-image") { Ok("tagB".into()) } else { Ok("tagA".into()) }
        }
        fn mirror_ref(&self, _: &Path, branch: &str, _: &str) -> Result<(), String> {
            if branch == "missing" { Err("no such branch".into()) } else { Ok(()) }
        }
    }

    fn wait_until(cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(60), "fake remote: condition never held");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A VM whose remote tree is a map of remote path → files it holds.
    struct FakeRemote {
        reachable: bool,
        rc: Result<i32, String>,
        files: BTreeMap<&'static str, Vec<(&'static str, &'static str)>>,
        jobs: Mutex<Vec<String>>,
        /// the corpus runs until the first attribution rerun has started (no sleep to race)
        hold_for_attr: bool,
        /// (job id, whether the batch was still running when the job started)
        attrs: Mutex<Vec<(String, bool)>>,
        batch_running: std::sync::atomic::AtomicBool,
    }
    impl FakeRemote {
        fn green() -> FakeRemote {
            let mut files = BTreeMap::new();
            files.insert("round-work/.runtime/spira/batch-results/", vec![
                ("KEY/test-a.sh.result", "ok 1 30 - p e 0\n"),
                ("KEY/test-b.sh.result", "ok 1 12 - p e 0\n"),
                ("KEY/runner.meta", "build_wall_s=90\n"),
                ("KEY/batch.meta", "key=KEY\ntree=treesha\n"),
            ]);
            files.insert("round-work/.runtime/spira/tsd/", vec![("suite.jsonl", "{\"suite\":\"a\"}\n")]);
            files.insert(REMOTE_BINS, vec![("batcher", "bin")]);
            files.insert("attr-results/j1/", vec![("K2/test-a.sh.result", "ok 1 2 - p e 0\n")]);
            files.insert("attr-results/j2/", vec![("K3/test-a.sh.result", "red 1 2 fp p e 1\n"), ("K3/batch.meta", "tree=t2\n")]);
            FakeRemote {
                reachable: true,
                rc: Ok(0),
                files,
                jobs: Mutex::new(vec![]),
                hold_for_attr: false,
                attrs: Mutex::new(vec![]),
                batch_running: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }
    impl Remote for FakeRemote {
        fn reachable(&self, _: &str) -> bool {
            self.reachable
        }
        fn run_batch(&self, _: &str, job: &BatchJob) -> Result<i32, String> {
            self.jobs.lock().unwrap().push(remote_command(job));
            self.batch_running.store(true, std::sync::atomic::Ordering::SeqCst);
            if self.hold_for_attr {
                wait_until(|| !self.attrs.lock().unwrap().is_empty());
            }
            self.batch_running.store(false, std::sync::atomic::Ordering::SeqCst);
            self.rc.clone()
        }
        fn run_attr(&self, _: &str, job: &crate::spool::AttrJob) -> Result<i32, String> {
            if self.hold_for_attr && job.job == "j1" {
                wait_until(|| self.batch_running.load(std::sync::atomic::Ordering::SeqCst));
            }
            let running = self.batch_running.load(std::sync::atomic::Ordering::SeqCst);
            self.attrs.lock().unwrap().push((job.job.clone(), running));
            Ok(if job.job == "j2" { 1 } else { 0 })
        }
        fn pull(&self, _: &str, remote_path: &str, local: &Path) -> Result<(), String> {
            let files = self.files.get(remote_path).ok_or("no such remote dir")?;
            for (rel, body) in files {
                let p = local.join(rel);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                if remote_path == REMOTE_BINS {
                    // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
                    testkit::write_exe(&p, body);
                } else {
                    fs::write(&p, body).unwrap();
                }
            }
            Ok(())
        }
    }

    struct NoSpawn;
    impl Spawner for NoSpawn {
        fn spawn_next(&self) -> Result<ProcId, String> {
            Err("not in tests".into())
        }
    }

    struct Fixture {
        d: TempDir,
        fp: FakeProvider,
        cfg: Config,
    }

    fn fixture() -> Fixture {
        let d = TempDir::new();
        let run_dir = d.path().join("run");
        let key = d.path().join("host_key");
        fs::write(&key, "k").unwrap();
        // Values passed directly to Config::build (DESIGN.md / round-vm's config migration
        // notes) — this used to be a fake env Config::load read through; now it is the
        // Fields literal the old defaults would have produced, with this fixture's own
        // overrides (host_key/host_addr/cache_home/ssh_tries/boot_poll) spelled out alongside
        // them instead of layered on top by Config::load itself.
        let cfg = Config::build(Fields {
            run: run_dir.to_string_lossy().to_string(),
            spira_home: None,
            pve_env: String::new(),
            ssh_user: "root".to_string(),
            ssh_port: 22,
            state_dir: run_dir.join("round-vm").to_string_lossy().to_string(),
            host_key: key.to_string_lossy().to_string(),
            host_pubkey: format!("{}.pub", key.to_string_lossy()),
            host_addr: Some("192.168.1.10".to_string()),
            cache_home: Some("/opt/spira/cargo".to_string()),
            testenv_registry: None,
            vcpus: 16,
            maxpar: 16,
            max_retries: 0,
            retry_interval_secs: 60,
            mirror_port: 9430,
            setup_alarm_secs: 180,
            acquire_deadline_secs: 3600,
            mailbox: "operator".to_string(),
            net_iface: "ens18".to_string(),
            boot_tries: 60,
            boot_poll_secs: 0,
            ssh_tries: 2,
            stream_every_secs: 10,
            attr_linger_secs: 3600,
        });
        Fixture { d, fp: FakeProvider::new(), cfg }
    }

    fn go(fx: &Fixture, remote: &FakeRemote, a: &RunArgs) -> i32 {
        let pool = Pool { state_dir: fx.cfg.state_dir.clone(), retry_interval: Duration::ZERO, max_retries: 1, wait_poll: Duration::ZERO, acquire_deadline: Duration::ZERO };
        let fp = fx.fp.clone();
        let f = move || {
            Ok(Attempt {
                provider: Box::new(fp.clone()),
                iface: "ens18".into(),
                ssh_user: "root".into(),
                pubkey: "k".into(),
                timing: Timing { boot_tries: 1, poll: Duration::ZERO, gone_tries: 1 },
            })
        };
        let alarm = FakeAlarm::default();
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &NoSpawn };
        run(&RunEnv { cfg: &fx.cfg, pool: &pool, deps: &deps, host: &FakeHost, remote }, a)
    }

    fn tree(fx: &Fixture) -> RunArgs {
        RunArgs { tree_dir: fx.d.path().join("tree"), maxpar: Some(24), ..Default::default() }
    }

    fn record_template(fx: &mut Fixture, vmid: &str, image: &str, pve_names: &str) {
        let ca = fx.d.path().join("ca.pem");
        fs::write(&ca, "x").unwrap();
        let env = fx.d.path().join("pve.env");
        fs::write(&env, format!("PVE_TOKEN_ID=t\nPVE_TOKEN_SECRET=s\nPVE_NODE=n\nPVE_CACERT={}\nPVE_TEMPLATE_VMID={pve_names}\n", ca.display())).unwrap();
        fx.cfg.pve_env_path = env;
        fs::create_dir_all(&fx.cfg.state_dir).unwrap();
        Record { vmid: vmid.into(), image: image.into() }.write(&fx.cfg.state_dir).unwrap();
    }

    fn newer_tree(fx: &Fixture) -> RunArgs {
        RunArgs { tree_dir: fx.d.path().join("newer-image"), maxpar: Some(24), ..Default::default() }
    }

    #[test]
    fn a_run_refuses_when_the_recorded_template_holds_another_image_and_boots_nothing() {
        let mut fx = fixture();
        record_template(&mut fx, "130", "localhost/spira-testenv:tagA", "130");
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &newer_tree(&fx)), 3);
        assert!(remote.jobs.lock().unwrap().is_empty(), "no batch ran");
        assert!(fx.fp.name("100").is_none(), "no VM was acquired");
        assert_eq!(go(&fx, &FakeRemote::green(), &tree(&fx)), 0, "positive control: the matching tree runs");
    }

    #[test]
    fn a_record_for_a_template_pve_env_no_longer_names_is_not_evidence() {
        let mut fx = fixture();
        record_template(&mut fx, "130", "localhost/spira-testenv:tagA", "131");
        assert_eq!(go(&fx, &FakeRemote::green(), &newer_tree(&fx)), 0);
    }

    #[test]
    fn the_remote_script_reports_setup_against_suites_and_alarms_over_the_limit() {
        let setup = REMOTE_SCRIPT.find("setup_secs=$(( $(date +%s) - t_start ))").expect("setup is measured");
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(setup < batch, "setup ends where the suites start");
        assert!(REMOTE_SCRIPT.contains("round-vm: setup ${setup_secs}s, suites ${suites_secs}s"));
        assert!(REMOTE_SCRIPT.contains("SETUP-SLOW"));
        assert!(REMOTE_SCRIPT.contains("[ \"$setup_secs\" -gt \"$setup_alarm\" ]"));
    }

    #[test]
    fn green_run_pulls_everything_writes_the_manifest_installs_and_releases() {
        let fx = fixture();
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 0);
        assert!(fx.fp.live_vms().is_empty(), "released: {:?}", fx.fp.live_vms());
        let results = fx.cfg.run_dir.join("batch-results");
        assert!(results.join("test-a.sh.result").is_file(), "flattened out of the BATCH_KEY dir");
        let m: Manifest = serde_json::from_str(&fs::read_to_string(fx.cfg.state_dir.join("manifests/treesha.json")).unwrap()).unwrap();
        assert_eq!((m.vm.as_str(), m.acquire, m.vcpus, m.maxpar), ("100", AcquireMode::Cold, 16, 24));
        assert_eq!((m.build_wall_secs, m.suite_wall_secs_sum), (Some(90), 42));
        assert_eq!(m.tree_sha_found.as_deref(), Some("treesha"));
        assert_eq!(m.commit_sha, "commitsha");
        // path-ok: a test asserting where round-vm installs a fixture binary in a temp worktree
        assert!(tree(&fx).tree_dir.join("target/release/batcher").is_file(), "installed into the round worktree");
        assert!(fs::read_to_string(fx.cfg.run_dir.join("tsd/suite.jsonl")).unwrap().contains("\"ran_on\":\"100\""));
        assert!(remote.jobs.lock().unwrap()[0].ends_with("'' '24' '' '/opt/spira/cargo' '' '180'"), "{:?}", remote.jobs.lock().unwrap());
    }

    #[test]
    fn the_configured_testenv_registry_is_forwarded_to_the_vm() {
        let mut fx = fixture();
        fx.cfg.testenv_registry = Some("registry.example/spira".into());
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 0);
        assert!(remote.jobs.lock().unwrap()[0].contains("'registry.example/spira'"), "{:?}", remote.jobs.lock().unwrap());
    }

    #[test]
    fn a_red_remote_exit_code_passes_through_and_the_vm_is_released() {
        for rc in [1, 3, 4] {
            let fx = fixture();
            let remote = FakeRemote { rc: Ok(rc), ..FakeRemote::green() };
            assert_eq!(go(&fx, &remote, &tree(&fx)), rc);
            assert!(fx.fp.live_vms().is_empty());
        }
    }

    /// sp-dp872: the first round cut from the release exited 101 — cargo's own exit code
    /// (`cargo run` could not pick a binary once testenv grew `bd-meter`), passed through as if
    /// it were a testenv verdict. The batcher had no row for it. A remote exit outside
    /// testenv's contract (0-4) is round-vm's own harness fault: exit 2, named, VM released —
    /// on the no-ready-VM (cold) acquire path the walkthrough rounds took.
    #[test]
    fn a_remote_exit_outside_testenv_s_contract_is_exit_2_on_the_cold_path() {
        for rc in [101, 128, 137, -1] {
            let fx = fixture();
            let remote = FakeRemote { rc: Ok(rc), files: BTreeMap::new(), ..FakeRemote::green() };
            assert_eq!(go(&fx, &remote, &tree(&fx)), 2, "remote rc {rc}");
            assert!(fx.fp.live_vms().is_empty(), "leaked {:?}", fx.fp.live_vms());
            let m: Manifest = serde_json::from_str(&fs::read_to_string(fx.cfg.state_dir.join("manifests/treesha.json")).unwrap()).unwrap();
            assert_eq!(m.acquire, AcquireMode::Cold, "no VM was ready: the no-ready-VM path");
        }
    }

    #[test]
    fn a_spool_run_whose_remote_exits_outside_the_contract_writes_corpus_done_rc_2() {
        let fx = fixture();
        let remote = FakeRemote { rc: Ok(101), files: BTreeMap::new(), ..FakeRemote::green() };
        let (a, sp) = spool_fixture(&fx);
        assert_eq!(go(&fx, &remote, &a), 2);
        assert_eq!(fs::read_to_string(sp.dir.join("corpus.done")).unwrap(), "rc=2\n");
        assert!(fx.fp.live_vms().is_empty());
    }

    /// sp-dp872: testenv has two binaries (testenv, bd-meter); an unqualified `cargo run -p
    /// testenv` cannot choose and exits 101 before a suite runs.
    #[test]
    fn every_remote_cargo_run_names_the_testenv_binary() {
        for script in [REMOTE_SCRIPT, crate::spool::REMOTE_ATTR_SCRIPT] {
            for line in script.lines().filter(|l| l.contains("cargo run")) {
                assert!(line.contains("--bin testenv"), "unqualified cargo run: {line}");
            }
        }
    }

    #[test]
    fn every_failure_on_the_vm_is_exit_2_and_releases_it() {
        let cases: Vec<FakeRemote> = vec![
            FakeRemote { reachable: false, ..FakeRemote::green() },
            FakeRemote { rc: Ok(255), ..FakeRemote::green() },
            FakeRemote { rc: Err("ssh: spawn failed".into()), ..FakeRemote::green() },
            FakeRemote { files: BTreeMap::new(), ..FakeRemote::green() },
        ];
        for remote in cases {
            let fx = fixture();
            assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
            assert!(fx.fp.live_vms().is_empty(), "leaked {:?}", fx.fp.live_vms());
        }
    }

    #[test]
    fn a_green_run_with_another_tree_s_binaries_is_exit_2_and_installs_nothing() {
        let fx = fixture();
        let mut remote = FakeRemote::green();
        remote.files.insert("round-work/.runtime/spira/batch-results/", vec![
            ("KEY/test-a.sh.result", "ok 1 30 - p e 0\n"),
            ("KEY/batch.meta", "key=KEY\ntree=othersha\n"),
        ]);
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        // path-ok: a test asserting where round-vm installs a fixture binary in a temp worktree
        assert!(!tree(&fx).tree_dir.join("target/release/batcher").exists());
        assert!(fx.fp.live_vms().is_empty());
    }

    #[test]
    fn preflight_errors_exit_2_before_any_vm_is_cloned() {
        let fx = fixture();
        let a = RunArgs { tree_dir: fx.d.path().join("not-a-repo"), ..Default::default() };
        assert_eq!(go(&fx, &FakeRemote::green(), &a), 2);

        let mut fx2 = fixture();
        fx2.cfg.host_addr = None;
        assert_eq!(go(&fx2, &FakeRemote::green(), &tree(&fx2)), 2);

        let mut fx3 = fixture();
        fx3.cfg.host_key = fx3.d.path().join("no-such-key");
        assert_eq!(go(&fx3, &FakeRemote::green(), &tree(&fx3)), 2);

        // sp-xjnzl-2: never an ambient CARGO_HOME fallback — unset is refused, named, before
        // any VM is touched. The exact fault a production sweep hit when its systemd unit
        // (unlike an interactive shell) never set CARGO_HOME at all.
        let mut fx4 = fixture();
        fx4.cfg.cache_home = None;
        assert_eq!(go(&fx4, &FakeRemote::green(), &tree(&fx4)), 2);

        for f in [&fx, &fx2, &fx3, &fx4] {
            assert_eq!(f.fp.count("clone"), 0);
        }
    }

    #[test]
    fn an_acquire_that_gives_up_is_exit_2_and_never_runs_the_batch() {
        let fx = fixture();
        fx.fp.fail(crate::testutil::Step::NextId);
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        assert!(remote.jobs.lock().unwrap().is_empty(), "no local mode, no batch without a VM");
    }

    // ── --attr-spool (DESIGN.md §2.2a) ───────────────────────────────────────────────

    fn spool_fixture(fx: &Fixture) -> (RunArgs, Spool) {
        let dir = fx.d.path().join("spool");
        let req = dir.join("req");
        fs::create_dir_all(&req).unwrap();
        fs::write(req.join("j1.req"), "branch=spira/batcher-attr/r-j1\nsuites=test-a.sh\nbuild=aeon\n").unwrap();
        fs::write(req.join("j2.req"), "branch=spira/batcher-attr/r-j2\nsuites=test-a.sh\nbuild=round\n").unwrap();
        fs::write(req.join("j3.req"), "branch=missing\nsuites=test-a.sh\nbuild=artifacts\n").unwrap();
        fs::write(dir.join("close"), "").unwrap();
        let a = RunArgs { attr_spool: Some(dir.clone()), results_dir: Some(fx.d.path().join("results")), ..tree(fx) };
        (a, Spool { dir })
    }

    #[test]
    fn the_spool_serves_reruns_while_the_corpus_runs_and_a_round_build_after_it() {
        let fx = fixture();
        let remote = FakeRemote { hold_for_attr: true, ..FakeRemote::green() };
        let (a, sp) = spool_fixture(&fx);
        assert_eq!(go(&fx, &remote, &a), 0, "the exit code is the corpus's");
        let attrs = remote.attrs.lock().unwrap().clone();
        assert!(attrs.contains(&("j1".to_string(), true)), "j1 ran while the corpus did: {attrs:?}");
        assert!(attrs.contains(&("j2".to_string(), false)), "the round build waited for the corpus: {attrs:?}");
        let res = sp.res_dir();
        assert_eq!(fs::read_to_string(res.join("j1.done")).unwrap(), "rc=0\n");
        assert!(res.join("j1/test-a.sh.result").is_file(), "flattened out of the key dir");
        assert_eq!(fs::read_to_string(res.join("j2.done")).unwrap(), "rc=1\n");
        assert!(res.join("j2/bins/batcher").is_file(), "a round build brings its binaries back");
        assert_eq!(fs::read_to_string(res.join("j3.done")).unwrap(), "rc=2\n", "a branch that cannot be mirrored is a fault");
        assert_eq!(fs::read_to_string(sp.dir.join("corpus.done")).unwrap(), "rc=0\n");
        assert!(fs::read_to_string(fx.d.path().join("results/test-b.sh.result")).is_ok(), "the corpus's results streamed in");
        assert!(fx.fp.live_vms().is_empty(), "released once the spool closed");
    }

    #[test]
    fn a_spool_run_whose_ssh_fails_writes_corpus_done_and_does_not_linger() {
        let fx = fixture();
        let remote = FakeRemote { rc: Ok(255), ..FakeRemote::green() };
        let (a, sp) = spool_fixture(&fx);
        fs::remove_file(sp.dir.join("close")).unwrap();
        assert_eq!(go(&fx, &remote, &a), 2);
        assert_eq!(fs::read_to_string(sp.dir.join("corpus.done")).unwrap(), "rc=2\n");
        assert!(fx.fp.live_vms().is_empty());
    }

    #[test]
    fn an_unclosed_spool_releases_the_vm_after_the_linger() {
        let mut fx = fixture();
        fx.cfg.attr_linger = Duration::from_millis(300);
        let remote = FakeRemote::green();
        let (a, sp) = spool_fixture(&fx);
        fs::remove_file(sp.dir.join("close")).unwrap();
        let t0 = Instant::now();
        assert_eq!(go(&fx, &remote, &a), 0);
        assert!(t0.elapsed() >= Duration::from_millis(300));
        assert!(fx.fp.live_vms().is_empty());
    }
}
