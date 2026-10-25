//! Suite-level resume: a suite that passed for this exact tree and test environment is not
//! run again when the same job is re-run on another instance. The store is a directory the
//! CI cache carries between attempts; any doubt about a record runs the suite.

use std::fs;
use std::path::{Path, PathBuf};

use crate::verdict::KeyInputs;

const MAGIC: &str = "testenv-resume-v1";

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    key: String,
    attempt: String,
}

impl Store {
    pub fn new(root: &Path, inputs: &KeyInputs, attempt: &str) -> Store {
        let key = inputs.resume_key();
        Store { dir: root.join(&key), key, attempt: attempt.to_string() }
    }

    fn file(&self, suite: &str) -> Option<PathBuf> {
        let safe = !suite.is_empty() && !suite.contains(['/', '\n', ' ']) && suite != "." && suite != "..";
        safe.then(|| self.dir.join(format!("{suite}.pass")))
    }

    /// The run/attempt that passed `suite` for this key, or None on any doubt.
    pub fn passed(&self, suite: &str) -> Option<String> {
        let text = fs::read_to_string(self.file(suite)?).ok()?;
        let f: Vec<&str> = text.trim_end_matches('\n').split(' ').collect();
        match f.as_slice() {
            [m, k, s, attempt] if *m == MAGIC && *k == self.key && *s == suite && !attempt.is_empty() => {
                Some((*attempt).to_string())
            }
            _ => None,
        }
    }

    pub fn record(&self, suite: &str) {
        let Some(path) = self.file(suite) else { return };
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let line = format!("{MAGIC} {} {suite} {}\n", self.key, self.attempt);
        if fs::write(&tmp, line).is_ok() {
            let _ = fs::rename(&tmp, &path);
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
        }
    }

    fn root(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("testenv-resume-{tag}"))
    }

    #[test]
    fn a_pass_recorded_on_one_attempt_is_found_by_the_next_for_the_same_tree_whatever_the_selection() {
        let d = root("hit");
        Store::new(d.path(), &inputs("t1", "img", &["a", "b", "c"]), "10/1").record("a");
        Store::new(d.path(), &inputs("t1", "img", &["a", "b", "c"]), "10/1").record("b");
        let next = Store::new(d.path(), &inputs("t1", "img", &["c", "b", "a"]), "10/2");
        assert_eq!(next.passed("a").as_deref(), Some("10/1"));
        assert_eq!(next.passed("b").as_deref(), Some("10/1"));
        assert_eq!(next.passed("c"), None);
    }

    #[test]
    fn a_changed_tree_or_environment_skips_nothing() {
        let d = root("miss");
        Store::new(d.path(), &inputs("t1", "img", &["a"]), "10/1").record("a");
        assert_eq!(Store::new(d.path(), &inputs("t2", "img", &["a"]), "x").passed("a"), None);
        assert_eq!(Store::new(d.path(), &inputs("t1", "img2", &["a"]), "x").passed("a"), None);
    }

    #[test]
    fn a_record_that_is_unreadable_or_misfiled_runs_the_suite() {
        let d = root("doubt");
        let i = inputs("t1", "img", &["a", "b"]);
        let s = Store::new(d.path(), &i, "10/1");
        s.record("a");
        let dir = d.path().join(i.resume_key());
        fs::write(dir.join("b.pass"), "garbage").unwrap();
        fs::copy(dir.join("a.pass"), dir.join("c.pass")).unwrap();
        assert_eq!(s.passed("a").as_deref(), Some("10/1"));
        assert_eq!(s.passed("b"), None);
        assert_eq!(s.passed("c"), None, "a record filed under another suite's name");
        assert_eq!(s.passed("../a"), None);
        let gone = Store::new(&d.path().join("absent"), &i, "x");
        assert_eq!(gone.passed("a"), None);
    }

    #[test]
    fn the_attempt_label_names_run_and_attempt() {
        assert_eq!(attempt_label(Some("77"), Some("2")), "77/2");
        assert_eq!(attempt_label(Some("77"), None), "77/1");
        assert_eq!(attempt_label(None, Some("2")), "local");
    }
}
