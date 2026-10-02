//! Unit templates: which installed unit comes from which template, and rendering one
//! against a release (DESIGN.md "Which units", "Render").

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// One installed unit and the template it is rendered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    /// The installed file name, e.g. `spira-sentinel-prod.service`.
    pub installed: String,
    /// The template file name in the release's `systemd/`, e.g. `spira-sentinel.service`.
    pub template: String,
    /// For `spira-watch@.service`: the watcher name `%i` becomes.
    pub watcher: Option<String>,
}

/// Map an installed unit file name back to its template, given the templates a release
/// carries. `None`: not a unit any template makes.
pub fn template_for(installed: &str, templates: &BTreeSet<String>, instance: &str) -> Option<Mapped> {
    let mapped = |t: &str, w: Option<String>| Some(Mapped { installed: installed.into(), template: t.into(), watcher: w });
    if !installed.starts_with("spira-") {
        return if templates.contains(installed) { mapped(installed, None) } else { None };
    }
    for ext in [".service", ".timer", ".socket"] {
        let Some(stem) = installed.strip_suffix(ext) else { continue };
        let base = stem.strip_suffix(&format!("-{instance}"))?;
        let t = format!("{base}{ext}");
        if templates.contains(&t) && t != "spira-watch@.service" {
            return mapped(&t, None);
        }
        if ext == ".service" {
            if let Some(name) = base.strip_prefix("spira-watch-") {
                if !name.is_empty() && templates.contains("spira-watch@.service") {
                    return mapped("spira-watch@.service", Some(name.to_string()));
                }
            }
        }
        return None;
    }
    None
}

/// The unit templates in `<rel>/systemd`.
pub fn templates(rel: &Path) -> Result<BTreeSet<String>, String> {
    let dir = rel.join("systemd");
    let rd = fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut out = BTreeSet::new();
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if [".service", ".timer", ".socket"].iter().any(|x| n.ends_with(x)) && e.path().is_file() {
            out.insert(n);
        }
    }
    Ok(out)
}

/// Every installed unit in `unit_dir` that a template in `rel` makes. Symlinks (masked
/// units) are skipped: a mask is the operator's, not the release's.
pub fn installed(unit_dir: &Path, rel: &Path, instance: &str) -> Result<Vec<Mapped>, String> {
    let tpl = templates(rel)?;
    let rd = match fs::read_dir(unit_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", unit_dir.display())),
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let n = e.file_name().to_string_lossy().to_string();
        if let Some(m) = template_for(&n, &tpl, instance) {
            out.push(m);
        }
    }
    out.sort_by(|a, b| a.installed.cmp(&b.installed));
    Ok(out)
}

/// `SPIRA_<X>_BIN` placeholder to the binary it names, where the name is not just
/// `<x>` lowercased with `_` as `-`.
const BIN_NAMES: &[(&str, &str)] = &[("SUPERVISE", "spira-supervise")];

/// The values that come from the release itself (DESIGN.md "Render").
pub fn release_values(rel: &Path) -> BTreeMap<String, String> {
    let r = rel.display().to_string();
    let mut m = BTreeMap::new();
    for k in ["SPIRA_HOME", "SPIRA_PROD"] {
        m.insert(k.to_string(), format!("{r}/spira"));
    }
    // SPIRA_RELEASE: the root every unit's `Environment=PATH=` is built from (sp-31gtu).
    for k in ["SPIRA_PROD_ROOT", "SPIRA_REPO", "SPIRA_RELEASE"] {
        m.insert(k.to_string(), r.clone());
    }
    for k in ["SPIRA_PROD_COCK", "SPIRA_COCKPIT"] {
        m.insert(k.to_string(), format!("{r}/cockpit"));
    }
    m
}

/// The binary a `SPIRA_<X>_BIN` key names, if `key` is one.
pub fn bin_for_key(key: &str) -> Option<String> {
    let x = key.strip_prefix("SPIRA_")?.strip_suffix("_BIN")?;
    if x.is_empty() {
        return None;
    }
    Some(BIN_NAMES.iter().find(|(k, _)| *k == x).map(|(_, n)| n.to_string()).unwrap_or_else(|| x.to_lowercase().replace('_', "-")))
}

/// Every `@KEY@` placeholder in `text`, in order of first appearance.
pub fn placeholders(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'@' {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_uppercase() || b[j] == b'_') {
                j += 1;
            }
            if j > i + 1 && j < b.len() && b[j] == b'@' {
                let k = text[i + 1..j].to_string();
                if !out.contains(&k) {
                    out.push(k);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Replace every `@KEY@` for which `value` returns something.
fn substitute(text: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let mut out = text.to_string();
    for k in placeholders(text) {
        if let Some(v) = value(&k) {
            out = out.replace(&format!("@{k}@"), &v);
        }
    }
    out
}

/// Host key naming the shared compilation cache's own address (sp-xtdqi/sp-xtdqi-2) — the
/// ONE place this key's name lives. `install`'s manifest and `release`'s own activate/render
/// both read a template's gate through [`gate_key`]/[`gate_open`], never a second,
/// independently hand-written literal.
pub const SCCACHE_DAV_ADDR_KEY: &str = "SPIRA_SCCACHE_DAV_ADDR";

/// A template whose UNIT IS INSTALLED AT ALL only when a host key is non-empty — distinct
/// from a key the template merely *uses* (several templates read `SPIRA_DOLT_DATA`, but only
/// `dolt-beads.service` is gated on it existing at all). sp-xtdqi's `sccache-dav.service` is
/// the first entry: `install`'s manifest correctly declined it when `SPIRA_SCCACHE_DAV_ADDR`
/// was unset, but `activate::switch`'s own render loop — which re-renders whatever is
/// ALREADY on disk, not what the manifest would choose today — kept trying to fill its
/// placeholder anyway and refused the WHOLE activation on any box that had the unit installed
/// before the gate existed (sp-xtdqi-2).
pub const OPTIONAL_UNIT_GATES: &[(&str, &str)] = &[("sccache-dav.service", SCCACHE_DAV_ADDR_KEY)];

/// The host key gating `template_name`'s installation, if it has one.
pub fn gate_key(template_name: &str) -> Option<&'static str> {
    OPTIONAL_UNIT_GATES.iter().find(|(t, _)| *t == template_name).map(|(_, k)| *k)
}

/// Whether `template_name` may be installed or re-rendered: true when it carries no gate at
/// all, or its gate key is set (non-empty) in `host` — the environment or the host config,
/// exactly as [`Config::host_values`](crate::config::Config::host_values) itself resolves
/// every other key.
pub fn gate_open(template_name: &str, host: &BTreeMap<String, String>) -> bool {
    match gate_key(template_name) {
        None => true,
        Some(k) => host.get(k).is_some_and(|v| !v.trim().is_empty()),
    }
}

/// Placeholders an empty value fills rather than refuses — just `SPIRA_PATH_TAIL`
/// (sp-c7b85): "nothing configured" is its ordinary, common case, unlike every other host key
/// here, where an empty value means the caller forgot to set something the unit needs.
const OPTIONAL_EMPTY: &[&str] = &["SPIRA_PATH_TAIL"];

/// Render one template against release `rel` (systemd/render.py's rules, in Rust).
/// `host` supplies the host keys. A placeholder no key fills, or a key the template uses
/// with an empty value, is an error naming it — except [`OPTIONAL_EMPTY`].
pub fn render(template_name: &str, text: &str, rel: &Path, host: &BTreeMap<String, String>, watcher: Option<&str>, instance: &str) -> Result<String, String> {
    let rv = release_values(rel);
    let lookup = |k: &str| -> Option<String> {
        if let Some(v) = rv.get(k) {
            return Some(v.clone());
        }
        if let Some(b) = bin_for_key(k) {
            return Some(format!("{}/bin/{b}", rel.display()));
        }
        host.get(k).cloned()
    };
    render_from_lookup(template_name, text, lookup, watcher, instance)
}

/// The shared core of [`render`] and of `install`'s general-purpose renderer (which has no
/// single release directory to derive keys from — [[`SPIRA_HOME`]] and `SPIRA_PROD` can
/// differ, e.g. a split dev/prod checkout): given a complete, precomputed value map, apply
/// systemd/render.py's rules — every `@KEY@` filled or the template is refused by name
/// ([`OPTIONAL_EMPTY`] aside), `%i` becomes the watcher name, a `spira-*.timer`'s
/// `Unit=spira-<x>.service` gains the instance suffix, and the output carries exactly one
/// trailing newline.
pub fn render_from_values(template_name: &str, text: &str, values: &BTreeMap<String, String>, watcher: Option<&str>, instance: &str) -> Result<String, String> {
    render_from_lookup(template_name, text, |k| values.get(k).cloned(), watcher, instance)
}

fn render_from_lookup(template_name: &str, text: &str, lookup: impl Fn(&str) -> Option<String>, watcher: Option<&str>, instance: &str) -> Result<String, String> {
    let mut unknown = Vec::new();
    let mut empty = Vec::new();
    for k in placeholders(text) {
        match lookup(&k) {
            None => unknown.push(k),
            Some(v) if v.is_empty() && !OPTIONAL_EMPTY.contains(&k.as_str()) => empty.push(k),
            Some(_) => {}
        }
    }
    if !unknown.is_empty() {
        return Err(format!("{template_name} has placeholders nothing fills: {}", unknown.join(", ")));
    }
    if !empty.is_empty() {
        return Err(format!("{template_name} uses {} but no value is set for it (environment or host config)", empty.join(", ")));
    }
    let mut out = substitute(text, lookup);
    if let Some(w) = watcher {
        out = out.replace("%i", w);
    }
    if template_name.starts_with("spira-") && template_name.ends_with(".timer") {
        out = out
            .lines()
            .map(|l| match l.strip_prefix("Unit=spira-").and_then(|r| r.strip_suffix(".service")) {
                Some(name) if !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') => {
                    format!("Unit=spira-{name}-{instance}.service")
                }
                _ => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    Ok(normalize(&out))
}

/// Exactly one trailing newline — what `units-install` and `unit-ensure` (sp-31dm0; formerly
/// `systemd/install.sh` and `systemd/unit-ensure.sh`) write.
pub fn normalize(text: &str) -> String {
    format!("{}\n", text.trim_end_matches('\n'))
}

/// The `Exec*=` lines of a unit (what it runs), in order.
pub fn exec_lines(text: &str) -> Vec<String> {
    text.lines().filter(|l| l.starts_with("Exec") && l.contains('=')).map(str::to_string).collect()
}

/// A service whose `Exec*=` lines differ between two renderings.
pub fn exec_changed(old: &str, new: &str) -> bool {
    exec_lines(old) != exec_lines(new)
}

/// Verify's unit check (DESIGN.md "verify"): in every template in `<rel>/systemd`, every
/// path under `rel` an `Exec*=` line names (after the release's own placeholders are filled)
/// exists, and the program each line runs is executable.
pub fn check_binaries(rel: &Path) -> Result<Vec<String>, String> {
    let root = rel.display().to_string();
    let rv = release_values(rel);
    let mut problems = Vec::new();
    for t in templates(rel)? {
        let p = rel.join("systemd").join(&t);
        let text = fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        let text = substitute(&text, |k| rv.get(k).cloned().or_else(|| bin_for_key(k).map(|b| format!("{root}/bin/{b}"))));
        for line in exec_lines(&text) {
            let (_, cmd) = line.split_once('=').unwrap_or(("", ""));
            let prog = cmd.trim_start_matches(['-', '@', '+', '!', ':']).split_whitespace().next().unwrap_or("");
            if prog.starts_with(&format!("{root}/")) && !crate::fsutil::is_executable(Path::new(prog)) {
                problems.push(format!("{t}: runs {prog}, which is not an executable file in the release"));
            }
            let mut rest = cmd;
            while let Some(i) = rest.find(&format!("{root}/")) {
                let tail = &rest[i..];
                let end = tail.find(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';').unwrap_or(tail.len());
                let path = &tail[..end];
                if path != prog && fs::symlink_metadata(path).is_err() {
                    problems.push(format!("{t}: names {path}, which is not in the release"));
                }
                rest = &tail[end..];
            }
        }
    }
    Ok(problems)
}
