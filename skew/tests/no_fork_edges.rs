//! No Spira→Spira subprocess edge survives in this crate's source except a site marked
//! `FORK-EDGE:` (on its line or the two above), each naming what it still needs.

use std::path::Path;

const BANNED: &[&str] = &["spira-lc", "bd", "bdq", "bash", "systemctl", "systemd-run", "tsd-write", "mail", "queue", "pgrep"];
/// Where a child's program is named: a spawn, or the resolution of a program a spawn runs.
const OPENERS: &[&str] = &["Command::new(", "bounded(", "bounded_for(", "prog: ", "tool(", "name("];

/// The banned program a line starts, if any: a literal one directly, or one `timeout` runs
/// (its program is the first banned literal in the next three lines).
fn banned_on(lines: &[&str], i: usize) -> Option<&'static str> {
    let line = lines[i];
    if ["Command::new(", "bounded(", "bounded_for("].iter().any(|o| line.contains(o)) && line.contains("lc_bin()") {
        return Some("spira-lc");
    }
    for opener in OPENERS {
        let Some(at) = line.find(opener) else { continue };
        let rest = &line[at + opener.len()..];
        if let Some(b) = BANNED.iter().find(|b| rest.starts_with(&format!("\"{b}\""))) {
            return Some(b);
        }
        if rest.starts_with("\"timeout\"") {
            let window = lines[i..lines.len().min(i + 4)].join("\n");
            if let Some(b) = BANNED.iter().find(|b| window.contains(&format!(".arg(\"{b}\")"))) {
                return Some(b);
            }
        }
    }
    None
}

fn unmarked(text: &str) -> Vec<(usize, &'static str)> {
    let lines: Vec<&str> = text.lines().collect();
    (0..lines.len())
        .filter_map(|i| banned_on(&lines, i).map(|b| (i, b)))
        .filter(|(i, _)| !lines[i.saturating_sub(2)..=*i].iter().any(|l| l.contains("FORK-EDGE:")))
        .map(|(i, b)| (i + 1, b))
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let text = std::fs::read_to_string(&p).unwrap();
            for (n, b) in unmarked(&text) {
                out.push(format!("{}:{n}: {b}", p.display()));
            }
        }
    }
}

#[test]
fn no_unmarked_spira_subprocess_edge_remains() {
    let mut found = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
    assert!(found.is_empty(), "unmarked fork edges:\n{}", found.join("\n"));
}

#[test]
fn a_planted_site_is_caught_and_a_marked_one_is_not() {
    let planted = concat!("let o = Command::new(", "\"spira-lc\").arg(\"show\");\n");
    assert_eq!(unmarked(planted), vec![(1, "spira-lc")]);
    let wrapped = concat!("Command::new(", "\"timeout\")\n    .arg(\"5\")\n    .arg(\"bdq\")\n");
    assert_eq!(unmarked(wrapped), vec![(1, "bdq")]);
    let marked = concat!("// FORK-EDGE: unserved verb hold\nlet o = Command::new(", "\"spira-lc\");\n");
    assert!(unmarked(marked).is_empty());
    assert!(unmarked(concat!("Command::new(", "\"git\")")).is_empty());
    assert_eq!(unmarked(concat!("let c = bounded(spira_store::lifecycle_row::", "lc_bin()?);")), vec![(1, "spira-lc")]);
    assert_eq!(unmarked(concat!("mail: ", "tool(\"mail\")?,")), vec![(1, "mail")]);
}

/// The marked fork edges left in `src` (sp-ipwlic): a flip onto the store's door removes a mark
/// and lowers this; a new edge raises it only on purpose.
const MARKED_EDGES: usize = 3;

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
