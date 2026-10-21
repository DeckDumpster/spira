use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

const TODAY: &str = "2026-10-21";

#[derive(Default)]
struct Fake {
    files: BTreeMap<(String, String), String>,
    rust: Option<String>,
    node: Option<EolSupport>,
    py: Option<EolSupport>,
    open: RefCell<BTreeMap<String, String>>,
    filed: RefCell<Vec<(String, String)>>,
}

impl Fake {
    fn file(mut self, repo: &str, f: &str, text: &str) -> Fake {
        self.files.insert((repo.into(), f.into()), text.into());
        self
    }
}

impl World for Fake {
    fn repos(&self) -> Vec<Repo> {
        let mut names: Vec<String> = self.files.keys().map(|(r, _)| r.clone()).collect();
        names.dedup();
        names.into_iter().map(|name| Repo { name, path: "/r".into(), git_ref: "main".into() }).collect()
    }
    fn read(&self, repo: &Repo, file: &str) -> Result<Option<String>, String> {
        Ok(self.files.get(&(repo.name.clone(), file.into())).cloned())
    }
    fn naming(&self, repo: &Repo, needle: &str) -> Vec<String> {
        self.files.iter().filter(|((r, _), t)| *r == repo.name && t.contains(needle)).map(|((_, f), _)| f.clone()).chain([".github/ci.yml".to_string()]).collect()
    }
    fn rust(&self) -> Result<RustSupport, String> {
        self.rust.clone().map(|stable| RustSupport { stable }).ok_or_else(|| "static.rust-lang.org unreachable".into())
    }
    fn node(&self) -> Result<EolSupport, String> {
        self.node.clone().ok_or_else(|| "nodejs unreachable".into())
    }
    fn python(&self) -> Result<EolSupport, String> {
        self.py.clone().ok_or_else(|| "endoflife.date unreachable".into())
    }
    fn open_bump(&self, repo: &Repo, rt: Runtime) -> Result<Option<String>, String> {
        Ok(self.open.borrow().get(&format!("{} {}", repo.name, rt.name())).cloned())
    }
    fn file_bump(&self, repo: &Repo, rt: Runtime, title: &str, body: &str) -> Result<String, String> {
        self.open.borrow_mut().insert(format!("{} {}", repo.name, rt.name()), "sp-x".into());
        self.filed.borrow_mut().push((title.into(), body.into()));
        Ok("sp-x".into())
    }
    fn today(&self) -> String {
        TODAY.into()
    }
}

fn eol(pairs: &[(&str, &str)], target: &str) -> EolSupport {
    EolSupport { eol: pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), target: target.into() }
}

fn tc(v: &str) -> String {
    format!("[toolchain]\nchannel = \"{v}\"\n")
}

#[test]
fn a_rust_pin_two_releases_behind_files_one_bead_and_a_second_pass_files_nothing() {
    let mut w = Fake { rust: Some("1.94.0".into()), ..Default::default() }.file("old", "rust-toolchain.toml", &tc("1.92.0"));
    w.files.insert(("old".into(), "ci.yml".into()), "toolchain: 1.92.0".into());
    let o = pass(&w, 10);
    assert_eq!(o.filed, vec!["old rust"]);
    let filed = w.filed.borrow();
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0].0, "bump rust 1.92.0 to 1.94.0");
    assert!(filed[0].1.contains("- rust-toolchain.toml") && filed[0].1.contains("- ci.yml") && filed[0].1.contains("## Build"));
    drop(filed);
    let o = pass(&w, 10);
    assert!(o.filed.is_empty());
    assert_eq!(o.open, vec!["old rust"]);
    assert_eq!(w.filed.borrow().len(), 1);
}

#[test]
fn a_rust_pin_on_stable_or_inside_the_grace_files_nothing() {
    let w = Fake { rust: Some("1.94.0".into()), ..Default::default() }
        .file("current", "rust-toolchain.toml", &tc("1.94.0"))
        .file("grace", "rust-toolchain.toml", &tc("1.93.1"))
        .file("floating", "rust-toolchain.toml", &tc("stable"));
    let o = pass(&w, 10);
    assert!(o.filed.is_empty() && o.cannot_check.is_empty(), "{o:?}");
    assert_eq!(o.current, vec!["current rust", "grace rust"]);
}

#[test]
fn an_unreachable_source_is_cannot_check_and_files_nothing() {
    let w = Fake::default().file("old", "rust-toolchain.toml", &tc("1.50.0")).file("n", ".nvmrc", "14\n");
    let o = pass(&w, 10);
    assert!(o.filed.is_empty() && o.current.is_empty());
    assert_eq!(o.cannot_check.len(), 2, "{o:?}");
}

#[test]
fn a_node_pin_past_end_of_life_files_one_bead() {
    let node = eol(&[("18", "2025-04-30"), ("22", "2027-04-30")], "22.11.0");
    let w = Fake { node: Some(node), ..Default::default() }
        .file("web", ".nvmrc", "v18.20.0\n")
        .file("web", "package.json", "{\"engines\":{\"node\":\">=18\"}}")
        .file("ok", ".nvmrc", "22\n");
    let o = pass(&w, 10);
    assert_eq!(o.filed, vec!["web node"]);
    assert_eq!(o.current, vec!["ok node"]);
    let filed = w.filed.borrow();
    assert_eq!(filed[0].0, "bump node 18.20.0 to 22.11.0");
    assert!(filed[0].1.contains("`18` in `package.json`"));
}

#[test]
fn a_python_floor_past_end_of_life_files_one_bead() {
    let py = eol(&[("3.8", "2024-10-07"), ("3.13", "2029-10-31")], "3.13.1");
    let w = Fake { py: Some(py), ..Default::default() }.file("deck", "pyproject.toml", "requires-python = \">=3.8\"\n");
    assert_eq!(pass(&w, 10).filed, vec!["deck python"]);
}

#[test]
fn the_batch_is_bounded() {
    let mut w = Fake { rust: Some("1.94.0".into()), ..Default::default() };
    for r in ["a", "b", "c"] {
        w = w.file(r, "rust-toolchain.toml", &tc("1.80.0"));
    }
    assert_eq!(pass(&w, 2).filed.len(), 2);
}

#[test]
fn an_unlisted_node_line_is_cannot_check_not_supported() {
    let w = Fake { node: Some(eol(&[("22", "2027-04-30")], "22.1.0")), ..Default::default() }.file("w", ".nvmrc", "99\n");
    let o = pass(&w, 10);
    assert!(o.filed.is_empty() && o.current.is_empty());
    assert_eq!(o.cannot_check.len(), 1);
}

#[test]
fn parsers_read_the_upstream_shapes() {
    let toml = "[pkg.cargo]\nversion = \"1.0.0 (x)\"\n[pkg.rust]\nversion = \"1.94.0 (abc 2026-03-01)\"\n";
    assert_eq!(parse_rust_stable(toml).unwrap(), "1.94.0");
    assert!(parse_rust_stable("[pkg.cargo]\nversion = \"1.0.0\"\n").is_err());
    let n = parse_node(
        r#"{"v18":{"end":"2025-04-30"},"v20":{"end":"2026-04-30"},"v24":{"end":"2028-04-30"}}"#,
        r#"[{"version":"v25.0.0","lts":false},{"version":"v24.1.0","lts":"Krypton"},{"version":"v20.9.0","lts":"Iron"}]"#,
        TODAY,
    )
    .unwrap();
    assert_eq!(n.target, "24.1.0");
    let p = parse_python(r#"[{"cycle":"3.14","eol":"2030-10-31","latest":"3.14.0"},{"cycle":"3.9","eol":"2025-10-31","latest":"3.9.20"},{"cycle":"2.7","eol":false,"latest":"2.7.18"}]"#, TODAY).unwrap();
    assert_eq!(p.target, "3.14.0");
    assert!(parse_node("not json", "[]", TODAY).is_err());
}

#[test]
fn civil_date_is_the_calendar_date() {
    assert_eq!(civil_date(0), "1970-01-01");
    assert_eq!(civil_date(1_792_595_783), "2026-10-21");
    assert_eq!(civil_date(951_782_400), "2000-02-29");
}
