use crate::{Open, Site, World, LABEL};
use spira_config::bounded::{bounded, bounded_for};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

pub struct Real {
    pub registry: String,
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

/// Where a crate's entry sits in a sparse index.
pub fn index_path(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    match n.len() {
        1 | 2 => format!("{}/{n}", n.len()),
        3 => format!("3/{}/{n}", &n[..1]),
        _ => format!("{}/{}/{n}", &n[..2], &n[2..4]),
    }
}

/// The index text of a curl reply ending in a `\n<status>` line; `None` for a 404 (nothing published).
/// `file://` replies carry status 000 and are read as the body they are.
pub fn index_body<'a>(reply: &'a str, url: &str) -> Result<Option<&'a str>, String> {
    let (body, status) = reply.rsplit_once('\n').ok_or_else(|| format!("{url}: reply has no status line"))?;
    match status.trim() {
        "200" | "000" => Ok(Some(body)),
        "404" | "410" => Ok(None),
        s => Err(format!("{url} answered HTTP {s}")),
    }
}

fn git(path: &str, args: &[&str]) -> Result<String, String> {
    let mut c = bounded("git");
    c.arg("-C").arg(path).args(args);
    run(c)
}

impl Real {
    pub fn new(registry: String, crate_name: String, state: PathBuf, persona: String) -> Real {
        let bumps = std::fs::read_to_string(&state)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some((f.next()?.to_string(), (f.next()?.to_string(), f.next()?.to_string())))
            })
            .collect();
        Real { registry, crate_name, state, persona, bumps: RefCell::new(bumps) }
    }

    fn record(&self, site: &Site, id: &str, target: &str) -> Result<(), String> {
        let mut m = self.bumps.borrow_mut();
        m.insert(site.name.clone(), (id.to_string(), target.to_string()));
        let text = m.iter().fold(String::new(), |out, (s, (i, t))| out + &format!("{s}\t{i}\t{t}\n"));
        std::fs::write(&self.state, text).map_err(|e| format!("cannot record {id} in {}: {e}", self.state.display()))
    }

    fn body_file(body: &str) -> Result<PathBuf, String> {
        let p = std::env::temp_dir().join(format!("kit-bump-{}.md", std::process::id()));
        std::fs::write(&p, format!("{body}\n\n## Build\n\nBump the kit dependency at this site as described above and land the change.\n")).map_err(|e| e.to_string())?;
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
    fn latest_release(&self) -> Result<Option<String>, String> {
        let url = format!("{}/{}", self.registry.trim_end_matches('/'), index_path(&self.crate_name));
        let mut c = bounded_for("curl", "30");
        c.args(["-sS", "--max-time", "25", "-w", "\n%{http_code}", &url]);
        let reply = run(c)?;
        let Some(index) = index_body(&reply, &url)? else {
            return Ok(None);
        };
        crate::newest_release(index).map(Some).ok_or_else(|| format!("no release of `{}` in {url}", self.crate_name))
    }

    fn site_requirement(&self, site: &Site) -> Result<String, String> {
        let manifest = git(&site.path, &["show", &format!("{}:{}", site.git_ref, site.file)])?;
        crate::parse_requirement(&manifest, &self.crate_name)
            .ok_or_else(|| format!("no `{}` requirement in {}:{}", self.crate_name, site.git_ref, site.file))
    }

    fn site_locked(&self, site: &Site) -> Result<String, String> {
        let lock = Path::new(&site.file).with_file_name("Cargo.lock");
        let lock = git(&site.path, &["show", &format!("{}:{}", site.git_ref, lock.display())])?;
        crate::parse_locked(&lock, &self.crate_name)
            .ok_or_else(|| format!("no `{}` in the lockfile beside {}:{}", self.crate_name, site.git_ref, site.file))
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
}
