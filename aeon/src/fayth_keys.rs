//! Startup guard: a fayth may not declare a `FAYTH_*` key nothing consumes. A dead key reads
//! as a live control and does nothing.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Keys that are documentation, read by no program.
pub const DOCUMENTARY: &[&str] = &["FAYTH_DESC"];

const SOURCE_EXTS: &[&str] = &["rs", "sh", "py", "toml"];

pub fn declared_keys(fayth_text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in fayth_text.lines() {
        let l = line.trim_start();
        let l = l.strip_prefix("export ").map_or(l, str::trim_start);
        let Some((name, _)) = l.split_once('=') else { continue };
        if name.starts_with("FAYTH_") && name.bytes().all(is_word) {
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

fn scan(dir: &Path, skip: &Path, pending: &mut BTreeSet<String>, files: &mut usize) {
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
            scan(&p, skip, pending, files);
        } else if ft.is_file() && p.extension().and_then(|x| x.to_str()).is_some_and(|x| SOURCE_EXTS.contains(&x)) {
            let Ok(text) = fs::read_to_string(&p) else { continue };
            *files += 1;
            pending.retain(|k| !has_bareword(&text, k));
        }
    }
}

/// Keys the fayth declares that no source file under `corpus_root` mentions as a bareword
/// (`$FAYTH_X` and `fayth_get f FAYTH_X` both count), excluding `chamber`, where every key is
/// declared and never consumed. Err when no source file could be read: a scan over nothing
/// would call every key dead, or pass vacuously.
pub fn unconsumed(fayth_text: &str, corpus_root: &Path, chamber: &Path) -> Result<Vec<String>, String> {
    let mut pending: BTreeSet<String> =
        declared_keys(fayth_text).into_iter().filter(|k| !DOCUMENTARY.contains(&k.as_str())).collect();
    let mut files = 0;
    scan(corpus_root, chamber, &mut pending, &mut files);
    if files == 0 {
        return Err(format!("no source files under {} to check FAYTH_* keys against", corpus_root.display()));
    }
    Ok(pending.into_iter().collect())
}

/// The guard `main` runs right after locating the fayth.
pub fn check(fayth_file: &Path, home: &Path) -> Result<(), String> {
    let text = fs::read_to_string(fayth_file).map_err(|e| format!("{}: {e}", fayth_file.display()))?;
    let root = home.parent().unwrap_or(home);
    let dead = unconsumed(&text, root, &home.join("chamber"))?;
    if dead.is_empty() {
        return Ok(());
    }
    Err(format!("{} declares FAYTH_* keys nothing consumes: {}", fayth_file.display(), dead.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(tag: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(&format!("fayth-keys-{tag}"));
        fs::create_dir_all(d.join("spira/chamber")).unwrap();
        d
    }

    const FAYTH: &str = "FAYTH_NAME=x\nFAYTH_LIVE_VAR=1\nFAYTH_LIVE_GET=2\nFAYTH_DEAD=3\nFAYTH_DESC=\"doc\"\n# FAYTH_COMMENTED=1\n";

    #[test]
    fn dead_key_found_and_live_keys_pass() {
        let d = tree("fixture");
        fs::write(d.join("spira/chamber/x.fayth"), FAYTH).unwrap();
        fs::write(d.join("spira/lib.sh"), "echo \"$FAYTH_LIVE_VAR\"\nfayth_get \"$f\" FAYTH_LIVE_GET 0\nFAYTH_NAME_X=1\n").unwrap();
        fs::write(d.join("spira/other.sh"), "FAYTH_DEAD_SUFFIX=1\nXFAYTH_DEAD=1\n").unwrap();
        let dead = unconsumed(FAYTH, &d, &d.join("spira/chamber")).unwrap();
        assert_eq!(dead, vec!["FAYTH_DEAD", "FAYTH_NAME"]);
        fs::write(d.join("spira/ok.rs"), "\"FAYTH_DEAD\" \"FAYTH_NAME\"").unwrap();
        assert!(unconsumed(FAYTH, &d, &d.join("spira/chamber")).unwrap().is_empty());
    }

    #[test]
    fn check_names_the_dead_key_and_refuses_an_empty_corpus() {
        let d = tree("check");
        fs::write(d.join("spira/chamber/x.fayth"), "FAYTH_DEAD=1\n").unwrap();
        let home = d.join("spira");
        let e = check(&home.join("chamber/x.fayth"), &home).unwrap_err();
        assert!(e.contains("no source files"), "{e}");
        fs::write(d.join("spira/lib.sh"), "true\n").unwrap();
        let e = check(&home.join("chamber/x.fayth"), &home).unwrap_err();
        assert!(e.contains("FAYTH_DEAD"), "{e}");
    }

    #[test]
    fn every_shipped_fayth_is_fully_consumed() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let mut n = 0;
        for dir in [home.join("chamber"), home.join("chamber/disabled")] {
            let Ok(rd) = fs::read_dir(dir) else { continue };
            for e in rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "fayth")) {
                check(&e.path(), &home).unwrap();
                n += 1;
            }
        }
        assert!(n >= 8, "found only {n} fayths");
    }
}
