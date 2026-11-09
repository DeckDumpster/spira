//! T4, second half: a task module launches no process. Every module under `src/watchers/`, the
//! task table, any module under `src/tasks/`, and the world, forge and reconcile host they read through.

use std::fs;
use std::path::{Path, PathBuf};

const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

/// The first launcher `code` names, outside its `#[cfg(test)]` tail.
fn launcher(text: &str) -> Option<&'static str> {
    let code = text.split("#[cfg(test)]").next().unwrap_or_default();
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    let mut out = vec![];
    for p in rd.flatten().map(|e| e.path()) {
        if p.is_dir() {
            out.extend(rs_files(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out
}

#[test]
fn the_matcher_sees_every_launcher_and_ignores_test_code() {
    for l in LAUNCHERS {
        assert_eq!(launcher(&format!("fn f() {{ {l}(\"x\"); }}")), Some(*l), "the matcher must see {l}");
    }
    assert_eq!(launcher("fn f() {}\n#[cfg(test)]\nmod t { fn g() { std::process::Command::new(\"x\"); } }"), None);
}

#[test]
fn no_task_module_launches_a_process() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = rs_files(&src.join("watchers"));
    files.extend(rs_files(&src.join("tasks")));
    files.extend([src.join("tasks.rs"), src.join("world.rs"), src.join("forge.rs"), src.join("reconcile_host.rs")].into_iter().filter(|p| p.exists()));
    assert!(files.iter().any(|p| p.ends_with("reconcile_host.rs")), "the reconciler's host reads are scanned too");
    assert!(files.len() >= 17 && files.iter().any(|p| p.ends_with("watchers/asks.rs")) && files.iter().any(|p| p.ends_with("tasks.rs")), "the scan must find the task modules: {files:?}");
    let offenders: Vec<String> = files.iter().filter_map(|p| launcher(&fs::read_to_string(p).unwrap()).map(|l| format!("{}: {l}", p.display()))).collect();
    assert!(offenders.is_empty(), "task modules that launch a process: {offenders:?}");
}
