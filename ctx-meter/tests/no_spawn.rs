//! One status line refresh is one process that starts no other: no source here may start a
//! child. The positive control plants each spelling and requires the scan to catch it, so a
//! scan that matches nothing cannot pass.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};

const SPAWNS: [&str; 9] = ["Command::new", "process::Command", "bounded::", "posix_spawn", "libc::fork", "libc::system", "libc::exec", ".spawn(", "tokio::process"];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn spawns(name: &str, text: &str) -> Vec<String> {
    text.lines().enumerate().filter(|(_, l)| SPAWNS.iter().any(|s| l.contains(s))).map(|(i, _)| format!("{name}:{}", i + 1)).collect()
}

#[test_id(tb4thkl)]
#[test]
fn no_source_of_the_meter_starts_a_process() {
    let mut files = Vec::new();
    rs_files(&testkit::manifest_dir().join("src"), &mut files);
    assert!(files.iter().any(|f| f.ends_with("main.rs")) && files.iter().any(|f| f.ends_with("lib.rs")), "the scan read the wrong files");
    let found: Vec<String> = files.iter().flat_map(|f| spawns(&f.display().to_string(), &std::fs::read_to_string(f).unwrap())).collect();
    assert!(found.is_empty(), "the meter starts a child:\n{}", found.join("\n"));
}

#[test_id(t77k5qe)]
#[test]
fn the_scan_catches_every_planted_spelling_of_a_spawn() {
    for planted in ["let o = std::process::Command::new(\"git\");", "use std::process::Command;", "spira_config::bounded::bounded(\"x\")", "libc::fork();", "child.spawn()", "libc::system(c)", "libc::execv(p, a)", "posix_spawn(..)", "tokio::process::Command"] {
        assert_eq!(spawns("p.rs", &format!("fn f() {{\n{planted}\n}}\n")), vec!["p.rs:2".to_string()], "{planted}");
    }
    assert!(spawns("ok.rs", "let id = std::process::id();\nuse std::process::ExitCode;\n").is_empty());
}
