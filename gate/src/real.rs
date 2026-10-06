//! The production [`World`]: git, the helper scripts, lib.sh, flock, the filesystem.

use crate::compose::{self, Changed};
use crate::ports::{Ctx, Merge, World};
use spira_config::GateMode;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// TERM/INT/HUP set a flag the trial polls, so a killed gate still meters and cleans up.
pub fn install_signal_handlers() {
    for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe {
            libc::signal(
                s,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
}

/// Close every fd ≥ 3 in the child before it execs (same primitive as testenv's own
/// `detach`, testenv/src/testdb.rs), so a descendant this command starts that outlives it
/// — podman's conmon, a cargo build's auto-started sccache server — never inherits a
/// caller's lock fd this process did not open itself and so could not mark CLOEXEC
/// (sp-ohwg7). `close_range` is the fast path; a kernel too old for it (< 5.9) falls back
/// to `fcntl(F_SETFD)` per fd. Only async-signal-safe calls run between fork and exec.
fn close_inherited_fds(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
            let r = libc::syscall(libc::SYS_close_range, 3u32, libc::c_uint::MAX, CLOSE_RANGE_CLOEXEC);
            if r != 0 {
                for fd in 3..4096 {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
            }
            Ok(())
        });
    }
}

/// The variables read after `. lib.sh` (DESIGN.md "Environment it reads"). Every name here
/// has NO `spira/conf.d/<KEY>` entry — a per-invocation value the gate's own caller sets
/// (SPIRA_GATE_BEAD, SPIRA_GATE_CALLER, ...) or an
/// ambient launcher var (SPIRA_RELEASE, HOME) — so these stay read straight out of the
/// `CONTEXT` bash subprocess's own environment.
///
/// UNTIL WAVE 4.8 ("retire conf re-import seams in Rust") this list also carried every
/// `conf.d`-registry-backed name (SPIRA_REPO_MAP, SPIRA_RUN, SPIRA_VERDICT_TTL,
/// SPIRA_GATE_TIMEOUT, SPIRA_GATE_LOCK_WAIT, SPIRA_CERTIFY_PAR, SPIRA_GATE_SUITES,
/// SPIRA_GATE_BUDGET, SPIRA_CERTIFY_ALWAYS_COVERS, SPIRA_BATCH_MAXPAR, SPIRA_PATH — kept as
/// [`RETIRED_VARS`] for a parity diff): `Real::context` read each one back out of the bash
/// process that had just sourced `lib.sh`/conf.sh, a second, bash-shaped derivation of
/// values `spira_config::resolve()` already computes in-process. `. "$HERE/lib.sh"` still
/// runs (the fixed VARS list below and `host_cores` are all this script still echoes back),
/// but the retired names are no longer echoed through the NUL-framed dump;
/// `merge_resolved_config` (below) computes them afterward and merges them into the same
/// `kv` map. `repo_root`/`spira_landref`/`repo_gate`/`spira_home_repo` are ALSO no longer
/// this script's job (sp-k6lku, "wave 4.13", family U/W): `Real::context` resolves them
/// in-process through `spira_config::repos::Registry::from_env`, built from this same
/// `kv`/`merge_resolved_config` snapshot.
const VARS: &[&str] = &[
    "SPIRA_GATE_LOG",
    "SPIRA_VERDICTS",
    "SPIRA_GATE_BEAD",
    "SPIRA_GATE_CALLER",
    "SPIRA_GATE_ALL",
    "SPIRA_GATE_CLASS",
    "SPIRA_VERDICT_REPEAT_CONSIDERED",
    "SPIRA_TESTENV_SETUP_SHARE",
    "SPIRA_TESTENV_WARM_SLOTS",
    "SPIRA_BUILD_CACHE",
    "SPIRA_GATE_TARGET_ROOT",
    "SPIRA_GATE_TARGET_CAP_MIB",
    "SPIRA_GATE_TARGET_MIN_FREE_MIB",
    "SPIRA_GATE_TARGET_MIN_MEM_MIB",
    // sp-ardq8: the scratch-ledger reservation a gate tree takes; was read by target.rs but never
    // admitted here, so it was a dead knob pinned at the 4096 MiB default.
    "SPIRA_GATE_TARGET_RESERVE_MIB",
    // sp-s8v5r: the shared floor testenv's warm-slot shedding also reads — same tmpfs.
    "SPIRA_TMPFS_SHED_FREE_MIB",
    "SPIRA_RELEASE",
    "HOME",
];

/// Every `VARS` name wave 4.8 retired from the bash dump, resolved in-process instead
/// (`merge_resolved_config`) — kept as its own list so a parity check can diff this
/// crate's old and new answers key by key.
pub const RETIRED_VARS: &[&str] = &[
    "SPIRA_REPO_MAP",
    "SPIRA_RUN",
    "SPIRA_VERDICT_TTL",
    "SPIRA_GATE_TIMEOUT",
    "SPIRA_GATE_LOCK_WAIT",
    "SPIRA_CERTIFY_PAR",
    "SPIRA_GATE_SUITES",
    "SPIRA_GATE_BUDGET",
    "SPIRA_GATE_DEADLINE",
    "SPIRA_CERTIFY_ALWAYS_COVERS",
    "SPIRA_BATCH_MAXPAR",
    "SPIRA_PATH",
    // sp-xtdqi: the shared compilation cache's own address — a config-file-only setting
    // (no default) must reach the gate's build the same way `SPIRA_RUN` etc. do, never only
    // when an operator's shell happened to export it first.
    "SPIRA_SCCACHE_DAV_ADDR",
];

/// ONE SOURCE (per Ryan 2026-10-05): `spira_config::process::cfg` — the same per-process
/// `$SPIRA_TOML` resolution every other binary now goes through, in place of this crate's own
/// `resolve_for_process`/`derive_home_repo` call (which duplicated `cfg`'s resolution without
/// its cache). Every [`RETIRED_VARS`] name is a registered key; a key that does not resolve is
/// a refusal naming it — never a value `kv` quietly lacks, and never a reason for an `ctx.var_or`
/// fallback deeper in `engine.rs` to fire. Plain `insert`, not `entry().or_insert()`: `VARS`
/// and `RETIRED_VARS` are disjoint lists, so there is nothing here to avoid overriding.
fn merge_resolved_config(kv: &mut HashMap<String, String>) -> Result<(), String> {
    for name in RETIRED_VARS {
        let v = spira_config::process::cfg(name)?;
        kv.insert((*name).to_string(), v);
    }
    Ok(())
}

const CONTEXT: &str = r#"set -uo pipefail
HERE="$1"; RN="$2"; shift 2
. "$HERE/lib.sh" >/dev/null || exit 96
__kv() { printf '%s=%s\0' "$1" "$2"; }
__kv repo_name "$RN"
for __v in "$@"; do __kv "$__v" "${!__v-}"; done
__kv host_cores "$(host_cores)"
exit 0
"#;

pub struct Real {
    pub home: PathBuf,
    admission: RefCell<Vec<File>>,
    tree_lock: RefCell<Option<File>>,
}

impl Real {
    pub fn new(home: PathBuf) -> Real {
        Real {
            home,
            admission: RefCell::new(Vec::new()),
            tree_lock: RefCell::new(None),
        }
    }

    fn git(&self, repo: &Path) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(repo).stdin(Stdio::null());
        c
    }

    /// Ok(stdout) on 0, Err((status, stdout+stderr)) otherwise.
    fn out(c: &mut Command) -> Result<String, (i32, String)> {
        match c.output() {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err((
                o.status.code().unwrap_or(-1),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                ),
            )),
            Err(e) => Err((-1, e.to_string())),
        }
    }

    /// `bash -c '. lib.sh >/dev/null 2>&1; <body>' <args…>`.
    fn lib(&self, body: &str, args: &[&str]) {
        let script = format!(". \"$0\" >/dev/null 2>&1 || exit 97\n{body}");
        let _ = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(self.home.join("lib.sh"))
            .args(args)
            // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
            // release's bin/+spira/ on the CHILD's PATH, never only inherited.
            .envs(spira_config::release_env::child_path_env_for_process())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn flock_nb(f: &File) -> bool {
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }
}

fn trim_nl(s: String) -> String {
    s.trim_end_matches('\n').to_string()
}

impl World for Real {
    fn context(&self, repo_name: Option<&str>) -> Result<Ctx, String> {
        let o = Command::new("bash")
            .arg("-c")
            .arg(CONTEXT)
            .arg("gate-context")
            .arg(&self.home)
            .arg(repo_name.unwrap_or(""))
            .args(VARS)
            // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
            // release's bin/+spira/ on the CHILD's PATH, never only inherited.
            .envs(spira_config::release_env::child_path_env_for_process())
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("bash: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "the lib.sh context exited {}",
                o.status.code().unwrap_or(-1)
            ));
        }
        let mut kv: HashMap<String, String> = HashMap::new();
        for rec in String::from_utf8_lossy(&o.stdout).split('\0') {
            if let Some((k, v)) = rec.split_once('=') {
                kv.insert(k.to_string(), v.to_string());
            }
        }
        merge_resolved_config(&mut kv)?;
        let mut take = |k: &str| kv.remove(k);
        let raw_repo_name = take("repo_name").unwrap_or_default();
        let host_cores = take("host_cores").unwrap_or_else(|| "1".into());

        // spira_config::repos (sp-37rmg/sp-o88bx, in-process here since sp-k6lku, "wave
        // 4.13"): repo_root/spira_landref/repo_gate/spira_home_repo no longer a bash seam
        // call — the CONTEXT script above only sources lib.sh now for VARS and host_cores.
        // Registry::from_env (sp-k6lku, following the Concierge's structural directive)
        // is the one door onto a registry built from a bare env map — `kv` already carries
        // SPIRA_REPO_MAP if `merge_resolved_config` just resolved it, and `from_env` fills
        // whatever it did not.
        let snap: std::collections::BTreeMap<String, String> = kv.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let reg = spira_config::repos::Registry::from_env(snap, &self.home);
        let repo_name = if raw_repo_name.is_empty() { reg.home_repo().to_string() } else { raw_repo_name };
        let (repo_root, landref, gate_cmd) = match reg.root(&repo_name) {
            Some(root) => {
                let landref = spira_config::repos::landref(&reg, &root);
                (Some(root), landref, reg.gate(&repo_name).unwrap_or_default())
            }
            None => (None, None, String::new()),
        };

        Ok(Ctx {
            repo_name,
            vars: kv,
            repo_root,
            landref,
            gate_cmd,
            host_cores,
        })
    }

    fn readable(&self, p: &Path) -> bool {
        File::open(p).is_ok()
    }
    fn which(&self, name: &str) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
    }
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
    fn read(&self, p: &Path) -> Option<String> {
        fs::read(p)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }
    fn mkdir_p(&self, p: &Path) {
        let _ = fs::create_dir_all(p);
    }
    fn write_atomic(&self, dir: &Path, name: &str, content: &str) {
        let tmp = dir.join(format!(".{name}.{}", std::process::id()));
        if fs::write(&tmp, content).is_ok() && fs::rename(&tmp, dir.join(name)).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
    fn append(&self, p: &Path, line: &str) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
            let _ = f.write_all(line.as_bytes());
        }
    }
    fn remove(&self, p: &Path) {
        let _ = fs::remove_file(p);
    }
    fn temp_file(&self, content: &str) -> Option<PathBuf> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let p =
            std::env::temp_dir().join(format!("spira-gate-files-{}-{nanos}", std::process::id()));
        fs::write(&p, content).ok().map(|_| p)
    }

    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String> {
        Self::out(self.git(repo).args(["rev-parse", "--verify", "-q", rev]))
            .ok()
            .map(trim_nl)
            .filter(|s| !s.is_empty())
    }
    fn diff_names(&self, repo: &Path, range: &str) -> Result<String, String> {
        Self::out(self.git(repo).args(["diff", "--name-only", range]))
            .map(trim_nl)
            .map_err(|(_, e)| trim_nl(e))
    }
    fn diff_name_status(&self, repo: &Path, range: &str) -> String {
        Self::out(self.git(repo).args(["diff", "--name-status", range]))
            .map(trim_nl)
            .unwrap_or_default()
    }
    fn diff_raw(&self, repo: &Path, base: &str, rev: &str) -> Result<Vec<Changed>, String> {
        let o = self
            .git(repo)
            .args([
                "diff",
                "--raw",
                "-z",
                "--no-renames",
                "--no-abbrev",
                base,
                rev,
            ])
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).into_owned());
        }
        Ok(compose::parse_diff_raw(&o.stdout))
    }
    fn gate_mode(&self, repo_name: &str) -> Result<GateMode, String> {
        // law-config-through-the-cli-only: the library finds and validates the document.
        match spira_config::discover(None) {
            None => Ok(GateMode::Suites),
            Some(p) => {
                spira_config::load(&p).map(|doc| spira_config::repo_gate_mode(&doc, repo_name))
            }
        }
    }
    fn certify_par_live(&self) -> Option<u64> {
        let p = spira_config::discover(None)?;
        let doc = spira_config::load(&p).ok()?;
        doc.spira.as_ref()?.certify_par.map(u64::from)
    }
    fn cargo_metadata(&self, tree: &Path, path: &str, home: &str) -> Result<String, String> {
        let o = Command::new("cargo")
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--offline",
            ])
            .current_dir(tree)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cargo: {e}"))?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).into_owned());
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }
    fn merge_tree(&self, repo: &Path, base: &str, branch: &str) -> Merge {
        let o = self
            .git(repo)
            .args([
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                base,
                branch,
            ])
            .output();
        let Ok(o) = o else {
            return Merge::Failed("git merge-tree could not run".into());
        };
        let so = String::from_utf8_lossy(&o.stdout).into_owned();
        let mut lines = so.lines();
        let tree = lines.next().unwrap_or("").trim().to_string();
        match o.status.code() {
            Some(0) if !tree.is_empty() => Merge::Clean(tree),
            Some(1) => {
                let mut paths: Vec<String> = Vec::new();
                for l in lines.filter(|l| !l.is_empty()) {
                    if !paths.iter().any(|p| p == l) {
                        paths.push(l.to_string());
                    }
                }
                Merge::Conflict(paths)
            }
            c => Merge::Failed(format!(
                "git merge-tree exited {c:?}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
        }
    }
    fn is_ancestor(&self, repo: &Path, a: &str, b: &str) -> bool {
        self.git(repo)
            .args(["merge-base", "--is-ancestor", a, b])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    fn commit_merge(&self, repo: &Path, tree: &str, base: &str, branch: &str) -> Option<String> {
        let b = self.rev_parse(repo, &format!("{base}^{{commit}}"))?;
        let r = self.rev_parse(repo, &format!("{branch}^{{commit}}"))?;
        let mut c = self.git(repo);
        c.args([
            "commit-tree",
            tree,
            "-p",
            &b,
            "-p",
            &r,
            "-m",
            &format!("spira gate: {branch} merged onto {base}"),
        ]);
        for (k, v) in [
            ("GIT_AUTHOR_NAME", "spira-gate"),
            ("GIT_AUTHOR_EMAIL", "gate@spira.invalid"),
            ("GIT_COMMITTER_NAME", "spira-gate"),
            ("GIT_COMMITTER_EMAIL", "gate@spira.invalid"),
            ("GIT_AUTHOR_DATE", "1000000000 +0000"),
            ("GIT_COMMITTER_DATE", "1000000000 +0000"),
        ] {
            c.env(k, v);
        }
        Self::out(&mut c)
            .ok()
            .map(trim_nl)
            .filter(|s| !s.is_empty())
    }
    fn show_blob(&self, repo: &Path, rev: &str, path: &str) -> Option<Vec<u8>> {
        let o = self
            .git(repo)
            .args(["show", &format!("{rev}:{path}")])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        o.status.success().then_some(o.stdout)
    }
    fn ls_tree_all(&self, repo: &Path, rev: &str) -> String {
        Self::out(self.git(repo).args(["ls-tree", "-r", "--name-only", rev])).unwrap_or_default()
    }
    fn ls_tree_has(&self, repo: &Path, rev: &str, path: &str) -> bool {
        Self::out(self.git(repo).args(["ls-tree", "--name-only", rev, path]))
            .map(|s| s.lines().any(|l| l.contains(path)))
            .unwrap_or(false)
    }

    fn bash_n(&self, content: &[u8]) -> Result<(), String> {
        let p = std::env::temp_dir().join(format!("spira-gate-{}.sh", std::process::id()));
        if fs::write(&p, content).is_err() {
            return Ok(());
        }
        let o = Command::new("bash")
            .arg("-n")
            .arg(&p)
            .stdin(Stdio::null())
            .output();
        let _ = fs::remove_file(&p);
        match o {
            Ok(o) if !o.status.success() => Err(trim_nl(format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ))),
            _ => Ok(()),
        }
    }
    fn exclude_filter(&self, exclude: &Path, names: &str) -> String {
        let Ok(mut child) = Command::new("bash")
            .arg(exclude)
            .arg("filter")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return String::new();
        };
        if let Some(mut si) = child.stdin.take() {
            let n = names.to_string();
            std::thread::spawn(move || {
                let _ = si.write_all(n.as_bytes());
            });
        }
        child
            .wait_with_output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }
    fn commit_cite(&self, script: &Path, repo: &Path, base: &str, branch: &str) -> (i32, String) {
        let o = Command::new("bash")
            .arg(script)
            .arg("land")
            .arg(repo)
            .arg(base)
            .arg(branch)
            .env("SPIRA_HOME", &self.home)
            .stdin(Stdio::null())
            .output();
        match o {
            Ok(o) => (
                o.status.code().unwrap_or(-1),
                trim_nl(format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                )),
            ),
            Err(e) => (3, e.to_string()),
        }
    }
    fn skew_foreign(&self, skew: &Path, repo: &Path, base: &str, branch: &str) -> (i32, String) {
        // `skew` is a compiled binary now (sp-yyk47), executed directly — never wrapped in
        // `bash`, unlike the retired `skew.sh`. skew.sh found lib.sh beside its OWN script
        // ($(dirname "$0")); `skew` does the release-relative equivalent from its own
        // `current_exe()`, which — resolved through a symlinked release `bin/`, as every
        // gate fixture builds one — can canonicalize to a path with no `../spira` sibling
        // at all. SPIRA_HOME is passed explicitly, the same value `--home` gave this
        // process, so `skew` finds lib.sh regardless of how its own binary was reached.
        let o = Command::new(skew)
            .arg("foreign")
            .arg(repo)
            .arg(base)
            .arg(branch)
            .env("SPIRA_HOME", &self.home)
            .stdin(Stdio::null())
            .output();
        match o {
            Ok(o) => (
                o.status.code().unwrap_or(-1),
                trim_nl(format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                )),
            ),
            Err(e) => (3, e.to_string()),
        }
    }
    fn sweep(&self, sweep: &Path, repo: &Path) {
        let _ = Command::new("bash")
            .arg(sweep)
            .arg(repo)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    fn yield_sh(&self, y: &Path, run: &str, args: &[&str]) {
        let _ = Command::new("bash")
            .arg(y)
            .args(args)
            .env("SPIRA_RUN", run)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    fn lc_certify(&self, bead: &str, tip: &str, outcome: &str, detail: &str) {
        // spira-lc's caller verb (lifecycle-cert.sh's lc_certify until sp-arpjt): it reads
        // the switch itself and answers "cannot tell" having touched nothing when it is off.
        let _ = Command::new("spira-lc")
            .args(["certify", bead, tip, outcome, detail, "gate"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    fn harness_hash(&self) -> Option<String> {
        let mut buf = Vec::new();
        for f in ["gate.sh", "exclude.sh"] {
            if let Ok(mut h) = File::open(self.home.join(f)) {
                let _ = h.read_to_end(&mut buf);
            }
        }
        // `skew` is a compiled binary now (sp-yyk47), resolved on PATH like every other
        // release tool (sp-gypjk) rather than found beside gate.sh/exclude.sh in `home`.
        if let Some(skew) = self.which("skew") {
            if let Ok(mut h) = File::open(skew) {
                let _ = h.read_to_end(&mut buf);
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            buf.extend(fs::read(exe).ok()?);
        }
        Some(crate::key::sha256_hex(&buf))
    }

    fn nproc_all(&self) -> u64 {
        let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
        if n > 0 {
            n as u64
        } else {
            4
        }
    }
    fn mem_avail_mib(&self) -> u64 {
        fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|m| {
                m.lines()
                    .find(|l| l.starts_with("MemAvailable"))
                    .and_then(|l| {
                        l.split_whitespace()
                            .nth(1)
                            .and_then(|k| k.parse::<u64>().ok())
                    })
            })
            .map(|kib| kib / 1024)
            .unwrap_or(1600)
    }
    fn admission_try(&self, dir: &Path, slot: u64, who: &str) -> bool {
        let Ok(f) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(dir.join(format!("slot.{slot}.lock")))
        else {
            return false;
        };
        if Self::flock_nb(&f) {
            self.admission.borrow_mut().push(f);
            use spira_config::admission as adm;
            let me = std::process::id();
            let start = adm::Procs::start_of(&adm::RealProcs, me).unwrap_or(0);
            let _ = fs::write(
                dir.join(format!("slot.{slot}.holder")),
                adm::gate_holder_line(me, start, who, adm::now_epoch()),
            );
            return true;
        }
        false
    }
    fn admission_wait_line(&self, run: &str, par: u64) -> String {
        use spira_config::admission as adm;
        let o = adm::gate_occupancy(Path::new(run), par);
        adm::wait_line(adm::Pool::Gate, par, &o.holders, adm::now_epoch())
    }
    fn tree_lock_open(&self, lock: &Path) -> bool {
        match OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(lock)
        {
            Ok(f) => {
                *self.tree_lock.borrow_mut() = Some(f);
                true
            }
            Err(_) => false,
        }
    }
    fn tree_lock_try(&self) -> bool {
        self.tree_lock
            .borrow()
            .as_ref()
            .map(Self::flock_nb)
            .unwrap_or(false)
    }
    fn write_holder(&self, p: &Path) {
        let pgid = unsafe { libc::getpgid(0) };
        let _ = fs::write(p, format!("{} {}\n", std::process::id(), pgid));
    }
    fn checkout(&self, repo: &Path, tree: &Path, rev: &str, want: &str) -> Result<(), String> {
        let add = || {
            self.lib(
                r#"spira_prune_worktrees "$1" >/dev/null 2>&1"#,
                &[&repo.to_string_lossy()],
            );
            self.git(repo)
                .args(["worktree", "add", "-q", "--detach"])
                .arg(tree)
                .arg(rev)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        let quiet = |c: &mut Command| {
            c.stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !tree.join(".git").exists() {
            if !add() {
                return Err(format!(
                    "gate: cannot create a gate worktree at {}",
                    tree.display()
                ));
            }
        } else {
            if !quiet(
                self.git(tree)
                    .args(["checkout", "-q", "--force", "--detach", rev]),
            ) {
                eprintln!("gate: reuse checkout failed; recreating worktree");
                quiet(
                    self.git(repo)
                        .args(["worktree", "remove", "--force"])
                        .arg(tree),
                );
                if !add() {
                    return Err(format!(
                        "gate: cannot create a gate worktree at {}",
                        tree.display()
                    ));
                }
            }
            quiet(self.git(tree).args(["clean", "-xdff", "-e", "target"]));
            let _ = File::open(tree).and_then(|d| d.set_modified(SystemTime::now()));
        }
        let have = Self::out(self.git(tree).args(["rev-parse", "HEAD"]))
            .map(trim_nl)
            .unwrap_or_default();
        if have == want {
            return Ok(());
        }
        Err(format!(
            "gate: {} is at {}, not {rev} ({want}) — refusing to judge a tree it cannot identify",
            tree.display(),
            if have.is_empty() { "nothing" } else { &have }
        ))
    }
    fn install_tools(&self, tree: &Path, pkgs: &[String], dir: &Path, tree_id: &str) -> Result<(), String> {
        install_tools_at(tree, pkgs, dir, tree_id)
    }
    fn install_tools_from(&self, src: &Path, pkgs: &[String], dir: &Path, tree_id: &str) -> Result<(), String> {
        install_from(src, pkgs, dir, tree_id)?;
        // Recently used: the store's eviction order.
        let _ = fs::File::open(src).and_then(|d| d.set_modified(SystemTime::now()));
        Ok(())
    }
    fn tool_inputs(&self, tree: &Path, pkgs: &[String]) -> Result<Vec<String>, String> {
        tool_inputs_at(tree, pkgs)
    }
    fn tool_entries(&self, store: &Path, base: &str) -> Vec<PathBuf> {
        tool_entries_at(store, base)
    }
    fn publish_tools(&self, from: &Path, pkgs: &[String], store: &Path, name: &str, inputs: &str, keep: usize) -> Result<(), String> {
        publish_tools_at(from, pkgs, store, name, inputs, keep)
    }
    fn build_wrapper(&self, path: &str, setting: &str) -> Result<spira_config::build::Wrapper, String> {
        spira_config::build::wrapper(path, Some(setting))
    }
    fn target_on_tmpfs(&self, tree: &Path, explicit_root: &str, run: &str, lim: &crate::target::Limits) -> Result<String, String> {
        use crate::target;
        let Some(root) = target::root(explicit_root, run, target::on_tmpfs(Path::new("/tmp"))) else {
            return Ok(format!("gate: no tmpfs for the build (/tmp is not one and SPIRA_GATE_TARGET_ROOT is unset) — {} builds in the tree", tree.display()));
        };
        let trees = tree.parent().unwrap_or(Path::new("/"));
        let p = target::prepare(&root, trees, tree, lim, &target::free_mib, &target::mem_available_mib)?;
        let mut line = format!("gate: build on tmpfs at {}", p.dir.display());
        if p.disk_freed_mib > 0 {
            line.push_str(&format!(" ({} MiB of build output moved off the disk)", p.disk_freed_mib));
        }
        if !p.orphans.is_empty() || !p.evicted.is_empty() {
            line.push_str(&format!("; removed {} orphaned and evicted {} least recently used target(s)", p.orphans.len(), p.evicted.len()));
        }
        Ok(line)
    }
    fn release_target(&self, tree: &Path, keep_release: bool) {
        crate::target::release_tree(tree, keep_release);
    }
    fn reserve_scratch(&self, tree: &Path, explicit_root: &str, run: &str, lim: &crate::target::Limits, class: &str, wait_secs: u64) -> Result<Box<dyn std::any::Any>, String> {
        use crate::target;
        let Some(root) = target::root(explicit_root, run, target::on_tmpfs(Path::new("/tmp"))) else {
            return Ok(Box::new(()));
        };
        let owner = format!("gate-{}", tree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let g = spira_config::scratch::reserve_class(
            &spira_config::scratch::ledger_for(&root),
            &owner,
            spira_config::scratch::Class::parse(class),
            lim.reserve_mib,
            0,
            std::time::Duration::from_secs(wait_secs),
            &|| target::free_mib(&root),
        )?;
        Ok(Box::new(g))
    }
    fn remove_worktree(&self, repo: &Path, tree: &Path) {
        let _ = self
            .git(repo)
            .args(["worktree", "remove", "--force"])
            .arg(tree)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn run_gate(
        &self,
        tree: &Path,
        env: &[(String, String)],
        timeout: &str,
        cmd: &str,
    ) -> (i32, String) {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return (
                NOVERDICT_RC,
                "gate: cannot open a pipe for the gate command".into(),
            );
        }
        let (r, wr) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
        let Ok(wr2) = wr.try_clone() else {
            return (NOVERDICT_RC, "gate: cannot duplicate the pipe".into());
        };
        let mut c = Command::new("timeout");
        c.arg(timeout)
            .arg("bash")
            .arg("-c")
            .arg(cmd)
            .current_dir(tree)
            .env_clear();
        for (k, v) in env {
            c.env(k, v);
        }
        c.stdout(Stdio::from(wr)).stderr(Stdio::from(wr2));
        // Caught signals reset to their default at exec, so the trial does not inherit the
        // handler; every descriptor THIS PROCESS opened is close-on-exec (DESIGN.md). That
        // claim does not cover a descriptor this process never opened — one inherited from
        // whatever spawned it (a bash lander's `exec 9>…` lock with no O_CLOEXEC, surviving
        // this binary's own `exec -a` from gate.sh) — and the trial started here can run
        // testenv, which starts podman, whose conmon daemonizes and outlives the trial,
        // holding that fd (and the lock it names) forever (sp-ohwg7). close_inherited_fds
        // drops every fd ≥ 3 right before exec so none of them reach the trial at all.
        close_inherited_fds(&mut c);
        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(e) => return (127, format!("gate: cannot run the gate command: {e}")),
        };
        drop(c);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut r = r;
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
        let mut termed = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s.code().unwrap_or(-1),
                Ok(None) => {}
                Err(_) => break -1,
            }
            if SIGNALLED.load(Ordering::SeqCst) && !termed {
                unsafe {
                    libc::kill(child.id() as i32, libc::SIGTERM);
                }
                termed = true;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        // A leaked grandchild holding the pipe must not hold the verdict hostage forever.
        let buf = rx.recv_timeout(Duration::from_secs(30)).unwrap_or_default();
        (status, trim_nl(String::from_utf8_lossy(&buf).into_owned()))
    }

    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    fn utc(&self) -> String {
        utc_of(self.now())
    }
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
    fn pid(&self) -> u32 {
        std::process::id()
    }
    fn signalled(&self) -> bool {
        SIGNALLED.load(Ordering::SeqCst)
    }
    fn eprint(&self, s: &str) {
        eprintln!("{s}");
    }
}

const NOVERDICT_RC: i32 = crate::engine::NOVERDICT;

/// [`World::install_tools`] on the filesystem (sp-g9f3t). Copies, never links: a hard link
/// into `target/aeon` would change under the next build of a different tree.
pub fn install_tools_at(tree: &Path, pkgs: &[String], dir: &Path, tree_id: &str) -> Result<(), String> {
    install_from(&tree.join("target").join("aeon"), pkgs, dir, tree_id)
}

/// [`install_tools_at`] from any directory of built binaries (`<src>/<pkg>`).
pub fn install_from(src_dir: &Path, pkgs: &[String], dir: &Path, tree_id: &str) -> Result<(), String> {
    let parent = dir
        .parent()
        .ok_or_else(|| format!("gate: {} has no parent directory", dir.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("gate: cannot create {}: {e}", parent.display()))?;
    // Every other keyed directory goes first: the tree is locked for this trial, so nothing
    // else reads them, and none of them may be mistaken for this tree's.
    if let Ok(rd) = fs::read_dir(parent) {
        for e in rd.flatten() {
            if e.path() != dir {
                let p = e.path();
                let _ = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
            }
        }
    }
    let _ = fs::remove_dir_all(dir);
    let tmp = PathBuf::from(format!("{}.tmp", dir.display()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("gate: cannot create {}: {e}", tmp.display()))?;
    for p in pkgs {
        let src = src_dir.join(p);
        fs::copy(&src, tmp.join(p)).map_err(|e| {
            let _ = fs::remove_dir_all(&tmp);
            format!("gate: cannot install {} into {}: {e}", src.display(), tmp.display())
        })?;
    }
    fs::write(tmp.join(crate::def::TOOLS_STAMP), format!("{tree_id}\n"))
        .and_then(|_| fs::rename(&tmp, dir))
        .map_err(|e| {
            let _ = fs::remove_dir_all(&tmp);
            format!("gate: cannot stamp {} for tree {tree_id}: {e}", dir.display())
        })
}

/// [`World::tool_inputs`] on the filesystem.
pub fn tool_inputs_at(tree: &Path, pkgs: &[String]) -> Result<Vec<String>, String> {
    let aeon = tree.join("target").join("aeon");
    let mut out = Vec::new();
    for p in pkgs {
        let d = aeon.join(format!("{p}.d"));
        let text = fs::read_to_string(&d).map_err(|e| format!("cannot read {}: {e}", d.display()))?;
        let deps = crate::toolkey::dep_info_paths(&text);
        if deps.is_empty() {
            return Err(format!("{} names no source", d.display()));
        }
        out.extend(deps);
    }
    if let Ok(rd) = fs::read_dir(aeon.join("build")) {
        for e in rd.flatten() {
            if let Ok(text) = fs::read_to_string(e.path().join("output")) {
                out.extend(crate::toolkey::rerun_paths(&text));
            }
        }
    }
    Ok(out)
}

fn mtime_of(p: &Path) -> SystemTime {
    fs::metadata(p).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH)
}

/// Every entry directory of `store` (no dot-named temporaries), most recently used first.
fn store_entries(store: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(store)
        .map(|rd| {
            rd.flatten()
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_dir())
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|p| std::cmp::Reverse(mtime_of(p)));
    v
}

/// [`World::tool_entries`] on the filesystem.
pub fn tool_entries_at(store: &Path, base: &str) -> Vec<PathBuf> {
    let prefix = format!("{base}-");
    store_entries(store)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)))
        .collect()
}

/// [`World::publish_tools`] on the filesystem.
pub fn publish_tools_at(from: &Path, pkgs: &[String], store: &Path, name: &str, inputs: &str, keep: usize) -> Result<(), String> {
    fs::create_dir_all(store).map_err(|e| format!("cannot create {}: {e}", store.display()))?;
    let dest = store.join(name);
    let tmp = store.join(format!(".tmp.{name}.{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let made = (|| -> std::io::Result<()> {
        fs::create_dir_all(&tmp)?;
        for p in pkgs {
            fs::copy(from.join(p), tmp.join(p))?;
        }
        fs::write(tmp.join(crate::toolkey::INPUTS), inputs)?;
        fs::write(tmp.join(crate::toolkey::KEY), format!("{name}\n"))?;
        Ok(())
    })();
    if let Err(e) = made {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("cannot stage {}: {e}", tmp.display()));
    }
    if let Err(e) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_dir_all(&tmp);
        if !dest.join(crate::toolkey::KEY).is_file() {
            return Err(format!("cannot publish {}: {e}", dest.display()));
        }
    }
    // Keep the most recently used; a temporary older than an hour is a crashed publisher's.
    for old in store_entries(store).into_iter().skip(keep) {
        let _ = fs::remove_dir_all(old);
    }
    if let Ok(rd) = fs::read_dir(store) {
        for e in rd.flatten() {
            let stale = SystemTime::now().duration_since(mtime_of(&e.path())).is_ok_and(|a| a > Duration::from_secs(3600));
            if e.file_name().to_string_lossy().starts_with(".tmp.") && stale {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(())
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` for an epoch.
pub fn utc_of(t: u64) -> String {
    let days = (t / 86400) as i64;
    let rem = t % 86400;
    // civil_from_days (Howard Hinnant).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn install_tools_copies_stamps_and_clears_every_other_tree() {
        use std::fs;
        let t = testkit::TempDir::new("gate-tools");
        let tree = t.path();
        fs::create_dir_all(tree.join("target/aeon")).unwrap();
        fs::write(tree.join("target/aeon/spira-lint"), "built-from-A").unwrap();
        let root = tree.join(crate::def::TOOLS_DIR);
        fs::create_dir_all(root.join("B")).unwrap();
        fs::write(root.join("B/spira-lint"), "built-from-B").unwrap();
        let dir = root.join("A");
        super::install_tools_at(tree, &["spira-lint".into()], &dir, "A").unwrap();
        assert_eq!(fs::read_to_string(dir.join("spira-lint")).unwrap(), "built-from-A");
        assert_eq!(fs::read_to_string(dir.join("TREE")).unwrap(), "A\n");
        assert!(!root.join("B").exists(), "another tree's tools survived");
        // A later build of the tree does not reach into the installed copy.
        fs::write(tree.join("target/aeon/spira-lint"), "built-from-C").unwrap();
        assert_eq!(fs::read_to_string(dir.join("spira-lint")).unwrap(), "built-from-A");
        // A package the build did not produce: Err, and nothing left at the keyed path.
        let e = super::install_tools_at(tree, &["nope".into()], &root.join("D"), "D").unwrap_err();
        assert!(e.contains("cannot install"), "{e}");
        assert!(!root.join("D").exists() && !root.join("D.tmp").exists());
    }

    #[test]
    fn utc_of_formats_like_date() {
        assert_eq!(super::utc_of(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::utc_of(1790700000), "2026-09-29T16:40:00Z");
        assert_eq!(super::utc_of(951782400), "2000-02-29T00:00:00Z");
    }

    // ENV VARS ARE PROCESS-GLOBAL (spira-config's own locate.rs/lib.rs tests guard the same
    // hazard): this test takes a lock and pins SPIRA_TOML to a nonexistent path — locate()'s
    // own exclusive-pin rule — so it never depends on a real operator config document on
    // the machine running this suite.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// ONE SOURCE (per Ryan 2026-10-05): `merge_resolved_config` now goes straight through
    /// `spira_config::process::cfg`, so this is the one test in this binary allowed to drive
    /// it live (rule: only a test that truly exercises the top-level read may set `SPIRA_TOML`
    /// — `cfg`'s resolution is cached once per *process*, in a `OnceLock`, so a second test
    /// pinning a DIFFERENT config file in the same test binary would just see this one's
    /// answer, not its own; the old "missing registry" sibling test that used to live here
    /// is gone for exactly that reason, not because the refusal it checked stopped existing —
    /// that refusal is `spira_config`'s own, and `spira_config`'s own tests are where it
    /// belongs now).
    #[test]
    fn merge_resolved_config_fills_retired_vars_without_overriding_the_bash_dump() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved_toml = std::env::var("SPIRA_TOML").ok();
        let saved_home = std::env::var("SPIRA_HOME").ok();
        let dir = testkit::TempDir::new("gate-real-merge");
        let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_GATE_TIMEOUT", "1234")]);
        // SPIRA_HOME must be the checkout's own spira/ (where conf.d — the key registry —
        // lives), never the throwaway fixture dir.
        let real_home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        std::env::set_var("SPIRA_HOME", &real_home);
        std::env::set_var("SPIRA_TOML", &toml);

        let mut kv = std::collections::HashMap::new();
        kv.insert("SPIRA_GATE_BEAD".to_string(), "sp-xyz".to_string());
        let result = super::merge_resolved_config(&mut kv);

        match saved_toml {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }
        match saved_home {
            Some(v) => std::env::set_var("SPIRA_HOME", v),
            None => std::env::remove_var("SPIRA_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        result.unwrap();
        assert_eq!(kv.get("SPIRA_GATE_BEAD").map(String::as_str), Some("sp-xyz"), "the bash dump's own value must survive the merge");
        assert_eq!(kv.get("SPIRA_GATE_TIMEOUT").map(String::as_str), Some("1234"), "a RETIRED_VARS key cfg() covers must reach kv in-process");
    }

    /// sp-ohwg7: a podman conmon started by a gate trial held an flock the gate's CALLER
    /// had open (a lander's `exec 9>…` lock, opened by bash with no O_CLOEXEC) for 53+
    /// minutes after the caller exited. Reproduced here without podman: `cmd` backgrounds
    /// a `sleep`, the same shape (a long-lived child, started from inside a locked
    /// section, that outlives the trial) — before `close_inherited_fds`, it inherits the
    /// open-but-not-CLOEXEC lock fd by plain fork, and the lock stays held after this test
    /// drops its own reference.
    #[test]
    fn run_gate_never_lets_a_daemon_it_starts_inherit_the_callers_lock_fd() {
        use crate::ports::World;
        use std::ffi::CString;

        let dir = testkit::TempDir::new("gate-run-gate-fd-leak");
        let tree = dir.path();
        let lockfile = dir.join("caller.lock");
        let lock_c = CString::new(lockfile.as_os_str().as_encoded_bytes()).unwrap();

        // Simulate the lander's own `exec 9>lockfile; flock 9`: opened directly via
        // libc::open with no O_CLOEXEC — exactly what bash's redirection does, and
        // exactly what std::fs::File never does (SAFETY: a plain open/flock on a path we
        // own, cleaned up below).
        let lock_fd = unsafe { libc::open(lock_c.as_ptr(), libc::O_WRONLY | libc::O_CREAT, 0o644) };
        assert!(lock_fd >= 0, "open {}: {}", lockfile.display(), std::io::Error::last_os_error());
        assert_eq!(unsafe { libc::flock(lock_fd, libc::LOCK_EX) }, 0, "acquire the caller's lock");

        let real = super::Real::new(std::path::PathBuf::new());
        let env: Vec<(String, String)> = vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())];
        let (rc, out) = real.run_gate(
            tree,
            &env,
            "10",
            "sleep 30 >/dev/null 2>&1 & echo $! > child.pid",
        );
        assert_eq!(rc, 0, "trial command: {out}");

        // The caller exits: drop our own reference to the lock, exactly as the lander's
        // own fd 9 closes when its process exits.
        unsafe { libc::close(lock_fd) };

        // A fresh probe, from a fresh fd: free unless some other open file description —
        // the backgrounded "daemon", if it inherited one — still holds it.
        let probe_fd = unsafe { libc::open(lock_c.as_ptr(), libc::O_WRONLY, 0) };
        assert!(probe_fd >= 0);
        let free = unsafe { libc::flock(probe_fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if free {
            unsafe { libc::flock(probe_fd, libc::LOCK_UN) };
        }
        unsafe { libc::close(probe_fd) };

        // Clean up the daemon regardless of the assertion below.
        if let Ok(s) = std::fs::read_to_string(tree.join("child.pid")) {
            if let Ok(pid) = s.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }

        assert!(free, "the backgrounded child inherited the caller's lock fd and is still holding it");
    }
}
