use super::*;
use std::cell::RefCell;

const NOW: i64 = 1_791_576_000;

fn site(n: &str) -> Site {
    Site { name: n.into(), path: format!("/s/{n}"), git_ref: "main".into(), file: "Cargo.toml".into() }
}

fn commit(sha: &str, subject: &str, ago_h: i64) -> Commit {
    Commit { sha: sha.into(), subject: subject.into(), at: NOW - ago_h * 3600 }
}

#[derive(Default)]
struct Fake {
    tip: RefCell<String>,
    pins: BTreeMap<String, String>,
    history: RefCell<Vec<Commit>>,
    open: RefCell<BTreeMap<String, Open>>,
    filed: RefCell<Vec<(String, String)>>,
    amended: RefCell<Vec<(String, String)>>,
}

impl Fake {
    fn land(&self, c: Commit) {
        *self.tip.borrow_mut() = c.sha.clone();
        self.history.borrow_mut().push(c);
    }
}

impl World for Fake {
    fn kit_tip(&self) -> Result<String, String> {
        Ok(self.tip.borrow().clone())
    }
    fn site_pin(&self, s: &Site) -> Result<String, String> {
        self.pins.get(&s.name).cloned().ok_or_else(|| "no pin".into())
    }
    fn is_ancestor(&self, a: &str, d: &str) -> bool {
        let h = self.history.borrow();
        let pos = |x: &str| h.iter().position(|c| c.sha == x);
        matches!((pos(a), pos(d)), (Some(i), Some(j)) if i <= j)
    }
    fn commits(&self, pin: &str, tip: &str) -> Vec<Commit> {
        let h = self.history.borrow();
        let (i, j) = (h.iter().position(|c| c.sha == pin), h.iter().position(|c| c.sha == tip));
        match (i, j) {
            (Some(i), Some(j)) if i < j => h[i + 1..=j].to_vec(),
            _ => vec![],
        }
    }
    fn open_bump(&self, s: &Site) -> Result<Option<Open>, String> {
        Ok(self.open.borrow().get(&s.name).cloned())
    }
    fn file_bump(&self, s: &Site, t: &str, body: &str, target: &str) -> Result<String, String> {
        let id = format!("sp-bump{}", self.open.borrow().len() + self.filed.borrow().len());
        self.filed.borrow_mut().push((t.into(), body.into()));
        self.open.borrow_mut().insert(s.name.clone(), Open { id: id.clone(), target: target.into() });
        Ok(id)
    }
    fn amend_bump(&self, s: &Site, o: &Open, body: &str, target: &str) -> Result<(), String> {
        self.amended.borrow_mut().push((o.id.clone(), body.into()));
        self.open.borrow_mut().insert(s.name.clone(), Open { id: o.id.clone(), target: target.into() });
        Ok(())
    }
    fn now(&self) -> i64 {
        NOW
    }
}

fn fixture() -> Fake {
    let f = Fake::default();
    f.land(commit("k0", "sp-old: base", 100));
    let mut pins = BTreeMap::new();
    pins.insert("dumpstar".to_string(), "k0".to_string());
    pins.insert("pokedumpster".to_string(), "k0".to_string());
    pins.insert("current".to_string(), "k0".to_string());
    Fake { pins, ..f }
}

#[test]
fn a_landing_files_exactly_one_bump_per_behind_site_and_a_second_landing_amends_it() {
    let f = fixture();
    let sites = [site("dumpstar"), site("pokedumpster")];
    f.land(commit("k1", "sp-em45o0: fix the walk", 5));
    let o = pass(&f, &sites, "dumpster-kit").unwrap();
    assert_eq!(o.filed, ["dumpstar", "pokedumpster"]);
    assert_eq!(f.filed.borrow().len(), 2);
    assert!(f.filed.borrow()[0].1.contains("sp-em45o0"));
    assert_eq!(f.filed.borrow()[0].0, "bump dumpster-kit pin in dumpstar");

    let again = pass(&f, &sites, "dumpster-kit").unwrap();
    assert!(again.filed.is_empty() && again.amended.is_empty(), "{again:?}");

    f.land(commit("k2", "sp-xg4u9v: another fix", 1));
    let o = pass(&f, &sites, "dumpster-kit").unwrap();
    assert!(o.filed.is_empty(), "a second landing must not file another");
    assert_eq!(o.amended, ["dumpstar", "pokedumpster"]);
    assert_eq!(f.filed.borrow().len(), 2);
    let amended = f.amended.borrow();
    assert!(amended[0].1.contains("sp-em45o0") && amended[0].1.contains("sp-xg4u9v"), "{}", amended[0].1);
    assert_eq!(f.open.borrow()["dumpstar"].target, "k2");
}

#[test]
fn a_site_already_at_or_past_the_tip_gets_no_bump() {
    let f = fixture();
    f.land(commit("k1", "sp-a: x", 2));
    let mut f = f;
    f.pins.insert("current".into(), "k1".into());
    let o = pass(&f, &[site("current")], "dumpster-kit").unwrap();
    assert_eq!(o.current, ["current"]);
    assert!(f.filed.borrow().is_empty());
}

#[test]
fn lag_is_hours_since_the_oldest_unpinned_commit() {
    let f = fixture();
    f.land(commit("k1", "sp-a: x", 10));
    f.land(commit("k2", "sp-b: y", 1));
    let o = pass(&f, &[site("dumpstar")], "dumpster-kit").unwrap();
    assert_eq!(o.lag_hours["dumpstar"], 10.0);
}

#[test]
fn a_site_whose_pin_cannot_be_read_fails_alone() {
    let f = fixture();
    f.land(commit("k1", "sp-a: x", 2));
    let o = pass(&f, &[site("nopin"), site("dumpstar")], "dumpster-kit").unwrap();
    assert_eq!(o.failed.len(), 1);
    assert_eq!(o.filed, ["dumpstar"]);
}

#[test]
fn the_pin_is_the_rev_on_the_line_naming_the_crate() {
    let m = "[dependencies]\nserde = { version = \"1\", rev = \"zzz\" }\ndumpster-kit = { git = \"https://x/y\", rev = \"0123abcd\" }\n";
    assert_eq!(parse_pin(m, "dumpster-kit").as_deref(), Some("0123abcd"));
    assert_eq!(parse_pin("dumpster-kit = \"1\"", "dumpster-kit"), None);
    assert_eq!(parse_pin("dumpster-kit = { rev = \"not hex!\" }", "dumpster-kit"), None);
}

#[test]
fn carried_beads_are_named_once_in_first_seen_order() {
    let cs = [commit("a", "sp-aaa1: x (sp-bbb2)", 3), commit("b", "spira: land sp-aaa1 and sp-ccc3", 2), commit("c", "unsp-ddd4 nope", 1)];
    assert_eq!(carried_beads(&cs), ["sp-aaa1", "sp-bbb2", "sp-ccc3"]);
}

mod git {
    use super::*;
    use crate::real::Real;
    use std::process::Command;

    fn g(dir: &std::path::Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn repo(t: &testkit::TempDir, name: &str) -> std::path::PathBuf {
        let d = t.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        g(&d, &["init", "-q", "-b", "main"]);
        d
    }

    #[test]
    fn real_git_reads_the_tip_the_pin_and_the_commits_between() {
        let t = testkit::TempDir::new("kit-bump");
        let kit = repo(&t, "kit");
        std::fs::write(kit.join("a"), "1").unwrap();
        g(&kit, &["add", "."]);
        g(&kit, &["commit", "-qm", "sp-base1: base"]);
        let base = g(&kit, &["rev-parse", "HEAD"]);
        std::fs::write(kit.join("a"), "2").unwrap();
        g(&kit, &["commit", "-qam", "sp-next2: fix"]);
        let tip = g(&kit, &["rev-parse", "HEAD"]);

        let s = repo(&t, "site");
        std::fs::write(s.join("Cargo.toml"), format!("dumpster-kit = {{ git = \"u\", rev = \"{base}\" }}\n")).unwrap();
        g(&s, &["add", "."]);
        g(&s, &["commit", "-qm", "pin"]);

        let real = Real::new(kit.to_string_lossy().into(), "main".into(), "dumpster-kit".into(), kit.join("state"), "builder".into());
        let site = Site { name: "site".into(), path: s.to_string_lossy().into(), git_ref: "main".into(), file: "Cargo.toml".into() };
        assert_eq!(real.kit_tip().unwrap(), tip);
        let pin = real.site_pin(&site).unwrap();
        assert_eq!(pin, base);
        assert!(real.is_ancestor(&pin, &tip) && !real.is_ancestor(&tip, &pin));
        let cs = real.commits(&pin, &tip);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].subject, "sp-next2: fix");
        assert_eq!(real.commits("deadbeef", &tip), vec![]);
    }
}
