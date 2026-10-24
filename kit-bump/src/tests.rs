use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

fn site(n: &str) -> Site {
    Site { name: n.into(), path: format!("/s/{n}"), git_ref: "main".into(), file: "Cargo.toml".into() }
}

#[derive(Default)]
struct Fake {
    release: String,
    reqs: BTreeMap<String, String>,
    locked: BTreeMap<String, String>,
    open: RefCell<BTreeMap<String, Open>>,
    filed: RefCell<Vec<(String, String)>>,
    amended: RefCell<Vec<(String, String)>>,
}

impl World for Fake {
    fn latest_release(&self) -> Result<String, String> {
        Ok(self.release.clone())
    }
    fn site_requirement(&self, s: &Site) -> Result<String, String> {
        self.reqs.get(&s.name).cloned().ok_or_else(|| "no requirement".into())
    }
    fn site_locked(&self, s: &Site) -> Result<String, String> {
        self.locked.get(&s.name).cloned().ok_or_else(|| "no lock".into())
    }
    fn open_bump(&self, s: &Site) -> Result<Option<Open>, String> {
        Ok(self.open.borrow().get(&s.name).cloned())
    }
    fn file_bump(&self, s: &Site, t: &str, body: &str, target: &str) -> Result<String, String> {
        let id = format!("sp-bump{}", self.filed.borrow().len());
        self.filed.borrow_mut().push((t.into(), body.into()));
        self.open.borrow_mut().insert(s.name.clone(), Open { id: id.clone(), target: target.into() });
        Ok(id)
    }
    fn amend_bump(&self, s: &Site, o: &Open, body: &str, target: &str) -> Result<(), String> {
        self.amended.borrow_mut().push((o.id.clone(), body.into()));
        self.open.borrow_mut().insert(s.name.clone(), Open { id: o.id.clone(), target: target.into() });
        Ok(())
    }
}

fn fixture(release: &str) -> Fake {
    let mut f = Fake { release: release.into(), ..Fake::default() };
    for n in ["dumpstar", "pokedumpster", "current"] {
        f.reqs.insert(n.into(), "0.3".into());
        f.locked.insert(n.into(), "0.3.0".into());
    }
    f.locked.insert("current".into(), release.into());
    f
}

#[test]
fn a_new_patch_release_files_one_lockfile_bead_per_site_and_a_newer_one_amends_it() {
    let f = fixture("0.3.1");
    let sites = [site("dumpstar"), site("pokedumpster"), site("current")];
    let o = pass(&f, &sites, "dumpster-kit").unwrap();
    assert_eq!(o.filed, ["dumpstar", "pokedumpster"]);
    assert_eq!(o.current, ["current"]);
    let filed = f.filed.borrow();
    assert_eq!(filed[0].0, "update dumpster-kit lockfile to 0.3.1 in dumpstar");
    assert!(filed[0].1.contains("cargo update -p dumpster-kit --precise 0.3.1"), "{}", filed[0].1);
    drop(filed);

    let again = pass(&f, &sites, "dumpster-kit").unwrap();
    assert!(again.filed.is_empty() && again.amended.is_empty(), "{again:?}");

    let f = Fake { release: "0.3.2".into(), ..f };
    let o = pass(&f, &sites[..2], "dumpster-kit").unwrap();
    assert!(o.filed.is_empty(), "a second release must not file another");
    assert_eq!(o.amended, ["dumpstar", "pokedumpster"]);
    assert_eq!(f.open.borrow()["dumpstar"].target, "0.3.2");
}

#[test]
fn a_kit_commit_with_no_release_files_nothing() {
    let f = fixture("0.3.0");
    let o = pass(&f, &[site("dumpstar"), site("pokedumpster")], "dumpster-kit").unwrap();
    assert_eq!(o.current, ["dumpstar", "pokedumpster"]);
    assert!(o.filed.is_empty() && o.amended.is_empty() && f.filed.borrow().is_empty());
}

#[test]
fn a_release_outside_the_range_files_a_bead_naming_the_breaking_upgrade() {
    let f = fixture("0.4.0");
    let o = pass(&f, &[site("dumpstar")], "dumpster-kit").unwrap();
    assert_eq!(o.filed, ["dumpstar"]);
    let filed = f.filed.borrow();
    assert_eq!(filed[0].0, "upgrade dumpster-kit to 0.4.0 (breaking) in dumpstar");
    assert!(filed[0].1.contains("breaking") && filed[0].1.contains("`0.3`"), "{}", filed[0].1);
}

#[test]
fn a_site_whose_lock_cannot_be_read_fails_alone() {
    let f = fixture("0.3.1");
    let o = pass(&f, &[site("nolock"), site("dumpstar")], "dumpster-kit").unwrap();
    assert_eq!(o.failed.len(), 1);
    assert_eq!(o.filed, ["dumpstar"]);
}

#[test]
fn requirements_follow_cargos_ranges() {
    let cases = [
        ("0.3", "0.3.9", true),
        ("0.3", "0.4.0", false),
        ("1", "1.9.0", true),
        ("1", "2.0.0", false),
        ("^1.2.3", "1.2.2", false),
        ("~1.2", "1.2.9", true),
        ("~1.2", "1.3.0", false),
        ("=1.2.3", "1.2.4", false),
        (">=1.2, <1.5", "1.4.9", true),
        (">=1.2, <1.5", "1.5.0", false),
        ("0.0.3", "0.0.4", false),
        ("*", "9.9.9", true),
        ("1.*", "1.7.0", true),
    ];
    for (req, v, want) in cases {
        assert_eq!(semver::admits(req, v), Some(want), "{req} vs {v}");
    }
    assert_eq!(semver::admits("not a req", "1.0.0"), None);
}

#[test]
fn the_requirement_and_lock_are_read_from_the_lines_naming_the_crate() {
    let m = "[dependencies]\nserde = \"9\"\ndumpster-kit-extra = \"7\"\ndumpster-kit = { version = \"0.3\", registry = \"r\" }\n";
    assert_eq!(parse_requirement(m, "dumpster-kit").as_deref(), Some("0.3"));
    assert_eq!(parse_requirement("dumpster-kit = \"1.2\"\n", "dumpster-kit").as_deref(), Some("1.2"));
    assert_eq!(parse_requirement("dumpster-kit = { git = \"u\" }", "dumpster-kit"), None);
    let lock = "[[package]]\nname = \"dumpster-kit-extra\"\nversion = \"7.0.0\"\n\n[[package]]\nname = \"dumpster-kit\"\nversion = \"0.3.4\"\n";
    assert_eq!(parse_locked(lock, "dumpster-kit").as_deref(), Some("0.3.4"));
    assert_eq!(parse_locked(lock, "absent"), None);
}

#[test]
fn the_newest_release_ignores_yanked_and_pre_release_versions() {
    let idx = "{\"vers\":\"0.3.0\"}\n{\"vers\":\"0.10.0\"}\n{\"vers\":\"0.11.0\",\"yanked\":true}\n{\"vers\":\"0.12.0-rc.1\"}\n";
    assert_eq!(newest_release(idx).as_deref(), Some("0.10.0"));
    assert_eq!(newest_release("garbage"), None);
}

mod registry {
    use super::*;
    use crate::real::{index_path, Real};

    #[test]
    fn index_paths_follow_the_sparse_layout() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("abc"), "3/a/abc");
        assert_eq!(index_path("Dumpster-Kit"), "du/mp/dumpster-kit");
    }

    fn g(dir: &std::path::Path, args: &[&str]) {
        let o = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    #[test]
    fn the_real_world_reads_a_stub_registry_and_a_sites_manifest_and_lock() {
        let t = testkit::TempDir::new("kit-bump");
        let reg = t.path().join("reg");
        std::fs::create_dir_all(reg.join("du/mp")).unwrap();
        std::fs::write(reg.join("du/mp/dumpster-kit"), "{\"vers\":\"0.3.0\"}\n{\"vers\":\"0.3.1\"}\n").unwrap();

        let s = t.path().join("site");
        std::fs::create_dir_all(&s).unwrap();
        g(&s, &["init", "-q", "-b", "main"]);
        std::fs::write(s.join("Cargo.toml"), "dumpster-kit = { version = \"0.3\", registry = \"tailnet\" }\n").unwrap();
        std::fs::write(s.join("Cargo.lock"), "[[package]]\nname = \"dumpster-kit\"\nversion = \"0.3.0\"\n").unwrap();
        g(&s, &["add", "."]);
        g(&s, &["commit", "-qm", "site"]);

        let real = Real::new(format!("file://{}", reg.display()), "dumpster-kit".into(), t.path().join("state"), "builder".into());
        let site = Site { name: "site".into(), path: s.to_string_lossy().into(), git_ref: "main".into(), file: "Cargo.toml".into() };
        assert_eq!(real.latest_release().unwrap(), "0.3.1");
        assert_eq!(real.site_requirement(&site).unwrap(), "0.3");
        assert_eq!(real.site_locked(&site).unwrap(), "0.3.0");
    }
}
