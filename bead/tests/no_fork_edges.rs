//! No Spira→Spira child remains in this crate's sources except a site marked `FORK-EDGE:`
//! (the scheduler consolidation's §2 edges): a library call or the port replaces each.

use std::path::{Path, PathBuf};

const BANNED: &[&str] = &["spira-lc", "bd", "bdq", "bash", "systemctl", "systemd-run", "tsd-write", "mail", "queue", "pgrep"];

/// Every unmarked site in `text`, as `line: text`.
fn unmarked(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let quoted = |s: &str, x: &str| s.contains(&format!("\"{x}\""));
    let mut hits = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let opener = ["Command::new(", "bounded::bounded(", "bounded("].iter().find_map(|o| line.find(o).map(|p| &line[p + o.len()..]));
        let Some(rest) = opener else { continue };
        let banned = if rest.starts_with("\"timeout\"") {
            let next: String = lines[i..(i + 4).min(lines.len())].join("\n");
            BANNED.iter().any(|x| quoted(&next, x))
        } else {
            BANNED.iter().any(|x| rest.starts_with(&format!("\"{x}\"")))
        };
        let marked = lines[i.saturating_sub(2)..=i].iter().any(|l| l.contains("FORK-EDGE:"));
        if banned && !marked {
            hits.push(format!("{}: {}", i + 1, line.trim()));
        }
    }
    hits
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("read src").flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_unmarked_spira_child_remains_in_src() {
    let mut files = Vec::new();
    rust_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut found = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).expect("read");
        found.extend(unmarked(&text).into_iter().map(|h| format!("{}:{h}", f.display())));
    }
    assert!(found.is_empty(), "unmarked Spira→Spira children:\n{}", found.join("\n"));
}

#[test]
fn a_planted_site_is_caught_and_a_marked_one_is_not() {
    let planted = "fn f() {\n    let _ = Command::new(\"spira-lc\").arg(\"show\");\n}\n";
    assert_eq!(unmarked(planted).len(), 1);
    let behind_timeout = "fn f() {\n    Command::new(\"timeout\")\n        .args([\"5\", \"bd\", \"show\"]);\n}\n";
    assert_eq!(unmarked(behind_timeout).len(), 1);
    let runner = "fn f() { spira_config::bounded::bounded(\"mail\").arg(\"send\"); }\n";
    assert_eq!(unmarked(runner).len(), 1);
    let marked = "fn f() {\n    // FORK-EDGE: unserved verb hold\n    let _ = Command::new(\"spira-lc\").arg(\"hold\");\n}\n";
    assert!(unmarked(marked).is_empty());
    let git = "fn f() { Command::new(\"git\").arg(\"status\"); }\n";
    assert!(unmarked(git).is_empty());
}
