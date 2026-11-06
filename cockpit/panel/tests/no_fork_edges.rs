//! No Spira→Spira subprocess edge remains in this crate's source except a site marked
//! `FORK-EDGE:` (on its line or the two above), which names what the library still lacks.

use std::path::{Path, PathBuf};

const BANNED: &[&str] = &[
    "spira-lc",
    "bd",
    "bdq",
    "bash",
    "systemctl",
    "systemd-run",
    "tsd-write",
    "mail",
    "queue",
    "pgrep",
];
/// The literal first argument of any function call on `line` (`Command::new("x")`,
/// `bounded("x")`, or a crate-local runner such as `run("x", ..)`): a runner whose child is a
/// parameter carries its caller's child, so the caller's literal is the site.
fn literal_children(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = line.as_bytes();
    let mut i = 0;
    while let Some(off) = line[i..].find("(\"") {
        let at = i + off;
        // An argument to `.arg(..)`/`.env(..)` is not a child; the `timeout` window below reads it.
        let callee = at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') && !line[..at].ends_with(".arg") && !line[..at].ends_with(".env");
        let rest = &line[at + 2..];
        if callee {
            if let Some(end) = rest.find('"') {
                out.push(rest[..end].to_string());
            }
        }
        i = at + 2;
    }
    out
}

/// `(line number, child)` of every unmarked banned spawn in `src`.
fn unmarked(src: &str) -> Vec<(usize, String)> {
    let code = src.split("#[cfg(test)]").next().unwrap_or("");
    let lines: Vec<&str> = code.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for child in literal_children(line) {
            let child = if child == "timeout" {
                // `timeout N <banned>`: the real child is the first banned literal in the next lines.
                let window = lines[i..lines.len().min(i + 4)].join(" ");
                match BANNED.iter().find(|b| window.contains(&format!("\"{b}\""))) {
                    Some(b) => b.to_string(),
                    None => continue,
                }
            } else {
                child
            };
            if !BANNED.contains(&child.as_str()) {
                continue;
            }
            let marked = lines[i.saturating_sub(2)..=i]
                .iter()
                .any(|l| l.contains("FORK-EDGE:"));
            if !marked {
                found.push((i + 1, child));
            }
        }
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_unmarked_fork_edge_remains_in_src() {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty(), "no source files found");
    let mut bad = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for (n, child) in unmarked(&text) {
            bad.push(format!("{}:{n} {child}", f.display()));
        }
    }
    assert!(
        bad.is_empty(),
        "unmarked Spira→Spira subprocess edges:\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_scan_catches_a_planted_site_and_passes_a_marked_one() {
    let planted = "fn f() {\n    let _ = Command::new(\"spira-lc\").arg(\"show\");\n}\n";
    assert_eq!(unmarked(planted), vec![(2, "spira-lc".to_string())]);
    let timeout =
        "fn f() {\n    Command::new(\"timeout\")\n        .arg(\"5\")\n        .arg(\"bd\");\n}\n";
    assert_eq!(unmarked(timeout), vec![(2, "bd".to_string())]);
    let bounded = "fn f() { spira_config::bounded::bounded(\"systemctl\").arg(\"x\"); }\n";
    assert_eq!(unmarked(bounded), vec![(1, "systemctl".to_string())]);
    let marked = "fn f() {\n    // FORK-EDGE: unserved verb hold\n    let _ = Command::new(\"spira-lc\").arg(\"hold\");\n}\n";
    assert!(unmarked(marked).is_empty());
    let runner = "fn f() { run(\"spira-lc\", &[\"content\"], cfg); }\n";
    assert_eq!(unmarked(runner), vec![(1, "spira-lc".to_string())]);
    let git = "fn f() { Command::new(\"git\"); format!(\"mail {x}\"); }\n";
    assert!(unmarked(git).is_empty());
}

/// The marked fork edges left in `src` (sp-ipwlic): a flip onto the store's door removes a mark
/// and lowers this; a new edge raises it only on purpose.
const MARKED_EDGES: usize = 4;

fn marks(dir: &std::path::Path) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(dir).expect("read src").flatten() {
        let p = e.path();
        if p.is_dir() {
            n += marks(&p);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let text = std::fs::read_to_string(&p).expect("read file");
            n += text.lines().take_while(|l| !l.trim_start().starts_with("#[cfg(test)]")).filter(|l| l.contains("FORK-EDGE:")).count();
        }
    }
    n
}

#[test]
fn the_marked_fork_edges_only_shrink() {
    assert_eq!(marks(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")), MARKED_EDGES, "a FORK-EDGE mark was added or removed: move MARKED_EDGES with it");
}
