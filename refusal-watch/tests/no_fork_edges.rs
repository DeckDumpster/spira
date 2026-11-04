//! No Spira→Spira subprocess edge survives in this crate's source unless it is marked
//! `FORK-EDGE:` (sp-ipwlic): the store is reached over `LcClient`, units through the `Units`
//! port, mail and incidents in-process. A spawn names one of the external programs that stay
//! children, or carries the mark on its line or the two above; a program resolved at run time
//! (`lc_bin()`, `tool("bdq")`) is never assumed external.

use std::path::Path;

/// The programs that stay children (design §2: not in scope).
const EXTERNAL: &[&str] = &["git", "gh", "aws", "podman", "ssh", "curl", "duckdb", "tar", "zstd", "flock", "python3"];

/// The program a spawn on `line` names: `Some(literal)`, `Some("")` for one resolved at run
/// time, `None` when the line spawns nothing.
fn spawned(line: &str) -> Option<String> {
    let at = ["Command::new(", "bounded::bounded(", "bounded::bounded_for("].iter().filter_map(|p| line.find(p).map(|i| i + p.len())).min()?;
    let rest = line[at..].trim_start();
    Some(rest.strip_prefix('"').map(|r| r.split('"').next().unwrap_or("").to_string()).unwrap_or_default())
}

/// Every `file:line` in `text` (before its test module) that spawns anything but an external
/// program with no `FORK-EDGE:` mark on the line or the two above it.
fn unmarked(file: &str, text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().take_while(|l| !l.trim_start().starts_with("#[cfg(test)]")).collect();
    let mut hits = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let Some(prog) = spawned(line) else { continue };
        let external = if prog == "timeout" {
            lines[i..lines.len().min(i + 4)].iter().any(|l| EXTERNAL.iter().any(|x| l.contains(&format!(".arg(\"{x}\")"))))
        } else {
            EXTERNAL.contains(&prog.as_str())
        };
        let marked = lines[i.saturating_sub(2)..=i].iter().any(|l| l.contains("FORK-EDGE:"));
        if !external && !marked {
            hits.push(format!("{file}:{}", i + 1));
        }
    }
    hits
}

fn walk(dir: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).expect("read src").flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.extend(unmarked(&p.display().to_string(), &std::fs::read_to_string(&p).expect("read file")));
        }
    }
}

#[test]
fn no_unmarked_fork_edge_remains_in_src() {
    let mut hits = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut hits);
    assert!(hits.is_empty(), "unmarked fork edges: {hits:?}");
}

#[test]
fn positive_control_a_planted_site_is_caught_and_a_marked_one_is_not() {
    let n = concat!("Command::", "new");
    let b = concat!("bounded::", "bounded");
    for planted in ["spira-lc", "bd", "bdq", "bash", "systemctl", "systemd-run", "tsd-write", "mail", "queue", "pgrep"] {
        assert_eq!(unmarked("x.rs", &format!("let c = {n}(\"{planted}\");\n")), vec!["x.rs:1"], "{planted}");
    }
    assert_eq!(unmarked("x.rs", &format!("let c = spira_config::{b}(lc_bin()?);\n")), vec!["x.rs:1"]);
    assert_eq!(unmarked("x.rs", &format!("let mut c = {n}(\"timeout\");\nc.arg(\"5\").arg(&self.bdq);\n")), vec!["x.rs:1"]);
    assert!(unmarked("x.rs", &format!("// FORK-EDGE: unserved verb content\nlet c = {n}(\"bdq\");\n")).is_empty());
    assert!(unmarked("x.rs", &format!("let c = {b}(\"git\");\n")).is_empty());
    assert!(unmarked("x.rs", &format!("#[cfg(test)]\nmod tests {{ fn f() {{ {n}(\"bd\"); }} }}\n")).is_empty());
}
