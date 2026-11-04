//! No Spira→Spira subprocess edge is left in this crate's sources except at a site that says
//! why (`FORK-EDGE:` on the line or the two above it): the static half of
//! law-a-loops-hot-read-is-in-process. The positive control plants an unmarked site and
//! requires the scan to catch it, so a scan that matches nothing cannot pass.

use std::path::{Path, PathBuf};

const BANNED: [&str; 10] = ["spira-lc", "bd", "bdq", "bash", "systemctl", "systemd-run", "tsd-write", "mail", "queue", "pgrep"];
/// The generic runners: a call through one of these names its child as its first argument.
const RUNNERS: [&str; 2] = ["Command::new(", "bounded::bounded("];

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

fn quoted_after<'a>(line: &'a str, open: &str) -> Option<&'a str> {
    let rest = &line[line.find(open)? + open.len()..];
    let rest = rest.trim_start().strip_prefix('"')?;
    Some(&rest[..rest.find('"')?])
}

fn banned_in(s: &str) -> bool {
    BANNED.iter().any(|b| s.contains(&format!("\"{b}\"")))
}

/// Every `file:line` that spawns a banned child with no `FORK-EDGE:` reason near it.
fn unmarked(name: &str, text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(open) = RUNNERS.iter().find(|r| line.contains(**r)) else { continue };
        let child = quoted_after(line, open);
        let hit = match child {
            Some(c) if BANNED.contains(&c) => true,
            // `timeout <secs> <child>`: the child is among the next few arguments.
            Some("timeout") => banned_in(&lines[i..(i + 4).min(lines.len())].join(" ")),
            _ => false,
        };
        let marked = lines[i.saturating_sub(2)..=i].iter().any(|l| l.contains("FORK-EDGE:"));
        if hit && !marked {
            found.push(format!("{name}:{}", i + 1));
        }
    }
    found
}

#[test]
fn no_unmarked_spira_fork_edge_remains_in_src() {
    let mut files = Vec::new();
    rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(!files.is_empty(), "the scan read no sources");
    let found: Vec<String> = files.iter().flat_map(|f| unmarked(&f.display().to_string(), &std::fs::read_to_string(f).unwrap())).collect();
    assert!(found.is_empty(), "Spira fork edges with no FORK-EDGE reason:\n{}", found.join("\n"));
}

#[test]
fn the_scan_catches_a_planted_site_and_passes_a_marked_one() {
    let planted = "fn f() {\n    let o = Command::new(\"spira-lc\").arg(\"list\");\n}\n";
    assert_eq!(unmarked("planted.rs", planted), vec!["planted.rs:2".to_string()]);
    let timeout = "let o = Command::new(\"timeout\")\n    .arg(\"5\")\n    .arg(\"systemctl\");\n";
    assert_eq!(unmarked("t.rs", timeout), vec!["t.rs:1".to_string()]);
    let runner = "let o = spira_config::bounded::bounded(\"tsd-write\");\n";
    assert_eq!(unmarked("r.rs", runner), vec!["r.rs:1".to_string()]);
    let marked = "// FORK-EDGE: unserved verb content\nlet o = Command::new(\"spira-lc\");\n";
    assert!(unmarked("m.rs", marked).is_empty());
    assert!(unmarked("g.rs", "let o = Command::new(\"git\");\n").is_empty());
}
