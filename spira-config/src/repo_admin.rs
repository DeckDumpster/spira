//! `repo add` and `repo check`: registering a repository with the harness, and proving every
//! piece of plumbing a bead for it needs. `add` is idempotent and writes only through the
//! config writers; `check` stops at the first missing piece and names it.

use crate::repos::{self, Registry};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEPLOY_WORKFLOW: &str = ".github/workflows/deploy.yml";
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
    pub publish: Option<String>,
    pub publish_branch: Option<String>,
    pub publish_mode: Option<String>,
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

const PUBLISH_FLAGS: &str = "--publish <remote>|none [--publish-branch <b>] [--publish-mode pr|push]";

/// Refuses every publish mistake before the checkout, the repo-map or the toml is touched, and
/// returns the `[publish.<name>]` keys that differ from what is stored.
fn preflight_publish(a: &AddArgs, t: &Targets) -> Result<Vec<(String, String)>, String> {
    let given = a.publish.is_some() || a.publish_branch.is_some() || a.publish_mode.is_some();
    if a.queue == Queue::Forge {
        return if given {
            Err("repo add: --publish, --publish-branch and --publish-mode go with --queue local; queue.forge deploys from the forge".to_string())
        } else {
            Ok(Vec::new())
        };
    }
    let Some(remote) = a.publish.as_deref().filter(|r| !r.is_empty()) else {
        return Err(format!(
            "repo add: --queue local needs the publish target: {PUBLISH_FLAGS}; `--publish none` registers a repository that publishes nowhere"
        ));
    };
    if remote == repos::NO_PUBLISH && (a.publish_branch.is_some() || a.publish_mode.is_some()) {
        return Err("repo add: --publish-branch and --publish-mode need a remote, not --publish none".to_string());
    }
    if let Some(m) = a.publish_mode.as_deref() {
        if !matches!(m, "pr" | "push") {
            return Err(format!("repo add: --publish-mode is pr or push, not {m:?}"));
        }
    }
    if a.publish_branch.as_deref() == Some("") {
        return Err("repo add: --publish-branch may not be empty".to_string());
    }
    if remote != repos::NO_PUBLISH {
        let exists = a.path.join(".git").exists() && git_ok(&a.path, &["remote", "get-url", remote]);
        if !exists && !(remote == "origin" && a.remote.is_some()) {
            return Err(format!(
                "repo add: --publish {remote}: the checkout at {} has no remote {remote}",
                a.path.display()
            ));
        }
    }
    if let Some(url) = &a.remote {
        if a.path.join(".git").exists() {
            if let Ok(have) = git(&a.path, &["remote", "get-url", "origin"]) {
                if &have != url {
                    return Err(format!("repo add: origin is already {have}, not {url}"));
                }
            }
        }
    }
    let toml = t
        .toml
        .as_ref()
        .ok_or("repo add: no spira.toml located to carry [publish.<name>]")?;
    let have = crate::load(toml).ok().and_then(|d| d.publish.get(&a.name).cloned());
    let mut want: Vec<(&str, String, Option<String>)> = vec![(
        "remote",
        remote.to_string(),
        have.as_ref().and_then(|p| p.remote.clone()),
    )];
    if let Some(b) = &a.publish_branch {
        want.push(("branch", b.clone(), have.as_ref().and_then(|p| p.branch.clone())));
    }
    if let Some(m) = &a.publish_mode {
        want.push((
            "mode",
            m.clone(),
            have.as_ref().and_then(|p| p.mode).map(|m| match m {
                crate::PublishMode::Pr => "pr".to_string(),
                crate::PublishMode::Push => "push".to_string(),
            }),
        ));
    }
    let mut pairs = Vec::new();
    for (key, value, stored) in want {
        match stored {
            Some(s) if s == value => {}
            Some(s) => {
                return Err(format!(
                    "repo add: {} is already registered differently in {}: [publish.{}] {key} = {s:?}, not {value:?}",
                    a.name,
                    toml.display(),
                    a.name
                ))
            }
            None => pairs.push((format!("publish.{}.{key}", a.name), value)),
        }
    }
    Ok(pairs)
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
    let publish_pairs = preflight_publish(a, t)?;

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

    if !publish_pairs.is_empty() {
        let toml = t.toml.as_ref().expect("preflight_publish required it");
        let pairs: Vec<(&str, &str)> = publish_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        crate::set_paths_in_file(toml, &pairs)?;
        for (k, v) in &publish_pairs {
            did.push(format!("wrote {k} = {v:?}"));
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
        write_map(map, &out)?;
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

fn map_row_index(text: &str, name: &str) -> Option<(usize, repos::Row)> {
    text.lines()
        .enumerate()
        .find_map(|(i, l)| repos::parse(l).into_iter().next().filter(|r| r.name == name).map(|r| (i, r)))
}

fn rebuild(text: &str, idx: usize, replacement: Option<String>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    match replacement {
        Some(l) => lines[idx] = l,
        None => {
            lines.remove(idx);
        }
    }
    lines.join("\n") + "\n"
}

fn write_map(map: &Path, out: &str) -> Result<(), String> {
    crate::backup_existing(map).map_err(|e| format!("{}: {e}", map.display()))?;
    crate::write_atomic(map, out).map_err(|e| format!("{}: {e}", map.display()))
}

fn read_map(verb: &str, map: Option<&PathBuf>) -> Result<(PathBuf, String), String> {
    let map = map
        .filter(|m| m.is_file())
        .ok_or_else(|| format!("repo {verb}: no repo-map is in force"))?;
    let text = std::fs::read_to_string(map).map_err(|e| format!("{}: {e}", map.display()))?;
    Ok((map.clone(), text))
}

/// Changes one field of a registered repo's repo-map row and returns `(old, new)`. `queue`
/// is the land-mode column and `land` the landing ref (`base`).
pub fn set(name: &str, field: &str, value: &str, t: &Targets) -> Result<(String, String), String> {
    if let Some(key) = field.strip_prefix("publish-") {
        return set_publish(name, key, value, t);
    }
    let (map, text) = read_map("set", t.map.as_ref())?;
    let (idx, mut row) = map_row_index(&text, name)
        .ok_or_else(|| format!("repo set: {name} is not registered in {}", map.display()))?;
    if matches!(field, "format" | "gate") && (value.contains('|') || value.contains('\n')) {
        return Err(format!("repo set: {field} may not contain a pipe or newline"));
    }
    let old = match field {
        "path" => {
            if !Path::new(value).is_absolute() {
                return Err(format!("repo set: path {value} must be absolute"));
            }
            std::mem::replace(&mut row.path, value.to_string())
        }
        "queue" => {
            let q = Queue::parse(value)
                .ok_or_else(|| format!("repo set: queue must be local or forge, not {value:?}"))?;
            std::mem::replace(&mut row.land, q.mode().to_string())
        }
        "land" => std::mem::replace(&mut row.base, value.to_string()),
        "format" => std::mem::replace(&mut row.format, value.to_string()),
        "gate" => std::mem::replace(&mut row.gate, value.to_string()),
        "lanes" => {
            let word = |l: &str| {
                !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            };
            if !value.split(',').all(word) {
                return Err(format!("repo set: lanes must be a comma list of lane names, not {value:?}"));
            }
            std::mem::replace(&mut row.lanes, value.to_string())
        }
        other => {
            return Err(format!(
                "repo set: unknown field {other:?} (path, queue, land, format, gate, lanes)"
            ))
        }
    };
    if matches!(field, "queue" | "land")
        && row.land == Queue::Forge.mode()
        && repos::ref_remote(&row.base, None).is_none()
    {
        return Err(format!(
            "repo set: queue.forge lands on a remote ref such as origin/main, not {}",
            row.base
        ));
    }
    let line = format!(
        "{} | {} | {} | {} | {} | {} | {}",
        row.name, row.path, row.land, row.base, row.format, row.gate, row.lanes
    );
    write_map(&map, &rebuild(&text, idx, Some(line)))?;
    Ok((old, value.to_string()))
}

fn registered_anywhere(name: &str, t: &Targets) -> bool {
    let in_map = t
        .map
        .as_ref()
        .and_then(|m| std::fs::read_to_string(m).ok())
        .is_some_and(|text| map_row_index(&text, name).is_some());
    in_map
        || t.toml
            .as_ref()
            .and_then(|f| crate::load(f).ok())
            .is_some_and(|d| d.repo.contains_key(name))
}

fn stored_publish(name: &str, t: &Targets) -> crate::PublishSection {
    t.toml
        .as_ref()
        .and_then(|f| crate::load(f).ok())
        .and_then(|d| d.publish.get(name).cloned())
        .unwrap_or_default()
}

fn mode_word(m: crate::PublishMode) -> &'static str {
    match m {
        crate::PublishMode::Pr => "pr",
        crate::PublishMode::Push => "push",
    }
}

/// `repo set <name> publish-remote|publish-branch|publish-mode`: `[publish.<name>]`, returning
/// `(old, new)` with an unset key as the empty string.
fn set_publish(name: &str, key: &str, value: &str, t: &Targets) -> Result<(String, String), String> {
    if !matches!(key, "remote" | "branch" | "mode") {
        return Err(format!(
            "repo set: unknown field \"publish-{key}\" (publish-remote, publish-branch, publish-mode)"
        ));
    }
    if !registered_anywhere(name, t) {
        return Err(format!("repo set: {name} is not registered"));
    }
    let toml = t
        .toml
        .as_ref()
        .ok_or("repo set: no spira.toml located to carry [publish.<name>]")?;
    if key == "remote" && value.is_empty() {
        return Err(format!(
            "repo set: publish-remote may not be empty; use {:?} for a repository that publishes nowhere",
            repos::NO_PUBLISH
        ));
    }
    if key == "mode" && !value.is_empty() && !matches!(value, "pr" | "push") {
        return Err(format!("repo set: publish-mode is pr or push, not {value:?}"));
    }
    let have = stored_publish(name, t);
    let old = match key {
        "remote" => have.remote,
        "branch" => have.branch,
        _ => have.mode.map(|m| mode_word(m).to_string()),
    }
    .unwrap_or_default();
    let path = format!("publish.{name}.{key}");
    if value.is_empty() {
        crate::unset_paths_in_file(toml, &[&path])?;
    } else {
        crate::set_paths_in_file(toml, &[(&path, value)])?;
    }
    Ok((old, value.to_string()))
}

/// The stored publish target of a registered repository as a JSON object; an unset key is `null`.
pub fn publish_target(name: &str, t: &Targets) -> Result<serde_json::Value, String> {
    if !registered_anywhere(name, t) {
        return Err(format!("repo publish: {name} is not registered"));
    }
    let p = stored_publish(name, t);
    Ok(serde_json::json!({
        "repo": name,
        "remote": p.remote,
        "branch": p.branch,
        "mode": p.mode.map(mode_word),
    }))
}

/// Removes a repository's repo-map row and its `[publish.<name>]` table, returning the row it
/// removed. The toml is written first and restored if the repo-map write then fails.
pub fn rm(name: &str, t: &Targets) -> Result<String, String> {
    let (map, text) = read_map("rm", t.map.as_ref())?;
    let (idx, _) = map_row_index(&text, name)
        .ok_or_else(|| format!("repo rm: {name} is not registered in {}", map.display()))?;
    let old = text.lines().nth(idx).unwrap_or_default().to_string();
    let saved = match &t.toml {
        Some(f) => {
            let before = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
            crate::remove_named_table_in_file(f, "publish", name)?.then_some((f, before))
        }
        None => None,
    };
    if let Err(e) = write_map(&map, &rebuild(&text, idx, None)) {
        if let Some((f, before)) = saved {
            crate::write_atomic(f, &before).map_err(|re| format!("{e}; restoring {}: {re}", f.display()))?;
        }
        return Err(e);
    }
    Ok(old)
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

fn publish_piece(reg: &Registry, name: &str, path: &Path, land: &str) -> Result<String, String> {
    if !reg.publish_declared(name) {
        return Err(format!("no [publish.{name}] block"));
    }
    if !repos::publishes_nowhere(reg, name, &BTreeMap::new()) {
        let p = reg.publish.get(name).cloned().unwrap_or_default();
        let mode = p.mode.map(mode_word).unwrap_or("pr");
        return Ok(format!(
            "[publish.{name}] remote = {:?}, branch = {:?}, mode = {mode}",
            p.remote.unwrap_or_default(),
            p.branch.unwrap_or_default()
        ));
    }
    if in_tree(path, land, DEPLOY_WORKFLOW) {
        return Err(format!(
            "deploys from the forge ({DEPLOY_WORKFLOW} on {land}) but publishes nowhere ([publish.{name}] remote = \"none\") — set it with spira-config repo set {name} publish-remote <remote>"
        ));
    }
    Ok(format!("[publish.{name}] remote = \"none\": publishes nowhere"))
}

fn show(repo: &Path, land_ref: &str, rel: &str) -> Option<String> {
    git(repo, &["show", &format!("{land_ref}:{rel}")]).ok()
}

fn gate_detail(reg: &Registry, name: &str, path: &Path, land: &str) -> Result<String, String> {
    use crate::gate_source::{resolve, GateSource};
    let declaration = show(path, land, TESTENV_DECL);
    let mode = reg.land(name);
    let column = reg.gate(name).unwrap_or_default();
    let source = resolve(
        in_tree(path, land, "gate.steps"),
        declaration.as_deref(),
        in_tree(path, land, HARNESS_IMAGE),
        &mode,
        &column,
    )
    .map_err(|e| format!("{TESTENV_DECL} on {land} does not parse: {e}"))?;
    match source {
        GateSource::Steps => Ok(format!("gate.steps on {land}")),
        GateSource::Testenv { tiers, build_tier } => Ok(format!(
            "testenv default tiers: {}{}",
            tiers.join(", "),
            if build_tier { ", build tier" } else { "" }
        )),
        GateSource::Column(c) if command_exists(&c) => Ok(c),
        GateSource::Column(c) => Err(format!("command not found: {c}")),
        GateSource::Nothing { .. } if matches!(mode.as_str(), "queue" | "queue.forge") => {
            Ok("certifies nothing; the round's pull request CI judges".to_string())
        }
        GateSource::Nothing { .. } if mode == "pr" => {
            Ok("certifies nothing; the pull request's CI judges".to_string())
        }
        GateSource::Nothing { .. } => Err(format!(
            "certifies nothing: {land} has no gate.steps and no ci_entry in {TESTENV_DECL}"
        )),
    }
}

/// Every piece of plumbing, in order, stopping after the first that is missing.
pub fn check(reg: &Registry, name: &str) -> Vec<Piece> {
    check_cutting_in(reg, name, &worktree_root)
}

fn check_cutting_in(reg: &Registry, name: &str, tree_root: &dyn Fn() -> Result<PathBuf, String>) -> Vec<Piece> {
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
    piece!("worktree", tree_root().and_then(|r| cut_worktree_in(&r, &path, &land)).map(|_| format!("cut from {land}")));
    {
        let v = reg.format(name).unwrap_or_default();
        piece!(
            "format",
            if v.is_empty() {
                Ok("none declared".to_string())
            } else if command_exists(&v) {
                Ok(v)
            } else {
                Err(format!("command not found: {v}"))
            }
        );
    }
    piece!("gate", gate_detail(reg, name, &path, &land));
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
            piece!("publish", publish_piece(reg, name, &path, &land));
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
        "pr" => piece!(
            "queue-mode",
            if remote.is_some() {
                Ok(format!("{land} tracks a remote"))
            } else {
                Err(format!("pr needs a remote landing ref, {land} is local"))
            }
        ),
        other => piece!(
            "queue-mode",
            Err(format!("land mode {other:?} has no queue to open"))
        ),
    }
    out
}

/// A detached worktree of a repository, removed when dropped.
pub struct Worktree {
    repo: PathBuf,
    dir: PathBuf,
}

impl Worktree {
    pub fn path(&self) -> PathBuf {
        self.dir.clone()
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let dir = self.dir.display().to_string();
        let _ = git(&self.repo, &["worktree", "unlock", &dir]);
        let _ = git(&self.repo, &["worktree", "remove", "--force", &dir]);
        let _ = git(&self.repo, &["worktree", "prune"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const CHECK_PREFIX: &str = "repo-check-";

/// Where repo-check trees are cut: on the run disk, inside `hel reap-terminal`'s reach.
pub fn worktree_root() -> Result<PathBuf, String> {
    Ok(worktree_root_under(&crate::resolve::run_dir_for_process()?))
}

fn worktree_root_under(run: &Path) -> PathBuf {
    run.join("worktree")
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes for the pid; nothing is delivered.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    let probe = rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    probe && !is_dead_or_zombie(pid)
}

fn is_dead_or_zombie(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("{}/{pid}/stat", system_paths::PROC)) else { return false };
    stat_state_is_gone(&stat)
}

fn stat_state_is_gone(stat: &str) -> bool {
    let state = stat.rsplit_once(')').and_then(|(_, r)| r.split_whitespace().next());
    matches!(state, Some("Z" | "X"))
}

/// A killed check leaves its registration locked, and `git worktree prune` skips a locked one.
fn unlock_registration(tree: &Path) {
    let Ok(pointer) = std::fs::read_to_string(tree.join(".git")) else { return };
    if let Some(admin) = pointer.trim().strip_prefix("gitdir:") {
        let _ = std::fs::remove_file(Path::new(admin.trim()).join("locked"));
    }
}

/// Remove every `repo-check-<pid>-*` entry under `root` whose pid is gone; a name whose pid
/// does not parse is kept. Returns the number removed.
pub fn reap_dead_checks(root: &Path, pid_alive: &dyn Fn(u32) -> bool) -> usize {
    reap_dead_checks_reporting(root, pid_alive, &mut |line| eprintln!("{line}"))
}

fn reap_dead_checks_reporting(root: &Path, pid_alive: &dyn Fn(u32) -> bool, report: &mut dyn FnMut(String)) -> usize {
    let Ok(rd) = std::fs::read_dir(root) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(CHECK_PREFIX) else { continue };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        let path = e.path().display().to_string();
        if pid_alive(pid) {
            report(format!("repo-check: kept {path} (pid {pid} still running)"));
            continue;
        }
        unlock_registration(&e.path());
        match std::fs::remove_dir_all(e.path()) {
            Ok(()) => {
                report(format!("repo-check: reaped {path} (pid {pid} gone)"));
                n += 1;
            }
            Err(err) => report(format!("repo-check: cannot reap {path}: {err}")),
        }
    }
    n
}

pub fn cut_worktree(repo: &Path, land: &str) -> Result<Worktree, String> {
    cut_worktree_in(&worktree_root()?, repo, land)
}

pub fn cut_worktree_in(root: &Path, repo: &Path, land: &str) -> Result<Worktree, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    reap_dead_checks(root, &pid_alive);
    let _ = git(repo, &["worktree", "prune"]);
    let dir = root.join(format!(
        "{CHECK_PREFIX}{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let guard = Worktree {
        repo: repo.to_path_buf(),
        dir,
    };
    git(
        repo,
        &["worktree", "add", "-q", "--detach", &guard.dir.display().to_string(), land],
    )
    .map_err(|e| format!("cannot cut a worktree from {land}: {e}"))?;
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        _d: testkit::TempDir,
        map: PathBuf,
        toml: PathBuf,
    }

    fn fx() -> Fx {
        let d = testkit::TempDir::new("repo-admin");
        let map = d.path().join("repo-map");
        let toml = d.path().join("spira.toml");
        std::fs::write(&map, "# rows\n").unwrap();
        std::fs::write(&toml, "").unwrap();
        Fx {
            map,
            toml,
            _d: d,
        }
    }

    fn reg(f: &Fx) -> Registry {
        let text = std::fs::read_to_string(&f.map).unwrap();
        let publish = crate::load(&f.toml).map(|d| d.publish).unwrap_or_default();
        Registry::new(Some(&text), &BTreeMap::new(), Path::new("nonexistent"))
            .with_publish(publish)
    }

    fn first_failure(f: &Fx) -> Option<&'static str> {
        check_cutting_in(&reg(f), "fixture", &|| Ok(f._d.path().join("wt")))
            .into_iter()
            .find(|p| !p.ok)
            .map(|p| p.name)
    }

    #[test_id(txwf3lf)]
    #[test]
    fn unregistered_is_named_first() {
        let f = fx();
        assert_eq!(first_failure(&f), Some("registered"));
    }

    fn registered(f: &Fx, gate: &str) -> Targets {
        std::fs::write(
            &f.map,
            format!("# rows\nfixture | srv/fx | queue.local | local/main | fmt | {gate} | plan\n"),
        )
        .unwrap();
        Targets {
            map: Some(f.map.clone()),
            toml: None,
        }
    }

    #[test_id(tc1utlg)]
    #[test]
    fn set_gate_to_empty_reads_back_empty_and_reports_old_and_new() {
        let f = fx();
        let t = registered(&f, "bash gate.sh");
        let (old, new) = set("fixture", "gate", "", &t).unwrap();
        assert_eq!((old.as_str(), new.as_str()), ("bash gate.sh", ""));
        let r = reg(&f);
        assert_eq!(r.gate("fixture").as_deref(), Some(""));
        assert_eq!(r.format("fixture").as_deref(), Some("fmt"));
        assert_eq!(r.root("fixture").as_deref(), Some("srv/fx"));
    }

    #[test_id(t7d1mba)]
    #[test]
    fn set_refuses_an_unknown_field_and_leaves_the_map_alone() {
        let f = fx();
        let t = registered(&f, "g");
        let before = std::fs::read_to_string(&f.map).unwrap();
        let e = set("fixture", "colour", "red", &t).unwrap_err();
        assert!(e.contains("unknown field"), "{e}");
        assert_eq!(std::fs::read_to_string(&f.map).unwrap(), before);
    }

    #[test_id(tvm5znw)]
    #[test]
    fn set_refuses_an_unregistered_name_and_bad_values() {
        let f = fx();
        let t = registered(&f, "g");
        assert!(set("nope", "gate", "", &t).unwrap_err().contains("not registered"));
        assert!(set("fixture", "path", "rel", &t).is_err());
        assert!(set("fixture", "gate", "a|b", &t).is_err());
        assert!(set("fixture", "queue", "sideways", &t).is_err());
        assert!(set("fixture", "lanes", "plan,b@d", &t).is_err());
        set("fixture", "land", "main", &t).unwrap();
        assert!(set("fixture", "queue", "forge", &t).is_err());
    }

    fn gated_fx(land: &str, column: &str, files: &[(&str, &str)]) -> (Fx, PathBuf) {
        let f = fx();
        let repo = f._d.path().join("gated");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "local/main"]).unwrap();
        for (rel, body) in files {
            let p = repo.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        git(&repo, &["add", "-A"]).unwrap();
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "c"]).unwrap();
        std::fs::write(
            &f.map,
            format!("# rows\nfixture | {} | {land} | local/main | | {column} | plan\n", repo.display()),
        )
        .unwrap();
        (f, repo)
    }

    fn piece(f: &Fx, name: &str) -> Piece {
        { let all = check_cutting_in(&reg(f), "fixture", &|| Ok(f._d.path().join("wt"))); all.iter().find(|p| p.name == name).cloned().unwrap_or_else(|| panic!("{all:?}")) }
    }

    const DECL_OK: &str = "ci_entry = \"bash gate.sh\"\ndefault = [\"gate\"]\n[tier.gate]\nargv = [\"true\"]\n";
    const DECL_NO_ENTRY: &str = "default = [\"gate\"]\n[tier.gate]\nargv = [\"true\"]\n";

    #[test_id(tpg1a01)]
    #[test]
    fn a_queue_local_declaration_with_a_ci_entry_names_its_default_tiers_as_the_gate() {
        let (f, _) = gated_fx("queue.local", "", &[(".spira/testenv.toml", DECL_OK)]);
        let g = piece(&f, "gate");
        assert!(g.ok, "{g:?}");
        assert_eq!(g.detail, "testenv default tiers: gate");
    }

    #[test_id(tpg1a02)]
    #[test]
    fn a_queue_local_repository_whose_gate_runs_nothing_fails_at_gate() {
        let (f, _) = gated_fx("queue.local", "", &[(".spira/testenv.toml", DECL_NO_ENTRY)]);
        assert_eq!(first_failure(&f), Some("gate"));
        let (f, _) = gated_fx("queue.local", "", &[("README", "x")]);
        assert_eq!(first_failure(&f), Some("gate"));
    }

    #[test_id(tpg1a03)]
    #[test]
    fn a_queue_forge_repository_certifies_nothing_and_says_the_pull_request_judges() {
        let (f, _) = gated_fx("queue.forge", "", &[(".spira/testenv.toml", DECL_OK)]);
        let g = piece(&f, "gate");
        assert!(g.ok && g.detail.contains("certifies nothing"), "{g:?}");
    }

    #[test_id(t1azeev)]
    #[test]
    fn a_pr_repository_certifies_nothing_and_says_the_pull_request_judges_but_push_still_fails() {
        let (f, _) = gated_fx("pr", "", &[(".spira/testenv.toml", DECL_NO_ENTRY)]);
        let g = piece(&f, "gate");
        assert!(g.ok, "{g:?}");
        assert!(g.detail.contains("certifies nothing"), "{g:?}");
        assert!(g.detail.contains("pull request"), "{g:?}");
        let (f, _) = gated_fx("push", "", &[("README", "x")]);
        assert_eq!(first_failure(&f), Some("gate"));
    }

    #[test_id(tgvwkab)]
    #[test]
    fn a_pr_repository_passes_every_piece_only_with_a_remote_landing_ref() {
        let (f, repo) = gated_fx("pr", "", &[(".spira/testenv.toml", DECL_NO_ENTRY)]);
        assert_eq!(first_failure(&f), Some("queue-mode"));
        git(&repo, &["remote", "add", "origin", "unused"]).unwrap();
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]).unwrap();
        let map = std::fs::read_to_string(&f.map).unwrap().replace("| local/main |", "| origin/main |");
        std::fs::write(&f.map, map).unwrap();
        assert_eq!(piece(&f, "landref").detail, "origin/main");
        assert_eq!(first_failure(&f), None);
    }

    #[test_id(tpg1a04)]
    #[test]
    fn gate_steps_on_the_landing_ref_is_named_as_the_gate() {
        let (f, _) = gated_fx("queue.local", "", &[("gate.steps", "step bash x.sh\n")]);
        let g = piece(&f, "gate");
        assert!(g.ok && g.detail.contains("gate.steps"), "{g:?}");
    }

    #[test_id(tpg1a05)]
    #[test]
    fn a_column_naming_a_missing_command_still_fails_the_gate() {
        let (f, _) = gated_fx("queue.local", "no-such-cmd-xyz run", &[("README", "x")]);
        let g = piece(&f, "gate");
        assert!(!g.ok && g.detail.contains("command not found"), "{g:?}");
    }

    #[test_id(tpg1a06)]
    #[test]
    fn a_declaration_that_does_not_parse_fails_the_gate_with_the_parse_error() {
        let (f, _) = gated_fx("queue.local", "", &[(".spira/testenv.toml", "ci_entry = [")]);
        let g = piece(&f, "gate");
        assert!(!g.ok && g.detail.contains("does not parse: "), "{g:?}");
    }

    #[test_id(tpg1a07)]
    #[test]
    fn the_testenv_gate_names_a_build_tier_and_a_plain_queue_certifies_nothing() {
        let with_build = format!("{DECL_OK}[tier.build]\nargv = [\"true\"]\n");
        let (f, _) = gated_fx("queue.local", "", &[(".spira/testenv.toml", &with_build)]);
        assert_eq!(piece(&f, "gate").detail, "testenv default tiers: gate, build tier");
        let (f, _) = gated_fx("queue", "", &[("README", "x")]);
        let g = piece(&f, "gate");
        assert!(g.ok && g.detail.contains("certifies nothing"), "{g:?}");
    }

    #[test_id(tpg1a08)]
    #[test]
    fn a_harness_containerfile_stops_the_declaration_gating_and_a_present_column_passes() {
        let (f, _) = gated_fx("queue.local", "", &[(".spira/testenv.toml", DECL_OK), (HARNESS_IMAGE, "FROM x\n")]);
        assert_eq!(first_failure(&f), Some("gate"));
        let (f, _) = gated_fx("queue.local", "git status", &[("README", "x")]);
        let g = piece(&f, "gate");
        assert!(g.ok && g.detail == "git status", "{g:?}");
    }

    #[test_id(tzam1ri)]
    #[test]
    fn rm_drops_only_the_named_row() {
        let f = fx();
        let t = registered(&f, "g");
        let mut text = std::fs::read_to_string(&f.map).unwrap();
        text.push_str("other | /srv/o | queue.local | local/main | | | plan\n");
        std::fs::write(&f.map, text).unwrap();
        rm("fixture", &t).unwrap();
        assert_eq!(reg(&f).names(), vec!["other".to_string()]);
        assert!(rm("fixture", &t).is_err());
    }

    #[test_id(tofcl2v)]
    #[test]
    fn reap_dead_checks_removes_only_the_dead_pids_tree() {
        let d = testkit::TempDir::new("repo-check-reap");
        for n in ["repo-check-111-1", "repo-check-222-2", "repo-check-x-3", "other"] {
            std::fs::create_dir(d.path().join(n)).unwrap();
        }
        let n = reap_dead_checks(d.path(), &|p| p == 222);
        assert_eq!(n, 1);
        assert!(!d.path().join("repo-check-111-1").exists());
        for kept in ["repo-check-222-2", "repo-check-x-3", "other"] {
            assert!(d.path().join(kept).exists(), "{kept}");
        }
    }

    fn repo_with_commit(d: &Path) -> PathBuf {
        let repo = d.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]).unwrap();
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "c"]).unwrap();
        repo
    }

    #[test_id(the9gp9)]
    #[test]
    fn a_locked_check_worktree_is_removed_when_its_guard_drops() {
        let d = testkit::TempDir::new("wt-locked");
        let repo = repo_with_commit(d.path());
        let root = d.path().join("wt");
        let wt = cut_worktree_in(&root, &repo, "main").unwrap();
        git(&repo, &["worktree", "lock", &wt.path().display().to_string()]).unwrap();
        drop(wt);
        let listed = git(&repo, &["worktree", "list"]).unwrap();
        assert!(!listed.contains("repo-check-"), "{listed}");
    }

    #[test_id(tpoeamc)]
    #[test]
    fn a_dead_pids_locked_check_registration_is_pruned_by_the_next_cut() {
        let d = testkit::TempDir::new("wt-dead-locked");
        let repo = repo_with_commit(d.path());
        let root = d.path().join("wt");
        let wt = cut_worktree_in(&root, &repo, "main").unwrap();
        let tree = wt.path();
        git(&repo, &["worktree", "lock", &tree.display().to_string()]).unwrap();
        std::mem::forget(wt);
        let dead = tree.file_name().unwrap().to_string_lossy().replace(&std::process::id().to_string(), "1");
        std::fs::rename(&tree, root.join(&dead)).unwrap();
        git(&repo, &["worktree", "repair", &root.join(&dead).display().to_string()]).unwrap();
        assert_eq!(reap_dead_checks(&root, &|_| false), 1);
        git(&repo, &["worktree", "prune"]).unwrap();
        let listed = git(&repo, &["worktree", "list"]).unwrap();
        assert!(!listed.contains("repo-check-"), "{listed}");
    }

    #[test_id(togag7w)]
    #[test]
    fn a_failed_cut_leaves_no_check_worktree_listed() {
        let d = testkit::TempDir::new("wt-failed");
        let repo = repo_with_commit(d.path());
        let root = d.path().join("wt");
        assert!(cut_worktree_in(&root, &repo, "no-such-ref").is_err());
        let listed = git(&repo, &["worktree", "list"]).unwrap();
        assert!(!listed.contains("repo-check-"), "{listed}");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test_id(ttf72r7)]
    #[test]
    fn worktree_root_is_under_the_run_dir_not_temp() {
        let d = testkit::TempDir::new("repo-check-root");
        let root = worktree_root_under(d.path());
        assert_eq!(root, d.path().join("worktree"));
        assert!(root.starts_with(d.path()));
    }

    #[test_id(tm4kdy4)]
    #[test]
    fn a_zombie_pid_is_not_alive() {
        // SAFETY: the child calls only async-signal-safe `_exit`.
        let pid = unsafe {
            match libc::fork() {
                0 => libc::_exit(0),
                p => p,
            }
        };
        assert!(pid > 0);
        let stat = format!("{}/{pid}/stat", system_paths::PROC);
        let mut polls = 0u64;
        while !std::fs::read_to_string(&stat).is_ok_and(|s| stat_state_is_gone(&s)) {
            polls += 1;
            assert!(polls < 50_000_000, "child never became a zombie");
            std::thread::yield_now();
        }
        let alive = pid_alive(pid as u32);
        // SAFETY: collects the child forked above.
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
        assert!(!alive);
    }

    #[test_id(tjdtfrf)]
    #[test]
    fn a_kept_check_tree_is_reported_with_its_reason() {
        let d = testkit::TempDir::new("repo-check-kept");
        std::fs::create_dir(d.path().join("repo-check-222-2")).unwrap();
        std::fs::write(d.path().join("repo-check-1-1"), "").unwrap();
        let mut lines = Vec::new();
        let n = reap_dead_checks_reporting(d.path(), &|p| p == 222, &mut |l| lines.push(l));
        assert_eq!(n, 0);
        lines.sort();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("repo-check: cannot reap ") && lines[0].contains("repo-check-1-1: "), "{lines:?}");
        assert!(lines[1].starts_with("repo-check: kept ") && lines[1].ends_with("repo-check-222-2 (pid 222 still running)"), "{lines:?}");
    }

    #[test_id(tjfqc12)]
    #[test]
    fn a_stat_line_is_gone_only_in_state_z_or_x() {
        assert!(stat_state_is_gone("5 (a) b) Z 1 2"));
        assert!(stat_state_is_gone("5 (sleep) X 1 2"));
        assert!(!stat_state_is_gone("5 (x Z) S 1 2"));
        assert!(!stat_state_is_gone("garbage"));
    }

    fn local_checkout(d: &Path, with_origin: bool) -> PathBuf {
        let repo = repo_with_commit(d);
        git(&repo, &["branch", "local/main"]).unwrap();
        if with_origin {
            git(&repo, &["remote", "add", "origin", "https://example.invalid/x.git"]).unwrap();
        }
        repo
    }

    fn add_args(repo: &Path, publish: Option<&str>) -> AddArgs {
        AddArgs {
            name: "fixture".into(),
            path: repo.to_path_buf(),
            queue: Queue::Local,
            remote: None,
            land_ref: None,
            format: String::new(),
            gate: String::new(),
            publish: publish.map(str::to_string),
            publish_branch: None,
            publish_mode: None,
        }
    }

    fn both(f: &Fx) -> Targets {
        Targets { map: Some(f.map.clone()), toml: Some(f.toml.clone()) }
    }

    fn snapshot(f: &Fx, repo: &Path) -> (String, String, String) {
        (
            std::fs::read_to_string(&f.map).unwrap(),
            std::fs::read_to_string(&f.toml).unwrap(),
            git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"]).unwrap()
                + &git(repo, &["status", "--porcelain", "-uall"]).unwrap(),
        )
    }

    fn stored(f: &Fx) -> crate::PublishSection {
        crate::load(&f.toml).unwrap().publish.get("fixture").cloned().unwrap_or_default()
    }

    #[test_id(tl3p0aa)]
    #[test]
    fn add_for_a_local_checkout_with_origin_refuses_without_a_publish_flag_and_touches_nothing() {
        let f = fx();
        let repo = local_checkout(f._d.path(), true);
        let before = snapshot(&f, &repo);
        let e = add(&add_args(&repo, None), &both(&f)).unwrap_err();
        assert!(e.contains("--publish") && e.contains("none"), "{e}");
        assert_eq!(snapshot(&f, &repo), before);
    }

    #[test_id(tl3p0ab)]
    #[test]
    fn add_stores_the_three_publish_keys_and_a_rerun_changes_nothing() {
        let f = fx();
        let repo = local_checkout(f._d.path(), true);
        let mut a = add_args(&repo, Some("origin"));
        a.publish_branch = Some("main".into());
        a.publish_mode = Some("push".into());
        add(&a, &both(&f)).unwrap();
        let p = stored(&f);
        assert_eq!(p.remote.as_deref(), Some("origin"));
        assert_eq!(p.branch.as_deref(), Some("main"));
        assert_eq!(p.mode, Some(crate::PublishMode::Push));
        let before = snapshot(&f, &repo);
        assert_eq!(add(&a, &both(&f)).unwrap(), Vec::<String>::new());
        assert_eq!(snapshot(&f, &repo), before);
        git(&repo, &["remote", "add", "other", "https://example.invalid/o.git"]).unwrap();
        let before = snapshot(&f, &repo);
        let mut b = a.clone();
        b.publish = Some("other".into());
        let e = add(&b, &both(&f)).unwrap_err();
        assert!(e.contains("already registered differently"), "{e}");
        assert_eq!(snapshot(&f, &repo), before);
    }

    #[test_id(tl3p0ac)]
    #[test]
    fn a_second_add_without_remote_leaves_a_hand_set_publish_remote_alone() {
        let f = fx();
        let repo = local_checkout(f._d.path(), true);
        add(&add_args(&repo, Some("origin")), &both(&f)).unwrap();
        let e = add(&add_args(&repo, Some("none")), &both(&f)).unwrap_err();
        assert!(e.contains("already registered differently"), "{e}");
        assert_eq!(stored(&f).remote.as_deref(), Some("origin"));
    }

    #[test_id(tl3p0ad)]
    #[test]
    fn add_accepts_publish_none_and_refuses_a_missing_remote_and_forge_publish() {
        let f = fx();
        let repo = local_checkout(f._d.path(), false);
        add(&add_args(&repo, Some("none")), &both(&f)).unwrap();
        assert_eq!(stored(&f).remote.as_deref(), Some("none"));
        let g = fx();
        let repo = local_checkout(g._d.path(), false);
        let e = add(&add_args(&repo, Some("nosuch")), &both(&g)).unwrap_err();
        assert!(e.contains("nosuch"), "{e}");
        let mut a = add_args(&repo, Some("origin"));
        a.queue = Queue::Forge;
        a.land_ref = Some("origin/main".into());
        assert!(add(&a, &both(&g)).is_err());
        let mut n = add_args(&repo, Some("none"));
        n.publish_mode = Some("push".into());
        assert!(add(&n, &both(&g)).is_err());
    }

    fn deploy_repo(d: &Path, with_workflow: bool) -> PathBuf {
        let repo = local_checkout(d, false);
        if with_workflow {
            std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
            std::fs::write(repo.join(DEPLOY_WORKFLOW), "on: push\n").unwrap();
            git(&repo, &["add", "."]).unwrap();
            git(&repo, &["commit", "-q", "-m", "deploy"]).unwrap();
            git(&repo, &["branch", "-f", "local/main", "HEAD"]).unwrap();
        }
        repo
    }

    fn publish_detail(f: &Fx, repo: &Path, remote: &str) -> Result<String, String> {
        crate::set_paths_in_file(&f.toml, &[("publish.fixture.remote", remote)]).unwrap();
        std::fs::write(
            &f.map,
            format!("fixture | {} | queue.local | local/main | | | plan\n", repo.display()),
        )
        .unwrap();
        publish_piece(&reg(f), "fixture", repo, "local/main")
    }

    #[test_id(tl3p0ae)]
    #[test]
    fn check_fails_publish_when_the_landing_ref_deploys_from_the_forge_and_remote_is_none() {
        let f = fx();
        let repo = deploy_repo(f._d.path(), true);
        let e = publish_detail(&f, &repo, "none").unwrap_err();
        assert!(
            e.starts_with("deploys from the forge (.github/workflows/deploy.yml on local/main) but publishes nowhere ([publish.fixture] remote = \"none\")")
                && e.ends_with("spira-config repo set fixture publish-remote <remote>"),
            "{e}"
        );
        assert!(publish_detail(&f, &repo, "origin").is_ok());
    }

    #[test_id(tl3p0af)]
    #[test]
    fn check_passes_publish_none_without_a_deploy_workflow() {
        let f = fx();
        let repo = deploy_repo(f._d.path(), false);
        assert!(publish_detail(&f, &repo, "none").is_ok());
    }

    #[test_id(tl3p0ag)]
    #[test]
    fn set_publish_fields_report_old_and_new_clear_and_refuse_bad_values() {
        let f = fx();
        let t = both(&f);
        let t = { registered(&f, "g"); t };
        assert_eq!(set("fixture", "publish-remote", "origin", &t).unwrap(), ("".into(), "origin".into()));
        assert_eq!(set("fixture", "publish-mode", "push", &t).unwrap().1, "push");
        assert_eq!(set("fixture", "publish-branch", "main", &t).unwrap().1, "main");
        assert_eq!(set("fixture", "publish-mode", "", &t).unwrap().0, "push");
        assert_eq!(stored(&f).mode, None);
        assert!(set("fixture", "publish-remote", "", &t).unwrap_err().contains("none"));
        assert!(set("fixture", "publish-mode", "merge", &t).is_err());
        assert!(set("nope", "publish-remote", "origin", &t).is_err());
    }

    #[test_id(tl3p0ah)]
    #[test]
    fn rm_removes_the_publish_table_and_leaves_other_repositories_tables() {
        let f = fx();
        let t = both(&f);
        registered(&f, "g");
        crate::set_paths_in_file(
            &f.toml,
            &[("publish.fixture.remote", "origin"), ("publish.other.remote", "none")],
        )
        .unwrap();
        rm("fixture", &t).unwrap();
        let d = crate::load(&f.toml).unwrap();
        assert!(!d.publish.contains_key("fixture"));
        assert_eq!(d.publish["other"].remote.as_deref(), Some("none"));
    }
}
