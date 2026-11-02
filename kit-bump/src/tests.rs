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
    fn latest_release(&self) -> Result<Option<String>, String> {
        Ok((!self.release.is_empty()).then(|| self.release.clone()))
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
    use crate::real::index_path;

    #[test]
    fn index_paths_follow_the_sparse_layout() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("abc"), "3/a/abc");
        assert_eq!(index_path("Dumpster-Kit"), "du/mp/dumpster-kit");
    }

    #[test]
    fn a_404_index_is_no_release_yet_and_other_failures_are_errors() {
        use crate::real::index_body;
        assert_eq!(index_body("{\"vers\":\"0.1.0\"}\n200", "u").unwrap(), Some("{\"vers\":\"0.1.0\"}"));
        assert_eq!(index_body("not found\n404", "u").unwrap(), None);
        assert!(index_body("oops\n500", "u").is_err());
        assert!(index_body("", "u").is_err());
    }

    #[test]
    fn a_pass_over_an_unpublished_crate_files_nothing_and_says_so() {
        let o = pass(&Fake::default(), &[site("dumpstar")], "dumpster-kit").unwrap();
        assert!(o.unreleased);
        assert_eq!(o, Outcome { unreleased: true, ..Outcome::default() });
    }
}
