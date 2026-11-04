//! Suite-level resume: a suite that passed for this exact tree and test environment is not
//! run again when the same job is re-run on another instance. Nor is one that passed, in the
//! same environment, on any tree where its INPUT CLOSURE was byte-identical
//! (suite_select::closure; over-broad-scopes #1, sp-ipwlic): an eject, mend or re-cut re-runs
//! only the suites whose inputs it changed. A suite with no computable closure has no inputs
//! key and only ever resumes for its own tree. Records go through one
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
/// The record of a suite's passes by input closure: a header, then `<inputs> <attempt>` lines,
/// newest first, at most [`INPUTS_KEPT`].
const MAGIC_INPUTS: &str = "testenv-resume-inputs-v1";
const INPUTS_KEPT: usize = 8;

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

    fn put_error(&self, key: &str, value: &str) -> Option<String> {
        let url = self.url(key);
        let fail = |why: String| Some(format!("resume: cannot write {url}: {why}"));
        let Ok(mut child) = self.cmd(&["-", &url]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn() else {
            return fail(format!("{} did not start", self.aws.display()));
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(value.as_bytes());
        }
        match child.wait_with_output() {
            Ok(out) if out.status.success() => None,
            Ok(out) => fail(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Err(e) => fail(e.to_string()),
        }
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
        if let Some(err) = self.put_error(key, value) {
            report_once(&err);
        }
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
    /// Suite → the digest of its input closure; a suite absent here resumes by tree alone.
    inputs: Arc<HashMap<String, String>>,
    by_inputs: Arc<OnceLock<HashMap<String, String>>>,
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
            inputs: Arc::new(HashMap::new()),
            by_inputs: Arc::new(OnceLock::new()),
            pending: Arc::new(Pending::default()),
        }
    }

    /// Keys each suite in `inputs` by the digest of its input closure as well as by the tree, so
    /// its pass on another tree with identical inputs resumes here. Looked up on every attempt:
    /// a first attempt has no pass of its own tree, but may have one of its inputs.
    pub fn with_inputs(mut self, inputs: HashMap<String, String>) -> Store {
        self.inputs = Arc::new(inputs.into_iter().filter(|(s, d)| safe(s) && safe(d)).collect());
        self
    }

    fn inputs_prefix(&self) -> String {
        format!("{}/inputs/{}", self.repo, self.env_key)
    }

    fn inputs_loaded(&self) -> &HashMap<String, String> {
        self.by_inputs.get_or_init(|| {
            if self.inputs.is_empty() || !safe(&self.repo) || !safe(&self.env_key) {
                return HashMap::new();
            }
            self.backend.get_all(&self.inputs_prefix()).unwrap_or_default()
        })
    }

    /// The `(inputs, attempt)` lines of `suite`'s inputs record, newest first; empty on any doubt.
    fn inputs_lines(&self, suite: &str) -> Vec<(String, String)> {
        let Some(text) = self.inputs_loaded().get(suite) else { return vec![] };
        let mut lines = text.lines();
        let header = format!("{MAGIC_INPUTS} {} {} {suite}", self.repo, self.env_key);
        if lines.next() != Some(header.as_str()) {
            return vec![];
        }
        lines
            .filter_map(|l| {
                let (d, a) = l.split_once(' ')?;
                (safe(d) && !a.is_empty() && !a.contains(' ')).then(|| (d.to_string(), a.to_string()))
            })
            .collect()
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

    /// The run/attempt that passed `suite` for this tree and environment, or for a tree whose
    /// input closure for it was identical, or None on any doubt.
    pub fn passed(&self, suite: &str) -> Option<String> {
        self.key(suite)?;
        self.passed_tree(suite).or_else(|| {
            let d = self.inputs.get(suite)?;
            self.inputs_lines(suite).into_iter().find(|(x, _)| x == d).map(|(_, a)| a)
        })
    }

    fn passed_tree(&self, suite: &str) -> Option<String> {
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
        if let Some(d) = self.inputs.get(suite) {
            let mut body = format!("{MAGIC_INPUTS} {} {} {suite}\n{d} {}\n", self.repo, self.env_key, self.attempt);
            for (x, a) in self.inputs_lines(suite).into_iter().filter(|(x, _)| x != d).take(INPUTS_KEPT - 1) {
                body.push_str(&format!("{x} {a}\n"));
            }
            let (backend, key) = (self.backend.clone(), format!("{}/{suite}", self.inputs_prefix()));
            let handle = std::thread::spawn(move || backend.put(&key, &body));
            self.pending.0.lock().unwrap_or_else(|e| e.into_inner()).push(handle);
        }
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
            tests: None,
            unit: None,
            runner: None,
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

        fn store(&self, i: &KeyInputs, attempt: &str) -> Store {
            let b = backend_from_spec(&self.spec, &self.aws).unwrap().expect("a backend");
            Store::new(b, i, attempt, !attempt.ends_with("/1"))
        }
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

    /// Over-broad-scopes #1 (sp-ipwlic): a suite's pass on one tree resumes it on another tree
    /// whose input closure for it is identical — even on a first attempt — and never on one
    /// whose closure differs; a suite with no inputs key resumes only for its own tree.
    #[test]
    fn a_pass_resumes_on_another_tree_only_when_the_suites_input_closure_is_identical() {
        let b = Shipped::dir("inputs");
        let with = |tree: &str, attempt: &str, keys: &[(&str, &str)]| {
            b.store(&inputs(tree, "img", &["a", "b", "c"]), attempt)
                .with_inputs(keys.iter().map(|(s, d)| (s.to_string(), d.to_string())).collect())
        };
        let s = with("t1", "10/1", &[("a", "da"), ("b", "db")]);
        for n in ["a", "b", "c"] {
            s.record(n);
        }
        s.flush();
        // An eject moved the tree: a's inputs are unchanged, b's changed, c has none.
        let s = with("t2", "11/1", &[("a", "da"), ("b", "db2")]);
        assert_eq!(s.passed("a").as_deref(), Some("10/1"), "identical closure on another tree");
        assert_eq!(s.passed("b"), None, "its inputs changed: it runs");
        assert_eq!(s.passed("c"), None, "no closure: it runs on any tree but its own");
        // b passes on t2; a later tree with t1's closure for b still finds the older pass.
        s.record("b");
        s.flush();
        let s = with("t3", "12/1", &[("a", "da"), ("b", "db")]);
        assert_eq!(s.passed("b").as_deref(), Some("10/1"));
        let s = with("t4", "13/1", &[("b", "db2")]);
        assert_eq!(s.passed("b").as_deref(), Some("11/1"));
        // Another environment never answers.
        let s = b.store(&inputs("t5", "img2", &["a"]), "14/1").with_inputs([("a".to_string(), "da".to_string())].into());
        assert_eq!(s.passed("a"), None, "another image is another environment");
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
