//! Test scaffolding shared across the workspace. Contract: DESIGN.md.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod broker;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::Write;
use std::ops::Deref;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

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

    /// The piped stdout and stderr, for a test that reads them while the child runs.
    pub fn pipes(&mut self) -> (Option<std::process::ChildStdout>, Option<std::process::ChildStderr>) {
        (self.0.stdout.take(), self.0.stderr.take())
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
    static ENV_READ_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
/// under it. Reentrant: a no-op on a thread already holding an [`EnvGuard`] or an
/// [`EnvReadGuard`].
pub fn env_read() -> EnvReadGuard {
    if ENV_HELD.with(|h| h.get()) || ENV_READ_HELD.with(|h| h.get()) {
        return EnvReadGuard(None);
    }
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ENV_READ_HELD.with(|h| h.set(true));
    EnvReadGuard(Some(lock))
}

/// What [`env_read`] returns. Only the outermost one on a thread holds the lock, so it must
/// outlive the ones nested in it.
#[must_use]
pub struct EnvReadGuard(Option<std::sync::MutexGuard<'static, ()>>);

impl Drop for EnvReadGuard {
    fn drop(&mut self) {
        if self.0.is_some() {
            ENV_READ_HELD.with(|h| h.set(false));
        }
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
/// because nothing builds it for the test, and so is one older than the sources it is built from.
pub fn bin_exe(name: &str) -> PathBuf {
    let dir = manifest_dir();
    if let Err(why) = declared_bin(&dir, name) {
        panic!("{why}");
    }
    built_beside(&dir, &std::env::current_exe().expect("current_exe"), name)
}

fn read_manifest(dir: &Path, name: &str) -> Result<toml::Value, String> {
    let manifest = dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("binary {name}: cannot read {}: {e}", manifest.display()))?;
    text.parse().map_err(|e| format!("binary {name}: {} is not TOML: {e}", manifest.display()))
}

fn package_name(doc: &toml::Value) -> Option<&str> {
    doc.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str())
}

/// Whether the crate at `dir` has a `[[bin]]` or `src/bin` target called `name`.
fn names_bin_target(dir: &Path, doc: &toml::Value, name: &str) -> bool {
    doc.get("bin").and_then(|b| b.as_array()).is_some_and(|b| b.iter().any(|b| b.get("name").and_then(|n| n.as_str()) == Some(name)))
        || std::fs::read_dir(dir.join("src/bin")).into_iter().flatten().flatten().any(|e| {
            let p = e.path();
            p.file_stem().and_then(|s| s.to_str()) == Some(name) && (p.is_dir() || p.extension().is_some_and(|x| x == "rs"))
        })
}

fn declared_bin(manifest_dir: &Path, name: &str) -> Result<(), String> {
    let manifest = manifest_dir.join("Cargo.toml");
    let doc = read_manifest(manifest_dir, name)?;
    let own = package_name(&doc) == Some(name) || names_bin_target(manifest_dir, &doc, name);
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

fn built_beside(manifest_dir: &Path, test_exe: &Path, name: &str) -> PathBuf {
    let bin = bin_beside(test_exe, name);
    assert!(
        bin.is_file(),
        "binary {name} is not built at {}: build it with cargo before running this test",
        bin.display()
    );
    if let Some(why) = staleness(manifest_dir, &bin, name) {
        let test = std::thread::current().name().unwrap_or("<test name>").to_string();
        panic!("{why}, or run the test through `testenv --tests {test} <branch>`");
    }
    bin
}

/// [`stale_why`], judged once per binary per process.
fn staleness(manifest_dir: &Path, bin: &Path, name: &str) -> Option<String> {
    static SEEN: Mutex<BTreeMap<PathBuf, Option<String>>> = Mutex::new(BTreeMap::new());
    let seen = || SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(verdict) = seen().get(bin) {
        return verdict.clone();
    }
    let verdict = stale_why(manifest_dir, bin, name);
    seen().insert(bin.to_path_buf(), verdict.clone());
    verdict
}

/// Why `bin` is older than what it is built from; None when it is not. Reads files only, and
/// panics or refuses where it cannot tell, since a pass would run the binary unchecked.
fn stale_why(manifest_dir: &Path, bin: &Path, name: &str) -> Option<String> {
    let (root, package, dir) = building_member(manifest_dir, name);
    let built = modified(bin).unwrap_or_else(|| panic!("binary {name}: cannot read the build time of {}", bin.display()));
    let crates = build_inputs(&dir, name);
    let newer: Vec<(SystemTime, usize, &PathBuf)> = crates
        .iter()
        .enumerate()
        .flat_map(|(i, c)| c.files.iter().map(move |f| (i, f)))
        .filter_map(|(i, f)| Some((modified(f)?, i, f)))
        .filter(|(changed, ..)| *changed > built)
        .collect();
    if newer.is_empty() {
        return None;
    }
    // cargo does not relink for a file it left out of the binary (a cfg(test) module, a manifest
    // edit), so only what its dep-info lists can make the binary stale.
    let (compiled, unknown) = compiled_inputs(&root, bin, name, &crates);
    let head = |changed: SystemTime, file: &Path| {
        format!("binary {name} at {} was built {} and {} changed after it ({})", bin.display(), utc(built), file.display(), utc(changed))
    };
    if let Some((changed, _, file)) = newer.iter().filter(|(.., f)| compiled.contains(*f)).max() {
        return Some(format!("{}: rebuild it with `cargo build -p {package} --bins`", head(*changed, file)));
    }
    let (changed, _, file) = newer.iter().filter(|(_, i, _)| unknown.contains(i)).max()?;
    let missing: Vec<&str> = unknown.iter().map(|i| crates[*i].package.as_str()).collect();
    let profile = match bin.parent().and_then(Path::file_name).and_then(OsStr::to_str) {
        Some("debug") | None => "dev",
        Some(profile) => profile,
    };
    // A rebuild alone relinks nothing here, so cargo never rewrites the lost dep-info.
    let clean: String = missing.iter().map(|p| format!(" -p {p}")).collect();
    Some(format!(
        "{}, and cargo left no dep-info for {} to say whether it is compiled in, so the binary cannot be judged: \
         run `cargo clean --profile {profile}{clean}`, then `cargo build -p {package} --bins` under that profile",
        head(*changed, file),
        missing.join(", ")
    ))
}

/// The files cargo's dep-info says `bin` is compiled from, and the indices into `crates` of
/// those it says nothing about. cargo writes `<bin>.d` beside the binary only when the build
/// asked for that binary; otherwise rustc's own `deps/<crate>-<hash>.d` holds the binary's
/// sources, and one more for each library it links.
fn compiled_inputs(root: &Path, bin: &Path, name: &str, crates: &[CrateInputs]) -> (BTreeSet<PathBuf>, BTreeSet<usize>) {
    let describes = |inputs: &BTreeSet<PathBuf>, i: usize| crates[i].files.iter().any(|f| inputs.contains(f));
    if let Ok(text) = std::fs::read_to_string(bin.with_extension("d")) {
        let inputs = dep_info_inputs(root, &text);
        if (0..crates.len()).any(|i| describes(&inputs, i)) {
            return (inputs, BTreeSet::new());
        }
    }
    let deps = DepsDir::read(bin);
    let (mut compiled, mut unknown) = (BTreeSet::new(), BTreeSet::new());
    for (i, c) in crates.iter().enumerate() {
        let own = (i == 0).then(|| deps.of_binary(root, bin, name));
        let lib = c.lib.as_deref().map(|lib| deps.of_library(root, lib));
        for inputs in own.into_iter().chain(lib) {
            if !describes(&inputs, i) {
                unknown.insert(i);
            }
            compiled.extend(inputs);
        }
    }
    (compiled, unknown)
}

/// `<profile>/deps` beside a binary.
struct DepsDir {
    dir: PathBuf,
    names: Vec<String>,
}

impl DepsDir {
    fn read(bin: &Path) -> DepsDir {
        let dir = bin.with_file_name("deps");
        let names = std::fs::read_dir(&dir).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
        DepsDir { dir, names }
    }

    /// Each `<krate>-<16 hex>` that has a dep-info file.
    fn units<'a>(&'a self, krate: &'a str) -> impl Iterator<Item = &'a str> {
        self.names.iter().filter_map(move |n| {
            let stem = n.strip_suffix(".d")?;
            let hash = stem.strip_prefix(krate)?.strip_prefix('-')?;
            (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(stem)
        })
    }

    fn dep_info(&self, unit: &str) -> String {
        std::fs::read_to_string(self.dir.join(format!("{unit}.d"))).unwrap_or_default()
    }

    /// The unit `bin` is a hard link to: its own sources, whatever else shares its crate name.
    fn of_binary(&self, root: &Path, bin: &Path, name: &str) -> BTreeSet<PathBuf> {
        let id = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()));
        let uplifted = id(bin);
        let krate = name.replace('-', "_");
        let found = self.units(&krate).find(|unit| id(&self.dir.join(unit)) == uplifted);
        found.map(|unit| dep_info_inputs(root, &self.dep_info(unit))).unwrap_or_default()
    }

    /// Every linkable build of `lib`. A test harness shares the crate name and lists cfg(test)
    /// files, and so does a test-mode check, which writes only `.rmeta`.
    fn of_library(&self, root: &Path, lib: &str) -> BTreeSet<PathBuf> {
        let mut inputs = BTreeSet::new();
        for unit in self.units(lib) {
            let text = self.dep_info(unit);
            let linkable = [format!("lib{unit}.rlib"), format!("lib{unit}.so")];
            let mut targets = text.lines().filter_map(|l| l.split_once(": ")).filter_map(|(target, _)| Path::new(target).file_name()?.to_str());
            if targets.any(|t| linkable.iter().any(|a| a == t)) {
                inputs.extend(dep_info_inputs(root, &text));
            }
        }
        inputs
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn manifest_of(dir: &Path, name: &str) -> toml::Value {
    read_manifest(dir, name).unwrap_or_else(|why| panic!("{why}"))
}

/// The workspace root, and the package name and directory of the member that builds the binary `name`.
fn building_member(manifest_dir: &Path, name: &str) -> (PathBuf, String, PathBuf) {
    let (root, members) = manifest_dir
        .ancestors()
        .filter(|dir| dir.join("Cargo.toml").is_file())
        .find_map(|dir| {
            let members = manifest_of(dir, name).get("workspace")?.get("members")?.as_array()?.clone();
            Some((dir.to_path_buf(), members))
        })
        .unwrap_or_else(|| panic!("binary {name}: no workspace holds {}, so its age cannot be judged", manifest_dir.display()));
    let crates: Vec<(PathBuf, toml::Value)> = members
        .iter()
        .filter_map(|m| m.as_str())
        .map(|m| {
            let dir = root.join(m);
            let doc = manifest_of(&dir, name);
            (dir, doc)
        })
        .collect();
    // A bin target outranks a package of the same name, which may build no binary.
    crates
        .iter()
        .find(|(dir, doc)| names_bin_target(dir, doc, name))
        .or_else(|| crates.iter().find(|(dir, doc)| package_name(doc) == Some(name) && dir.join("src/main.rs").is_file()))
        .and_then(|(dir, doc)| Some((root.clone(), package_name(doc)?.to_string(), normal(dir))))
        .unwrap_or_else(|| panic!("binary {name}: no member of {} builds it, so its age cannot be judged", root.display()))
}

struct CrateInputs {
    package: String,
    /// The crate name of its library target.
    lib: Option<String>,
    files: Vec<PathBuf>,
}

/// `Cargo.toml` and `src/**` of the crate at `dir`, first, and of every crate it reaches through
/// non-dev `path =` dependencies.
fn build_inputs(dir: &Path, name: &str) -> Vec<CrateInputs> {
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    let mut crates = vec![dir.to_path_buf()];
    while let Some(dir) = crates.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        let doc = manifest_of(&dir, name);
        let package = package_name(&doc).unwrap_or_else(|| panic!("binary {name}: {} names no package", dir.join("Cargo.toml").display())).to_string();
        let lib_table = doc.get("lib");
        let lib = (lib_table.is_some() || dir.join("src/lib.rs").is_file())
            .then(|| lib_table.and_then(|l| l.get("name")).and_then(|n| n.as_str()).map_or_else(|| package.replace('-', "_"), str::to_string));
        let mut files = vec![dir.join("Cargo.toml")];
        files_under(&dir.join("src"), &mut files);
        found.push(CrateInputs { package, lib, files });
        let targets = doc.get("target").and_then(|t| t.as_table()).into_iter().flat_map(|t| t.values());
        for scope in std::iter::once(&doc).chain(targets) {
            for kind in ["dependencies", "build-dependencies"] {
                let deps = scope.get(kind).and_then(|d| d.as_table()).into_iter().flat_map(|d| d.values());
                crates.extend(deps.filter_map(|d| d.get("path")?.as_str()).map(|p| normal(&dir.join(p))));
            }
        }
    }
    found
}

fn files_under(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() => files_under(&entry.path(), into),
            Ok(_) => into.push(entry.path()),
            Err(_) => {}
        }
    }
}

/// `path` with `.` and `..` resolved by name, as cargo writes paths in dep-info.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The inputs of a `<target>: <input> <input>` dep-info file; a space in a path is `\ `, and
/// rustc writes a workspace member's paths relative to the workspace `root`.
fn dep_info_inputs(root: &Path, text: &str) -> BTreeSet<PathBuf> {
    let rules = text.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split_once(": "));
    rules
        .map(|(_, inputs)| inputs.replace("\\ ", "\0"))
        .flat_map(|l| l.split_whitespace().map(|p| normal(&root.join(p.replace('\0', " ")))).collect::<Vec<_>>())
        .collect()
}

fn utc(at: SystemTime) -> String {
    let secs = at.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
    // SAFETY: gmtime_r writes only the zeroed tm it is handed.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&secs, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
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

    #[test_id(tlhq2jy)]
    #[test]
    fn env_read_nests_on_one_thread_and_frees_the_lock_when_the_outer_guard_drops() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let wait = Duration::from_secs(5);
        let (nested_tx, nested_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
        // A timed channel, so a self-deadlock fails the test instead of hanging it.
        let reader = std::thread::spawn(move || {
            let outer = env_read();
            let inner = env_read();
            drop(inner);
            // Dropping a nested guard must not forget the outer hold.
            drop(env_read());
            nested_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(outer);
            release_rx.recv().unwrap();
            // The hold is forgotten with its guard: this one takes the lock afresh.
            let again = env_read();
            nested_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(again);
        });
        let editor = || {
            let (tx, rx) = channel();
            let t = std::thread::spawn(move || {
                let _g = env(&[]);
                tx.send(()).unwrap();
            });
            (t, rx)
        };
        for hold in ["nested", "again"] {
            nested_rx.recv_timeout(wait).expect("a nested env_read returns without blocking");
            let (t, got) = editor();
            assert!(got.recv_timeout(Duration::from_millis(100)).is_err(), "{hold}: env() was taken under a held env_read");
            release_tx.send(()).unwrap();
            got.recv_timeout(wait).expect("env() is free once the outer guard drops");
            t.join().unwrap();
            if hold == "nested" {
                release_tx.send(()).unwrap();
            }
        }
        reader.join().unwrap();
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
        let t = built_workspace();
        assert_eq!(built_tool(&t), t.join("target/debug/tool"));
    }

    fn age(path: &Path, secs: u64) {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(at).unwrap();
    }

    /// A workspace whose `tool` binary (package `me`) links the path dependency `dep`, with every
    /// source at time 100 and the binary built at 200.
    fn built_workspace() -> TempDir {
        let t = crate_with("[workspace]\nmembers = [\"me\", \"dep\", \"dev\"]\n", &[]);
        let me = "[package]\nname = \"me\"\n[[bin]]\nname = \"tool\"\npath = \"src/main.rs\"\n\
                  [dependencies]\ndep = { path = \"../dep\" }\n[dev-dependencies]\ndev = { path = \"../dev\" }\n";
        for (rel, body) in [
            ("me/Cargo.toml", me),
            ("me/src/main.rs", ""),
            ("me/src/deep/tests.rs", ""),
            ("dep/Cargo.toml", "[package]\nname = \"dep\"\n"),
            ("dep/src/lib.rs", ""),
            ("dev/Cargo.toml", "[package]\nname = \"dev\"\n"),
            ("dev/src/lib.rs", ""),
            ("target/debug/tool", ""),
        ] {
            let p = t.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            age(&p, if rel.starts_with("target") { 200 } else { 100 });
        }
        t
    }

    fn built_tool(t: &Path) -> PathBuf {
        built_beside(&t.join("me"), &t.join("target/debug/deps/x-1"), "tool")
    }

    fn refusal(t: &Path) -> String {
        let why = std::panic::catch_unwind(|| built_tool(t)).expect_err("a stale binary is refused");
        why.downcast_ref::<String>().expect("a formatted panic").clone()
    }

    fn assert_stale(changed: &str) {
        let t = built_workspace();
        age(&t.join(changed), 300);
        let why = refusal(&t);
        for needle in [
            "binary tool at ".to_string(),
            t.join("target/debug/tool").display().to_string(),
            "was built 1970-01-01T00:03:20Z".to_string(),
            format!("{} changed after it (1970-01-01T00:05:00Z)", t.join(changed).display()),
            "`cargo build -p me --bins`".to_string(),
            "`testenv --tests ".to_string(),
        ] {
            assert!(why.contains(&needle), "{needle:?} missing from {why}");
        }
    }

    #[test_id(t0a25gt)]
    #[test]
    fn a_binary_older_than_its_own_source_is_refused_naming_the_rebuild() {
        assert_stale("me/src/deep/tests.rs");
    }

    #[test_id(tiddxlb)]
    #[test]
    fn a_binary_older_than_a_path_dependencys_source_is_refused() {
        assert_stale("dep/src/lib.rs");
    }

    #[test_id(tjxl71t)]
    #[test]
    fn a_binary_older_than_a_manifest_is_refused_when_cargo_left_no_dep_info() {
        assert_stale("dep/Cargo.toml");
    }

    #[test_id(t8qwhgj)]
    #[test]
    fn a_binary_newer_than_every_file_that_builds_it_is_returned() {
        let t = built_workspace();
        age(&t.join("dev/src/lib.rs"), 300);
        assert_eq!(built_tool(&t), t.join("target/debug/tool"));
    }

    #[test_id(tedffl1)]
    #[test]
    fn dep_info_clears_a_newer_file_cargo_did_not_compile_into_the_binary() {
        let t = built_workspace();
        let dep_info = format!("{0}/target/debug/tool: {0}/me/src/main.rs {0}/dep/src/lib.rs\n", t.display());
        std::fs::write(t.join("target/debug/tool.d"), dep_info).unwrap();
        age(&t.join("me/src/deep/tests.rs"), 300);
        age(&t.join("me/Cargo.toml"), 300);
        assert_eq!(built_tool(&t), t.join("target/debug/tool"));
    }

    #[test_id(tswip0m)]
    #[test]
    fn dep_info_keeps_a_newer_compiled_source_refused() {
        let t = built_workspace();
        let dep_info = format!("{0}/target/debug/tool: {0}/me/src/main.rs {0}/dep/src/lib.rs\n", t.display());
        std::fs::write(t.join("target/debug/tool.d"), dep_info).unwrap();
        age(&t.join("dep/src/lib.rs"), 300);
        assert!(refusal(&t).contains("dep/src/lib.rs"));
    }

    const BIN_HASH: &str = "0123456789abcdef";
    const LIB_HASH: &str = "aaaaaaaaaaaaaaaa";
    const HARNESS_HASH: &str = "bbbbbbbbbbbbbbbb";
    const CHECK_HASH: &str = "cccccccccccccccc";

    /// [`built_workspace`] laid out as a round leaves its target: `tool` hard-linked into
    /// `target/unittest/deps`, no `tool.d` beside it, and rustc's dep-info under `deps/` with
    /// workspace-relative paths, for the binary, the `dep` library and a test harness of each.
    fn round_workspace() -> TempDir {
        let t = built_workspace();
        let deps = t.join("target/unittest/deps");
        std::fs::create_dir_all(&deps).unwrap();
        for rel in ["me/src/only_tests.rs", "dep/src/only_tests.rs"] {
            std::fs::write(t.join(rel), "").unwrap();
            age(&t.join(rel), 100);
        }
        let exe = deps.join(format!("tool-{BIN_HASH}"));
        std::fs::write(&exe, "").unwrap();
        age(&exe, 200);
        std::fs::hard_link(&exe, t.join("target/unittest/tool")).unwrap();
        let info = |stem: String, target: String, inputs: &str| {
            let d = deps.display();
            let phony: String = inputs.split(' ').map(|i| format!("{i}:\n")).collect();
            let text = format!("{d}/{stem}.d: {inputs}\n\n{d}/{target}: {inputs}\n\n{phony}\n# env-dep:X=a: b\n");
            std::fs::write(deps.join(format!("{stem}.d")), text).unwrap();
        };
        info(format!("tool-{BIN_HASH}"), format!("tool-{BIN_HASH}"), "me/src/main.rs");
        info(format!("tool-{HARNESS_HASH}"), format!("tool-{HARNESS_HASH}"), "me/src/main.rs me/src/only_tests.rs");
        info(format!("dep-{LIB_HASH}"), format!("libdep-{LIB_HASH}.rlib"), "dep/src/lib.rs");
        info(format!("dep-{HARNESS_HASH}"), format!("dep-{HARNESS_HASH}"), "dep/src/lib.rs dep/src/only_tests.rs");
        // `cargo check --all-targets` names its test-mode check like a library.
        info(format!("dep-{CHECK_HASH}"), format!("libdep-{CHECK_HASH}.rmeta"), "dep/src/lib.rs dep/src/only_tests.rs");
        t
    }

    fn round_tool(t: &Path) -> PathBuf {
        built_beside(&t.join("me"), &t.join("target/unittest/deps/x-1"), "tool")
    }

    fn round_refusal(t: &Path) -> String {
        let why = std::panic::catch_unwind(|| round_tool(t)).expect_err("the binary is refused");
        why.downcast_ref::<String>().expect("a formatted panic").clone()
    }

    #[test_id(typatts)]
    #[test]
    fn with_no_dep_info_beside_the_binary_a_newer_file_cargo_did_not_compile_into_it_does_not_make_it_stale() {
        let t = round_workspace();
        for rel in ["me/src/only_tests.rs", "dep/src/only_tests.rs", "me/Cargo.toml", "dep/Cargo.toml"] {
            age(&t.join(rel), 300);
        }
        assert_eq!(round_tool(&t), t.join("target/unittest/tool"));
    }

    #[test_id(tokbjkd)]
    #[test]
    fn dep_info_under_deps_keeps_a_newer_file_of_the_binary_or_its_library_refused() {
        for changed in ["me/src/main.rs", "dep/src/lib.rs"] {
            let t = round_workspace();
            age(&t.join(changed), 300);
            age(&t.join("me/src/only_tests.rs"), 400);
            let why = round_refusal(&t);
            let needle = format!("{} changed after it (1970-01-01T00:05:00Z): rebuild it with `cargo build -p me --bins`", t.join(changed).display());
            assert!(why.contains(&needle), "{needle:?} missing from {why}");
        }
    }

    #[test_id(tpwpo6o)]
    #[test]
    fn a_newer_file_of_a_crate_with_no_dep_info_is_refused_as_unjudgeable_naming_a_command_that_clears_it() {
        for (removed, package, changed) in [
            (format!("tool-{BIN_HASH}.d"), "me", "me/src/only_tests.rs"),
            (format!("dep-{LIB_HASH}.d"), "dep", "dep/src/only_tests.rs"),
        ] {
            let t = round_workspace();
            std::fs::remove_file(t.join("target/unittest/deps").join(removed)).unwrap();
            age(&t.join(changed), 300);
            let why = round_refusal(&t);
            for needle in [
                format!("{} changed after it", t.join(changed).display()),
                format!("no dep-info for {package} "),
                "cannot be judged".to_string(),
                format!("`cargo clean --profile unittest -p {package}`, then `cargo build -p me --bins` under that profile"),
            ] {
                assert!(why.contains(&needle), "{needle:?} missing from {why}");
            }
        }
    }

    #[test_id(trmua45)]
    #[test]
    fn a_build_or_target_specific_path_dependency_makes_the_binary_stale() {
        for table in ["build-dependencies", "target.'cfg(unix)'.dependencies", "target.'cfg(unix)'.build-dependencies"] {
            let t = built_workspace();
            let manifest = format!("[package]\nname = \"dep\"\n[{table}]\ndev = {{ path = \"../dev\" }}\n");
            std::fs::write(t.join("dep/Cargo.toml"), manifest).unwrap();
            age(&t.join("dep/Cargo.toml"), 100);
            age(&t.join("dev/src/lib.rs"), 300);
            assert!(refusal(&t).contains("dev/src/lib.rs"), "{table}");
        }
    }

    #[test_id(tuk4uay)]
    #[test]
    fn a_package_named_binary_is_judged_by_the_member_that_has_its_main() {
        let t = built_workspace();
        std::fs::write(t.join("target/debug/dep"), "").unwrap();
        age(&t.join("target/debug/dep"), 200);
        std::fs::write(t.join("dep/src/main.rs"), "").unwrap();
        age(&t.join("dep/src/main.rs"), 300);
        let why = std::panic::catch_unwind(|| built_beside(&t.join("me"), &t.join("target/debug/deps/x-1"), "dep")).unwrap_err();
        let why = why.downcast_ref::<String>().unwrap();
        assert!(why.contains("dep/src/main.rs") && why.contains("`cargo build -p dep --bins`"), "{why}");
    }

    #[test_id(t5m47cn)]
    #[test]
    fn a_bin_target_outranks_an_earlier_package_of_the_same_name() {
        let t = built_workspace();
        std::fs::write(t.join("Cargo.toml"), "[workspace]\nmembers = [\"tool\", \"me\", \"dep\"]\n").unwrap();
        std::fs::create_dir_all(t.join("tool/src")).unwrap();
        std::fs::write(t.join("tool/Cargo.toml"), "[package]\nname = \"tool\"\n").unwrap();
        std::fs::write(t.join("tool/src/main.rs"), "").unwrap();
        age(&t.join("me/src/main.rs"), 300);
        assert!(refusal(&t).contains("`cargo build -p me --bins`"));
    }

    #[test_id(tr1akxa)]
    #[test]
    fn dep_info_from_another_tree_does_not_clear_a_stale_binary() {
        let t = built_workspace();
        std::fs::write(t.join("target/debug/tool.d"), "other/tool: other/me/src/main.rs\n").unwrap();
        age(&t.join("me/src/main.rs"), 300);
        assert!(refusal(&t).contains("me/src/main.rs"));
    }

    #[test_id(td932ft)]
    #[test]
    fn dep_info_paths_may_hold_spaces() {
        let abs = TempDir::new("testkit-dep-info").join("c.rs");
        let inputs = dep_info_inputs(Path::new("w"), &format!("t/a b: s/with\\ space.rs {}\n", abs.display()));
        assert_eq!(inputs, BTreeSet::from([PathBuf::from("w/s/with space.rs"), abs]));
    }

    #[test_id(thx7uke)]
    #[test]
    fn a_binary_is_judged_once_per_process() {
        let t = built_workspace();
        built_tool(&t);
        age(&t.join("me/src/main.rs"), 300);
        built_tool(&t);
    }

    #[test_id(t2qavqi)]
    #[test]
    #[should_panic(expected = "no member of")]
    fn a_binary_no_member_builds_is_refused_not_passed() {
        let t = built_workspace();
        std::fs::write(t.join("target/debug/stray"), "").unwrap();
        built_beside(&t.join("me"), &t.join("target/debug/deps/x-1"), "stray");
    }

    #[test_id(tbi73wh)]
    #[test]
    #[should_panic(expected = "cannot read")]
    fn an_unreadable_path_dependency_is_refused_not_skipped() {
        let t = built_workspace();
        std::fs::write(t.join("Cargo.toml"), "[workspace]\nmembers = [\"me\"]\n").unwrap();
        std::fs::remove_file(t.join("dep/Cargo.toml")).unwrap();
        built_tool(&t);
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
