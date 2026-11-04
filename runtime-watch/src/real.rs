use crate::{civil_date, names_bump, parse_node, parse_python, parse_rust_stable, EolSupport, Repo, Runtime, RustSupport, World, LABEL};
use spira_config::bounded::{bounded, bounded_for};
use spira_config::lc_call::LcClient;
use spira_config::repos::{Column, Registry};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

pub const RUST_URL: &str = "https://static.rust-lang.org/dist/channel-rust-stable.toml";
pub const NODE_SCHEDULE_URL: &str = "https://raw.githubusercontent.com/nodejs/Release/main/schedule.json";
pub const NODE_INDEX_URL: &str = "https://nodejs.org/dist/index.json";
pub const PYTHON_URL: &str = "https://endoflife.date/api/python.json";
const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

pub struct Real {
    pub registry: Registry,
    pub state: PathBuf,
    pub persona: String,
    bumps: RefCell<BTreeMap<String, String>>,
    /// The lifecycle store, in-process over its socket.
    lc: LcClient,
}

fn run(mut c: Command) -> Result<String, String> {
    let out = c.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{:?} exited {}: {}", c.get_program(), out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn fetch(url: &str) -> Result<String, String> {
    // batch-job: an upstream download, not a store read
    let mut c = bounded_for("curl", "30");
    c.args(["--fail", "--silent", "--show-error", "--location", "--max-time", "25", url]);
    run(c).map_err(|e| format!("cannot fetch {url}: {e}"))
}

fn git(path: &str, args: &[&str]) -> Result<String, String> {
    let mut c = bounded("git");
    c.arg("-C").arg(path).args(args);
    run(c)
}

fn key(repo: &Repo, rt: Runtime) -> String {
    format!("{}\t{}", repo.name, rt.name())
}

impl Real {
    pub fn new(registry: Registry, state: PathBuf, persona: String, lc: LcClient) -> Real {
        let bumps = std::fs::read_to_string(&state)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.rsplit_once('\t').map(|(k, id)| (k.to_string(), id.to_string())))
            .collect();
        Real { registry, state, persona, bumps: RefCell::new(bumps), lc }
    }

    fn record(&self, k: String, id: &str) -> Result<(), String> {
        let mut m = self.bumps.borrow_mut();
        m.insert(k, id.to_string());
        let text = m.iter().fold(String::new(), |out, (k, i)| out + &format!("{k}\t{i}\n"));
        std::fs::write(&self.state, text).map_err(|e| format!("cannot record {id} in {}: {e}", self.state.display()))
    }

    fn ledger_bump(&self, repo: &Repo, rt: Runtime) -> Result<Option<String>, String> {
        let Some(id) = self.bumps.borrow().get(&key(repo, rt)).cloned() else { return Ok(None) };
        let out = self.lc.call(&["show", &id]).map_err(|e| format!("cannot tell whether {id} is open: {e}"))?;
        match out.code {
            1 => Ok(None),
            0 => {
                let doc: serde_json::Value = serde_json::from_str(&out.out).map_err(|e| format!("{id}: not JSON: {e}"))?;
                let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                Ok((!TERMINAL.contains(&state)).then_some(id))
            }
            _ => Err(format!("cannot tell whether {id} is open: {}", out.out.trim())),
        }
    }

    fn bead(&self, args: &[&str]) -> Result<String, String> {
        // batch-job: bead.sh opens the store and runs as long as the filing takes
        // FORK-EDGE: content store write (bead.sh file/label is the operator filing tool)
        let mut c = bounded_for("bead.sh", "120");
        c.args(args);
        run(c)
    }
}

impl World for Real {
    fn repos(&self) -> Vec<Repo> {
        self.registry
            .all()
            .into_iter()
            .filter_map(|name| {
                let path = self.registry.root(&name).filter(|p| !p.is_empty())?;
                let git_ref = self.registry.field(&name, Column::Base).filter(|r| !r.is_empty())?;
                Some(Repo { name, path, git_ref })
            })
            .collect()
    }

    fn read(&self, repo: &Repo, file: &str) -> Result<Option<String>, String> {
        let spec = format!("{}:{file}", repo.git_ref);
        if git(&repo.path, &["ls-tree", "--name-only", &repo.git_ref, "--", file])?.trim().is_empty() {
            return Ok(None);
        }
        git(&repo.path, &["show", &spec]).map(Some)
    }

    fn naming(&self, repo: &Repo, needle: &str) -> Vec<String> {
        let prefix = format!("{}:", repo.git_ref);
        git(&repo.path, &["grep", "-l", "-F", "-e", needle, &repo.git_ref])
            .unwrap_or_default()
            .lines()
            .map(|l| l.strip_prefix(&prefix).unwrap_or(l).to_string())
            .collect()
    }

    fn rust(&self) -> Result<RustSupport, String> {
        parse_rust_stable(&fetch(RUST_URL)?).map(|stable| RustSupport { stable })
    }

    fn node(&self) -> Result<EolSupport, String> {
        parse_node(&fetch(NODE_SCHEDULE_URL)?, &fetch(NODE_INDEX_URL)?, &self.today())
    }

    fn python(&self) -> Result<EolSupport, String> {
        parse_python(&fetch(PYTHON_URL)?, &self.today())
    }

    fn open_bump(&self, repo: &Repo, rt: Runtime) -> Result<Option<String>, String> {
        if let Some(id) = self.ledger_bump(repo, rt)? {
            return Ok(Some(id));
        }
        let out = self.lc.call(&["list", "--live", "--repo", &repo.name]).map_err(|e| format!("cannot list {}'s open beads: {e}", repo.name))?;
        if out.code != 0 {
            return Err(format!("cannot list {}'s open beads: {}", repo.name, out.out.trim()));
        }
        let beads: Vec<serde_json::Value> = serde_json::from_str(&out.out).map_err(|e| format!("{}: bead list not JSON: {e}", repo.name))?;
        Ok(beads
            .iter()
            .filter(|b| b["state"].as_str().is_some_and(|s| !TERMINAL.contains(&s)))
            .find(|b| b["title"].as_str().is_some_and(|t| names_bump(t, rt)))
            .and_then(|b| b["bead_id"].as_str().map(str::to_string)))
    }

    fn file_bump(&self, repo: &Repo, rt: Runtime, title: &str, body: &str) -> Result<String, String> {
        let p = std::env::temp_dir().join(format!("runtime-watch-{}.md", std::process::id()));
        std::fs::write(&p, body).map_err(|e| e.to_string())?;
        let out = self.bead(&["file", title, "--for", &self.persona, "--repo", &repo.name, "--body-file", &p.to_string_lossy()]);
        let _ = std::fs::remove_file(&p);
        let id = out?.lines().rev().map(str::trim).find(|l| !l.is_empty()).ok_or("bead.sh printed no id")?.to_string();
        self.record(key(repo, rt), &id)?;
        let _ = self.bead(&["label", "add", &id, LABEL]);
        Ok(id)
    }

    fn today(&self) -> String {
        civil_date(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64))
    }
}

#[cfg(test)]
mod lc_tests {
    use super::*;
    use spira_config::lc_call::{Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// The store's answers by verb, and every argv it was asked.
    struct Store {
        answers: Vec<(&'static str, i32, &'static str)>,
        asked: Mutex<Vec<Vec<String>>>,
    }

    impl Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.asked.lock().unwrap().push(argv.to_vec());
            let (_, code, out) = self.answers.iter().find(|(v, _, _)| argv[0] == *v).ok_or("unexpected verb")?;
            Ok(Reply { code: *code, out: out.to_string(), err: String::new() })
        }
    }

    fn real(dir: &std::path::Path, ledger: &str, answers: Vec<(&'static str, i32, &'static str)>) -> (Real, Arc<Store>) {
        let state = dir.join("ledger");
        std::fs::write(&state, ledger).unwrap();
        let store = Arc::new(Store { answers, asked: Mutex::default() });
        let lc = LcClient::over(Arc::clone(&store) as Arc<dyn Transport>, true);
        (Real::new(Registry::new(None, &BTreeMap::new(), dir), state, "ops".into(), lc), store)
    }

    fn repo() -> Repo {
        Repo { name: "deck".into(), path: "/r".into(), git_ref: "main".into() }
    }

    #[test]
    fn an_open_ledger_bump_is_read_from_the_store_in_process() {
        let d = testkit::TempDir::new("rw-ledger");
        let (r, store) = real(d.path(), "deck\tnode\tsp-bump1\n", vec![("show", 0, r#"{"bead":{"state":"READY"}}"#)]);
        assert_eq!(r.open_bump(&repo(), Runtime::Node), Ok(Some("sp-bump1".into())));
        assert_eq!(store.asked.lock().unwrap().as_slice(), &[vec!["show".to_string(), "sp-bump1".into()]]);
    }

    #[test]
    fn a_landed_ledger_bump_falls_through_to_the_live_list() {
        let d = testkit::TempDir::new("rw-list");
        let list = r#"[{"bead_id":"sp-other","state":"READY","title":"bump node to 24"},{"bead_id":"sp-old","state":"LANDED","title":"bump node to 22"}]"#;
        let (r, store) = real(d.path(), "deck\tnode\tsp-bump1\n", vec![("show", 0, r#"{"bead":{"state":"LANDED"}}"#), ("list", 0, list)]);
        let found = r.open_bump(&repo(), Runtime::Node).unwrap();
        assert_eq!(store.asked.lock().unwrap()[1], ["list", "--live", "--repo", "deck"]);
        assert_eq!(found, names_bump("bump node to 24", Runtime::Node).then(|| "sp-other".to_string()));
    }

    #[test]
    fn a_store_that_cannot_tell_is_an_error_not_no_bump() {
        let d = testkit::TempDir::new("rw-cannot");
        let (r, _) = real(d.path(), "deck\tnode\tsp-bump1\n", vec![("show", 2, "cannot tell: down")]);
        assert!(r.open_bump(&repo(), Runtime::Node).unwrap_err().contains("cannot tell whether sp-bump1 is open"));
    }
}
