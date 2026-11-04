//! The fork fence: every child this crate's production code starts is an external edge (git,
//! gh, aws, podman, ssh) or carries a `FORK-EDGE:` comment saying why it is still a child, or an
//! `EXTERNAL:` comment naming the external program a variable or `timeout` holds.

use std::path::Path;

const SPAWNS: [&str; 3] = ["Command::new(", "bounded(", "bounded_for("];
const EXTERNAL: [&str; 5] = ["\"git\"", "\"gh\"", "\"aws\"", "\"podman\"", "\"ssh\""];
const MARKS: [&str; 2] = ["FORK-EDGE:", "EXTERNAL:"];
/// How many lines above a spawn its mark may sit.
const REACH: usize = 3;

/// Which lines belong to a `#[cfg(test)]` item: the item after the attribute, to its closing brace.
fn test_lines(lines: &[&str]) -> Vec<bool> {
    let mut skip = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "#[cfg(test)]" {
            i += 1;
            continue;
        }
        skip[i] = true;
        let mut depth: i64 = 0;
        let mut opened = false;
        let mut j = i + 1;
        while j < lines.len() {
            skip[j] = true;
            let code = lines[j].split("//").next().unwrap_or("");
            for c in code.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            j += 1;
            if (opened && depth <= 0) || (!opened && code.trim_end().ends_with(';')) {
                break;
            }
        }
        i = j;
    }
    skip
}

/// `(line, text)` of each unmarked spawn in `src` outside its `#[cfg(test)]` items.
pub fn offenders(src: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = src.lines().collect();
    let skip = test_lines(&lines);
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if skip[i] {
            continue;
        }
        let code = l.split("//").next().unwrap_or("");
        let Some(at) = SPAWNS.iter().filter_map(|s| code.find(s).map(|p| p + s.len())).min() else { continue };
        let arg = code[at..].trim_start();
        if EXTERNAL.iter().any(|e| arg.starts_with(e)) {
            continue;
        }
        let marked = lines[i.saturating_sub(REACH)..=i].iter().any(|m| MARKS.iter().any(|k| m.contains(k)));
        if !marked {
            out.push((i + 1, l.trim().to_string()));
        }
    }
    out
}

/// Every unmarked spawn under `dir`, skipping test files.
pub fn scan(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if p.is_dir() {
                if name != "tests" {
                    stack.push(p);
                }
            } else if name.ends_with(".rs") && !name.contains("test") && name != "fork_scan.rs" {
                let src = std::fs::read_to_string(&p).unwrap_or_default();
                found.extend(offenders(&src).into_iter().map(|(n, l)| format!("{}:{n}: {l}", p.display())));
            }
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_planted_spira_child_is_caught() {
        let planted = "fn f() {\n    let x = 1;\n    let _ = Command::new(\"spira-lc\").arg(\"list\");\n}\n";
        assert_eq!(offenders(planted).len(), 1);
        let via_var = "fn f() {\n    let _ = spira_config::bounded::bounded(forge).arg(\"x\");\n}\n";
        assert_eq!(offenders(via_var).len(), 1);
    }

    #[test]
    fn a_marked_or_external_child_passes() {
        let marked = "    // FORK-EDGE: machine leg testenv\n    let _ = Command::new(testenv);\n";
        let external = "    let _ = Command::new(\"git\").arg(\"-C\");\n";
        let in_test = "#[cfg(test)]\nmod t { fn f() { Command::new(\"queue\"); } }\n";
        assert!(offenders(marked).is_empty());
        assert!(offenders(external).is_empty());
        assert!(offenders(in_test).is_empty());
    }

    #[test]
    fn a_spawn_after_a_test_item_mid_file_is_still_caught() {
        let src = "#[cfg(test)]\nmod t {\n    fn f() { Command::new(\"queue\"); }\n}\n\nfn g() {\n    Command::new(\"incident.sh\");\n}\n";
        assert_eq!(offenders(src), vec![(7, "Command::new(\"incident.sh\");".to_string())]);
        let attr_on_fn = "#[cfg(test)]\nfn h() {\n    Command::new(\"queue\");\n}\nfn k() { Command::new(\"bash\"); }\n";
        assert_eq!(offenders(attr_on_fn).len(), 1);
    }

    #[test]
    fn this_crate_starts_no_unmarked_child() {
        let got = scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(got.is_empty(), "unmarked children:\n{}", got.join("\n"));
    }
}
