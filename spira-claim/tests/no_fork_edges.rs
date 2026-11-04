//! law-a-loops-hot-read-is-in-process: spira-claim reaches the store through `LcClient` and
//! units through the `Units` port, never by forking `spira-lc`, `systemctl` and friends. A
//! child that must stay (a verb the socket does not serve) carries a `FORK-EDGE` comment on
//! its line or the line above.

/// Programs that are Spira (or a unit manager) and must not be a child of this crate.
const BANNED: &[&str] = &["\"spira-lc\"", "\"systemctl\"", "\"systemd-run\"", "\"slay\"", "\"tsd-write\"", "\"mail\"", "\"bash\""];
/// The same programs held in a field or a variable: `Command::new(&self.lc)`, `&systemctl`.
const BANNED_HOLDERS: &[&str] = &[".lc)", ".lc_bin)", "lc)", "lc_bin)", "systemctl)", "slay)", "systemd_run)"];

/// Every `file:line` in `src` that forks a banned program without a `FORK-EDGE` mark.
fn unmarked(file: &str, src: &str) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut bad = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(at) = line.find("Command::new(") else { continue };
        let rest = &line[at + "Command::new(".len()..];
        let target = rest.split(';').next().unwrap_or(rest);
        let hit = BANNED.iter().any(|b| target.starts_with(b) || target.starts_with(&format!("&{b}")))
            || BANNED_HOLDERS.iter().any(|h| target.split_once(')').is_some_and(|(arg, _)| format!("{arg})").ends_with(h)));
        let marked = line.contains("FORK-EDGE") || (i > 0 && lines[i - 1].contains("FORK-EDGE"));
        if hit && !marked {
            bad.push(format!("{file}:{}: {}", i + 1, line.trim()));
        }
    }
    bad
}

#[test]
fn no_spira_child_is_forked_without_a_fork_edge_mark() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut bad = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "rs") {
            let src = std::fs::read_to_string(&p).unwrap();
            bad.extend(unmarked(&p.file_name().unwrap().to_string_lossy(), &src));
        }
    }
    assert!(bad.is_empty(), "Spira children forked without a FORK-EDGE mark:\n{}", bad.join("\n"));
}

#[test]
fn the_scan_catches_a_planted_site_and_honours_the_mark() {
    let planted = "fn f() {\n    let mut c = Command::new(&self.store.lc);\n    let _ = std::process::Command::new(\"systemctl\").status();\n    let _ = Command::new(&systemctl);\n    let _ = Command::new(\"git\");\n}\n";
    assert_eq!(unmarked("planted.rs", planted).len(), 3, "{:?}", unmarked("planted.rs", planted));
    let marked = "    // FORK-EDGE: unserved verb close\n    let mut c = Command::new(&self.lc_bin);\n    let d = Command::new(\"bash\"); // FORK-EDGE: needs x\n";
    assert!(unmarked("marked.rs", marked).is_empty());
}
