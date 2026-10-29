//! Suite-level resume: a suite that passed for this exact tree and test environment is not
//! run again when the same job is re-run on another instance. Records go through one
//! [`Backend`] chosen by configuration; testenv knows nothing of any CI system. Any doubt
//! about a record runs the suite, and a pass is written the moment it happens.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use crate::verdict::KeyInputs;

const MAGIC: &str = "testenv-resume-v2";

/// Where records live. `key` is `<repo>/<tree>/<env key>/<suite>`; a lookup that fails for
/// any reason is a miss. `get_all` answers every record under a key prefix in one call.
pub trait Backend: Send + Sync {
    fn get_all(&self, prefix: &str) -> Option<HashMap<String, String>>;
    fn put(&self, key: &str, value: &str);
}

fn read_records(dir: &Path) -> Option<HashMap<String, String>> {
    let mut all = HashMap::new();
    for entry in fs::read_dir(dir).ok()?.flatten() {
        if let (Some(name), Ok(text)) = (entry.file_name().to_str(), fs::read_to_string(entry.path())) {
            all.insert(name.to_string(), text);
        }
    }
    Some(all)
}

pub struct DirBackend {
    root: PathBuf,
}

impl Backend for DirBackend {
    fn get_all(&self, prefix: &str) -> Option<HashMap<String, String>> {
        read_records(&self.root.join(prefix))
    }

    fn put(&self, key: &str, value: &str) {
        let path = self.root.join(key);
        let Some(parent) = path.parent() else { return };
        if let Err(e) = fs::create_dir_all(parent) {
            return report_once(&format!("resume: cannot create {}: {e}", parent.display()));
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let written = fs::write(&tmp, value).and_then(|_| fs::rename(&tmp, &path));
        if let Err(e) = written {
            report_once(&format!("resume: cannot write {}: {e}", path.display()));
        }
    }
}

/// An S3 prefix reached through the `aws` CLI with the machine's own credentials (an
/// instance profile); no key is ever read or held here.
pub struct S3Backend {
    aws: PathBuf,
    base: String,
}

impl S3Backend {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.aws);
        c.args(["s3", "cp", "--only-show-errors", "--cli-connect-timeout", "5", "--cli-read-timeout", "15"]);
        c.args(args);
        c
    }

    fn url(&self, key: &str) -> String {
        format!("{}/{key}", self.base)
    }
}

impl Backend for S3Backend {
    fn get_all(&self, prefix: &str) -> Option<HashMap<String, String>> {
        let tmp = std::env::temp_dir().join(format!(
            "testenv-resume-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        fs::create_dir_all(&tmp).ok()?;
        let tmp_arg = tmp.to_string_lossy().into_owned();
        let url = format!("{}/", self.url(prefix));
        let ok = self
            .cmd(&["--recursive", &url, &tmp_arg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let all = ok.then(|| read_records(&tmp)).flatten();
        let _ = fs::remove_dir_all(&tmp);
        all
    }

    fn put(&self, key: &str, value: &str) {
        let Ok(mut child) = self.cmd(&["-", &self.url(key)]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        else {
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(value.as_bytes());
        }
        let _ = child.wait();
    }
}

/// `none` (or empty) runs everything; `dir:<path>` or an absolute path is a directory;
/// `s3://<bucket>/<prefix>` is S3. Anything else is refused, and the caller runs everything.
pub fn backend_from_spec(spec: &str, aws: &Path) -> Result<Option<Arc<dyn Backend>>, String> {
    let spec = spec.trim();
    if spec.is_empty() || spec == "none" {
        return Ok(None);
    }
    if let Some(rest) = spec.strip_prefix("s3://") {
        let base = rest.trim_end_matches('/');
        if base.is_empty() || base.starts_with('/') {
            return Err(format!("resume backend {spec:?} names no bucket"));
        }
        return Ok(Some(Arc::new(S3Backend { aws: aws.to_path_buf(), base: format!("s3://{base}") })));
    }
    let dir = spec.strip_prefix("dir:").unwrap_or(spec);
    if dir.starts_with('/') {
        return Ok(Some(Arc::new(DirBackend { root: PathBuf::from(dir) })));
    }
    Err(format!("resume backend {spec:?} is not none, dir:<absolute path> or s3://<bucket>/<prefix>"))
}

#[derive(Default)]
struct Pending(Mutex<Vec<JoinHandle<()>>>);

impl Pending {
    fn join(&self) {
        let handles = std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()));
        for h in handles {
            let _ = h.join();
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.join();
    }
}

#[derive(Clone)]
pub struct Store {
    backend: Arc<dyn Backend>,
    repo: String,
    tree: String,
    env_key: String,
    attempt: String,
    prior_attempt: bool,
    records: Arc<OnceLock<(HashMap<String, String>, std::time::Duration)>>,
    pending: Arc<Pending>,
}

/// The first failure to record a pass is said on stderr; the rest of the run stays quiet.
fn report_once(msg: &str) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::SeqCst) {
        eprintln!("{msg}");
    }
}

fn safe(seg: &str) -> bool {
    !seg.is_empty() && !seg.contains(['/', '\n', ' ', '\r']) && seg != "." && seg != ".."
}

impl Store {
    /// `prior_attempt` is false on the first attempt of a job, when nothing can have passed
    /// before: no lookup is made at all.
    pub fn new(backend: Arc<dyn Backend>, inputs: &KeyInputs, attempt: &str, prior_attempt: bool) -> Store {
        Store {
            backend,
            repo: inputs.repo_name.clone(),
            tree: inputs.tree.clone(),
            env_key: inputs.resume_env_key(),
            attempt: attempt.to_string(),
            prior_attempt,
            records: Arc::new(OnceLock::new()),
            pending: Arc::new(Pending::default()),
        }
    }

    fn loaded(&self) -> &(HashMap<String, String>, std::time::Duration) {
        self.records.get_or_init(|| {
            let t = std::time::Instant::now();
            let prefix = format!("{}/{}/{}", self.repo, self.tree, self.env_key);
            let all = if self.prior_attempt && [&self.repo, &self.tree, &self.env_key].iter().all(|s| safe(s)) {
                self.backend.get_all(&prefix).unwrap_or_default()
            } else {
                HashMap::new()
            };
            (all, t.elapsed())
        })
    }

    /// How long the one lookup took; zero when none was made.
    pub fn lookup_time(&self) -> std::time::Duration {
        self.loaded().1
    }

    /// Waits for every write started by `record`.
    pub fn flush(&self) {
        self.pending.join();
    }

    fn key(&self, suite: &str) -> Option<String> {
        [&self.repo, &self.tree, &self.env_key, suite]
            .iter()
            .all(|s| safe(s))
            .then(|| format!("{}/{}/{}/{suite}", self.repo, self.tree, self.env_key))
    }

    fn body(&self, suite: &str) -> String {
        format!("{MAGIC} {} {} {} {suite} {}\n", self.repo, self.tree, self.env_key, self.attempt)
    }

    /// The run/attempt that passed `suite` for this tree and environment, or None on any doubt.
    pub fn passed(&self, suite: &str) -> Option<String> {
        self.key(suite)?;
        let text = self.loaded().0.get(suite)?;
        let line = text.trim_end_matches('\n');
        let f: Vec<&str> = line.split(' ').collect();
        match f.as_slice() {
            [m, r, t, e, s, attempt]
                if *m == MAGIC && *r == self.repo && *t == self.tree && *e == self.env_key && *s == suite && !attempt.is_empty() =>
            {
                Some((*attempt).to_string())
            }
            _ => None,
        }
    }

    pub fn record(&self, suite: &str) {
        let Some(key) = self.key(suite) else {
            report_once(&format!(
                "resume: not recording {suite}: unsafe key segment (repo {:?}, tree {:?}, env {:?})",
                self.repo, self.tree, self.env_key
            ));
            return;
        };
        let backend = self.backend.clone();
        let body = self.body(suite);
        let handle = std::thread::spawn(move || backend.put(&key, &body));
        self.pending.0.lock().unwrap_or_else(|e| e.into_inner()).push(handle);
    }
}

/// `<run id>/<attempt>` of this job, `local` outside CI.
pub fn attempt_label(run_id: Option<&str>, attempt: Option<&str>) -> String {
    match run_id.filter(|r| !r.is_empty()) {
        Some(r) => format!("{r}/{}", attempt.filter(|a| !a.is_empty()).unwrap_or("1")),
        None => "local".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Mode, Producer};

    fn inputs(tree: &str, image: &str, suites: &[&str]) -> KeyInputs {
        KeyInputs {
            repo_name: "r".into(),
            tree: tree.into(),
            image_tag: image.into(),
            suites: suites.iter().map(|s| s.to_string()).collect(),
            harness_hash: "h".into(),
            mode: Mode::Serial,
            producer: Producer::Explicit,
            profile: "debug".into(),
            artifacts_id: None,
        }
    }

    #[test]
    fn a_dir_backend_that_cannot_create_its_root_records_nothing() {
        let d = testkit::TempDir::new("resume-unwritable");
        let blocker = d.path().join("blocker");
        fs::write(&blocker, "a file where a directory must go").unwrap();
        let backend = DirBackend { root: blocker.join("store") };
        backend.put("r/t/e/s", "body");
        assert!(!blocker.join("store").exists());
    }

    /// A backend over a fresh scratch space; `spec` is what a caller would configure.
    struct Shipped {
        _d: testkit::TempDir,
        spec: String,
        aws: PathBuf,
    }

    impl Shipped {
        fn dir(tag: &str) -> Shipped {
            let d = testkit::TempDir::new(&format!("testenv-resume-{tag}"));
            let spec = format!("dir:{}", d.path().join("store").display());
            Shipped { _d: d, spec, aws: PathBuf::from("aws") }
        }

        fn s3(tag: &str) -> Shipped {
            let d = testkit::TempDir::new(&format!("testenv-resume-{tag}"));
            let aws = d.path().join("aws");
            let script = format!(
                "#!/bin/sh\necho \"$*\" >> \"{calls}\"\n[ \"$1 $2\" = \"s3 cp\" ] || exit 2\nshift 2\nrec=0\nwhile [ \"${{1#--}}\" != \"$1\" ]; do case \"$1\" in --only-show-errors) shift;; --recursive) rec=1; shift;; *) shift 2;; esac; done\n\
                 map() {{ case \"$1\" in s3://*) printf '%s' \"{}/${{1#s3://}}\";; *) printf '%s' \"$1\";; esac; }}\n\
                 src=$(map \"$1\"); dst=$(map \"$2\")\n\
                 if [ $rec = 1 ]; then [ -d \"$src\" ] && cp -r \"$src/.\" \"$dst\"; exit 0; fi\n\
                 if [ \"$1\" = - ]; then mkdir -p \"$(dirname \"$dst\")\" && cat > \"$dst\"; \
                 elif [ \"$2\" = - ]; then cat \"$src\"; else exit 2; fi\n",
                d.path().join("remote").display(),
                calls = d.path().join("calls").display()
            );
            testkit::write_exe(&aws, &script);
            let spec = "s3://bkt/resume".to_string();
            Shipped { _d: d, spec, aws }
        }

        fn store(&self, i: &KeyInputs, attempt: &str) -> Store {
            let b = backend_from_spec(&self.spec, &self.aws).unwrap().expect("a backend");
            Store::new(b, i, attempt, !attempt.ends_with("/1"))
        }
    }

    fn calls(b: &Shipped) -> usize {
        fs::read_to_string(b._d.path().join("calls")).map(|t| t.lines().filter(|l| !l.contains(" - ")).count()).unwrap_or(0)
    }

    #[test]
    fn a_first_attempt_looks_nothing_up_and_a_later_one_makes_one_list_call_whatever_the_suite_count() {
        let b = Shipped::s3("calls");
        let names: Vec<String> = (0..40).map(|n| format!("s{n}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let i = inputs("t1", "img", &refs);
        let first = b.store(&i, "10/1");
        for n in &names {
            assert_eq!(first.passed(n), None);
        }
        assert_eq!(calls(&b), 0, "no lookup on a first attempt");
        for n in names.iter().take(30) {
            first.record(n);
        }
        first.flush();
        let next = b.store(&i, "10/2");
        let found = names.iter().filter(|n| next.passed(n).is_some()).count();
        assert_eq!(found, 30);
        assert_eq!(calls(&b), 1, "one recursive copy answers every suite");
    }

    fn each(f: impl Fn(&Shipped)) {
        f(&Shipped::dir("dir"));
        f(&Shipped::s3("s3"));
    }

    #[test]
    fn a_pass_recorded_on_one_attempt_is_found_by_the_next_for_the_same_tree_whatever_the_selection() {
        each(|b| {
            b.store(&inputs("t1", "img", &["a", "b", "c"]), "10/1").record("a");
            b.store(&inputs("t1", "img", &["a", "b", "c"]), "10/1").record("b");
            let next = b.store(&inputs("t1", "img", &["c", "b", "a"]), "10/2");
            assert_eq!(next.passed("a").as_deref(), Some("10/1"), "{}", b.spec);
            assert_eq!(next.passed("b").as_deref(), Some("10/1"), "{}", b.spec);
            assert_eq!(next.passed("c"), None, "{}", b.spec);
        });
    }

    #[test]
    fn a_changed_tree_or_environment_skips_nothing() {
        each(|b| {
            b.store(&inputs("t1", "img", &["a"]), "10/1").record("a");
            assert_eq!(b.store(&inputs("t2", "img", &["a"]), "x").passed("a"), None, "{}", b.spec);
            assert_eq!(b.store(&inputs("t1", "img2", &["a"]), "x").passed("a"), None, "{}", b.spec);
        });
    }

    #[test]
    fn a_record_that_is_unreadable_or_misfiled_runs_the_suite() {
        let b = Shipped::dir("doubt");
        let i = inputs("t1", "img", &["a", "b"]);
        let s = b.store(&i, "10/1");
        s.record("a");
        s.flush();
        let root = PathBuf::from(b.spec.strip_prefix("dir:").unwrap());
        let dir = root.join("r").join("t1").join(i.resume_env_key());
        fs::write(dir.join("b"), "garbage").unwrap();
        fs::copy(dir.join("a"), dir.join("c")).unwrap();
        let s = b.store(&i, "10/2");
        assert_eq!(s.passed("a").as_deref(), Some("10/1"));
        assert_eq!(s.passed("b"), None);
        assert_eq!(s.passed("c"), None, "a record filed under another suite's name");
        assert_eq!(s.passed("../a"), None);
    }

    #[test]
    fn an_unreachable_store_is_a_miss_and_a_failed_write_is_harmless() {
        let i = inputs("t1", "img", &["a"]);
        let gone = Shipped { _d: testkit::TempDir::new("testenv-resume-gone"), spec: "s3://bkt/resume".into(), aws: PathBuf::from("/nonexistent/aws") };
        let s = gone.store(&i, "1/1");
        s.record("a");
        assert_eq!(s.passed("a"), None);
        let blocked = Shipped::dir("blocked");
        let root = PathBuf::from(blocked.spec.strip_prefix("dir:").unwrap());
        fs::write(&root, "a file where the store should be").unwrap();
        let s = blocked.store(&i, "1/1");
        s.record("a");
        assert_eq!(s.passed("a"), None);
    }

    #[test]
    fn the_backend_is_chosen_by_its_spec_and_an_unset_one_is_none() {
        let aws = Path::new("aws");
        assert!(backend_from_spec("", aws).unwrap().is_none());
        assert!(backend_from_spec(" none ", aws).unwrap().is_none());
        assert!(backend_from_spec("dir:/x", aws).unwrap().is_some());
        assert!(backend_from_spec("/x", aws).unwrap().is_some());
        assert!(backend_from_spec("s3://b/resume/", aws).unwrap().is_some());
        for bad in ["s3://", "s3:///x", "relative/dir", "dir:rel", "gha:cache"] {
            assert!(backend_from_spec(bad, aws).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_attempt_label_names_run_and_attempt() {
        assert_eq!(attempt_label(Some("77"), Some("2")), "77/2");
        assert_eq!(attempt_label(Some("77"), None), "77/1");
        assert_eq!(attempt_label(None, Some("2")), "local");
    }
}
