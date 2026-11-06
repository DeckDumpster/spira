//! No Spira→Spira subprocess edge survives in watchtower's source unless it is marked
//! `FORK-EDGE:` (on the site's line or one of the two above it), naming why it stays.

use std::path::{Path, PathBuf};

const BANNED: &[&str] = &["spira-lc", "bd", "bdq", "bash", "systemctl", "systemd-run", "tsd-write", "mail", "queue", "pgrep"];
/// Programs this crate holds in a variable rather than a literal: the same edges by another name.
const BANNED_VARS: &[&str] = &["systemctl", "bdq", "lc_bin", "bd_bin", "incident_sh", "program()"];
const SPAWNERS: &[&str] = &["Command::new(", "bounded::bounded(", "bounded("];

fn program_of(line: &str) -> Option<String> {
    let at = SPAWNERS.iter().filter_map(|s| line.find(s).map(|i| i + s.len())).min()?;
    let rest = &line[at..];
    let end = rest.find(')')?;
    Some(rest[..end].trim().to_string())
}

fn banned(program: &str) -> bool {
    if let Some(lit) = program.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
        return BANNED.contains(&lit);
    }
    BANNED_VARS.iter().any(|v| program.contains(v))
}

fn first_arg(lines: &[&str], from: usize) -> Option<String> {
    lines.iter().skip(from).take(4).find_map(|l| {
        let i = l.find(".arg(")? + 5;
        let rest = &l[i..];
        Some(rest[..rest.find(')')?].trim().to_string())
    })
}

/// Every unmarked banned site in `text`, as 1-based line numbers.
pub fn unmarked(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut in_tests = false;
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") && lines.get(i + 1).is_some_and(|n| n.trim_start().starts_with("mod ")) {
            in_tests = true;
        }
        if in_tests || line.trim_start().starts_with("//") {
            continue;
        }
        let Some(program) = program_of(line) else { continue };
        let hit = banned(&program) || (program == "\"timeout\"" && first_arg(&lines, i).is_some_and(|a| banned(&a)));
        let marked = lines[i.saturating_sub(2)..=i].iter().any(|l| l.contains("FORK-EDGE:"));
        if hit && !marked {
            out.push(i + 1);
        }
    }
    out
}

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

#[test]
fn no_unmarked_fork_edge_remains_in_src() {
    let mut files = Vec::new();
    rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut found = Vec::new();
    for f in files {
        for n in unmarked(&std::fs::read_to_string(&f).unwrap()) {
            found.push(format!("{}:{n}", f.display()));
        }
    }
    assert!(found.is_empty(), "unmarked Spira→Spira fork sites:\n{}", found.join("\n"));
}

#[test]
fn the_scan_catches_a_planted_site_and_passes_a_marked_one() {
    let planted = "fn f() {\n    let _ = std::process::Command::new(\"spira-lc\").arg(\"list\");\n}\n";
    assert_eq!(unmarked(planted), vec![2], "a literal banned program is caught");
    let via_timeout = "fn f() {\n    let _ = Command::new(\"timeout\")\n        .arg(\"bash\");\n}\n";
    assert_eq!(unmarked(via_timeout), vec![2], "a banned program behind timeout is caught");
    let by_var = "fn f(cfg: &Cfg) {\n    let _ = spira_config::bounded::bounded(&cfg.systemctl).arg(\"show\");\n}\n";
    assert_eq!(unmarked(by_var), vec![2], "a banned program held in a variable is caught");
    let marked = "fn f() {\n    // FORK-EDGE: unserved verb hold\n    let _ = Command::new(\"spira-lc\").arg(\"hold\");\n}\n";
    assert!(unmarked(marked).is_empty(), "a FORK-EDGE site passes");
    let in_tests = "#[cfg(test)]\nmod tests {\n    fn f() { Command::new(\"bash\"); }\n}\n";
    assert!(unmarked(in_tests).is_empty(), "a test module is not scanned");
    let test_impl = "#[cfg(test)]\nimpl X {}\nfn f() {\n    Command::new(\"bash\");\n}\n";
    assert_eq!(unmarked(test_impl), vec![4], "a test-only item does not end the scan");
}
