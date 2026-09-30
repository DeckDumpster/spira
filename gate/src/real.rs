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
    "SPIRA_LINT_BIN",
    "SPIRA_TESTENV_BIN",
    "SPIRA_SELECT_BIN",
    "LANDSTATE",
    "PATH",
    "HOME",
];

const CONTEXT: &str = r#"set -uo pipefail
HERE="$1"; RN="$2"; shift 2
. "$HERE/lib.sh" >/dev/null || exit 96
__kv() { printf '%s=%s\0' "$1" "$2"; }
[ -n "$RN" ] || RN="$(spira_home_repo)"
__kv repo_name "$RN"
for __v in "$@"; do __kv "$__v" "${!__v-}"; done
__kv host_cores "$(host_cores)"
if [ -r "${SPIRA_REPO_MAP:-/nonexistent}" ] && __r="$(repo_root "$RN")"; then
    __kv repo_root "$__r"
    __b="$(spira_landref "$__r")" && __kv landref "$__b"
    __kv gate_cmd "$(repo_gate "$RN")"
fi
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
        let mut kv: HashMap<String, String> = HashMap::new();
        for rec in String::from_utf8_lossy(&o.stdout).split('\0') {
            if let Some((k, v)) = rec.split_once('=') {
                kv.insert(k.to_string(), v.to_string());
            }
        }
        let mut take = |k: &str| kv.remove(k);
        let repo_name = take("repo_name").unwrap_or_default();
        let repo_root = take("repo_root").filter(|s| !s.is_empty());
        let landref = take("landref").filter(|s| !s.is_empty());
        let gate_cmd = take("gate_cmd").unwrap_or_default();
        let host_cores = take("host_cores").unwrap_or_else(|| "1".into());
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
        let o = Command::new("bash")
            .arg(skew)
            .arg("foreign")
            .arg(repo)
            .arg(base)
            .arg(branch)
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
        self.lib(
            r#"command -v lc_certify >/dev/null 2>&1 || exit 0; lc_certify "$@""#,
            &[bead, tip, outcome, detail],
        );
    }
    fn harness_hash(&self) -> Option<String> {
        let mut buf = Vec::new();
        for f in ["gate.sh", "exclude.sh", "skew.sh"] {
            if let Ok(mut h) = File::open(self.home.join(f)) {
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
    fn admission_try(&self, dir: &Path, slot: u64) -> bool {
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
            return true;
        }
        false
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
    fn utc_of_formats_like_date() {
        assert_eq!(super::utc_of(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::utc_of(1790700000), "2026-09-29T16:40:00Z");
        assert_eq!(super::utc_of(951782400), "2000-02-29T00:00:00Z");
    }
}
