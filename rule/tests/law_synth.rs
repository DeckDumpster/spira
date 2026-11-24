// Exception to law-a-unit-test-calls-no-binary: the script is the subject. spira/law-synth.sh
// runs under `bash` with its `conf.sh` line replaced by exported values, a stub `spira-lc`
// serving the statute book, and real git and python3 over a throwaway wiki.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = include_str!("../../spira/law-synth.sh");
const PAGE: &str = "wiki/notes/common-law.md";

struct Fx {
    _tmp: testkit::TempDir,
    root: PathBuf,
}

struct Out {
    code: Option<i32>,
    text: String,
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).status().unwrap().success();
    assert!(ok, "git {args:?}");
}

fn book(laws: usize) -> String {
    let mut m: Vec<String> = vec![r#""schema_version": 1"#.into(), r#""sop-x": "A procedure.""#.into()];
    m.extend((1..=laws).map(|i| format!(r#""law-seed-{i}": "Seed statute {i}.""#)));
    format!("{{{}}}", m.join(","))
}

/// A wiki whose committed page holds `committed` statute headings, and a book of `laws`.
fn fixture(laws: usize, committed: usize) -> Fx {
    let tmp = testkit::TempDir::new("law-synth");
    let root = tmp.path().to_path_buf();
    let wiki = root.join("wiki");
    std::fs::create_dir_all(wiki.join("wiki/notes")).unwrap();
    std::fs::create_dir_all(root.join("db/.beads")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    git(&wiki, &["init", "-q"]);
    let page: String = (1..=committed).map(|i| format!("### Statute {i}\n\nText.\n\n")).collect();
    std::fs::write(wiki.join(PAGE), page).unwrap();
    git(&wiki, &["add", PAGE]);
    git(&wiki, &["commit", "-q", "-m", "baseline"]);
    std::fs::write(root.join("book.json"), book(laws)).unwrap();
    testkit::write_exe(root.join("bin/spira-lc"), "#!/bin/bash\ncat \"$STUB_BOOK\"\n");
    let script = SCRIPT.replace(". \"$SPIRA_HOME_DIR/conf.sh\"", ":");
    std::fs::write(root.join("law-synth.sh"), script).unwrap();
    Fx { _tmp: tmp, root }
}

fn run(fx: &Fx, env: &[(&str, &str)]) -> Out {
    let path = format!("{}:{}", fx.root.join("bin").display(), std::env::var("PATH").unwrap());
    let mut c = Command::new("bash");
    c.arg(fx.root.join("law-synth.sh")).env_clear().env("PATH", path).env("HOME", &fx.root);
    c.env("SPIRA_DB", fx.root.join("db")).env("SPIRA_WIKI", fx.root.join("wiki"));
    c.env("SPIRA_STATUTE_PAGE", PAGE).env("SPIRA_STATUTE_SOURCE", "llm-wiki").env("STUB_BOOK", fx.root.join("book.json"));
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    Out { code: o.status.code(), text: format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)) }
}

fn page(fx: &Fx, rel: &str) -> String {
    std::fs::read_to_string(fx.root.join("wiki").join(rel)).unwrap_or_default()
}

// covers: UC-operator-channel-43
#[test_id(tv3tkfa)]
#[test]
fn a_book_of_the_committed_size_regenerates_the_page() {
    let fx = fixture(10, 10);
    let o = run(&fx, &[]);
    assert_eq!(o.code, Some(0), "{}", o.text);
    assert!(o.text.contains("wrote"), "{}", o.text);
    assert!(page(&fx, PAGE).contains("`law-seed-10`"));
}

// covers: UC-operator-channel-43
#[test_id(tybgoi0)]
#[test]
fn a_book_with_no_statutes_is_refused_and_the_page_is_untouched() {
    let fx = fixture(0, 10);
    let before = page(&fx, PAGE);
    let o = run(&fx, &[("LAW_SYNTH_OVERRIDE", "1")]);
    assert_ne!(o.code, Some(0), "{}", o.text);
    assert!(o.text.contains("refusing") && !o.text.contains("wrote"), "{}", o.text);
    assert_eq!(page(&fx, PAGE), before);
}

// covers: UC-operator-channel-43
#[test_id(ty5syd4)]
#[test]
fn a_book_under_half_the_committed_page_is_refused_unless_overridden() {
    let fx = fixture(3, 10);
    let before = page(&fx, PAGE);
    let o = run(&fx, &[]);
    assert_ne!(o.code, Some(0), "{}", o.text);
    assert!(o.text.contains("refusing"), "{}", o.text);
    assert_eq!(page(&fx, PAGE), before);
    let forced = run(&fx, &[("LAW_SYNTH_OVERRIDE", "1")]);
    assert_eq!(forced.code, Some(0), "{}", forced.text);
    assert_ne!(page(&fx, PAGE), before);
}

// covers: UC-operator-channel-43
#[test_id(t1ob7l9)]
#[test]
fn with_no_wiki_configured_it_writes_nothing_and_exits_zero() {
    let fx = fixture(10, 10);
    let before = page(&fx, PAGE);
    let o = run(&fx, &[("SPIRA_WIKI", "")]);
    assert_eq!(o.code, Some(0), "{}", o.text);
    assert!(o.text.contains("nothing to synthesise") && !o.text.contains("wrote"), "{}", o.text);
    assert_eq!(page(&fx, PAGE), before);
}

// covers: UC-operator-channel-43
#[test_id(tibffo7)]
#[test]
fn an_unknown_source_kind_and_a_page_outside_the_wiki_are_refused() {
    let fx = fixture(10, 10);
    let o = run(&fx, &[("SPIRA_STATUTE_SOURCE", "notion")]);
    assert_ne!(o.code, Some(0));
    assert!(o.text.contains("notion"), "{}", o.text);
    let o = run(&fx, &[("SPIRA_STATUTE_PAGE", "../escape.md")]);
    assert_ne!(o.code, Some(0));
    assert!(!fx.root.join("escape.md").exists());
    let custom = run(&fx, &[("SPIRA_STATUTE_PAGE", "wiki/other/statutes.md")]);
    assert_eq!(custom.code, Some(0), "{}", custom.text);
    assert!(page(&fx, "wiki/other/statutes.md").contains("law-seed-1"));
}
