//! The production [`World`]: git, the helper scripts, lib.sh, flock, the filesystem.

use crate::compose::{self, Changed};
use crate::ports::{Ctx, Merge, World};
use spira_config::GateMode;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
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

/// The variables read after `. lib.sh` (DESIGN.md "Environment it reads").
const VARS: &[&str] = &[
    "SPIRA_REPO_MAP",
    "SPIRA_HOME_REPO",
    "SPIRA_REPO",
    "SPIRA_REPO_DERIVED",
    "SPIRA_RUN",
    "SPIRA_GATE_LOG",
    "SPIRA_VERDICTS",
    "SPIRA_VERDICT_TTL",
    "SPIRA_GATE_TIMEOUT",
    "SPIRA_GATE_LOCK_WAIT",
    "SPIRA_CERTIFY_PAR",
    "SPIRA_GATE_SUITES",
    "SPIRA_GATE_BEAD",
    "SPIRA_GATE_CALLER",
    "SPIRA_GATE_BUDGET",
    "SPIRA_GATE_ALL",
    "SPIRA_CERTIFY_ALWAYS_COVERS",
    "SPIRA_BATCH_MAXPAR",
    "SPIRA_VERDICT_REPEAT_CONSIDERED",
    "SPIRA_TESTENV_SETUP_SHARE",
    "SPIRA_TESTENV_WARM_SLOTS",
    "LANDSTATE",
    "SPIRA_BUILD_CACHE",
    "SPIRA_GATE_TARGET_ROOT",
    "SPIRA_GATE_TARGET_CAP_MIB",
    "SPIRA_GATE_TARGET_MIN_FREE_MIB",
    "SPIRA_GATE_TARGET_MIN_MEM_MIB",
    "SPIRA_RELEASE",
    "SPIRA_PATH",
    "HOME",
];

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
        let mut snap: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for rec in String::from_utf8_lossy(&o.stdout).split('\0') {
            if let Some((k, v)) = rec.split_once('=') {
                snap.insert(k.to_string(), v.to_string());
            }
        }
        let mut take = |k: &str| snap.remove(k);
        let raw_repo_name = take("repo_name").unwrap_or_default();
        let host_cores = take("host_cores").unwrap_or_else(|| "1".into());

        // spira_config::repos (sp-37rmg/sp-o88bx, in-process here since sp-k6lku, "wave
        // 4.13"): repo_root/spira_landref/repo_gate/spira_home_repo no longer a bash seam
        // call — the CONTEXT script above only sources lib.sh now for VARS and host_cores.
        let map_text = snap.get("SPIRA_REPO_MAP").filter(|p| !p.is_empty()).and_then(|p| fs::read_to_string(p).ok());
        let reg = spira_config::repos::Registry::new(map_text.as_deref(), &snap, &self.home);
        let repo_name = if raw_repo_name.is_empty() { reg.home_repo().to_string() } else { raw_repo_name };
        let (repo_root, landref, gate_cmd) = match reg.root(&repo_name) {
            Some(root) => {
                let landref = spira_config::repos::landref(&reg, &root);
                (Some(root), landref, reg.gate(&repo_name).unwrap_or_default())
            }
            None => (None, None, String::new()),
        };

        let kv: HashMap<String, String> = snap.into_iter().collect();
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
        // handler; every descriptor this process opened is close-on-exec (DESIGN.md).
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
        let src = tree.join("target").join("aeon").join(p);
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
}
