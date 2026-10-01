//! Which transcripts are a live session at the keyboard. Three filters, each one because
//! its absence produces a wrong answer rather than a noisy one:
//!
//!   too old     nothing here matters once nobody is still in the conversation; a
//!               transcript untouched for longer than the idle window is history.
//!   the harness's own   an aeon's unfinished business is its BEAD, not a transcript —
//!               archiving one files a second, worse copy of work the graph already
//!               tracks. The archivist's own session (it runs with its cwd inside
//!               `$SPIRA_RUN` for exactly this reason) is excluded the same way.
//!   empty       a session that has not spoken has nothing to rescue.
//!
//! The exclusion is derived from the runtime directory rather than written out, because
//! the client's project directory is the working directory with every non-alphanumeric
//! character replaced by a hyphen — any path under the runtime directory has that
//! directory's slug as a prefix.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub fn slug(p: &str) -> String {
    p.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn mtime_secs(p: &Path) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?;
    Some(t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
}

/// `<projects>/*/*.jsonl` that are non-empty, touched within `idle_secs`, and not under a
/// project directory whose slug matches the runtime directory's own (configured or
/// resolved path). Returns `(session-id, transcript-path)` sorted by path, matching the
/// bash's `sorted(glob.glob(...))`.
pub fn live_transcripts(projects: &Path, run: &Path, idle_secs: i64, now: i64) -> Vec<(String, PathBuf)> {
    let run_disp = run.to_string_lossy().into_owned();
    let run_real = std::fs::canonicalize(run).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| run_disp.clone());
    let ours = [slug(&run_disp), slug(&run_real)];

    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(projects) else { return out };
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        let dname = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if ours.iter().any(|o| dname.starts_with(o.as_str())) {
            continue;
        }
        let Ok(rd2) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = rd2.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "jsonl").unwrap_or(false)).collect();
        files.sort();
        for f in files {
            let Ok(meta) = std::fs::metadata(&f) else { continue };
            if meta.len() == 0 {
                continue;
            }
            let Some(mt) = mtime_secs(&f) else { continue };
            if now - mt > idle_secs {
                continue;
            }
            let sid = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            out.push((sid, f));
        }
    }
    out
}

/// The most recently written non-empty transcript under `projects` — the "now" mode's
/// no-argument pick, "the one they are sitting in".
pub fn newest_transcript(projects: &Path) -> Option<(String, PathBuf)> {
    let rd = std::fs::read_dir(projects).ok()?;
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for dir in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Ok(rd2) = std::fs::read_dir(&dir) else { continue };
        for f in rd2.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "jsonl").unwrap_or(false)) {
            let Ok(meta) = std::fs::metadata(&f) else { continue };
            if meta.len() == 0 {
                continue;
            }
            let Ok(mt) = meta.modified() else { continue };
            if best.as_ref().map(|(_, bt)| mt > *bt).unwrap_or(true) {
                best = Some((f, mt));
            }
        }
    }
    best.map(|(f, _)| {
        let sid = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        (sid, f)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, contents: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    #[test]
    fn excludes_empty_transcripts() {
        let root = testkit::TempDir::new("archivist-live");
        let projects = root.path().join("projects");
        touch(&projects.join("proj-a").join("sess1.jsonl"), "");
        let run = root.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let live = live_transcripts(&projects, &run, 999_999, 1_000_000);
        assert!(live.is_empty());
    }

    #[test]
    fn excludes_transcripts_older_than_idle() {
        let root = testkit::TempDir::new("archivist-live");
        let projects = root.path().join("projects");
        let f = projects.join("proj-a").join("sess1.jsonl");
        touch(&f, "hello");
        let mt = mtime_secs(&f).unwrap();
        let run = root.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let live = live_transcripts(&projects, &run, 10, mt + 1000);
        assert!(live.is_empty(), "a transcript 1000s stale with a 10s idle window must be excluded");
    }

    #[test]
    fn includes_a_fresh_nonempty_transcript() {
        let root = testkit::TempDir::new("archivist-live");
        let projects = root.path().join("projects");
        let f = projects.join("proj-a").join("sess1.jsonl");
        touch(&f, "hello");
        let mt = mtime_secs(&f).unwrap();
        let run = root.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let live = live_transcripts(&projects, &run, 10_000, mt);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].0, "sess1");
    }

    #[test]
    fn excludes_the_harness_own_project_directory() {
        let root = testkit::TempDir::new("archivist-live");
        let run = root.path().join("run-dir");
        std::fs::create_dir_all(&run).unwrap();
        let run_slug = slug(&run.to_string_lossy());
        let projects = root.path().join("projects");
        let ours_dir = projects.join(format!("{run_slug}-cwd"));
        touch(&ours_dir.join("sess-ours.jsonl"), "hello");
        touch(&projects.join("other-proj").join("sess-theirs.jsonl"), "hello");
        let now = mtime_secs(&projects.join("other-proj").join("sess-theirs.jsonl")).unwrap();
        let live = live_transcripts(&projects, &run, 10_000, now);
        let sids: Vec<&str> = live.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(sids, vec!["sess-theirs"]);
    }

    #[test]
    fn newest_transcript_picks_the_most_recently_modified() {
        let root = testkit::TempDir::new("archivist-newest");
        let projects = root.path().join("projects");
        touch(&projects.join("p").join("old.jsonl"), "x");
        std::thread::sleep(std::time::Duration::from_millis(20));
        touch(&projects.join("p").join("new.jsonl"), "y");
        let (sid, _) = newest_transcript(&projects).unwrap();
        assert_eq!(sid, "new");
    }

    #[test]
    fn newest_transcript_skips_empty_files() {
        let root = testkit::TempDir::new("archivist-newest");
        let projects = root.path().join("projects");
        touch(&projects.join("p").join("empty.jsonl"), "");
        touch(&projects.join("p").join("real.jsonl"), "x");
        let (sid, _) = newest_transcript(&projects).unwrap();
        assert_eq!(sid, "real");
    }
}
