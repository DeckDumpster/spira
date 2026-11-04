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
    lc: spira_config::lc_call::LcClient,
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

/// The newest release of `crate_name` in the sparse index at `registry`, read in-process over
/// HTTP (`file://` registries are read from disk); `None` when nothing is published (404/410).
pub fn fetch_release(registry: &str, crate_name: &str) -> Result<Option<String>, String> {
    let url = format!("{}/{}", registry.trim_end_matches('/'), index_path(crate_name));
    let index = if let Some(path) = url.strip_prefix("file://") {
        match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{url}: {e}")),
        }
    } else {
        match ureq::get(&url).timeout(std::time::Duration::from_secs(25)).call() {
            Ok(r) => r.into_string().map_err(|e| format!("{url}: {e}"))?,
            Err(ureq::Error::Status(404 | 410, _)) => return Ok(None),
            Err(ureq::Error::Status(s, _)) => return Err(format!("{url} answered HTTP {s}")),
            Err(e) => return Err(format!("{url}: {e}")),
        }
    };
    crate::newest_release(&index).map(Some).ok_or_else(|| format!("no release of `{crate_name}` in {url}"))
}

/// A site's checkout and base ref from the repo map, as `install::kit_bump` renders them.
pub fn resolve_site(reg: &spira_config::repos::Registry, name: &str, file: &str) -> Result<Site, String> {
    use spira_config::repos::Column;
    let path = reg.field(name, Column::Path).unwrap_or_default();
    let git_ref = reg.field(name, Column::Base).unwrap_or_default();
    if path.is_empty() || git_ref.is_empty() {
        return Err(format!("repo {name:?} needs a path and a base ref in the repo map"));
    }
    Ok(Site { name: name.into(), path, git_ref, file: file.into() })
}

fn git(path: &str, args: &[&str]) -> Result<String, String> {
    let mut c = bounded("git");
    c.arg("-C").arg(path).args(args);
    run(c)
}

impl Real {
    pub fn new(registry: String, crate_name: String, state: PathBuf, persona: String, lc: spira_config::lc_call::LcClient) -> Real {
        let bumps = std::fs::read_to_string(&state)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some((f.next()?.to_string(), (f.next()?.to_string(), f.next()?.to_string())))
            })
            .collect();
        Real { registry, crate_name, state, persona, bumps: RefCell::new(bumps), lc }
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
        // FORK-EDGE: content store write (bead.sh file/amend/label, the operator filing tool)
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
        let out = self.lc.call(&["show", &id]).map_err(String::from)?;
        match out.code {
            1 => Ok(None),
            0 => {
                let doc: serde_json::Value = serde_json::from_str(&out.out).map_err(|e| format!("{id}: not JSON: {e}"))?;
                let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                Ok((!TERMINAL.contains(&state)).then_some(Open { id, target }))
            }
            _ => Err(format!("cannot tell whether {id} is open: {}", out.out.trim())),
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

#[cfg(test)]
mod store_client_tests {
    use super::*;
    use spira_config::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};

    /// `show` answered as the store answers it: a row with its state, or exit 1 for none.
    pub struct Scripted(pub Mutex<Vec<String>>);

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.join(" "));
            let reply = |code, out: &str| Ok(Reply { code, out: out.into(), err: String::new() });
            match argv.get(1).map(String::as_str) {
                Some("sp-open") => reply(0, r#"{"bead":{"state":"READY"}}"#),
                Some("sp-done") => reply(0, r#"{"bead":{"state":"LANDED"}}"#),
                Some("sp-gone") => reply(1, "{}"),
                _ => reply(2, "cannot tell: down"),
            }
        }
    }

    pub fn client() -> (LcClient, Arc<Scripted>) {
        let t = Arc::new(Scripted(Mutex::default()));
        (LcClient::over(t.clone(), true), t)
    }

    #[test]
    fn an_open_bump_is_read_through_the_store_client() {
        let dir = testkit::TempDir::new("kit-bump-open");
        let state = dir.path().join("bumps");
        std::fs::write(&state, "a\tsp-open\t1.2\nb\tsp-done\t1.3\nc\tsp-gone\t1.4\nd\tsp-down\t1.5\n").unwrap();
        let (lc, t) = client();
        let real = Real::new("r".into(), "k".into(), state, "builder".into(), lc);
        let site = |n: &str| Site { name: n.into(), path: String::new(), git_ref: String::new(), file: String::new() };
        let open = real.open_bump(&site("a")).unwrap().unwrap();
        assert_eq!((open.id.as_str(), open.target.as_str()), ("sp-open", "1.2"));
        assert!(real.open_bump(&site("b")).unwrap().is_none());
        assert!(real.open_bump(&site("c")).unwrap().is_none());
        assert!(real.open_bump(&site("d")).is_err());
        assert_eq!(t.0.lock().unwrap().len(), 4);
    }
}
