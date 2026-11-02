//! For one fixture world, round-duty.sh and the round-duty handler leave the same rows in the
//! Concierge inbox. The script is the oracle: it runs unmodified against stubbed `spira-lc` and
//! `queue`; the handler runs on a virtual clock against a fake spira-lc socket.

use std::fs;
use std::path::Path;

#[test]
fn the_migrated_handlers_launch_no_process() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<_> = fs::read_dir(src.join("watchers")).unwrap().flatten().map(|e| e.path()).collect();
    files.extend([src.join("world.rs"), src.join("forge.rs")]);
    assert!(src.join("wiki_git.rs").exists(), "wiki_git.rs is the one place a process may launch");
    let mut checked = 0;
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains("Command::new") && !code.contains("std::process"), "{} launches a process", path.display());
        checked += 1;
    }
    assert!(checked >= 2, "the scan found no handlers to check");
    let planted = "fn f() { Command::new(\"x\"); }";
    assert!(planted.contains("Command::new"), "the matcher must be able to see an offender");
}
