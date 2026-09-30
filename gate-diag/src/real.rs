//! The production [`crate::ports::World`]: the filesystem and the environment.

use crate::ports::{ResultFile, World};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Real;

fn sorted_glob(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some(ext) && p.is_file() {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn sorted_subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn to_result_file(p: PathBuf) -> ResultFile {
    let suite = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let out_path = p.with_extension("out");
    ResultFile { suite, result_path: p, out_path }
}

impl World for Real {
    fn result_files(&self, root: &Path) -> Vec<ResultFile> {
        let mut out: Vec<ResultFile> = sorted_glob(root, "result").into_iter().map(to_result_file).collect();
        for sub in sorted_subdirs(root) {
            out.extend(sorted_glob(&sub, "result").into_iter().map(to_result_file));
        }
        out
    }

    fn read_result(&self, p: &Path) -> (String, Option<u64>, Option<i32>) {
        let Some(text) = fs::read_to_string(p).ok() else { return (String::new(), None, None) };
        let fields: Vec<&str> = text.split_whitespace().collect();
        let status = fields.first().copied().unwrap_or("").to_string();
        let secs = fields.get(2).and_then(|s| s.parse::<u64>().ok());
        let rc = fields.get(6).and_then(|s| s.parse::<i32>().ok());
        (status, secs, rc)
    }

    fn read(&self, p: &Path) -> Option<String> {
        fs::read_to_string(p).ok().map(|s| s.trim_end_matches('\n').to_string())
    }

    fn suite_source(&self, home: &Path, suite: &str) -> Option<String> {
        self.read(&home.join(suite))
    }

    fn retry_status(&self, root: &Path, suite: &str) -> Option<String> {
        let retry = PathBuf::from(format!("{}-retry", root.display()));
        if !retry.is_dir() {
            return None;
        }
        let direct = retry.join(format!("{suite}.result"));
        if direct.is_file() {
            return self.read(&direct).and_then(|s| s.split_whitespace().next().map(String::from));
        }
        for sub in sorted_subdirs(&retry) {
            let p = sub.join(format!("{suite}.result"));
            if p.is_file() {
                return self.read(&p).and_then(|s| s.split_whitespace().next().map(String::from));
            }
        }
        None
    }

    fn write(&self, p: &Path, content: &str) {
        let _ = fs::write(p, content);
    }

    fn append(&self, p: &Path, content: &str) {
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = f.write_all(content.as_bytes());
        }
    }

    fn github_actions(&self) -> bool {
        std::env::var("GITHUB_ACTIONS").map(|v| !v.is_empty()).unwrap_or(false)
    }

    fn step_summary_path(&self) -> Option<PathBuf> {
        std::env::var_os("GITHUB_STEP_SUMMARY").filter(|v| !v.is_empty()).map(PathBuf::from)
    }

    fn batch_tail_lines(&self) -> usize {
        std::env::var("SPIRA_BATCH_TAIL_LINES").ok().and_then(|v| v.parse().ok()).unwrap_or(50)
    }

    fn suite_timeout_default(&self) -> u64 {
        std::env::var("SPIRA_SUITE_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(600)
    }

    fn print(&self, s: &str) {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }
}
