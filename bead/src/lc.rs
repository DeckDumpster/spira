//! The lifecycle store's writes `bead` makes, through [`LcClient`] in-process: the row a filed
//! bead gets, the dependency edges it mirrors, and the repo a `repo:` label names. Each was a
//! `timeout spira-lc …` child; the store's socket answers them now.

use spira_config::lc_call::{LcClient, LcError};

/// `create-bead <id> --priority N [--title T] [--submitted-tip S] [--repo R]`.
pub fn create_bead_argv(id: &str, title: Option<&str>, priority: i64, tip: Option<&str>, repo: Option<&str>) -> Vec<String> {
    let mut argv = vec!["create-bead".to_string(), id.to_string(), "--priority".into(), priority.to_string()];
    for (flag, v) in [("--title", title), ("--submitted-tip", tip), ("--repo", repo)] {
        if let Some(v) = v {
            argv.extend([flag.to_string(), v.to_string()]);
        }
    }
    argv
}

/// The bead's lifecycle row; `Err` names why it was not written.
pub fn create_row(lc: &LcClient, id: &str, title: Option<&str>, priority: i64, tip: Option<&str>, repo: Option<&str>) -> Result<(), String> {
    match lc.call(&create_bead_argv(id, title, priority, tip, repo)) {
        Ok(r) if r.code == 0 => Ok(()),
        Ok(r) => Err(format!("spira-lc create-bead exited {}: {}", r.code, r.text())),
        Err(e) => Err(e.to_string()),
    }
}

/// `dep-add`/`dep-remove <id> <depid> [--type T]`.
pub fn dep_argv(verb: &str, id: &str, depid: &str, dep_type: Option<&str>) -> Vec<String> {
    let mut argv = vec![verb.to_string(), id.to_string(), depid.to_string()];
    if let Some(t) = dep_type {
        argv.extend(["--type".to_string(), t.to_string()]);
    }
    argv
}

/// Whether the store applied the edge.
pub fn mirror_dep(lc: &LcClient, verb: &str, id: &str, depid: &str, dep_type: Option<&str>) -> bool {
    lc.call(&dep_argv(verb, id, depid, dep_type)).is_ok_and(|r| r.code == 0)
}

/// What `set-repo` answered.
#[derive(Debug, PartialEq, Eq)]
pub enum SetRepo {
    Applied,
    /// The machine refused (exit 1); its stdout says why.
    Refused(String),
    Failed,
}

/// `set-repo <id> <repo>` on `add`; `set-repo <id> --clear --only-if <repo>` otherwise.
pub fn set_repo_argv(verb: &str, id: &str, repo: &str) -> Vec<String> {
    let mut argv = vec!["set-repo".to_string(), id.to_string()];
    if verb == "add" {
        argv.push(repo.to_string());
    } else {
        argv.extend(["--clear".to_string(), "--only-if".into(), repo.to_string()]);
    }
    argv
}

pub fn set_repo(lc: &LcClient, verb: &str, id: &str, repo: &str) -> SetRepo {
    match lc.call(&set_repo_argv(verb, id, repo)) {
        Ok(r) if r.code == 0 => SetRepo::Applied,
        Ok(r) if r.code == 1 => SetRepo::Refused(r.out),
        Ok(_) | Err(LcError::Refused(_)) | Err(LcError::Fault(_)) => SetRepo::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_config::lc_call::{Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Scripted {
        code: i32,
        out: String,
        seen: Mutex<Vec<Vec<String>>>,
    }

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.seen.lock().unwrap().push(argv.to_vec());
            if self.code < 0 {
                return Err("cannot tell: down".into());
            }
            Ok(Reply { code: self.code, out: self.out.clone(), err: String::new() })
        }
    }

    fn client(code: i32, out: &str) -> (LcClient, Arc<Scripted>) {
        let t = Arc::new(Scripted { code, out: out.into(), seen: Mutex::new(Vec::new()) });
        (LcClient::over(t.clone(), true), t)
    }

    #[test]
    fn create_row_sends_the_cli_argv_over_the_socket_and_reports_a_refusal() {
        let (lc, t) = client(0, "");
        assert_eq!(create_row(&lc, "sp-1", Some("T"), 2, Some("abc"), Some("spira")), Ok(()));
        assert_eq!(t.seen.lock().unwrap()[0], ["create-bead", "sp-1", "--priority", "2", "--title", "T", "--submitted-tip", "abc", "--repo", "spira"]);
        let (lc, _) = client(3, "refused: exists");
        assert!(create_row(&lc, "sp-1", None, 2, None, None).unwrap_err().contains("refused: exists"));
        let (lc, _) = client(-1, "");
        assert!(create_row(&lc, "sp-1", None, 2, None, None).is_err());
    }

    #[test]
    fn mirror_dep_is_applied_only_on_exit_zero() {
        let (lc, t) = client(0, "");
        assert!(mirror_dep(&lc, "dep-add", "a", "b", Some("blocks")));
        assert_eq!(t.seen.lock().unwrap()[0], ["dep-add", "a", "b", "--type", "blocks"]);
        let (lc, _) = client(2, "cannot tell");
        assert!(!mirror_dep(&lc, "dep-remove", "a", "b", None));
    }

    #[test]
    fn set_repo_tells_a_refusal_from_a_failure() {
        let (lc, t) = client(0, "");
        assert_eq!(set_repo(&lc, "add", "a", "spira"), SetRepo::Applied);
        assert_eq!(t.seen.lock().unwrap()[0], ["set-repo", "a", "spira"]);
        let (lc, t) = client(1, "WORKING\n");
        assert_eq!(set_repo(&lc, "remove", "a", "spira"), SetRepo::Refused("WORKING\n".into()));
        assert_eq!(t.seen.lock().unwrap()[0], ["set-repo", "a", "--clear", "--only-if", "spira"]);
        let (lc, _) = client(-1, "");
        assert_eq!(set_repo(&lc, "add", "a", "spira"), SetRepo::Failed);
    }
}
