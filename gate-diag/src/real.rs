//! The production [`crate::ports::World`]: the filesystem and the environment.

use crate::ports::{ResultFile, World};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Real;

/// `std::env::var`, trimmed to "set and non-empty" — no `spira_config` fallback. Used only
/// inside [`resolved_config`]'s own init, which must not call back into [`env`] (that
/// would recurse into `resolved_config()` while it is still being built).
fn raw_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Wave 4.8 ("retire conf re-import seams in Rust"): this crate used to read
/// SPIRA_SUITE_TIMEOUT (and SPIRA_BATCH_TAIL_LINES) straight out of its own process
/// environment, with no snapshot and no config document load at all (wave4-decomposition.md
/// row (b) names gate-diag by file). `spira_config::resolve_for_process`, using `home` —
/// the SAME `--home`/`SPIRA_HOME`/release-relative value `main.rs`'s own `run()` already
/// resolved once and threads through every `World` call, never recomputed independently
/// here (a `--home` override must reach this too) — and
/// `spira_config::resolve::derive_home_repo`. Computed fresh each call rather than cached:
/// [`Real::batch_tail_lines`]/[`Real::suite_timeout_default`] are each called exactly once
/// per process, so there is no hot loop to amortise against, and a cache keyed on nothing
/// would go stale the moment two different `home`s were ever in play (as two different
/// test fixtures would be). A resolution failure yields an empty
/// [`spira_config::resolve::Resolved`] — [`env`]'s own callers see exactly the behaviour
/// this crate had before this bead, never a panic.
fn env(home: &Path, name: &str) -> Option<String> {
    raw_env(name).or_else(|| {
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let repo = spira_config::resolve::derive_home_repo(home, &env_map);
        let resolved = spira_config::resolve::resolve_or_say("gate-diag", home, &repo, &env_map);
        let v = resolved.get(name);
        (!v.is_empty()).then(|| v.to_string())
    })
}

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

    fn batch_tail_lines(&self, home: &Path) -> usize {
        env(home, "SPIRA_BATCH_TAIL_LINES").and_then(|v| v.parse().ok()).unwrap_or(50)
    }

    fn suite_timeout_default(&self, home: &Path) -> u64 {
        env(home, "SPIRA_SUITE_TIMEOUT").and_then(|v| v.parse().ok()).unwrap_or(600)
    }

    fn print(&self, s: &str) {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ENV VARS ARE PROCESS-GLOBAL (spira-config's own locate.rs/lib.rs tests guard the
    // same hazard): `env()`'s own `resolve_for_process` discovers the config document from
    // THIS PROCESS's real environment (SPIRA_TOML/HOME/XDG_CONFIG_HOME), never from `home`
    // alone — this test takes a lock and pins SPIRA_TOML to a nonexistent path so it
    // never depends on a real operator config document on the machine running this suite.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `home`, not any OTHER process-global state, decides which registry this reads —
    /// `--home` reaches `env()` through it rather than a recomputed default.
    #[test]
    fn env_falls_back_to_the_registry_then_the_callers_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = std::env::var("SPIRA_TOML").ok();
        let dir = testkit::TempDir::new("gate-diag-real-env");
        std::env::set_var("SPIRA_TOML", dir.join("no-such-config.toml"));
        let home = dir.join("spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(
            home.join("conf.d/SPIRA_SUITE_TIMEOUT"),
            "TYPE=u32\nGROUP=gate\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_SUITE_TIMEOUT:=600}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();

        let got_timeout = env(&home, "SPIRA_SUITE_TIMEOUT");
        let got_missing = env(&home, "SPIRA_NO_SUCH_KEY_AT_ALL_EVER");

        match saved {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(got_timeout, Some("600".to_string()), "a registry default must reach env() without an env override");
        assert_eq!(got_missing, None);
    }
}
