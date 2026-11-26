//! Test scaffolding shared across the workspace. Contract: DESIGN.md.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod broker;

use std::ffi::OsStr;
use std::io::Write;
use std::ops::Deref;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fresh scratch directory under the system temp dir, removed with everything under it
/// when dropped — including when a failing test unwinds. The only way test code in this
/// workspace gets scratch space (spira-lint `tmp-leak`, sp-qgfdi).
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// `<temp_dir>/<tag>-<pid>-<n>-<nanos>`, created empty, its path canonical. The directory is
    /// claimed with a non-recursive `mkdir` and a taken name is skipped, so two processes that
    /// share a `/tmp` (containers each have their own pids) never share a directory or delete
    /// each other's on drop.
    pub fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir();
        loop {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let d = root.join(format!(
                "{tag}-{}-{}-{nanos}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&d) {
                Ok(()) => return TempDir(d.canonicalize().unwrap_or(d)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("mkdir {}: {e}", d.display()),
            }
        }
    }

    /// The directory. It lives exactly as long as this value.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

/// Runs `git -C dir args` with a fixed identity; panics on failure. Returns trimmed stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().expect("spawn git");
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A repo where `main` declares `f merge=union` and holds one side's edit of `f`, `b` holds
/// the other side's, and the checked-out branch (`stale`) carries no attributes. `b` merges
/// onto `main` clean only when the merge reads attributes from `main`'s tree.
pub fn union_repo(tag: &str) -> TempDir {
    let d = TempDir::new(tag);
    let p = d.path();
    let commit = |file: &str, body: &str| {
        std::fs::write(p.join(file), body).unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", file]);
    };
    git(p, &["init", "-q", "-b", "stale"]);
    commit("keep", "x\n");
    git(p, &["checkout", "-q", "-b", "main"]);
    commit(".gitattributes", "f merge=union\n");
    commit("f", "base\n");
    git(p, &["checkout", "-q", "-b", "a", "main"]);
    commit("f", "base\nfrom a\n");
    git(p, &["checkout", "-q", "-b", "b", "main"]);
    commit("f", "base\nfrom b\n");
    git(p, &["checkout", "-q", "main"]);
    git(p, &["merge", "-q", "--ff-only", "a"]);
    git(p, &["checkout", "-q", "stale"]);
    d
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<OsStr> for TempDir {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A spawned test fixture process, killed when this value is dropped — including when a
/// failing test unwinds out from under it (sp-r70dc).
///
/// A test fixture that stands in for a long-lived process (a fake dwarf, a fake gate —
/// typically `bash -c "exec -a NAME sleep 9999"`, held alive only long enough for one
/// assertion) used to be killed by an explicit `child.kill(); child.wait();` written after
/// the assertion. A failed assertion, a `panic!`, or the test binary itself being killed
/// (gate timeout) skips that line, and the fixture outlives the test as an orphan — found
/// in production as a `gate.sh 9999` process surviving up to 2.8 hours, once even holding
/// a pipe open that failed an unrelated gate run.
///
/// `ChildGuard::spawn` makes the child the leader of its own process group
/// (`process_group(0)`), so `Drop` can kill the WHOLE group, not just the one pid — a
/// fixture that backgrounds a grandchild (`sh -c "sleep 9999 &"`) leaves nothing behind
/// either. Killing is idempotent: calling it after the child has already exited (on its
/// own, or via an explicit `guard.kill()` the test called itself to observe the kill's
/// effect) is a no-op, never a signal to a reused pid.
#[derive(Debug)]
pub struct ChildGuard(std::process::Child);

impl ChildGuard {
    /// Spawn `cmd` as the leader of a fresh process group and hold it in a guard.
    /// Panics if the spawn itself fails — this is test scaffolding, and a fixture that
    /// cannot start must fail the test that needed it, the same contract `write_exe` keeps.
    pub fn spawn(cmd: &mut Command) -> ChildGuard {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let child = cmd.spawn().unwrap_or_else(|e| panic!("spawn {cmd:?}: {e}"));
        ChildGuard(child)
    }

    /// The child's pid (also its process group id, since it leads its own group).
    pub fn id(&self) -> u32 {
        self.0.id()
    }

    /// Kill the process group and wait for the leader to exit. Safe to call more than
    /// once — `Drop` calls it again as a backstop — and safe to call after the child has
    /// already exited on its own.
    pub fn kill(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        // SAFETY: a plain signal(2) by pid. The negative pid form targets every process in
        // the group this child leads (it was spawned with process_group(0)), not just it.
        unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
        let _ = self.0.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Write `body` to `path` as an executable (mode 0755) that a test will then exec,
/// WITHOUT THIS PROCESS EVER HOLDING A WRITE DESCRIPTOR ON IT.
///
/// `fs::write` followed by an exec fails with ETXTBSY whenever another test thread forks
/// while the write descriptor is open. The child carries a copy of it until it execs, and
/// the kernel refuses to exec a file anyone has open for writing. A child process writes
/// the file here instead, so no fork in this process can inherit a descriptor on it.
pub fn write_exe(path: impl AsRef<Path>, body: &str) {
    let path = path.as_ref();
    // /bin/sh by absolute path, and /bin/cat INSIDE the script also by absolute path: another
    // test in the same binary may have set the process PATH to a directory with neither in it
    // (release/src/tests.rs does, under its own ENV_LOCK) — sp-e7fe2 fixed the outer `sh` but
    // left the inner `cat` a bare word still resolved through that same mutated PATH, so the
    // race it was meant to remove kept firing, just one level deeper ("sh: 1: cat: not found",
    // sp-tuupa).
    let mut c = Command::new(system_paths::BIN_SH)
        .arg("-c")
        .arg(format!("{}/cat > \"$1\"", system_paths::BIN))
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("sh to write an executable");
    c.stdin
        .take()
        .expect("its stdin")
        .write_all(body.as_bytes())
        .expect("the executable's body");
    assert!(c.wait().expect("the writer").success(), "could not write {}", path.display());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod 755 {}: {e}", path.display()));
}

/// Run `attempt` again while it fails with ETXTBSY, up to a bounded backoff. For executing a
/// file this process's own code wrote: another thread's fork may hold the write descriptor
/// until its exec, and no rename removes that from the inode.
pub fn retry_busy<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut tries = 0;
    loop {
        match attempt() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 100 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            r => return r,
        }
    }
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    static ENV_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Process environment edits for one test, serialized across every test thread in the
/// binary and undone on drop — including when the test panics. The only way test code in
/// this workspace mutates the environment (spira-lint `env-set-var-leak`).
#[must_use]
pub struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(String, Option<std::ffi::OsString>)>,
}

thread_local! {
    static ENV_EDITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has called [`env`]: the witness that code under test edited no
/// environment, unaffected by other tests' threads.
pub fn env_edits() -> usize {
    ENV_EDITS.with(|c| c.get())
}

/// Hold the environment lock, then set (`Some`) or unset (`None`) each key.
pub fn env(edits: &[(&str, Option<&str>)]) -> EnvGuard {
    ENV_EDITS.with(|c| c.set(c.get() + 1));
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ENV_HELD.with(|h| h.set(true));
    let mut saved = Vec::new();
    for (k, v) in edits {
        saved.push((k.to_string(), std::env::var_os(k)));
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    EnvGuard { _lock: lock, saved }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..).rev() {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        ENV_HELD.with(|h| h.set(false));
    }
}

/// Held by a test that READS the environment without editing it, so no [`env`] edit lands
/// under it. Reentrant: a no-op on a thread already holding an [`EnvGuard`].
pub fn env_read() -> Option<std::sync::MutexGuard<'static, ()>> {
    if ENV_HELD.with(|h| h.get()) {
        None
    } else {
        Some(ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

fn run_time_var(var: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => panic!("{var} is not set: this test reads its crate's tree at run time — run it under cargo test or cargo nextest"),
    }
}

/// The directory of the crate under test, read from `CARGO_MANIFEST_DIR` at run time —
/// never `env!`, which bakes the compiling tree's path into the test binary.
pub fn manifest_dir() -> PathBuf {
    run_time_var("CARGO_MANIFEST_DIR")
}

/// Where the built binary `name` sits relative to a test executable in `<target>/<profile>/deps/`.
fn bin_beside(test_exe: &Path, name: &str) -> PathBuf {
    let profile = test_exe.parent().and_then(Path::parent);
    let profile = profile.unwrap_or_else(|| panic!("the test executable {} is not under <target>/<profile>/deps", test_exe.display()));
    profile.join(name)
}

/// The built binary `name`, beside the running test executable (`<target>/<profile>/<name>`),
/// found from `current_exe` at run time. `CARGO_BIN_EXE_*` is deliberately not read: cargo
/// and nextest differ on it, and there is one way to find the binary.
///
/// A binary that is neither the crate's own nor in its `[package.metadata.spira] test-bins` is refused,
/// because nothing builds it for the test.
pub fn bin_exe(name: &str) -> PathBuf {
    let dir = manifest_dir();
    if let Err(why) = declared_bin(&dir, name) {
        panic!("{why}");
    }
    built_beside(&std::env::current_exe().expect("current_exe"), name)
}

fn declared_bin(manifest_dir: &Path, name: &str) -> Result<(), String> {
    let manifest = manifest_dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("binary {name}: cannot read {}: {e}", manifest.display()))?;
    let doc: toml::Value = text.parse().map_err(|e| format!("binary {name}: {} is not TOML: {e}", manifest.display()))?;
    let own = doc.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) == Some(name)
        || doc.get("bin").and_then(|b| b.as_array()).is_some_and(|b| b.iter().any(|b| b.get("name").and_then(|n| n.as_str()) == Some(name)))
        || std::fs::read_dir(manifest_dir.join("src/bin")).into_iter().flatten().flatten().any(|e| {
            let p = e.path();
            p.file_stem().and_then(|s| s.to_str()) == Some(name) && (p.is_dir() || p.extension().is_some_and(|x| x == "rs"))
        });
    let declared = doc
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spira"))
        .and_then(|s| s.get("test-bins"))
        .and_then(|t| t.as_array())
        .is_some_and(|t| t.iter().any(|t| t.as_str() == Some(name)));
    if own || declared {
        Ok(())
    } else {
        Err(format!(
            "binary {name} is neither built by this crate nor declared: add {name:?} to `test-bins` under `[package.metadata.spira]` in {}",
            manifest.display()
        ))
    }
}

fn built_beside(test_exe: &Path, name: &str) -> PathBuf {
    let bin = bin_beside(test_exe, name);
    assert!(
        bin.is_file(),
        "binary {name} is not built at {}: build it with cargo before running this test",
        bin.display()
    );
    bin
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crate_with(manifest: &str, bins: &[&str]) -> TempDir {
        let t = TempDir::new("testkit-declared-bin");
        std::fs::write(t.path().join("Cargo.toml"), manifest).unwrap();
        for b in bins {
            let p = t.path().join("src/bin").join(b);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "fn main() {}").unwrap();
        }
        t
    }

    #[test_id(te3sqtk)]
    #[test]
    fn an_undeclared_binary_is_refused_naming_the_manifest_line_to_add() {
        let t = crate_with("[package]\nname = \"me\"\n", &[]);
        let why = declared_bin(t.path(), "testenv").unwrap_err();
        assert!(why.contains("testenv") && why.contains("test-bins") && why.contains("Cargo.toml"), "{why}");
    }

    #[test_id(t8k5zck)]
    #[test]
    #[should_panic(expected = "test-bins")]
    fn bin_exe_panics_on_an_undeclared_binary() {
        let t = crate_with("[package]\nname = \"me\"\n", &[]);
        let _g = env(&[("CARGO_MANIFEST_DIR", Some(t.path().to_str().unwrap()))]);
        bin_exe("testenv");
    }

    #[test_id(t9v9osw)]
    #[test]
    fn a_declared_binary_and_the_crates_own_are_accepted() {
        let t = crate_with(
            "[package]\nname = \"me\"\n[package.metadata.spira]\ntest-bins = [\"testenv\"]\n[[bin]]\nname = \"tool\"\npath = \"src/x.rs\"\n",
            &["extra.rs"],
        );
        for ok in ["testenv", "me", "tool", "extra"] {
            assert!(declared_bin(t.path(), ok).is_ok(), "{ok}");
        }
        assert!(declared_bin(t.path(), "other").is_err());
    }

    #[test_id(tc000z5)]
    #[test]
    #[should_panic(expected = "CARGO_MANIFEST_DIR is not set")]
    fn manifest_dir_refuses_when_unset() {
        let _g = env(&[("CARGO_MANIFEST_DIR", None)]);
        manifest_dir();
    }

    #[test_id(tt037tu)]
    #[test]
    fn manifest_dir_reads_the_variable_at_run_time() {
        let _g = env(&[("CARGO_MANIFEST_DIR", Some("x"))]);
        assert_eq!(manifest_dir(), PathBuf::from("x"));
    }

    #[test_id(trgk0ku)]
    #[test]
    fn bin_beside_is_the_profile_directory_above_deps() {
        assert_eq!(
            bin_beside(Path::new("t/debug/deps/x-1a2b"), "a"),
            PathBuf::from("t/debug/a")
        );
    }

    #[test_id(tzm4xdm)]
    #[test]
    #[should_panic(expected = "is not under <target>/<profile>/deps")]
    fn bin_beside_refuses_an_exe_with_no_profile_directory() {
        bin_beside(Path::new("x"), "a");
    }

    #[test_id(t0rq2lc)]
    #[test]
    fn a_binary_beside_the_test_exe_is_found() {
        let t = TempDir::new("testkit-bin");
        std::fs::create_dir_all(t.join("debug/deps")).unwrap();
        std::fs::write(t.join("debug/a"), b"").unwrap();
        assert_eq!(built_beside(&t.join("debug/deps/x-1"), "a"), t.join("debug/a"));
    }

    #[test_id(t3uc07s)]
    #[test]
    #[should_panic(expected = "binary no-such-bin-xyz is not built")]
    fn bin_exe_refuses_a_binary_that_is_not_built() {
        let t = crate_with("[package]\nname = \"me\"\n[package.metadata.spira]\ntest-bins = [\"no-such-bin-xyz\"]\n", &[]);
        let _g = env(&[("CARGO_MANIFEST_DIR", Some(t.path().to_str().unwrap()))]);
        bin_exe("no-such-bin-xyz");
    }

    #[test_id(twudxv2)]
    #[test]
    fn a_temp_dir_is_fresh_distinct_and_gone_after_drop() {
        let a = TempDir::new("testkit-td");
        let b = TempDir::new("testkit-td");
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir() && std::fs::read_dir(&a).unwrap().next().is_none());
        std::fs::create_dir_all(a.join("x/y")).unwrap();
        std::fs::write(a.join("x/y/f"), "z").unwrap();
        let p = a.to_path_buf();
        drop(a);
        assert!(!p.exists(), "{} survived its drop", p.display());
        assert!(b.is_dir());
    }

    #[test_id(ts5207n)]
    #[test]
    fn a_panicking_test_still_removes_its_dir() {
        let (tx, rx) = std::sync::mpsc::channel();
        let r = std::thread::spawn(move || {
            let d = TempDir::new("testkit-panic");
            tx.send(d.to_path_buf()).unwrap();
            panic!("the test fails");
        })
        .join();
        assert!(r.is_err());
        let p = rx.recv().unwrap();
        assert!(!p.exists(), "{} survived an unwinding test", p.display());
    }

}

/// A fixture OTLP/HTTP sink: answers 204 to every push until the pushes stop, and hands back
/// all the request bytes.
pub struct OtlpSink {
    pub addr: String,
    handle: std::thread::JoinHandle<Vec<u8>>,
}

impl OtlpSink {
    pub fn start() -> OtlpSink {
        use std::io::Read;
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind sink");
        l.set_nonblocking(true).expect("nonblocking");
        let addr = l.local_addr().expect("addr").to_string();
        let handle = std::thread::spawn(move || {
            let mut all = Vec::new();
            let mut deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                let Ok((mut conn, _)) = l.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                conn.set_nonblocking(false).ok();
                conn.set_read_timeout(Some(std::time::Duration::from_secs(2))).ok();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(n) = conn.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let len: usize = head.split("content-length: ").nth(1).and_then(|s| s.split("\r\n").next()).and_then(|s| s.parse().ok()).unwrap_or(0);
                    if buf.len() >= i + 4 + len {
                        break;
                    }
                }
                let _ = std::io::Write::write_all(&mut conn, b"HTTP/1.1 204 No Content\r\n\r\n");
                all.extend_from_slice(&buf);
                deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
            }
            all
        });
        OtlpSink { addr, handle }
    }

    /// Every push as text (names and label values are plain in the protobuf); empty when nothing arrived.
    pub fn body(self) -> String {
        String::from_utf8_lossy(&self.handle.join().unwrap_or_default()).into_owned()
    }
}

/// A fixture OTLP/HTTP metrics endpoint: accepts every push, answers 204 and keeps the bodies.
pub mod otlp {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    pub struct Sink {
        pub addr: String,
        got: Arc<Mutex<Vec<u8>>>,
    }

    impl Sink {
        pub fn start() -> Sink {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind the fixture sink");
            let addr = l.local_addr().expect("sink addr").to_string();
            let got = Arc::new(Mutex::new(Vec::new()));
            let keep = Arc::clone(&got);
            std::thread::spawn(move || {
                for c in l.incoming().flatten() {
                    let keep = Arc::clone(&keep);
                    std::thread::spawn(move || serve(c, &keep));
                }
            });
            Sink { addr, got }
        }

        /// Waits up to `within` for every needle to appear in the pushed bodies; panics naming the missing ones.
        pub fn expect(&self, needles: &[&str], within: Duration) -> String {
            let end = Instant::now() + within;
            loop {
                let text = String::from_utf8_lossy(&self.got.lock().expect("sink lock")).into_owned();
                let missing: Vec<&&str> = needles.iter().filter(|n| !text.contains(**n)).collect();
                if missing.is_empty() {
                    return text;
                }
                assert!(Instant::now() < end, "the sink never received {missing:?}; got {text:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn serve(mut c: std::net::TcpStream, keep: &Mutex<Vec<u8>>) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let Ok(n) = c.read(&mut chunk) else { return };
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
            let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
            let len: usize = head.split("content-length: ").nth(1).and_then(|s| s.split("\r\n").next()).and_then(|s| s.parse().ok()).unwrap_or(0);
            if buf.len() >= i + 4 + len {
                keep.lock().expect("sink lock").extend_from_slice(&buf[i + 4..]);
                let _ = c.write_all(b"HTTP/1.1 204 No Content\r\n\r\n");
                return;
            }
        }
    }
}

/// Load allowance for tests that open a real store and whose reads are not under test: the
/// default 100 ms `max_read_ms` ends a read on a loaded host. Tuned on the round corpus at the
/// width the round VM runs (sp-b0w0ox, sp-y6ipza); tests about the read limit set their own.
/// `spira/testlib/complete.toml` carries the same value as `lc_max_read_ms` for every test-started
/// serve; a spira-config test pins the two.
pub const LOADED_HOST_MAX_READ_MS: u64 = 120_000;
