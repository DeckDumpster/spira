use crate::{civil_date, names_bump, parse_node, parse_python, parse_rust_stable, EolSupport, Repo, Runtime, RustSupport, World, LABEL};
use spira_config::bounded::{bounded, bounded_for};
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
    pub fn new(registry: Registry, state: PathBuf, persona: String) -> Real {
        let bumps = std::fs::read_to_string(&state)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.rsplit_once('\t').map(|(k, id)| (k.to_string(), id.to_string())))
            .collect();
        Real { registry, state, persona, bumps: RefCell::new(bumps) }
    }

    fn record(&self, k: String, id: &str) -> Result<(), String> {
        let mut m = self.bumps.borrow_mut();
        m.insert(k, id.to_string());
        let text = m.iter().fold(String::new(), |out, (k, i)| out + &format!("{k}\t{i}\n"));
        std::fs::write(&self.state, text).map_err(|e| format!("cannot record {id} in {}: {e}", self.state.display()))
    }

    fn ledger_bump(&self, repo: &Repo, rt: Runtime) -> Result<Option<String>, String> {
        let Some(id) = self.bumps.borrow().get(&key(repo, rt)).cloned() else { return Ok(None) };
        let mut c = bounded(spira_config::lifecycle_row::lc_bin());
        c.args(["show", &id]);
        let out = c.output().map_err(|e| e.to_string())?;
        match out.status.code() {
            Some(1) => Ok(None),
            Some(0) => {
                let doc: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("{id}: not JSON: {e}"))?;
                let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                Ok((!TERMINAL.contains(&state)).then_some(id))
            }
            _ => Err(format!("cannot tell whether {id} is open: {}", String::from_utf8_lossy(&out.stdout).trim())),
        }
    }

    fn bead(&self, args: &[&str]) -> Result<String, String> {
        // batch-job: bead.sh opens the store and runs as long as the filing takes
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
        let mut c = bounded(spira_config::lifecycle_row::lc_bin());
        c.args(["list", "--live", "--repo", &repo.name]);
        let out = c.output().map_err(|e| e.to_string())?;
        if out.status.code() != Some(0) {
            return Err(format!("cannot list {}'s open beads: {}", repo.name, String::from_utf8_lossy(&out.stdout).trim()));
        }
        let beads: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).map_err(|e| format!("{}: bead list not JSON: {e}", repo.name))?;
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
