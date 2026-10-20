use crate::{Commit, Open, Site, World, LABEL};
use spira_config::bounded::{bounded, bounded_for};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

pub struct Real {
    pub kit_path: String,
    pub kit_ref: String,
    pub crate_name: String,
    pub state: PathBuf,
    pub persona: String,
    bumps: RefCell<BTreeMap<String, (String, String)>>,
}

fn run(mut c: Command) -> Result<String, String> {
    let out = c.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{:?} exited {}: {}", c.get_program(), out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn git(path: &str, args: &[&str]) -> Result<String, String> {
    let mut c = bounded("git");
    c.arg("-C").arg(path).args(args);
    run(c)
}

impl Real {
    pub fn new(kit_path: String, kit_ref: String, crate_name: String, state: PathBuf, persona: String) -> Real {
        let bumps = std::fs::read_to_string(&state)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some((f.next()?.to_string(), (f.next()?.to_string(), f.next()?.to_string())))
            })
            .collect();
        Real { kit_path, kit_ref, crate_name, state, persona, bumps: RefCell::new(bumps) }
    }

    fn record(&self, site: &Site, id: &str, target: &str) -> Result<(), String> {
        let mut m = self.bumps.borrow_mut();
        m.insert(site.name.clone(), (id.to_string(), target.to_string()));
        let text = m.iter().fold(String::new(), |out, (s, (i, t))| out + &format!("{s}\t{i}\t{t}\n"));
        std::fs::write(&self.state, text).map_err(|e| format!("cannot record {id} in {}: {e}", self.state.display()))
    }

    fn body_file(body: &str) -> Result<PathBuf, String> {
        let p = std::env::temp_dir().join(format!("kit-bump-{}.md", std::process::id()));
        std::fs::write(&p, body).map_err(|e| e.to_string())?;
        Ok(p)
    }

    fn bead(&self, args: &[&str]) -> Result<String, String> {
        // batch-job: bead.sh opens the store and runs as long as the filing takes
        let mut c = bounded_for("bead.sh", "120");
        c.args(args);
        run(c)
    }
}

impl World for Real {
    fn kit_tip(&self) -> Result<String, String> {
        git(&self.kit_path, &["rev-parse", "--verify", &format!("{}^{{commit}}", self.kit_ref)]).map(|s| s.trim().to_string())
    }

    fn site_pin(&self, site: &Site) -> Result<String, String> {
        let manifest = git(&site.path, &["show", &format!("{}:{}", site.git_ref, site.file)])?;
        crate::parse_pin(&manifest, &self.crate_name)
            .ok_or_else(|| format!("no `{}` rev in {}:{}", self.crate_name, site.git_ref, site.file))
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        git(&self.kit_path, &["merge-base", "--is-ancestor", ancestor, descendant]).is_ok()
    }

    fn commits(&self, pin: &str, tip: &str) -> Vec<Commit> {
        let range = format!("{pin}..{tip}");
        git(&self.kit_path, &["log", "--reverse", "--format=%H%x09%ct%x09%s", &range])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.splitn(3, '\t');
                Some(Commit { sha: f.next()?.into(), at: f.next()?.parse().ok()?, subject: f.next()?.into() })
            })
            .collect()
    }

    fn open_bump(&self, site: &Site) -> Result<Option<Open>, String> {
        let Some((id, target)) = self.bumps.borrow().get(&site.name).cloned() else { return Ok(None) };
        let mut c = bounded(spira_config::lifecycle_row::lc_bin());
        c.args(["show", &id]);
        let out = c.output().map_err(|e| e.to_string())?;
        match out.status.code() {
            Some(1) => Ok(None),
            Some(0) => {
                let doc: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("{id}: not JSON: {e}"))?;
                let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                Ok((!TERMINAL.contains(&state)).then_some(Open { id, target }))
            }
            _ => Err(format!("cannot tell whether {id} is open: {}", String::from_utf8_lossy(&out.stdout).trim())),
        }
    }

    fn file_bump(&self, site: &Site, title: &str, body: &str, target: &str) -> Result<String, String> {
        let f = Self::body_file(body)?;
        let out = self.bead(&["file", title, "--for", &self.persona, "--repo", &site.name, "--body-file", &f.to_string_lossy()]);
        let _ = std::fs::remove_file(&f);
        let id = out?.lines().rev().map(str::trim).find(|l| !l.is_empty()).ok_or("bead.sh printed no id")?.to_string();
        self.record(site, &id, target)?;
        let _ = self.bead(&["label", "add", &id, LABEL]);
        Ok(id)
    }

    fn amend_bump(&self, site: &Site, open: &Open, body: &str, target: &str) -> Result<(), String> {
        let f = Self::body_file(body)?;
        let out = self.bead(&["amend", &open.id, "--body-file", &f.to_string_lossy(), "--replace", "--note", &format!("kit-bump: target moved to {target}")]);
        let _ = std::fs::remove_file(&f);
        out?;
        self.record(site, &open.id, target)
    }

    fn now(&self) -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
    }
}
