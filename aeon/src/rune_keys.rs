//! Startup guard: a rune may not declare a `RUNE_*` key nothing consumes. A dead key reads
//! as a live control and does nothing.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Keys that are documentation, read by no program.
pub const DOCUMENTARY: &[&str] = &["RUNE_DESC"];

const SOURCE_EXTS: &[&str] = &["rs", "sh", "py", "toml"];

pub fn declared_keys(rune_text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in rune_text.lines() {
        let l = line.trim_start();
        let l = l.strip_prefix("export ").map_or(l, str::trim_start);
        let Some((name, _)) = l.split_once('=') else { continue };
        if name.starts_with("RUNE_") && name.bytes().all(is_word) {
            out.insert(name.to_string());
        }
    }
    out
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn has_bareword(hay: &str, key: &str) -> bool {
    let h = hay.as_bytes();
    hay.match_indices(key).any(|(i, _)| {
        let before = i == 0 || !is_word(h[i - 1]);
        let after = h.get(i + key.len()).is_none_or(|b| !is_word(*b));
        before && after
    })
}

fn scan(dir: &Path, skip: &Path, pending: &mut BTreeSet<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if pending.is_empty() {
            return;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            let n = e.file_name();
            if p == skip || n == "target" || n == ".git" {
                continue;
            }
            scan(&p, skip, pending);
        } else if ft.is_file() && p.extension().and_then(|x| x.to_str()).is_some_and(|x| SOURCE_EXTS.contains(&x)) {
            let Ok(text) = fs::read_to_string(&p) else { continue };
            pending.retain(|k| !has_bareword(&text, k));
        }
    }
}

/// Keys the rune declares that no source file under `corpus_root` mentions as a bareword
/// (`$RUNE_X` and `rune_get f RUNE_X` both count), excluding `runes`, where every key is
/// declared and never consumed. None when `corpus_root` is not a source workspace (no Cargo.toml): a
/// fixture home would call every key dead.
pub fn unconsumed(rune_text: &str, corpus_root: &Path, runes: &Path) -> Option<Vec<String>> {
    if !corpus_root.join("Cargo.toml").is_file() {
        return None;
    }
    let mut pending: BTreeSet<String> =
        declared_keys(rune_text).into_iter().filter(|k| !DOCUMENTARY.contains(&k.as_str())).collect();
    scan(corpus_root, runes, &mut pending);
    Some(pending.into_iter().collect())
}

/// The guard `main` runs right after locating the rune. A home whose parent is not a source workspace
/// (a fixture home) has nothing to judge against and is let through.
pub fn check(rune_file: &Path, home: &Path) -> Result<(), String> {
    let text = fs::read_to_string(rune_file).map_err(|e| format!("{}: {e}", rune_file.display()))?;
    let root = home.parent().unwrap_or(home);
    if root.join("Cargo.toml").is_file() && !declared_keys(&text).contains(spira_config::runes::WORKING_EXITS_KEY) {
        return Err(format!("{}: {} is not declared", rune_file.display(), spira_config::runes::WORKING_EXITS_KEY));
    }
    let Some(dead) = unconsumed(&text, root, &home.join("runes")) else { return Ok(()) };
    if dead.is_empty() {
        return Ok(());
    }
    Err(format!("{} declares RUNE_* keys nothing consumes: {}", rune_file.display(), dead.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(tag: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(&format!("rune-keys-{tag}"));
        fs::create_dir_all(d.join("spira/runes")).unwrap();
        fs::write(d.join("Cargo.toml"), "[workspace]\n").unwrap();
        d
    }

    const RUNE: &str = "RUNE_NAME=x\nRUNE_LIVE_VAR=1\nRUNE_LIVE_GET=2\nRUNE_DEAD=3\nRUNE_DESC=\"doc\"\n# RUNE_COMMENTED=1\n";

    #[test]
    fn dead_key_found_and_live_keys_pass() {
        let d = tree("fixture");
        fs::write(d.join("spira/runes/x.rune"), RUNE).unwrap();
        fs::write(d.join("spira/lib.sh"), "echo \"$RUNE_LIVE_VAR\"\nrune_get \"$f\" RUNE_LIVE_GET 0\nRUNE_NAME_X=1\n").unwrap();
        fs::write(d.join("spira/other.sh"), "RUNE_DEAD_SUFFIX=1\nXRUNE_DEAD=1\n").unwrap();
        let dead = unconsumed(RUNE, &d, &d.join("spira/runes")).unwrap();
        assert_eq!(dead, vec!["RUNE_DEAD", "RUNE_NAME"]);
        fs::write(d.join("spira/ok.rs"), "\"RUNE_DEAD\" \"RUNE_NAME\"").unwrap();
        assert!(unconsumed(RUNE, &d, &d.join("spira/runes")).unwrap().is_empty());
    }

    #[test]
    fn check_names_the_dead_key_and_passes_a_non_workspace_corpus() {
        let d = tree("check");
        fs::write(d.join("spira/runes/x.rune"), "RUNE_DEAD=1\nRUNE_WORKING_EXITS=\"DONE\"\n").unwrap();
        let home = d.join("spira");
        fs::remove_file(d.join("Cargo.toml")).unwrap();
        assert!(unconsumed("RUNE_DEAD=1\n", &d, &home.join("runes")).is_none());
        check(&home.join("runes/x.rune"), &home).unwrap();
        fs::write(d.join("Cargo.toml"), "").unwrap();
        let e = check(&home.join("runes/x.rune"), &home).unwrap_err();
        assert!(e.contains("RUNE_DEAD"), "{e}");
    }

    #[test]
    fn a_rune_without_working_exits_fails_to_load_naming_the_key() {
        let d = tree("exits");
        let home = d.join("spira");
        fs::write(home.join("runes/x.rune"), "RUNE_NAME=x\n").unwrap();
        let e = check(&home.join("runes/x.rune"), &home).unwrap_err();
        assert!(e.contains("RUNE_WORKING_EXITS is not declared"), "{e}");
    }

    #[test]
    fn every_shipped_rune_is_fully_consumed() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let mut n = 0;
        for dir in [home.join("runes"), home.join("runes/disabled")] {
            let Ok(rd) = fs::read_dir(dir) else { continue };
            for e in rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "rune")) {
                check(&e.path(), &home).unwrap();
                n += 1;
            }
        }
        assert!(n >= 8, "found only {n} runes");
    }
}
