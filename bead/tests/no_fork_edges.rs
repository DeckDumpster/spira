//! No Spira→Spira child remains in this crate's sources except a site marked `FORK-EDGE:`
//! (the scheduler consolidation's §2 edges): a library call or the port replaces each.

#[macro_use]
extern crate spira_test_id;
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

#[test_id(tg7584l)]
#[test]
fn no_unmarked_spira_child_remains_in_src() {
    let mut files = Vec::new();
    rust_files(&testkit::manifest_dir().join("src"), &mut files);
    let mut found = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).expect("read");
        found.extend(unmarked(&text).into_iter().map(|h| format!("{}:{h}", f.display())));
    }
    assert!(found.is_empty(), "unmarked Spira→Spira children:\n{}", found.join("\n"));
}

#[test_id(tb3v2b9)]
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

fn spawns_bdq(text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let code = l.split("//").next().unwrap_or("");
            code.contains("tool(\"bdq\")") || (code.contains("Command::new(") && code.contains("\"bdq\""))
        })
        .map(|(n, l)| format!("{}: {}", n + 1, l.trim()))
        .collect()
}

#[test_id(tlcp237)]
#[test]
fn no_bdq_spawn_remains_in_bead_main() {
    let text = std::fs::read_to_string(testkit::manifest_dir().join("src/main.rs")).expect("read main.rs");
    let found = spawns_bdq(&text);
    assert!(found.is_empty(), "bead's main.rs spawns bdq; its writes go to `spira-lc content`:\n{}", found.join("\n"));
}

#[test_id(tjt97of)]
#[test]
fn a_planted_bdq_spawn_is_caught_marked_or_not() {
    let planted = "// FORK-EDGE: store\nlet mut cmd = Command::new(spira_config::release_env::tool(\"bdq\")?);\n";
    assert_eq!(spawns_bdq(planted).len(), 1);
    assert_eq!(spawns_bdq("Command::new(\"bdq\")").len(), 1);
    assert!(spawns_bdq("let c = Command::new(\"git\"); // bdq\n").is_empty());
}

/// Whether the crate at `dir` has a `bdq` bin target: a `[[bin]]` of that name, or a source
/// cargo discovers by itself under `src/bin/`.
fn declares_bdq(dir: &Path) -> bool {
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).expect("read Cargo.toml");
    let named = manifest.split("[[bin]]").skip(1).any(|t| t.split("\n[").next().unwrap_or("").lines().any(|l| l.trim() == "name = \"bdq\""));
    named || dir.join("src/bin/bdq.rs").exists() || dir.join("src/bin/bdq/main.rs").exists()
}

#[test_id(t5g0hym)]
#[test]
fn the_bead_crate_builds_no_bdq_program() {
    let planted = testkit::TempDir::new("bead-bdq-target");
    std::fs::write(planted.path().join("Cargo.toml"), "[[bin]]\nname = \"bdq\"\npath = \"x.rs\"\n").unwrap();
    assert!(declares_bdq(planted.path()), "positive control: a declared target");
    std::fs::write(planted.path().join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
    assert!(!declares_bdq(planted.path()));
    std::fs::create_dir_all(planted.path().join("src/bin")).unwrap();
    std::fs::write(planted.path().join("src/bin/bdq.rs"), "fn main() {}\n").unwrap();
    assert!(declares_bdq(planted.path()), "positive control: a discovered target");

    assert!(!declares_bdq(&testkit::manifest_dir()), "bead builds a bdq program; no release ships one");
}
