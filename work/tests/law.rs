//! `work law` follows pointers: a superseded number, a merged or retired slug.

use std::path::Path;
use std::process::Command;

use rule::code::{parse_pointers, parse_toc, render_pointers, write_tree, generate_with, Pointers};

const TOC: &str = "[[part]]\nnumber = 1\nname = \"Alpha\"\nslug = \"alpha\"\nscope = \"First.\"\nslugs = [\"law-one\"]\n\n[[part]]\nnumber = 2\nname = \"Beta\"\nslug = \"beta\"\nscope = \"Second.\"\nslugs = [\"law-three\", \"law-two\"]\n";

fn text(slug: &str) -> Option<String> {
    match slug {
        "law-one" => Some("Do one.".into()),
        "law-two" => Some("Do two. Twice.".into()),
        "law-three" => Some("Do three.".into()),
        _ => None,
    }
}

fn worktree(pointers: &str) -> testkit::TempDir {
    let dir = testkit::TempDir::new("work-law-ptr");
    Command::new("git").arg("-C").arg(dir.path()).args(["init", "-q"]).status().unwrap();
    let toc = parse_toc(TOC).unwrap();
    let p: Pointers = parse_pointers(pointers).unwrap();
    let tree = generate_with(&toc, &p, text);
    write_tree(&dir.join("law"), TOC, &tree).unwrap();
    std::fs::write(dir.join("law/pointers.toml"), render_pointers(&p)).unwrap();
    dir
}

fn law(dir: &Path, arg: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_work")).args(["law", arg]).current_dir(dir).env_clear().env("PATH", std::env::var("PATH").unwrap()).output().unwrap();
    (out.status.code().unwrap(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

const MOVED: &str = "version = 1\n\n[[moved]]\nsection = \"1.2\"\nslug = \"law-two\"\non = \"2026-10-28\"\n";

#[test]
fn a_superseded_number_prints_its_hop_then_the_statute() {
    let d = worktree(MOVED);
    for arg in ["1.2", "§1.2"] {
        let (code, out, err) = law(d.path(), arg);
        assert_eq!(code, 0, "{arg}: {err}");
        assert!(out.starts_with("§1.2 moved: law-two is now §2.2\n"), "{out}");
        assert!(out.contains("Do two. Twice."), "{out}");
    }
}

#[test]
fn a_chain_prints_one_line_per_hop_and_ends_at_the_current_section() {
    let d = worktree("version = 1\n\n[[moved]]\nsection = \"1.2\"\nslug = \"law-one\"\non = \"d\"\n\n[[merged]]\nfrom = \"law-one\"\ninto = \"law-two\"\nreason = \"Same rule.\"\non = \"d\"\n");
    let (code, out, err) = law(d.path(), "1.2");
    assert_eq!(code, 0, "{err}");
    let lines: Vec<&str> = out.lines().take(2).collect();
    assert_eq!(lines, ["§1.2 moved: law-one is now §1.1", "law-one merged into law-two (§2.2): Same rule."]);
    assert!(out.contains("# §2.2 law-two") && out.contains("Do two. Twice."), "{out}");
}

#[test]
fn a_retired_statute_prints_its_reason_by_slug_and_by_number() {
    let d = worktree("version = 1\n\n[[retired]]\nslug = \"law-three\"\nreason = \"Obsolete.\"\non = \"2026-10-28\"\n");
    for arg in ["law-three", "2.1"] {
        let (code, out, err) = law(d.path(), arg);
        assert_eq!(code, 0, "{arg}: {err}");
        assert!(out.contains("[Retired 2026-10-28] Obsolete."), "{out}");
    }
}

#[test]
fn a_pointer_cycle_is_refused_by_name() {
    let d = worktree("version = 1\n\n[[merged]]\nfrom = \"law-one\"\ninto = \"law-two\"\nreason = \"r\"\non = \"d\"\n\n[[merged]]\nfrom = \"law-two\"\ninto = \"law-one\"\nreason = \"r\"\non = \"d\"\n");
    let (code, _, err) = law(d.path(), "law-one");
    assert_ne!(code, 0);
    assert!(err.contains("cycle") && err.contains("law-one") && err.contains("law-two"), "{err}");
}

#[test]
fn what_nothing_ever_held_still_refuses_with_the_parts() {
    let d = worktree(MOVED);
    for arg in ["law-nope", "9.1"] {
        let (code, out, err) = law(d.path(), arg);
        assert_ne!(code, 0, "{arg}");
        assert!(err.contains("no Part or section"), "{err}");
        assert!(out.contains("Statute code") || err.contains("Statute code"), "{out}{err}");
    }
}

#[test]
fn a_current_section_and_a_slug_print_what_they_always_did() {
    let d = worktree(MOVED);
    let (_, by_number, _) = law(d.path(), "2.2");
    let (_, by_slug, _) = law(d.path(), "law-two");
    assert_eq!(by_number, "# §2.2 law-two\n\nDo two. Twice.\n");
    assert_eq!(by_slug, by_number);
}
