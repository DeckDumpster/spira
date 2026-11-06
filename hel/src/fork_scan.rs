//! The static fence: no hel source starts a Spira program as a child (the store CLI, the
//! unit manager's CLI, the slayer, the metrics writer, mail, or a shell) unless the site
//! carries a `FORK-EDGE` comment naming why it is still a child.

use std::path::Path;

/// Programs whose work is in-process now: each a library call or a `Units` effect.
pub const BANNED: &[&str] = &["spira-lc", "systemctl", "systemd-run", "slay", "tsd-write", "mail", "bash"];
const STARTS: &[&str] = &["Command::new(", "bounded(", "bounded_for(", "command("];
const MARK: &str = "FORK-EDGE";

/// `file:line` of every child start of a [`BANNED`] program in `text`, unless that line or the
/// comment block directly above it carries the mark.
pub fn sites(name: &str, text: &str, banned: &[&str]) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut hits = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        let hit = STARTS.iter().any(|s| banned.iter().any(|b| ["\"", "&", ""].iter().any(|q| compact.contains(&format!("{s}{q}{b}")))));
        let above = lines[..i].iter().rev().take_while(|l| l.trim_start().starts_with("//"));
        let marked = line.contains(MARK) || above.into_iter().any(|l| l.contains(MARK));
        if hit && !marked {
            hits.push(format!("{name}:{}: {}", i + 1, line.trim()));
        }
    }
    hits
}

/// [`sites`] over every `.rs` file directly in `dir` (`skip` excepted). A directory that
/// cannot be read, or holds no source, is a hit: a fence that read nothing proves nothing.
pub fn scan(dir: &Path, banned: &[&str], skip: &[&str]) -> Vec<String> {
    let mut files: Vec<_> = match std::fs::read_dir(dir) {
        Ok(d) => d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "rs")).collect(),
        Err(e) => return vec![format!("{}: {e}", dir.display())],
    };
    if files.is_empty() {
        return vec![format!("{}: no .rs sources to scan", dir.display())];
    }
    files.sort();
    let mut hits = Vec::new();
    for f in files {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if skip.contains(&name.as_str()) {
            continue;
        }
        match std::fs::read_to_string(&f) {
            Ok(text) => hits.extend(sites(&name, &text, banned)),
            Err(e) => hits.push(format!("{name}: {e}")),
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hel_source_starts_a_spira_program_as_a_child() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let hits = scan(&dir, BANNED, &["fork_scan.rs"]);
        assert!(hits.is_empty(), "Spira children left in hel (in-process them, or mark FORK-EDGE):\n{}", hits.join("\n"));
    }

    #[test]
    fn a_planted_child_is_caught_and_a_marked_one_is_not() {
        let planted = "fn f() {\n    let _ = std::process::Command::new(\"spira-lc\").arg(\"list\");\n    let _ = Command::new(&systemctl);\n}\n";
        assert_eq!(sites("x.rs", planted, BANNED).len(), 2, "{:?}", sites("x.rs", planted, BANNED));
        let marked = "    // FORK-EDGE: unserved verb close\n    let _ = Command::new(\"spira-lc\");\n";
        assert!(sites("x.rs", marked, BANNED).is_empty());
        let block = "    // FORK-EDGE: needs a library\n    // for the rest\n    let _ = Command::new(\"bash\");\n";
        assert!(sites("x.rs", block, BANNED).is_empty());
        let detached = "    // FORK-EDGE: an earlier site\n    let a = 1;\n    let _ = Command::new(\"bash\");\n";
        assert_eq!(sites("x.rs", detached, BANNED).len(), 1);
        assert!(sites("x.rs", "let _ = Command::new(\"git\");", BANNED).is_empty());
    }

    #[test]
    fn a_directory_with_nothing_to_read_fails_closed() {
        let t = testkit::TempDir::new("hel-fork-scan-empty");
        assert_eq!(scan(t.path(), BANNED, &[]).len(), 1);
        assert_eq!(scan(&t.join("absent"), BANNED, &[]).len(), 1);
    }
}
