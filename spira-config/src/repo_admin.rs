//! `repo add` and `repo check`: registering a repository with the harness, and proving every
//! piece of plumbing a bead for it needs. `add` is idempotent and writes only through the
//! config writers; `check` stops at the first missing piece and names it.

use crate::repos::{self, Registry};
use std::path::{Path, PathBuf};

const TESTENV_DECL: &str = ".spira/testenv.toml";
const HARNESS_IMAGE: &str = "spira/testenv/Containerfile";
const UNITS_DECL: &str = ".spira/units";
const TESTENV_TEMPLATE: &str = "# Placeholder entry point: replace argv with the repository's own checks.\nimage = \"docker.io/library/debian:stable-slim\"\ndefault = [\"unit\"]\n\n[tier.unit]\nargv = [\"true\"]\n";
const UNITS_TEMPLATE: &str =
    "# Globs of the systemd units this repository's own deploy installs, one per line.\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queue {
    Local,
    Forge,
}

impl Queue {
    pub fn parse(s: &str) -> Option<Queue> {
        match s {
            "local" | "queue.local" => Some(Queue::Local),
            "forge" | "queue.forge" => Some(Queue::Forge),
            _ => None,
        }
    }

    fn mode(self) -> &'static str {
        match self {
            Queue::Local => "queue.local",
            Queue::Forge => "queue.forge",
        }
    }

    fn default_land_ref(self) -> &'static str {
        match self {
            Queue::Local => "local/main",
            Queue::Forge => "origin/main",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AddArgs {
    pub name: String,
    pub path: PathBuf,
    pub queue: Queue,
    pub remote: Option<String>,
    pub land_ref: Option<String>,
    pub format: String,
    pub gate: String,
}

/// The files `add` writes: the repo-map when one is in force (it outranks `[repo.*]`), else
/// `[repo.<name>]` in the toml; `[publish.<name>]` always goes to the toml.
#[derive(Debug, Clone)]
pub struct Targets {
    pub map: Option<PathBuf>,
    pub toml: Option<PathBuf>,
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = crate::bounded::bounded("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=spira", "-c", "user.email=spira@localhost"])
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn git_ok(repo: &Path, args: &[&str]) -> bool {
    git(repo, args).is_ok()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn row_line(a: &AddArgs, land_ref: &str) -> String {
    format!(
        "{} | {} | {} | {} | {} | {} | plan",
        a.name,
        a.path.display(),
        a.queue.mode(),
        land_ref,
        a.format,
        a.gate
    )
}

pub fn add(a: &AddArgs, t: &Targets) -> Result<Vec<String>, String> {
    let mut did = Vec::new();
    if !valid_name(&a.name) {
        return Err(format!(
            "repo add: {:?} is not a repository name (letters, digits, - and _)",
            a.name
        ));
    }
    if !a.path.is_absolute() {
        return Err(format!(
            "repo add: --path {} must be absolute",
            a.path.display()
        ));
    }
    for (what, v) in [("--format", &a.format), ("--gate", &a.gate)] {
        if v.contains('|') || v.contains('\n') {
            return Err(format!(
                "repo add: {what} may not contain a pipe or newline"
            ));
        }
    }
    let land_ref = a
        .land_ref
        .clone()
        .unwrap_or_else(|| a.queue.default_land_ref().to_string());
    if a.queue == Queue::Forge && repos::ref_remote(&land_ref, None).is_none() {
        return Err(format!(
            "repo add: queue.forge lands on a remote ref such as origin/main, not {land_ref}"
        ));
    }
    let branch = repos::ref_branch(&land_ref);

    if !a.path.join(".git").exists() {
        std::fs::create_dir_all(&a.path).map_err(|e| format!("{}: {e}", a.path.display()))?;
        git(&a.path, &["init", "-q"])?;
        let head = if a.queue == Queue::Local {
            land_ref.as_str()
        } else {
            branch.as_str()
        };
        git(
            &a.path,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{head}")],
        )?;
        did.push(format!(
            "initialised git repository at {}",
            a.path.display()
        ));
    } else if !git_ok(&a.path, &["rev-parse", "--git-dir"]) {
        return Err(format!(
            "repo add: {} has a .git that git cannot read",
            a.path.display()
        ));
    }
    if !git_ok(&a.path, &["rev-parse", "--verify", "-q", "HEAD"]) {
        git(
            &a.path,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("spira: register {}", a.name),
            ],
        )?;
        did.push("made the first commit".to_string());
    }
    if let Some(url) = &a.remote {
        match git(&a.path, &["remote", "get-url", "origin"]) {
            Ok(have) if &have == url => {}
            Ok(have) => return Err(format!("repo add: origin is already {have}, not {url}")),
            Err(_) => {
                git(&a.path, &["remote", "add", "origin", url])?;
                did.push(format!("added remote origin {url}"));
            }
        }
    }
    if a.queue == Queue::Local && !git_ok(&a.path, &["rev-parse", "--verify", "-q", &land_ref]) {
        git(&a.path, &["branch", &land_ref, "HEAD"])?;
        did.push(format!("created landing ref {land_ref}"));
    }

    register(a, &land_ref, t, &mut did)?;

    if a.queue == Queue::Local {
        let remote = if a.remote.is_some() {
            "origin"
        } else {
            repos::NO_PUBLISH
        };
        let toml = t
            .toml
            .as_ref()
            .ok_or("repo add: no spira.toml located to carry [publish.<name>]")?;
        let key = format!("publish.{}.remote", a.name);
        let have = crate::load(toml)
            .ok()
            .and_then(|d| d.publish.get(&a.name).and_then(|p| p.remote.clone()));
        if have.as_deref() != Some(remote) {
            crate::set_paths_in_file(toml, &[(&key, remote)])?;
            did.push(format!("wrote [publish.{}] remote = {remote:?}", a.name));
        }
    }

    let mut seeded = Vec::new();
    for (rel, body) in [
        (TESTENV_DECL, TESTENV_TEMPLATE),
        (UNITS_DECL, UNITS_TEMPLATE),
    ] {
        let f = a.path.join(rel);
        if !f.exists() {
            if let Some(dir) = f.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&f, body).map_err(|e| format!("{}: {e}", f.display()))?;
            seeded.push(rel);
        }
    }
    if !seeded.is_empty() {
        did.push(format!("seeded {}", seeded.join(", ")));
        let on_land = git(&a.path, &["symbolic-ref", "-q", "--short", "HEAD"])
            .map(|h| h == branch || h == land_ref)
            .unwrap_or(false);
        if on_land {
            let mut args = vec!["add", "--"];
            args.extend(seeded.iter().copied());
            git(&a.path, &args)?;
            let mut args = vec!["commit", "-q", "-m", "spira: seed .spira plumbing", "--"];
            args.extend(seeded.iter().copied());
            git(&a.path, &args)?;
            did.push("committed the seeded files".to_string());
        } else {
            did.push(format!(
                "left the seeded files uncommitted (HEAD is not {land_ref})"
            ));
        }
    }
    Ok(did)
}

fn register(a: &AddArgs, land_ref: &str, t: &Targets, did: &mut Vec<String>) -> Result<(), String> {
    if let Some(map) = t.map.as_ref().filter(|m| m.is_file()) {
        let text = std::fs::read_to_string(map).map_err(|e| format!("{}: {e}", map.display()))?;
        let want = repos::parse(&row_line(a, land_ref)).remove(0);
        if let Some(have) = repos::parse(&text).into_iter().find(|r| r.name == a.name) {
            let same = have.path == want.path
                && have.land == want.land
                && have.base == want.base
                && have.format == want.format
                && have.gate == want.gate;
            return if same {
                Ok(())
            } else {
                Err(format!(
                    "repo add: {} is already registered differently in {} ({} | {} | {} | {})",
                    a.name,
                    map.display(),
                    have.path,
                    have.land,
                    have.base,
                    have.format
                ))
            };
        }
        let mut out = text;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&row_line(a, land_ref));
        out.push('\n');
        crate::backup_existing(map).map_err(|e| format!("{}: {e}", map.display()))?;
        crate::write_atomic(map, &out).map_err(|e| format!("{}: {e}", map.display()))?;
        did.push(format!("appended the {} row to {}", a.name, map.display()));
        return Ok(());
    }
    let toml = t
        .toml
        .as_ref()
        .ok_or("repo add: neither a repo-map nor a spira.toml is in force")?;
    if !a.gate.is_empty() {
        return Err("repo add: --gate needs a repo-map; [repo.*] retired the gate key".to_string());
    }
    let doc = crate::load(toml).ok();
    let path_s = a.path.display().to_string();
    if let Some(have) = doc.as_ref().and_then(|d| d.repo.get(&a.name)) {
        let rows = repos::rows_from_toml(doc.as_ref().expect("just matched"));
        let r = rows.iter().find(|r| r.name == a.name).expect("same table");
        let same = have.path == path_s
            && r.land == a.queue.mode()
            && r.base == land_ref
            && r.format == a.format;
        return if same {
            Ok(())
        } else {
            Err(format!(
                "repo add: {} is already registered differently in {}",
                a.name,
                toml.display()
            ))
        };
    }
    let n = &a.name;
    let mut table = serde_json::json!({"mode": a.queue.mode(), "path": path_s, "base": land_ref, "lanes": ["plan"]});
    if !a.format.is_empty() {
        table["format"] = serde_json::Value::String(a.format.clone());
    }
    crate::set_paths_in_file(toml, &[(&format!("repo.{n}"), &table.to_string())])?;
    did.push(format!("wrote [repo.{n}] to {}", toml.display()));
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub name: &'static str,
    pub detail: String,
    pub ok: bool,
}

fn command_exists(cmd: &str) -> bool {
    let Some(word) = cmd.split_whitespace().next() else {
        return true;
    };
    if word.contains('/') {
        return Path::new(word).exists();
    }
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .any(|d| Path::new(d).join(word).is_file())
}

fn in_tree(repo: &Path, land_ref: &str, rel: &str) -> bool {
    git_ok(repo, &["cat-file", "-e", &format!("{land_ref}:{rel}")])
}

/// Every piece of plumbing, in order, stopping after the first that is missing.
pub fn check(reg: &Registry, name: &str) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    macro_rules! piece {
        ($n:expr, $r:expr) => {{
            let (ok, detail): (bool, String) = match $r {
                Ok(d) => (true, d),
                Err(d) => (false, d),
            };
            out.push(Piece {
                name: $n,
                detail,
                ok,
            });
            if !ok {
                return out;
            }
        }};
    }
    piece!(
        "registered",
        if reg.names().iter().any(|n| n == name) {
            Ok("in the repo registry".to_string())
        } else {
            Err(format!("{name} is not in the repo registry"))
        }
    );
    let root = reg.root(name).unwrap_or_default();
    let path = PathBuf::from(&root);
    piece!(
        "path",
        if path.join(".git").exists() {
            Ok(root.clone())
        } else {
            Err(format!("{root:?} is not a git checkout"))
        }
    );
    let land = repos::landref(reg, name);
    piece!(
        "landref",
        land.clone()
            .ok_or_else(|| "the landing ref does not resolve".to_string())
    );
    let land = land.expect("checked");
    piece!("worktree", cut_worktree(&path, &land));
    for (n, v) in [
        ("format", reg.format(name).unwrap_or_default()),
        ("gate", reg.gate(name).unwrap_or_default()),
    ] {
        piece!(
            n,
            if v.is_empty() {
                Ok("none declared".to_string())
            } else if command_exists(&v) {
                Ok(v)
            } else {
                Err(format!("command not found: {v}"))
            }
        );
    }
    piece!(
        "test-entry",
        if in_tree(&path, &land, TESTENV_DECL) || in_tree(&path, &land, HARNESS_IMAGE) {
            Ok(format!("test entry point on {land}"))
        } else {
            Err(format!("{TESTENV_DECL} is not committed on {land}"))
        }
    );
    out.push(Piece {
        name: "units",
        detail: if in_tree(&path, &land, UNITS_DECL) {
            format!("{UNITS_DECL} on {land}")
        } else {
            "no units declared".to_string()
        },
        ok: true,
    });
    let mode = reg.land(name);
    let remote = repos::ref_remote(&land, Some(&root));
    match mode.as_str() {
        "queue.local" => {
            piece!(
                "queue-mode",
                if remote.is_none() {
                    Ok(format!("{land} is a local branch"))
                } else {
                    Err(format!(
                        "queue.local needs a local landing ref, {land} is remote-tracking"
                    ))
                }
            );
            piece!(
                "publish",
                if reg.publish_declared(name) {
                    Ok("[publish] declared".to_string())
                } else {
                    Err(format!("no [publish.{name}] block"))
                }
            );
        }
        "queue" | "queue.forge" => {
            piece!(
                "queue-mode",
                if remote.is_some() {
                    Ok(format!("{land} tracks a remote"))
                } else {
                    Err(format!(
                        "queue.forge needs a remote landing ref, {land} is local"
                    ))
                }
            );
        }
        other => piece!(
            "queue-mode",
            Err(format!("land mode {other:?} has no queue to open"))
        ),
    }
    out
}

fn cut_worktree(repo: &Path, land: &str) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!(
        "spira-repo-check-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let wt = dir.join("wt");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let wt_s = wt.display().to_string();
    let r = git(repo, &["worktree", "add", "-q", "--detach", &wt_s, land]);
    let _ = git(repo, &["worktree", "remove", "--force", &wt_s]);
    let _ = git(repo, &["worktree", "prune"]);
    let _ = std::fs::remove_dir_all(&dir);
    r.map(|_| format!("cut from {land}"))
        .map_err(|e| format!("cannot cut a worktree from {land}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Fx {
        _d: testkit::TempDir,
        repo: PathBuf,
        map: PathBuf,
        toml: PathBuf,
    }

    fn fx() -> Fx {
        let d = testkit::TempDir::new("repo-admin");
        let map = d.path().join("repo-map");
        let toml = d.path().join("spira.toml");
        std::fs::write(&map, "# rows\n").unwrap();
        std::fs::write(&toml, "").unwrap();
        let repo = d.path().join("fixture-repo");
        Fx {
            repo,
            map,
            toml,
            _d: d,
        }
    }

    fn args(f: &Fx) -> AddArgs {
        AddArgs {
            name: "fixture".into(),
            path: f.repo.clone(),
            queue: Queue::Local,
            remote: None,
            land_ref: None,
            format: "git".into(),
            gate: String::new(),
        }
    }

    fn targets(f: &Fx) -> Targets {
        Targets {
            map: Some(f.map.clone()),
            toml: Some(f.toml.clone()),
        }
    }

    fn reg(f: &Fx) -> Registry {
        let text = std::fs::read_to_string(&f.map).unwrap();
        let publish = crate::load(&f.toml).map(|d| d.publish).unwrap_or_default();
        Registry::new(Some(&text), &BTreeMap::new(), Path::new("/nonexistent"))
            .with_publish(publish)
    }

    fn first_failure(f: &Fx) -> Option<&'static str> {
        check(&reg(f), "fixture")
            .into_iter()
            .find(|p| !p.ok)
            .map(|p| p.name)
    }

    #[test]
    fn add_from_nothing_checks_green() {
        let f = fx();
        let did = add(&args(&f), &targets(&f)).unwrap();
        assert!(did.iter().any(|d| d.contains("initialised")), "{did:?}");
        assert_eq!(first_failure(&f), None, "{:?}", check(&reg(&f), "fixture"));
        let text = std::fs::read_to_string(&f.map).unwrap();
        assert!(text.contains("fixture |"), "{text}");
        assert_eq!(
            crate::load(&f.toml).unwrap().publish["fixture"]
                .remote
                .as_deref(),
            Some("none")
        );
    }

    #[test]
    fn add_again_changes_nothing() {
        let f = fx();
        add(&args(&f), &targets(&f)).unwrap();
        let before = (
            std::fs::read_to_string(&f.map).unwrap(),
            std::fs::read_to_string(&f.toml).unwrap(),
        );
        let tip = git(&f.repo, &["rev-parse", "HEAD"]).unwrap();
        let did = add(&args(&f), &targets(&f)).unwrap();
        assert!(did.is_empty(), "{did:?}");
        assert_eq!(
            before,
            (
                std::fs::read_to_string(&f.map).unwrap(),
                std::fs::read_to_string(&f.toml).unwrap()
            )
        );
        assert_eq!(tip, git(&f.repo, &["rev-parse", "HEAD"]).unwrap());
    }

    #[test]
    fn a_conflicting_registration_is_refused() {
        let f = fx();
        add(&args(&f), &targets(&f)).unwrap();
        let mut other = args(&f);
        other.format = "git status".into();
        assert!(add(&other, &targets(&f))
            .unwrap_err()
            .contains("already registered differently"));
    }

    #[test]
    fn unregistered_is_named_first() {
        let f = fx();
        assert_eq!(first_failure(&f), Some("registered"));
    }

    #[test]
    fn a_missing_landing_ref_is_named() {
        let f = fx();
        add(&args(&f), &targets(&f)).unwrap();
        git(&f.repo, &["checkout", "-q", "--detach"]).unwrap();
        git(&f.repo, &["branch", "-D", "local/main"]).unwrap();
        assert_eq!(first_failure(&f), Some("landref"));
    }

    #[test]
    fn a_missing_publish_block_is_named() {
        let f = fx();
        add(&args(&f), &targets(&f)).unwrap();
        std::fs::write(&f.toml, "").unwrap();
        assert_eq!(first_failure(&f), Some("publish"));
    }

    #[test]
    fn an_unknown_format_command_is_named() {
        let f = fx();
        let mut a = args(&f);
        a.format = "no-such-formatter-xyz --all".into();
        add(&a, &targets(&f)).unwrap();
        assert_eq!(first_failure(&f), Some("format"));
    }

    #[test]
    fn a_missing_test_entry_is_named() {
        let f = fx();
        add(&args(&f), &targets(&f)).unwrap();
        git(&f.repo, &["rm", "-q", TESTENV_DECL]).unwrap();
        git(&f.repo, &["commit", "-q", "-m", "drop the test entry"]).unwrap();
        assert_eq!(first_failure(&f), Some("test-entry"));
    }

    #[test]
    fn a_toml_only_box_registers_through_the_repo_table() {
        let f = fx();
        let t = Targets {
            map: None,
            toml: Some(f.toml.clone()),
        };
        add(&args(&f), &t).unwrap();
        let doc = crate::load(&f.toml).unwrap();
        assert_eq!(doc.repo["fixture"].path, f.repo.display().to_string());
        assert!(add(&args(&f), &t).unwrap().is_empty());
    }
}
