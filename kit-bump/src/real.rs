use crate::{Open, Site, World, LABEL};
use spira_config::bounded::bounded;
use std::path::Path;

pub struct Real {
    pub crate_name: String,
    pub persona: String,
    lc: spira_store::lc_call::LcClient,
}

fn run(mut c: spira_config::bounded::Bounded) -> Result<String, String> {
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
        // batch-job: an upstream registry download, not a store read
        match ureq::get(&url).timeout(std::time::Duration::from_secs(25)).call() {
            Ok(r) => r.into_string().map_err(|e| format!("{url}: {e}"))?,
            Err(ureq::Error::Status(404 | 410, _)) => return Ok(None),
            Err(ureq::Error::Status(s, _)) => return Err(format!("{url} answered HTTP {s}")),
            Err(e) => return Err(format!("{url}: {e}")),
        }
    };
    crate::newest_release(&index).map(Some).ok_or_else(|| format!("no release of `{crate_name}` in {url}"))
}

/// A site's checkout and base ref from the repo map.
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

/// `git show <ref>:<file>` at a site, held under the ref's sha so a site whose ref has not moved starts no git.
fn show(site: &Site, file: &str) -> Result<String, String> {
    let spec = format!("{}:{file}", site.git_ref);
    let read = || git(&site.path, &["show", &spec]);
    match spira_config::git_refs::sha_of(Path::new(&site.path), &site.git_ref) {
        Some(tip) => spira_config::git_refs::held(Path::new(&site.path), &format!("show {spec}"), &tip, read),
        None => read(),
    }
}

impl Real {
    pub fn new(crate_name: String, persona: String, lc: spira_store::lc_call::LcClient) -> Real {
        Real { crate_name, persona, lc }
    }

    fn body(body: &str) -> String {
        format!("{body}\n\n## Build\n\nBump the kit dependency at this site as described above and land the change.\n")
    }
}

impl World for Real {
    fn site_requirement(&self, site: &Site) -> Result<String, String> {
        let manifest = show(site, &site.file)?;
        crate::parse_requirement(&manifest, &self.crate_name)
            .ok_or_else(|| format!("no `{}` requirement in {}:{}", self.crate_name, site.git_ref, site.file))
    }

    fn site_locked(&self, site: &Site) -> Result<String, String> {
        let lock = Path::new(&site.file).with_file_name("Cargo.lock");
        let lock = show(site, &lock.display().to_string())?;
        crate::parse_locked(&lock, &self.crate_name)
            .ok_or_else(|| format!("no `{}` in the lockfile beside {}:{}", self.crate_name, site.git_ref, site.file))
    }

    fn file_bump(&self, site: &Site, title: &str, body: &str) -> Result<String, String> {
        let id = bead::door::file_work(&self.lc, &self.persona, &site.name, title, &Self::body(body))?;
        let _ = bead::door::label_add(&self.lc, &id, LABEL);
        Ok(id)
    }

    fn amend_bump(&self, open: &Open, body: &str, target: &str) -> Result<(), String> {
        bead::door::amend(&self.lc, &open.id, bead::door::Change::Replace(&Self::body(body)), &format!("kit-bump: target moved to {target}"))
    }
}

#[cfg(test)]
mod store_client_tests {
    use super::*;
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};

    /// A content door that files `sp-new1`, accepts an update and a note, and refuses every other verb.
    pub struct Scripted(pub Mutex<Vec<String>>);

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.join(" "));
            let reply = |code, out: &str| Ok(Reply { code, out: out.into(), err: String::new(), read_ended: false });
            match argv.get(1).map(String::as_str) {
                Some("create") => reply(0, "sp-new1\n"),
                Some("update" | "note") => reply(0, ""),
                _ => reply(2, "cannot tell: down"),
            }
        }
    }

    pub fn client() -> (LcClient, Arc<Scripted>) {
        let t = Arc::new(Scripted(Mutex::default()));
        (LcClient::over(t.clone(), true), t)
    }

    #[test_id(t40bq55)]
    #[test]
    fn filing_a_bump_answers_the_filed_id_and_writes_no_file() {
        let config = testkit::TempDir::new("kit-bump-filing-config");
        let runes = spira_config::process::fixture_home().join("runes").display().to_string();
        let toml = spira_config::process::fixture_toml(config.path(), &[("SPIRA_RUNES_DIR", &runes)]).display().to_string();
        let _env = testkit::env(&[("SPIRA_TOML", Some(&toml))]);
        let dir = testkit::TempDir::new("kit-bump-no-file");
        let files = || std::fs::read_dir(dir.path()).unwrap().count();
        std::fs::write(dir.path().join("planted"), "").unwrap();
        assert_eq!(files(), 1, "the count sees a file");
        std::fs::remove_file(dir.path().join("planted")).unwrap();
        let (lc, t) = client();
        let real = Real::new("k".into(), "builder".into(), lc);
        let site = Site { name: "app".into(), path: String::new(), git_ref: String::new(), file: String::new() };
        let before = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let filed = real.file_bump(&site, "update k lockfile to 1.2.0 in app", "text");
        std::env::set_current_dir(before).unwrap();
        assert!(t.0.lock().unwrap().iter().any(|c| c.starts_with("content create ")), "the door was not asked to file: {filed:?}");
        assert_eq!(filed, Ok("sp-new1".to_string()));
        assert_eq!(files(), 0);
    }

    #[test_id(tjq6tp1)]
    #[test]
    fn amending_a_bump_rewrites_the_open_bead_and_notes_the_new_target() {
        let (lc, t) = client();
        let real = Real::new("k".into(), "builder".into(), lc);
        assert_eq!(real.amend_bump(&Open { id: "sp-open".into() }, "text", "1.3.0"), Ok(()));
        let sent = t.0.lock().unwrap();
        assert!(sent[0].starts_with("content update sp-open "), "{sent:?}");
        assert!(sent[1].starts_with("content note sp-open "), "{sent:?}");
        assert_eq!(sent.len(), 2);
    }

}

#[cfg(test)]
mod no_bead_fork {
    #[test_id(tesmdjf)]
    #[test]
    fn this_crate_files_through_the_content_door_and_forks_no_bead_tool() {
        let found = bead::door::forks_a_bead_tool_under(&testkit::manifest_dir().join("src"));
        assert!(found.is_empty(), "a bead tool is forked:\n{}", found.join("\n"));
    }
}
