//! No Spira→Spira subprocess edge survives in this crate's sources (scheduler consolidation
//! §2): every spawn of a Spira program or a store/unit tool named below is either gone or
//! carries a `FORK-EDGE:` comment on its line or one of the two above, saying why it stays.

use std::path::Path;

const BANNED: &[&str] = &["spira-lc", "bd", "bdq", "bash", "systemctl", "systemd-run", "tsd-write", "mail", "queue", "pgrep"];
const SPAWNERS: &[&str] = &["Command::new(\"", "bounded(\"", "bounded_for(\""];

/// Every `file:line` that spawns a banned program without a `FORK-EDGE:` mark.
fn unmarked(name: &str, text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let mut hit = false;
        for s in SPAWNERS {
            let mut rest = *line;
            while let Some(at) = rest.find(s) {
                let after = &rest[at + s.len()..];
                let prog = after.split('"').next().unwrap_or("");
                let base = prog.rsplit('/').next().unwrap_or(prog);
                if BANNED.contains(&base) {
                    hit = true;
                }
                // `timeout <secs> <banned>`: the banned program rides as an argument.
                if base == "timeout" {
                    let window = lines[i..lines.len().min(i + 4)].join(" ");
                    if BANNED.iter().any(|b| window.contains(&format!("\"{b}\""))) {
                        hit = true;
                    }
                }
                rest = after;
            }
        }
        let marked = lines[i.saturating_sub(2)..=i].iter().any(|l| l.contains("FORK-EDGE:"));
        if hit && !marked {
            found.push(format!("{name}:{}: {}", i + 1, line.trim()));
        }
    }
    found
}

fn scan(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            scan(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            out.extend(unmarked(&p.display().to_string(), &text));
        }
    }
}

#[test]
fn no_unmarked_fork_edge_remains() {
    let mut found = Vec::new();
    scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
    assert!(found.is_empty(), "unmarked fork edges:\n{}", found.join("\n"));
}

#[test]
fn the_scan_catches_a_planted_site_and_passes_a_marked_one() {
    let planted = "fn f() {\n    let _ = std::process::Command::new(\"spira-lc\").arg(\"show\");\n}\n";
    assert_eq!(unmarked("planted.rs", planted).len(), 1);
    let via_timeout = "fn f() {\n    let _ = Command::new(\"timeout\")\n        .arg(\"5\")\n        .arg(\"bdq\");\n}\n";
    assert_eq!(unmarked("planted.rs", via_timeout).len(), 1);
    let bounded = "let _ = spira_config::bounded::bounded(\"pgrep\").arg(\"-f\");\n";
    assert_eq!(unmarked("planted.rs", bounded).len(), 1);
    let marked = "// FORK-EDGE: unserved verb hold\nlet _ = Command::new(\"spira-lc\").arg(\"hold\");\n";
    assert!(unmarked("planted.rs", marked).is_empty());
    let other = "let _ = Command::new(\"git\").arg(\"status\");\n";
    assert!(unmarked("planted.rs", other).is_empty());
}
