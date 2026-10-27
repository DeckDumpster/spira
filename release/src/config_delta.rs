//! The config keys a release adds or drops (DESIGN.md "config delta"): declared in the
//! release's own tree, applied to the one source by `activate`, checked by the release's own
//! `spira-config` before anything is touched.
//!
//! Edits are made on the TOML value, never through the typed schema this binary links: the
//! schema that knows a new key is the new release's, and the one that knows a dropped key is
//! the old release's, so neither can be trusted to round-trip the other's file.

use crate::config::Config;
use crate::fsutil;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use toml::map::Map;
use toml::Value;

pub const DELTA_PATH: &str = "spira/config-delta.toml";
const UNDO_DIR: &str = "config-undo";
const RETIRED_FILE: &str = "config-retired";
const REGISTRY_DIRS: [&str; 2] = ["spira/conf.d", "spira/conf.toml.d"];

type Table = Map<String, Value>;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Delta {
    pub added: BTreeMap<String, Value>,
    pub removed: Vec<String>,
    /// `persona.<old>` → `persona.<new>`: the persona's table, roster entry and lane entry move.
    /// `spira.<old>` → `spira.<new>`: the operator's value is carried to the new key and the old one dropped.
    pub renamed: BTreeMap<String, String>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.renamed.is_empty()
    }

    pub fn parse(text: &str) -> Result<Delta, String> {
        let Value::Table(top) = text.parse::<Value>().map_err(|e| e.to_string())? else {
            return Err("not a TOML table".into());
        };
        let mut d = Delta::default();
        for (k, v) in top {
            match (k.as_str(), v) {
                ("added", Value::Table(t)) => d.added = t.into_iter().collect(),
                ("removed", Value::Array(a)) => {
                    for item in a {
                        let Value::String(s) = item else { return Err("removed must list key paths as strings".into()) };
                        d.removed.push(s);
                    }
                }
                ("renamed", Value::Table(t)) => {
                    for (from, to) in t {
                        let Value::String(to) = to else { return Err("renamed must map key paths to key paths as strings".into()) };
                        match (is_spira_key(&from), is_spira_key(&to)) {
                            (true, true) => {}
                            (false, false) => {
                                persona_name(&from)?;
                                persona_name(&to)?;
                            }
                            _ => return Err(format!("{from:?} = {to:?}: a rename is persona.<old> to persona.<new>, or spira.<old> to spira.<new>")),
                        }
                        d.renamed.insert(from, to);
                    }
                }
                (other, _) => return Err(format!("unknown or mistyped entry {other:?}: only `added` (a table of key = value), `removed` (a list of keys) and `renamed` (persona.<old> = \"persona.<new>\", spira.<old> = \"spira.<new>\") are allowed")),
            }
        }
        spira_config::placeholder_paths(d.added.iter().map(|(k, v)| (k.as_str(), v)))?;
        for k in d.added.keys().chain(d.removed.iter()) {
            if k.split('.').any(str::is_empty) {
                return Err(format!("{k:?} is not a dotted key path"));
            }
        }
        if let Some(k) = d.removed.iter().find(|k| d.added.contains_key(*k)) {
            return Err(format!("{k} is both added and removed"));
        }
        Ok(d)
    }

    /// The delta with each `spira.*` rename resolved against `layers`: a value the operator set
    /// under the old key becomes the new key's added value and the old key is dropped. A rename
    /// whose old key is absent changes nothing, so the new key keeps its declared default.
    pub fn expanded(&self, layers: &[Table]) -> Delta {
        let mut d = self.clone();
        for (from, to) in self.renamed.iter().filter(|(f, _)| is_spira_key(f)) {
            if let Some(v) = layers.iter().rev().find_map(|t| get(t, from)) {
                d.added.insert(to.clone(), v.clone());
                if !d.removed.contains(from) {
                    d.removed.push(from.clone());
                }
            }
        }
        d
    }

    pub fn render(&self) -> String {
        let mut top = Table::new();
        if !self.removed.is_empty() {
            top.insert("removed".into(), Value::Array(self.removed.iter().cloned().map(Value::String).collect()));
        }
        if !self.renamed.is_empty() {
            top.insert("renamed".into(), Value::Table(self.renamed.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect()));
        }
        if !self.added.is_empty() {
            top.insert("added".into(), Value::Table(self.added.clone().into_iter().collect()));
        }
        toml::to_string_pretty(&Value::Table(top)).unwrap_or_default()
    }
}

fn is_spira_key(path: &str) -> bool {
    path.strip_prefix("spira.").is_some_and(|n| !n.is_empty() && !n.contains('.'))
}

fn persona_name(path: &str) -> Result<&str, String> {
    match path.strip_prefix("persona.") {
        Some(n) if !n.is_empty() && !n.contains('.') => Ok(n),
        _ => Err(format!("{path:?}: only persona tables can be renamed, as persona.<name>")),
    }
}

/// The delta a release declares; `None` when its tree has none.
pub fn load(rel: &Path) -> Result<Option<Delta>, String> {
    let p = rel.join(DELTA_PATH);
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", p.display())),
    };
    Delta::parse(&text).map(Some).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn undo_path(state: &Path, sha: &str) -> PathBuf {
    state.join(UNDO_DIR).join(sha)
}

/// The record of how to put the config back for a rollback off `sha`, if one was kept.
pub fn load_undo(state: &Path, sha: &str) -> Result<Option<Delta>, String> {
    let p = undo_path(state, sha);
    match fs::read_to_string(&p) {
        Ok(t) => Delta::parse(&t).map(Some).map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", p.display())),
    }
}

pub fn save_undo(state: &Path, sha: &str, undo: &Delta) -> Result<(), String> {
    if undo.is_empty() {
        return Ok(());
    }
    let p = undo_path(state, sha);
    fs::create_dir_all(p.parent().unwrap_or(state)).map_err(|e| format!("cannot create {}: {e}", state.join(UNDO_DIR).display()))?;
    fsutil::write_atomic(&p, &undo.render())
}

/// Keys an earlier activation left in the config because a process running the code before it
/// may still resolve them; the next activation removes them.
pub fn load_retired(state: &Path) -> Vec<String> {
    fs::read_to_string(state.join(RETIRED_FILE)).map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()).unwrap_or_default()
}

pub fn save_retired(state: &Path, keys: &[String]) -> Result<(), String> {
    let p = state.join(RETIRED_FILE);
    if keys.is_empty() {
        let _ = fs::remove_file(&p);
        return Ok(());
    }
    fsutil::write_atomic(&p, &format!("{}\n", keys.join("\n")))
}

pub fn clear_undo(state: &Path, sha: &str) {
    let _ = fs::remove_file(undo_path(state, sha));
}

fn get<'a>(t: &'a Table, path: &str) -> Option<&'a Value> {
    let mut cur = t;
    let mut parts = path.split('.').peekable();
    while let Some(p) = parts.next() {
        let v = cur.get(p)?;
        if parts.peek().is_none() {
            return Some(v);
        }
        cur = v.as_table()?;
    }
    None
}

fn set(t: &mut Table, path: &str, v: Value) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, dirs) = parts.split_last().ok_or("empty key path")?;
    let mut cur = t;
    for d in dirs {
        let e = cur.entry(d.to_string()).or_insert_with(|| Value::Table(Table::new()));
        cur = e.as_table_mut().ok_or_else(|| format!("cannot set {path}: {d} is not a table"))?;
    }
    cur.insert(last.to_string(), v);
    Ok(())
}

fn remove(t: &mut Table, path: &str) -> Option<Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, dirs) = parts.split_last()?;
    let mut cur = t;
    for d in dirs {
        cur = cur.get_mut(*d)?.as_table_mut()?;
    }
    cur.remove(*last)
}

/// A delta resolved against the config files in force, validated by the target release.
pub struct Txn {
    files: Vec<PathBuf>,
    modes: Vec<u32>,
    original_text: Vec<String>,
    original: Vec<Table>,
    pre: Vec<Table>,
    post: Vec<Table>,
    /// What puts the config back for a rollback: the keys this added, the values this dropped.
    pub undo: Delta,
    /// The dropped keys this activation leaves in the config until the next one.
    pub deferred: Vec<String>,
}

/// `delta` applied to `original`: the added keys (one an operator already set stays; one held as
/// the empty string was written undefaulted by an install and takes the release's value), then
/// the keys `retired` (left by the previous activation) removed, and what undoes both. The
/// delta's own dropped keys are removed only when `defer` is off: a process still running the
/// previous release's code resolves them until it exits.
fn apply(original: &[Table], delta: &Delta, retired: &[String], defer: bool) -> Result<(Vec<Table>, Vec<Table>, Delta), String> {
    let delta = &delta.expanded(original);
    let mut pre = original.to_vec();
    let mut undo = Delta::default();
    for (k, v) in &delta.added {
        let blank = |t: &Table| get(t, k).and_then(Value::as_str) == Some("");
        if pre.iter().any(|t| get(t, k).is_some() && !blank(t)) {
            continue;
        }
        if pre.iter().any(blank) {
            for t in pre.iter_mut().filter(|t| blank(t)) {
                set(t, k, v.clone())?;
            }
            continue;
        }
        set(&mut pre[0], k, v.clone())?;
        undo.removed.push(k.clone());
    }
    let mut post = pre.clone();
    let dropping = retired.iter().chain(delta.removed.iter().filter(|_| !defer));
    for k in dropping {
        let mut dropped = None;
        for t in post.iter_mut() {
            if let Some(v) = remove(t, k) {
                dropped.get_or_insert(v);
            }
        }
        if let Some(v) = dropped {
            undo.added.insert(k.clone(), v);
        }
    }
    for (from, to) in delta.renamed.iter().filter(|(f, _)| !is_spira_key(f)) {
        let (old, new) = (persona_name(from)?, persona_name(to)?);
        let mut moved = false;
        for t in post.iter_mut() {
            let text = toml::to_string_pretty(&Value::Table(t.clone())).map_err(|e| e.to_string())?;
            let out = spira_config::rename::rename_persona_lenient(&text, old, new)?;
            let Value::Table(renamed) = out.parse::<Value>().map_err(|e| e.to_string())? else { return Err("rename left a non-table".into()) };
            if renamed != *t {
                *t = renamed;
                moved = true;
            }
        }
        if moved {
            undo.renamed.insert(to.clone(), from.clone());
        }
    }
    Ok((pre, post, undo))
}

fn read_layers(spec: &str) -> Result<(Vec<PathBuf>, Vec<u32>, Vec<String>, Vec<Table>), String> {
    let files: Vec<PathBuf> = spec.split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect();
    if files.is_empty() {
        return Err("this release declares config changes but SPIRA_TOML names no file to apply them to".into());
    }
    let (mut modes, mut original_text, mut original) = (Vec::new(), Vec::new(), Vec::new());
    for f in &files {
        let text = fs::read_to_string(f).map_err(|e| format!("cannot read {}: {e}", f.display()))?;
        let Value::Table(t) = text.parse::<Value>().map_err(|e| format!("{}: {e}", f.display()))? else {
            return Err(format!("{}: not a TOML table", f.display()));
        };
        modes.push(fs::metadata(f).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o644));
        original_text.push(text);
        original.push(t);
    }
    Ok((files, modes, original_text, original))
}

/// The layers `spec` names with the release's whole delta applied, staged in a scratch dir:
/// what a binary of `sha` must resolve its config from BEFORE `activate` has written the
/// delta, since that binary's registry requires the keys the delta adds and its schema
/// refuses the keys the delta drops. `None` when the release declares no delta, or the
/// releases directory (`releases` flag, else `spira.releases` in the layers) cannot be told —
/// the caller's plain resolution then reports whatever is wrong.
pub fn staged_for_resolution(spec: &str, releases: Option<&Path>, sha: &str) -> Result<Option<(PathBuf, String)>, String> {
    let (_, _, _, original) = read_layers(spec)?;
    let Some(releases) = releases_of(&original, releases) else { return Ok(None) };
    stage_tree_delta(&original, &releases.join(sha))
}

fn releases_of(original: &[Table], flag: Option<&Path>) -> Option<PathBuf> {
    let from_layers = original.iter().rev().find_map(|t| get(t, "spira.releases").and_then(Value::as_str)).filter(|s| !s.is_empty()).map(PathBuf::from);
    flag.map(Path::to_path_buf).or(from_layers)
}

/// The releases directory the layers `spec` name (or the `releases` flag) give, read from the
/// TOML itself so no registry resolution — and no process-wide config cache — is touched.
pub fn releases_for_spec(spec: &str, flag: Option<&Path>) -> Result<Option<PathBuf>, String> {
    Ok(releases_of(&read_layers(spec)?.3, flag))
}

/// [`staged_for_resolution`] for a source tree (or release) at `tree`: the round VM stages
/// the delta its head declares before any release exists to read it from.
pub fn staged_for_tree(spec: &str, tree: &Path) -> Result<Option<(PathBuf, String)>, String> {
    let (_, _, _, original) = read_layers(spec)?;
    stage_tree_delta(&original, tree)
}

/// [`staged_for_tree`] for a commit not yet built: `build` runs the candidate's binaries
/// against production config before any release directory exists to read the delta from.
pub fn staged_for_commit(git: &dyn crate::git::Git, spec: &str, repo: &Path, commit: &str) -> Result<Option<(PathBuf, String)>, String> {
    let (_, _, _, original) = read_layers(spec)?;
    let sha = git.resolve(repo, commit)?;
    let tree = scratch_dir()?;
    let staged = git.archive(repo, &sha, &tree).and_then(|()| stage_tree_delta(&original, &tree));
    let _ = fs::remove_dir_all(&tree);
    staged
}

fn stage_tree_delta(original: &[Table], tree: &Path) -> Result<Option<(PathBuf, String)>, String> {
    let active = original.iter().rev().find_map(|t| get(t, "spira.releases").and_then(Value::as_str)).filter(|s| !s.is_empty()).map(|r| Path::new(r).join(crate::activate::CURRENT));
    let Some(delta) = complete(original, tree, active.as_deref(), load(tree)?)? else { return Ok(None) };
    let (_, post, _) = apply(original, &delta, &[], true)?;
    let dir = scratch_dir()?;
    let mut staged = Vec::new();
    for (i, t) in post.iter().enumerate() {
        let p = dir.join(format!("layer{i}.toml"));
        let text = toml::to_string_pretty(&Value::Table(t.clone())).map_err(|e| e.to_string())?;
        if let Err(e) = fs::write(&p, text) {
            let _ = fs::remove_dir_all(&dir);
            return Err(format!("cannot write {}: {e}", p.display()));
        }
        staged.push(p.display().to_string());
    }
    Ok(Some((dir, staged.join(":"))))
}

/// Resolve `delta` against the layers `$SPIRA_TOML` names, and have `rel`'s own `spira-config`
/// validate the result. Nothing on disk changes. An added key already present in any layer
/// is the operator's and is left as it is.
pub fn scratch_dir() -> Result<PathBuf, String> {
    let d = std::env::temp_dir().join(format!(
        "release-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    Ok(d)
}

fn registry_keys(rel: &Path) -> Result<Vec<spira_config::registry::RegistryKey>, String> {
    let mut keys = Vec::new();
    for dir in REGISTRY_DIRS {
        let dir = rel.join(dir);
        if dir.is_dir() {
            keys.extend(spira_config::registry::load(&dir)?.into_values());
        }
    }
    Ok(keys)
}

/// The release's delta plus the registry default of every key its registry declares that the
/// active release's registry (`active`) does not, the layers lack and the delta does not
/// mention, so a key added to the registry without a delta entry still reaches the config.
/// A key the running release already declared is left alone: it runs without it today. A new
/// key whose default is absent or computed refuses, naming it. `None` when there is nothing
/// to apply.
pub fn complete(original: &[Table], rel: &Path, active: Option<&Path>, delta: Option<Delta>) -> Result<Option<Delta>, String> {
    let mut d = delta.unwrap_or_default();
    let mut unset = Vec::new();
    let known: std::collections::BTreeSet<String> = match active {
        Some(a) => registry_keys(a)?.into_iter().map(|k| k.name).collect(),
        None => Default::default(),
    };
    for k in registry_keys(rel)? {
        let path = format!("spira.{}", k.name.strip_prefix("SPIRA_").unwrap_or(&k.name).to_ascii_lowercase());
        if known.contains(&k.name) || d.added.contains_key(&path) || d.removed.contains(&path) || original.iter().any(|t| get(t, &path).is_some()) {
            continue;
        }
        if k.required && active.is_some() {
            unset.push(format!("{} (config key {path}): {}", k.name, k.doc));
            continue;
        }
        match literal_default(&k) {
            Ok(v) => {
                d.added.insert(path, v);
            }
            // No literal default, or one computed on the box: the key is optional and its code
            // supplies the value (hotfix_alert_hours defaults to 4 in code). Refusing it blocked
            // every round VM, which has no active release (r-auto-107 and r-auto-110, 2026-10-10).
            Err(_) => {}
        }
    }
    if !unset.is_empty() {
        return Err(format!(
            "this release makes config keys REQUIRED that this box does not set, and its config delta does not set them; set each in the config (or ship the value in config-delta.toml) and retry; nothing changed:\n  {}",
            unset.join("\n  ")
        ));
    }
    Ok((!d.is_empty()).then_some(d))
}

/// [`complete`] against the layers `cfg` names; `delta` as it stands when no layer is named.
pub fn complete_for(cfg: &Config, rel: &Path, delta: Option<Delta>) -> Result<Option<Delta>, String> {
    match cfg.toml_spec() {
        Some(spec) if REGISTRY_DIRS.iter().any(|d| rel.join(d).is_dir()) => {
            complete(&read_layers(&spec)?.3, rel, Some(&cfg.releases.join(crate::activate::CURRENT)), delta)
        }
        _ => Ok(delta),
    }
}

fn literal_default(k: &spira_config::registry::RegistryKey) -> Result<Value, String> {
    let expr = k.default_expr().ok_or("the registry gives it no default")?.trim();
    let bare = ["\"", "'"].iter().find_map(|q| expr.strip_prefix(q).and_then(|e| e.strip_suffix(q))).unwrap_or(expr);
    if bare.contains(['$', '`', '\\']) {
        return Err("its default is computed on the box".into());
    }
    match k.ty.as_str() {
        "u32" | "u64" => bare.parse::<i64>().map(Value::Integer).map_err(|_| format!("default {bare:?} is not a number")),
        "bool" => match bare {
            "1" | "true" => Ok(Value::Boolean(true)),
            "0" | "false" => Ok(Value::Boolean(false)),
            o => Err(format!("default {o:?} is not a boolean")),
        },
        "list" => Ok(Value::Array(bare.split_whitespace().map(|w| Value::String(w.into())).collect())),
        _ => Ok(Value::String(bare.into())),
    }
}

/// `defer` leaves the delta's dropped keys in the config for one more activation; a rollback
/// passes `false`, since the release it returns to rejects keys it does not know.
pub fn prepare(cfg: &Config, rel: &Path, delta: &Delta, defer: bool) -> Result<Txn, String> {
    let spec = cfg.toml_spec().ok_or("this release declares config changes but SPIRA_TOML names no file to apply them to")?;
    let (files, modes, original_text, original) = read_layers(&spec)?;
    let retired = if defer { load_retired(&cfg.state_dir()?) } else { Vec::new() };
    let (pre, post, undo) = apply(&original, delta, &retired, defer)?;
    let delta = &delta.expanded(&original);
    let deferred = if defer { delta.removed.iter().filter(|k| original.iter().any(|t| get(t, k).is_some())).cloned().collect() } else { Vec::new() };
    let txn = Txn { files, modes, original_text, original, pre, post, undo, deferred };
    txn.validate(cfg, rel)?;
    Ok(txn)
}

impl Txn {
    fn validate(&self, cfg: &Config, rel: &Path) -> Result<(), String> {
        let bin = rel.join("bin/spira-config");
        if !fsutil::is_executable(&bin) {
            return Err(format!("{} is missing; cannot check the config this release needs", bin.display()));
        }
        let scratch = scratch_dir()?;
        let result = self.validate_in(cfg, rel, &bin, &scratch);
        let _ = fs::remove_dir_all(&scratch);
        result
    }

    /// Write the layers with the delta fully applied into `dir`; returns the `SPIRA_TOML` value naming them.
    pub fn stage(&self, dir: &Path) -> Result<String, String> {
        let mut staged = Vec::new();
        for (i, t) in self.post.iter().enumerate() {
            let p = dir.join(format!("layer{i}.toml"));
            let text = toml::to_string_pretty(&Value::Table(t.clone())).map_err(|e| e.to_string())?;
            fs::write(&p, text).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
            staged.push(p.display().to_string());
        }
        Ok(staged.join(":"))
    }

    fn validate_in(&self, cfg: &Config, rel: &Path, bin: &Path, scratch: &Path) -> Result<(), String> {
        let spec = self.stage(scratch)?;
        // batch-job: the new binary's config load, bounded at 60 s by timeout(1).
        let mut cmd = Command::new("timeout");
        cmd.arg("60").arg(bin).arg("validate").env("SPIRA_TOML", spec);
        for (k, v) in crate::verify::pre_activate_env(cfg, rel)? {
            cmd.env(k, v);
        }
        let out = cmd.output().map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
        Err(format!(
            "this release cannot load the config with its declared changes applied ({}); nothing changed: {}",
            self.files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(":"),
            tail.join(" | ")
        ))
    }

    fn write_layer(&self, i: usize, text: &str) -> Result<(), String> {
        fsutil::write_atomic(&self.files[i], text)?;
        let _ = fs::set_permissions(&self.files[i], fs::Permissions::from_mode(self.modes[i]));
        Ok(())
    }

    fn write_changed(&self, from: &[Table], to: &[Table]) -> Result<(), String> {
        for i in 0..to.len() {
            if from[i] == to[i] {
                continue;
            }
            let text = toml::to_string_pretty(&Value::Table(to[i].clone())).map_err(|e| e.to_string())?;
            self.write_layer(i, &text)?;
        }
        Ok(())
    }

    /// Before the flip: write the keys the release adds. The old release has not stopped
    /// running yet, so everything it still needs stays.
    pub fn apply_pre(&self) -> Result<(), String> {
        for i in 0..self.files.len() {
            if self.original[i] != self.pre[i] {
                spira_config::backup_existing(&self.files[i]).map_err(|e| format!("cannot back up {}: {e}", self.files[i].display()))?;
            }
        }
        self.write_changed(&self.original, &self.pre).map_err(|e| {
            let undo = self.restore();
            if undo.is_empty() { e } else { format!("{e}; restoring the config also failed: {}", undo.join("; ")) }
        })
    }

    /// After the flip: drop the keys the release dropped, which the old release needed until now.
    pub fn apply_post(&self) -> Result<(), String> {
        self.write_changed(&self.pre, &self.post)
    }

    /// Put every layer back as it was found. Returns the problems it hit.
    pub fn restore(&self) -> Vec<String> {
        let mut errs = Vec::new();
        for i in 0..self.files.len() {
            if self.original[i] != self.post[i] {
                if let Err(e) = self.write_layer(i, &self.original_text[i]) {
                    errs.push(e);
                }
            }
        }
        errs
    }
}
