//! `--artifacts <dir>`: executables cargo already built from the tree under test, somewhere
//! else (DESIGN.md D8). Validated against the workspace's binary targets, hashed into the
//! batch key, staged to `<worktree>/target/prebuilt` for the container. Nothing here runs cargo.

use crate::verdict::sha256_hex;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Required whatever the workspace says: conf.sh refuses without `spira-config` under
/// SPIRA_ARTIFACTS, every exec carries `SPIRA_TEST_PLAN_BIN`, and `testenv` is the runner.
pub const FLOOR: [&str; 3] = ["spira-config", "test-plan", "testenv"];

/// The staging directory's name under `<worktree>/target/` (the container's profile dir).
pub const STAGE_DIR: &str = "prebuilt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prebuilt {
    /// The directory given, made absolute.
    pub dir: PathBuf,
    /// Every executable directly under `dir`, sorted.
    pub names: Vec<String>,
    /// sha256 of `<name>\0<sha256 of the file>\n` per executable, in name order.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    NotADirectory(PathBuf),
    Missing { dir: PathBuf, names: Vec<String> },
    Unreadable { path: PathBuf, err: String },
}

impl Invalid {
    pub fn message(&self) -> String {
        match self {
            Invalid::NotADirectory(d) => format!(
                "batch: --artifacts {} is not a directory — refusing (nothing is built in its place)",
                d.display()
            ),
            Invalid::Missing { dir, names } => format!(
                "batch: --artifacts {} is incomplete — missing executable(s): {} — refusing to run a partial set (nothing is built in its place)",
                dir.display(),
                names.join(" ")
            ),
            Invalid::Unreadable { path, err } => {
                format!("batch: --artifacts: cannot read {}: {err}", path.display())
            }
        }
    }
}

fn is_executable_file(p: &Path) -> bool {
    fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Executables directly under `dir`, sorted by name.
fn executables(dir: &Path) -> Result<Vec<String>, Invalid> {
    let rd = fs::read_dir(dir).map_err(|e| Invalid::Unreadable {
        path: dir.to_path_buf(),
        err: e.to_string(),
    })?;
    let mut v: Vec<String> = rd
        .flatten()
        .filter(|e| is_executable_file(&e.path()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    Ok(v)
}

/// Validate `dir` against `required` and hash its executables. `dir` must be absolute.
pub fn inspect(dir: &Path, required: &[String]) -> Result<Prebuilt, Invalid> {
    if !dir.is_dir() {
        return Err(Invalid::NotADirectory(dir.to_path_buf()));
    }
    let names = executables(dir)?;
    let missing: Vec<String> = required
        .iter()
        .filter(|r| !names.contains(r))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(Invalid::Missing {
            dir: dir.to_path_buf(),
            names: missing,
        });
    }
    let mut text = Vec::new();
    for n in &names {
        let p = dir.join(n);
        let bytes = fs::read(&p).map_err(|e| Invalid::Unreadable {
            path: p.clone(),
            err: e.to_string(),
        })?;
        text.extend_from_slice(n.as_bytes());
        text.push(0);
        text.extend_from_slice(sha256_hex(&bytes).as_bytes());
        text.push(b'\n');
    }
    Ok(Prebuilt {
        dir: dir.to_path_buf(),
        names,
        id: sha256_hex(&text),
    })
}

/// Copy every executable into `dest` (removed first). A `dest` that IS the source is left.
pub fn stage(p: &Prebuilt, dest: &Path) -> std::io::Result<()> {
    if let (Ok(a), Ok(b)) = (fs::canonicalize(&p.dir), fs::canonicalize(dest)) {
        if a == b {
            return Ok(());
        }
    }
    match fs::remove_dir_all(dest) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::create_dir_all(dest)?;
    for n in &p.names {
        fs::copy(p.dir.join(n), dest.join(n))?;
    }
    Ok(())
}

/// The value of `key = "value"` on a TOML line, for the keys this reader cares about.
fn string_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let rest = rest.strip_prefix('"')?;
    rest.split('"').next()
}

/// The binary targets one member's Cargo.toml declares: each `[[bin]] name`, and the package
/// name when `src/main.rs` exists, autobins is not off and no `[[bin]]` already claims it.
/// Conservative on purpose: anything it cannot read is not required, so a form this reader
/// does not know can only under-require (the floor still holds), never refuse a good set.
pub fn member_bins(member: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(member.join("Cargo.toml")) else {
        return vec![];
    };
    let mut section = String::new();
    let mut package: Option<String> = None;
    let mut autobins = true;
    let mut bins: Vec<(Option<String>, Option<String>)> = vec![];
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            section = line.to_string();
            if section == "[[bin]]" {
                bins.push((None, None));
            }
            continue;
        }
        match section.as_str() {
            "[package]" => {
                if let Some(v) = string_value(line, "name") {
                    package = Some(v.to_string());
                }
                if line.replace(' ', "") == "autobins=false" {
                    autobins = false;
                }
            }
            "[[bin]]" => {
                if let Some(last) = bins.last_mut() {
                    if let Some(v) = string_value(line, "name") {
                        last.0 = Some(v.to_string());
                    }
                    if let Some(v) = string_value(line, "path") {
                        last.1 = Some(v.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<String> = bins.iter().filter_map(|(n, _)| n.clone()).collect();
    if let Some(pkg) = package {
        let claimed = bins.iter().any(|(n, p)| {
            p.as_deref().map(|p| p.trim_start_matches("./")) == Some("src/main.rs")
                || (p.is_none() && n.as_deref() == Some(pkg.as_str()))
        });
        if autobins && !claimed && member.join("src/main.rs").is_file() && !out.contains(&pkg) {
            out.push(pkg);
        }
    }
    out
}

/// Workspace members from the root Cargo.toml's `members = [...]` (literal paths only).
pub fn members(root: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(root.join("Cargo.toml")) else {
        return vec![];
    };
    let mut in_ws = false;
    let mut collecting = false;
    let mut out = vec![];
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && !collecting {
            in_ws = line == "[workspace]";
            continue;
        }
        if !in_ws {
            continue;
        }
        let body = if collecting {
            line
        } else if let Some(r) = line
            .strip_prefix("members")
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('='))
        {
            collecting = true;
            r.trim().trim_start_matches('[')
        } else {
            continue;
        };
        for part in body.split(',') {
            let p = part.trim().trim_end_matches(']').trim().trim_matches('"');
            if !p.is_empty() && !p.contains('*') {
                let path = root.join(p);
                if !out.contains(&path) {
                    out.push(path);
                }
            }
        }
        if body.contains(']') {
            collecting = false;
            in_ws = false;
        }
    }
    out
}

/// What a prebuilt set for the tree at `worktree` must hold: the floor plus every workspace
/// binary target, sorted and deduplicated.
pub fn required(worktree: &Path) -> Vec<String> {
    let mut v: Vec<String> = FLOOR.iter().map(|s| s.to_string()).collect();
    for m in members(worktree) {
        v.extend(member_bins(&m));
    }
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("testenv-prebuilt-{tag}"))
    }

    fn exe(dir: &Path, name: &str, body: &str) {
        testkit::write_exe(dir.join(name), body);
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_complete_set_is_accepted_and_hashed_by_content() {
        let d = tmp("ok");
        for n in FLOOR {
            exe(&d, n, n);
        }
        fs::write(d.join("notes.d"), "not executable").unwrap();
        let p = inspect(&d, &strings(&FLOOR)).unwrap();
        assert_eq!(p.names, strings(&["spira-config", "test-plan", "testenv"]));
        assert_eq!(p.id.len(), 64);
        let same = inspect(&d, &strings(&FLOOR)).unwrap();
        assert_eq!(p.id, same.id);
        exe(&d, "testenv", "a different build");
        assert_ne!(p.id, inspect(&d, &strings(&FLOOR)).unwrap().id);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_directory_or_binary_is_refused_by_name() {
        let d = tmp("bad");
        assert_eq!(
            inspect(&d.join("nope"), &strings(&FLOOR)),
            Err(Invalid::NotADirectory(d.join("nope")))
        );
        exe(&d, "testenv", "t");
        fs::write(d.join("test-plan"), "not executable").unwrap();
        match inspect(&d, &strings(&FLOOR)) {
            Err(e @ Invalid::Missing { .. }) => {
                assert!(e
                    .message()
                    .contains("missing executable(s): spira-config test-plan"));
            }
            other => panic!("{other:?}"),
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn staging_replaces_the_previous_set() {
        let d = tmp("stage");
        let src = d.join("bin");
        let dest = d.join("wt/target/prebuilt");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("stale"), "old").unwrap();
        exe(&src, "testenv", "t");
        let p = inspect(&src, &strings(&["testenv"])).unwrap();
        stage(&p, &dest).unwrap();
        assert!(!dest.join("stale").exists());
        assert!(is_executable_file(&dest.join("testenv")));
        // staging onto itself is a no-op, not a self-deletion
        let p2 = inspect(&dest, &strings(&["testenv"])).unwrap();
        stage(&p2, &dest).unwrap();
        assert!(dest.join("testenv").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn workspace_binary_targets_are_read_from_the_manifests() {
        let d = tmp("ws");
        fs::write(
            d.join("Cargo.toml"),
            "[workspace]\nmembers = [\n    \"a\",\n    \"nested/b\", \"lib-only\",\n    \"c\", # comment\n    \"a\",\n]\nresolver = \"2\"\n\n[profile.aeon]\ninherits = \"dev\"\n",
        )
        .unwrap();
        let member = |p: &str, toml: &str, main: bool| {
            let m = d.join(p);
            fs::create_dir_all(m.join("src")).unwrap();
            fs::write(m.join("Cargo.toml"), toml).unwrap();
            if main {
                fs::write(m.join("src/main.rs"), "fn main(){}").unwrap();
            }
        };
        // [[bin]] renames the package's main
        member(
            "a",
            "[package]\nname = \"pkg-a\"\n\n[[bin]]\nname = \"tool-a\"\npath = \"src/main.rs\"\n",
            true,
        );
        // plain src/main.rs: the package name
        member(
            "nested/b",
            "[package]\nname = \"b\"\nversion = \"1.0.0\"\n",
            true,
        );
        // a library: nothing
        member("lib-only", "[package]\nname = \"lib-only\"\n", false);
        // autobins off: only the explicit [[bin]]
        member(
            "c",
            "[package]\nname = \"c\"\nautobins = false\n[[bin]]\nname = \"c-extra\"\npath = \"src/extra.rs\"\n[dependencies]\nname = \"not-a-bin\"\n",
            true,
        );
        assert_eq!(members(&d).len(), 4);
        assert_eq!(
            required(&d),
            strings(&[
                "b",
                "c-extra",
                "spira-config",
                "test-plan",
                "testenv",
                "tool-a"
            ])
        );
        let _ = fs::remove_dir_all(&d);
    }
}
