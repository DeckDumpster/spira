//! No Spira→Spira subprocess edge survives in this crate's source unless its site says why
//! (`FORK-EDGE:` on the line or one of the two above): a child of `spira-lc`, `bd`, `bdq`,
//! `bash`, `systemctl`, `systemd-run`, `tsd-write`, `mail`, `queue` or `pgrep`, named directly
//! to `Command::new`, to spira-config's `bounded`/`bounded_for`, or as the program a
//! `timeout` child runs.

use std::path::Path;

const BANNED: &[&str] = &["spira-lc", "bd", "bdq", "bash", "systemctl", "systemd-run", "tsd-write", "mail", "queue", "pgrep"];

/// The banned program a spawn on `lines[i]` (or, for `timeout`, its next three lines) names.
fn spawned(lines: &[&str], i: usize) -> Option<&'static str> {
    let names = |text: &str| -> Option<&'static str> {
        BANNED.iter().copied().find(|b| {
            ["Command::new(", "bounded(", "bounded_for("].iter().any(|f| text.contains(&format!("{f}\"{b}\"")))
        })
    };
    let line = lines[i];
    if let Some(b) = names(line) {
        return Some(b);
    }
    if line.contains("Command::new(\"timeout\")") {
        let tail = lines[i..lines.len().min(i + 4)].join("\n");
        return BANNED.iter().copied().find(|b| {
            let q = format!("\"{b}\"");
            tail.find(&q).is_some_and(|at| {
                // the first quoted word after the deadline is the program
                let after = &tail[tail.find("Command::new(\"timeout\")").unwrap_or(0) + 23..at];
                after.matches('"').count() <= 2
            })
        });
    }
    None
}

/// Every unmarked banned spawn in `text`, as `line: program`.
fn unmarked(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    (0..lines.len())
        .filter_map(|i| {
            let b = spawned(&lines, i)?;
            let marked = lines[i.saturating_sub(2)..=i].iter().any(|l| l.contains("FORK-EDGE:"));
            (!marked).then(|| format!("{}: {b}", i + 1))
        })
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).expect("src/ reads").flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let text = std::fs::read_to_string(&p).expect("a source file reads");
            out.extend(unmarked(&text).into_iter().map(|s| format!("{}:{s}", p.display())));
        }
    }
}

#[test]
fn no_unmarked_spira_fork_edge_remains() {
    let mut found = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
    assert!(found.is_empty(), "unmarked Spira→Spira fork edges:\n{}", found.join("\n"));
}

#[test]
fn the_scan_catches_a_planted_site_and_passes_a_marked_one() {
    let q = '"';
    let planted = format!("fn f() {{\n    let o = Command::new({q}bash{q}).arg({q}-c{q});\n}}\n");
    assert_eq!(unmarked(&planted), vec!["2: bash".to_string()]);
    let bounded = format!("    spira_config::bounded::bounded({q}spira-lc{q}).args([{q}held{q}]);\n");
    assert_eq!(unmarked(&bounded), vec!["1: spira-lc".to_string()]);
    let timeout = format!("    Command::new({q}timeout{q})\n        .arg({q}5{q})\n        .arg({q}bdq{q});\n");
    assert_eq!(unmarked(&timeout), vec!["1: bdq".to_string()]);
    let git = format!("    Command::new({q}timeout{q}).args([{q}5{q}, {q}git{q}, {q}-C{q}, {q}bd{q}]);\n");
    assert!(unmarked(&git).is_empty(), "a timeout running git is not an edge");
    let marked = format!("    // FORK-EDGE: unserved verb held\n    spira_config::bounded::bounded({q}spira-lc{q});\n");
    assert!(unmarked(&marked).is_empty());
}

/// The body of `fn <name>(` in `text`: up to the next item at the same indent.
fn body_of<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("    fn {name}(")).unwrap_or_else(|| panic!("no fn {name}"));
    let rest = &text[start + 1..];
    let end = rest.find("\n    fn ").or_else(|| rest.find("\n}")).unwrap_or(rest.len());
    &rest[..end]
}

/// Every verb the store's content door serves reaches it, never a forked bdq (sp-ipwlic): the
/// door applies bdq's protections and writes a create's lifecycle row itself.
#[test]
fn the_verbs_the_content_door_serves_never_fork_bdq() {
    let real = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/real.rs")).unwrap();
    for f in ["create", "note", "show_json", "list_asks"] {
        let body = body_of(&real, f);
        assert!(!body.contains("self.cmd()") && body.contains("\"content\""), "RealBd::{f} forks bdq:\n{body}");
    }
}
