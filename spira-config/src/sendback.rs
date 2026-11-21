//! The full output of the steps that judged a bead, kept where its builder can read it after
//! the bead is sent back. One entry is a directory of files under `<run>/sendback-logs`; its id
//! is `<actor>-<epoch>-<n>`. Every write removes the entries older than the retention, so the
//! store bounds itself.

use std::fs;
use std::path::{Path, PathBuf};

pub const DIR: &str = "sendback-logs";
const DAY_SECS: i64 = 24 * 3600;

pub fn root(run: &Path) -> PathBuf {
    run.join(DIR)
}

fn plain_name(n: &str) -> Result<(), String> {
    if n.is_empty() || n.contains('/') || n.contains("..") || n.contains('\0') {
        return Err(format!("{n:?} is not a plain name"));
    }
    Ok(())
}

fn epoch_of(id: &str) -> Option<i64> {
    let mut p = id.rsplitn(3, '-');
    let (_n, at) = (p.next()?, p.next()?);
    p.next()?;
    at.parse().ok()
}

/// Writes `files` as a new entry and returns its id; entries older than `keep_days` go.
pub fn keep(run: &Path, actor: &str, files: &[(String, Vec<u8>)], keep_days: u32, now: i64) -> Result<String, String> {
    plain_name(actor)?;
    for (name, _) in files {
        plain_name(name)?;
    }
    let dir = root(run);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    prune(&dir, keep_days, now);
    for n in 0.. {
        let id = format!("{actor}-{now}-{n}");
        let entry = dir.join(&id);
        match fs::create_dir(&entry) {
            Ok(()) => {
                for (name, bytes) in files {
                    if let Err(e) = fs::write(entry.join(name), bytes) {
                        let _ = fs::remove_dir_all(&entry);
                        return Err(format!("{}: {e}", entry.join(name).display()));
                    }
                }
                return Ok(id);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", entry.display())),
        }
    }
    unreachable!()
}

/// Keeps one text file as a new entry and returns the path it lives at.
pub fn keep_text(run: &Path, actor: &str, name: &str, text: &str, days: u32) -> Result<PathBuf, String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let id = keep(run, actor, &[(name.to_string(), text.as_bytes().to_vec())], days, now)?;
    Ok(root(run).join(id).join(name))
}

fn prune(dir: &Path, keep_days: u32, now: i64) {
    let cutoff = now - i64::from(keep_days) * DAY_SECS;
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if e.file_name().to_str().and_then(epoch_of).is_some_and(|at| at < cutoff) {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

fn entry_dir(run: &Path, id: &str) -> Result<PathBuf, String> {
    plain_name(id)?;
    let d = root(run).join(id);
    if d.is_dir() {
        Ok(d)
    } else {
        Err(format!("{id}: pruned (entries are kept SPIRA_SENDBACK_KEEP_DAYS days)"))
    }
}

/// Each file of an entry with its size, by name.
pub fn list(run: &Path, id: &str) -> Result<Vec<(String, u64)>, String> {
    let d = entry_dir(run, id)?;
    let mut out: Vec<(String, u64)> = fs::read_dir(&d)
        .map_err(|e| format!("{}: {e}", d.display()))?
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        .collect();
    out.sort();
    Ok(out)
}

pub fn read(run: &Path, id: &str, file: &str) -> Result<Vec<u8>, String> {
    let d = entry_dir(run, id)?;
    plain_name(file)?;
    fs::read(d.join(file)).map_err(|e| format!("{id}/{file}: {e}"))
}

const EXCERPT_CAP: usize = 300;
const SHELL_CONTEXT: usize = 2;
const TEST_BLOCK_CAP: usize = 20;
pub const NO_FAILURE_FOUND: &str =
    "(no failure lines matched; the last 80 lines of the output follow)";

/// The failure itself, not the output's tail: cargo's `---- test stdout ----` blocks, failures
/// list, `test result: FAILED` and `error: N target(s) failed`; or a shell suite's FAIL / not ok
/// lines with their context. Always a `FAIL ` line with its indented panic block, once per test.
/// The tail, announced as such, only when nothing matches.
pub fn failure_excerpt(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![false; lines.len()];
    let mut inside = false;
    let mut running: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("Running ") || t.starts_with("Doc-tests ") {
            running = Some(i);
        }
        let block = t.starts_with("---- ") && t.trim_end().ends_with(" ----");
        if block || (t == "failures:" && !inside) {
            if !inside {
                if let Some(r) = running.take() {
                    keep[r] = true;
                }
            }
            inside = true;
        }
        if t.starts_with("test result:") {
            if t.contains("FAILED") {
                keep[i] = true;
            }
            inside = false;
        } else if (t.starts_with("error: ")
            && t.contains("failed")
            && !t.starts_with("error: could not compile"))
            || (t.starts_with("error: could not compile") && t.contains("test"))
        {
            keep[i] = true;
        }
        if inside {
            keep[i] = true;
        }
    }
    let cargo = keep.iter().any(|k| *k);
    if !cargo {
        for (i, l) in lines.iter().enumerate() {
            if l.contains("FAIL") || l.trim_start().starts_with("not ok") {
                for k in keep
                    .iter_mut()
                    .take(lines.len().min(i + 1 + SHELL_CONTEXT))
                    .skip(i)
                {
                    *k = true;
                }
            }
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if !t.starts_with("FAIL ") {
            i += 1;
            continue;
        }
        let indent = lines[i].len() - t.len();
        let first = !seen.contains(&t);
        seen.push(t);
        i += 1;
        let mut end = i;
        while end < lines.len()
            && end - i < TEST_BLOCK_CAP
            && !lines[end].trim().is_empty()
            && lines[end].len() - lines[end].trim_start().len() > indent
        {
            end += 1;
        }
        if first {
            keep[i - 1..end].iter_mut().for_each(|k| *k = true);
        }
        i = end;
    }
    let picked: Vec<&str> = lines
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| *l)
        .collect();
    if picked.is_empty() {
        return format!("{NO_FAILURE_FOUND}\n{}", tail(text, 80));
    }
    if picked.len() > EXCERPT_CAP {
        return format!(
            "(failure excerpt cut to its last {EXCERPT_CAP} lines)\n{}",
            picked[picked.len() - EXCERPT_CAP..].join("\n")
        );
    }
    picked.join("\n")
}

pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(tag)
    }

    fn files() -> Vec<(String, Vec<u8>)> {
        vec![("trial.out".into(), b"red \xff\n".to_vec()), ("sift.txt".into(), b"why".to_vec())]
    }

    #[test]
    fn keep_writes_files_that_list_and_read_return_byte_for_byte() {
        let run = tmp("rt");
        let id = keep(run.path(), "gate", &files(), 7, 1000).unwrap();
        assert_eq!(id, "gate-1000-0");
        assert_eq!(list(run.path(), &id).unwrap(), vec![("sift.txt".to_string(), 3), ("trial.out".to_string(), 6)]);
        assert_eq!(read(run.path(), &id, "trial.out").unwrap(), b"red \xff\n");
        assert_eq!(keep(run.path(), "gate", &files(), 7, 1000).unwrap(), "gate-1000-1");
    }

    #[test]
    fn an_entry_past_the_retention_is_removed_on_the_next_write_and_a_younger_one_kept() {
        let run = tmp("prune");
        let now = 100 * DAY_SECS;
        let old = keep(run.path(), "round", &files(), 7, now - 8 * DAY_SECS).unwrap();
        let young = keep(run.path(), "batch-cut", &files(), 7, now - 6 * DAY_SECS).unwrap();
        keep(run.path(), "gate", &files(), 7, now).unwrap();
        assert!(read(run.path(), &old, "sift.txt").unwrap_err().contains("pruned"));
        assert!(read(run.path(), &young, "sift.txt").is_ok());
    }

    #[test]
    fn read_refuses_an_id_or_file_that_climbs_out_of_the_store() {
        let run = tmp("esc");
        let id = keep(run.path(), "gate", &files(), 7, 5).unwrap();
        assert!(read(run.path(), "../x", "trial.out").is_err());
        assert!(read(run.path(), &id, "../../etc").is_err());
        assert!(read(run.path(), &id, "a/b").is_err());
        assert!(list(run.path(), "../x").is_err());
    }
}
