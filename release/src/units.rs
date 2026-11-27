//! Unit templates: which installed unit comes from which template, and rendering one
//! against a release (DESIGN.md "Which units", "Render").

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// One installed unit and the template it is rendered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    /// The installed file name, e.g. `spira-sentinel-prod.service`.
    pub installed: String,
    /// The template file name in the release's `systemd/`, e.g. `spira-sentinel.service`.
    pub template: String,
    /// For an instanced template (`spira-watch@.service`, `spira-sub@.service`): the name `%i`
    /// becomes.
    pub watcher: Option<String>,
}

pub const SUB_TEMPLATE: &str = "spira-sub@.service";

/// The subscribers this harness runs a `spira-sub` for.
pub const SUBSCRIBERS: [&str; 1] = ["concierge"];

/// The installed name of `name`'s `spira-sub` unit.
pub fn subscriber_unit(name: &str, instance: &str) -> String {
    format!("spira-sub-{name}-{instance}.service")
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
        if templates.contains(&t) && !t.ends_with("@.service") {
            return mapped(&t, None);
        }
        if ext == ".service" {
            let subscriber = SUBSCRIBERS.into_iter().find(|name| installed == subscriber_unit(name, instance));
            if let Some(name) = subscriber.filter(|_| templates.contains(SUB_TEMPLATE)) {
                return mapped(SUB_TEMPLATE, Some(name.to_string()));
            }
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

/// Whether a template in `rel` makes the installed unit `unit`, so activating `rel` rewrites
/// and may restart it.
pub fn is_shipped_by(rel: &Path, instance: &str, unit: &str) -> bool {
    templates(rel).is_ok_and(|t| template_for(unit, &t, instance).is_some())
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

/// `@SPIRA_REPO@` for a unit: the repo map's home-repo root when that is a git checkout,
/// else the release directory, which has no `.git`.
pub fn units_repo(mapped: Option<String>, rel: &Path, is_git: impl Fn(&Path) -> bool) -> String {
    match mapped.filter(|m| !m.is_empty() && is_git(Path::new(m))) {
        Some(m) => m,
        None => rel.display().to_string(),
    }
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

/// Placeholders an empty value fills rather than refuses: `SPIRA_PATH_TAIL` ("nothing
/// configured" is its ordinary case) and `SPIRA_REPO_MAP` (conf.sh falls back to its own
/// candidate search on an empty value, and the pass refuses to run when that finds nothing),
/// and `SPIRA_LC_PASSWORD_FILE` (empty until a same-user credential exists; spira-lc then reads
/// SPIRA_LC_PASSWORD).
const OPTIONAL_EMPTY: &[&str] = &["SPIRA_PATH_TAIL", "SPIRA_REPO_MAP", "SPIRA_LC_PASSWORD_FILE"];

/// Render one template against release `rel` (systemd/render.py's rules, in Rust).
/// `host` supplies the host keys. A placeholder no key fills, or a key the template uses
/// with an empty value, is an error naming it — except [`OPTIONAL_EMPTY`].
pub fn render(template_name: &str, text: &str, rel: &Path, host: &BTreeMap<String, String>, watcher: Option<&str>, instance: &str) -> Result<String, String> {
    let mut rv = release_values(rel);
    if text.contains("@SPIRA_REPO@") {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let mapped = spira_config::repos::Registry::from_env(env, &rel.join("spira")).root("");
        rv.insert("SPIRA_REPO".into(), units_repo(mapped, rel, |p| p.join(".git").exists()));
    }
    let lookup = |k: &str| -> Option<String> {
        if let Some(v) = rv.get(k) {
            return Some(v.clone());
        }
        if let Some(b) = bin_for_key(k) {
            return Some(format!("{}/bin/{b}", rel.display()));
        }
        host.get(k).cloned()
    };
    render_from_lookup(template_name, text, lookup, watcher, instance).map(|out| pin_documentation(&out, rel))
}

/// `Documentation=` lines name the release by its `current` link, not its sha: they are the one
/// per-release difference in a unit whose behaviour did not change.
fn pin_documentation(text: &str, rel: &Path) -> String {
    let Some(parent) = rel.parent() else { return text.to_string() };
    let (from, to) = (rel.display().to_string(), parent.join("current").display().to_string());
    let mut out: String = text.lines().map(|l| if l.starts_with("Documentation=") { l.replace(&from, &to) } else { l.to_string() }).collect::<Vec<_>>().join("\n");
    out.push('\n');
    out
}

/// A unit's text without its `Documentation=` lines: what decides whether it changed.
pub fn without_documentation(text: &str) -> String {
    text.lines().filter(|l| !l.starts_with("Documentation=")).map(|l| format!("{l}\n")).collect()
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

/// Optional keys a host map may omit entirely; they render empty. `SPIRA_LC_PASSWORD_FILE`
/// (sp-9c2o5) is new to every unit, and a host map written before it — a test fixture or a
/// predecessor's activation — must not be refused for lacking a value that may be empty anyway.
const ABSENT_IS_EMPTY: &[&str] = &["SPIRA_LC_PASSWORD_FILE"];

fn render_from_lookup(template_name: &str, text: &str, lookup: impl Fn(&str) -> Option<String>, watcher: Option<&str>, instance: &str) -> Result<String, String> {
    let lookup = |k: &str| lookup(k).or_else(|| ABSENT_IS_EMPTY.contains(&k).then(String::new));
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

/// The program the first `ExecStart=` line runs, without systemd's prefixes.
pub fn exec_start_program(text: &str) -> Option<String> {
    let line = exec_lines(text).into_iter().find(|l| l.starts_with("ExecStart="))?;
    line["ExecStart=".len()..].trim_start_matches(['-', '@', '+', '!', ':']).split_whitespace().next().map(str::to_string)
}

/// Why the process running `exe` has outlived `prog`, or `None`. A file that cannot be read
/// is not shown to match.
pub fn outlived(exe: &Path, prog: &Path) -> Option<String> {
    let (have, want) = match (fs::read(exe), fs::read(prog)) {
        (Ok(have), Ok(want)) => (have, want),
        _ => return Some(format!("cannot compare its running executable with {}", prog.display())),
    };
    // A script's process runs its interpreter, so its bytes never match the script's.
    if want.starts_with(b"#!") {
        return None;
    }
    (have != want).then(|| format!("its running process's executable is not {}", prog.display()))
}

/// A unit that opts out of the activation drain: it touches no state an activation swaps, so
/// waiting for it to exit buys nothing.
pub fn undrained(text: &str) -> bool {
    text.lines().any(|l| l.trim().replace(' ', "").eq_ignore_ascii_case("X-Spira-NoDrain=true"))
}

/// Why activation must leave this service running (`X-Spira-NoRestart=<reason>`, sp-sv2lw2):
/// restarting it disturbs what the operator is using. A line with no reason, or only a
/// boolean, is no exemption: the unit is restarted like any other.
pub fn never_restart(text: &str) -> Option<String> {
    reason_of(text, "X-Spira-NoRestart")
}

/// The release-relative files and directories whose change in an activation starts this unit
/// once (`X-Spira-StartWhenChanged=<path> <path> ...`).
pub fn start_when_changed(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().split_once('='))
        .filter(|(k, _)| k.trim().eq_ignore_ascii_case("X-Spira-StartWhenChanged"))
        .flat_map(|(_, v)| v.split_whitespace().map(str::to_string))
        .collect()
}

/// How a service restarts itself (`X-Spira-RestartWhenIdle=<how>`): a restart now would cut
/// what it holds, and the process exits once the reason names is met and its release is no
/// longer current, so `Restart=` brings it up on the new one. Activation reports it and
/// does not restart it.
pub fn restart_when_idle(text: &str) -> Option<String> {
    reason_of(text, "X-Spira-RestartWhenIdle")
}

/// The reason a running watcher restarts itself: only a watcher whose installed unit carried
/// the same reason runs code that implements it. A changed reason means the running code
/// implements an older one, so activation restarts it.
pub fn restarts_itself_when_idle(installed: &str, shipped: &str) -> Option<String> {
    restart_when_idle(shipped).filter(|r| restart_when_idle(installed).as_ref() == Some(r))
}

fn reason_of(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.trim().split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty() && !["true", "yes", "on", "1", "false", "no", "off", "0"].contains(&v.to_ascii_lowercase().as_str()))
        .map(str::to_string)
}

/// The file a service's own output is appended to: `StandardError=append:` else
/// `StandardOutput=append:`, in `[Service]`.
pub fn own_log(unit_text: &str) -> Option<PathBuf> {
    let mut in_service = false;
    let (mut err, mut out) = (None, None);
    for l in unit_text.lines().map(str::trim) {
        if l.starts_with('[') {
            in_service = l.eq_ignore_ascii_case("[Service]");
            continue;
        }
        let Some((k, v)) = l.split_once('=').filter(|_| in_service) else { continue };
        let path = v.trim().strip_prefix("append:").map(str::trim).filter(|p| !p.is_empty());
        match (k.trim(), path) {
            ("StandardError", Some(p)) => err = Some(PathBuf::from(p)),
            ("StandardOutput", Some(p)) => out = Some(PathBuf::from(p)),
            _ => {}
        }
    }
    err.or(out)
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

#[cfg(test)]
mod absent_is_empty_tests {
    use super::render_from_lookup;

    #[test_id(t45h5d1)]
    #[test]
    fn an_omitted_lc_password_file_renders_empty_and_an_omitted_other_key_still_refuses() {
        let ok = render_from_lookup("x.service", "Environment=SPIRA_LC_PASSWORD_FILE=@SPIRA_LC_PASSWORD_FILE@\n", |_| None, None, "prod").unwrap();
        assert_eq!(ok, "Environment=SPIRA_LC_PASSWORD_FILE=\n");
        let err = render_from_lookup("x.service", "Environment=SPIRA_DB=@SPIRA_DB@\n", |_| None, None, "prod").unwrap_err();
        assert!(err.contains("SPIRA_DB"), "{err}");
    }
}

#[cfg(test)]
mod exec_changed_tests {
    use super::exec_changed;

    #[test_id(tq2bjv3)]
    #[test]
    fn a_release_that_moves_only_environment_lines_is_not_an_exec_change() {
        let old = "[Service]\nEnvironment=SPIRA_RELEASE=/r/aaa\nExecStart=/bin/dolt sql-server\nExecStopPost=/p/store-persist pass --offline\n";
        let new = old.replace("r/aaa", "r/bbb");
        assert!(!exec_changed(old, &new));
        assert!(exec_changed(old, &old.replace("--offline", "")));
    }
}

#[cfg(test)]
mod units_repo_tests {
    use super::*;
    #[test_id(t009r6s)]
    #[test]
    fn units_repo_is_the_mapped_checkout_when_it_is_git_else_the_release() {
        let rel = Path::new("r/rel");
        let git = |p: &Path| p == Path::new("h/harness");
        assert_eq!(units_repo(Some("h/harness".into()), rel, git), "h/harness");
        assert_eq!(units_repo(Some("h/plain".into()), rel, git), "r/rel");
        assert_eq!(units_repo(None, rel, git), "r/rel");
    }
}

/// `PATH-SHADOW` findings: a unit file or drop-in under `unit_dir` whose `Environment=PATH=`
/// puts a directory outside `releases` ahead of the release, holding an executable the
/// release also ships in `bin/` or `spira/` — the unit would run that copy, not the release's.
pub fn path_shadows(unit_dir: &Path, rel: &Path, releases: &Path) -> Vec<String> {
    let shipped = |name: &str| ["bin", "spira"].iter().any(|d| crate::fsutil::is_executable(&rel.join(d).join(name)));
    let mut files = Vec::new();
    for e in fs::read_dir(unit_dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files.extend(fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()));
        } else {
            files.push(p);
        }
    }
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else { continue };
        for line in text.lines() {
            let l = line.trim().trim_start_matches("Environment=").trim_matches('"');
            let Some(path) = l.strip_prefix("PATH=") else { continue };
            for dir in path.trim_matches('"').split(':').filter(|d| !d.is_empty()) {
                let d = Path::new(dir);
                if d.starts_with(releases) {
                    break;
                }
                let hit: Vec<String> = fs::read_dir(d)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| crate::fsutil::is_executable(&d.join(n)) && shipped(n))
                    .collect();
                if !hit.is_empty() {
                    out.push(format!("PATH-SHADOW {}: {dir} precedes the release on PATH and carries {} which the release also ships", f.display(), hit.join(", ")));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod own_log_tests {
    use super::own_log;
    use std::path::PathBuf;

    #[test_id(t41dxjo)]
    #[test]
    fn own_log_reads_the_append_path_from_either_directive_and_none_from_the_journal() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(own_log("[Service]\nStandardError=append:r/a.log\n"), p("r/a.log"));
        assert_eq!(own_log("[Service]\nStandardOutput=append:r/o.log\n"), p("r/o.log"));
        assert_eq!(own_log("[Service]\nStandardOutput=append:r/o.log\nStandardError=append:r/e.log\n"), p("r/e.log"));
        assert_eq!(own_log("[Service]\nStandardError=journal\nStandardOutput=journal\n"), None);
        assert_eq!(own_log("[Unit]\nStandardError=append:r/a.log\n"), None);
        assert_eq!(own_log("[Service]\nExecStart=/bin/true\n"), None);
    }
}

#[cfg(test)]
mod outlived_tests {
    use super::{exec_start_program, outlived};
    use std::fs;

    #[test_id(tpybzi5)]
    #[test]
    fn exec_start_program_strips_systemds_prefixes_and_drops_the_arguments() {
        assert_eq!(exec_start_program("[Service]\nExecStart=-/x/prog a\n").as_deref(), Some("/x/prog"));
        assert_eq!(exec_start_program("ExecStart=-@+!:/x/prog\n").as_deref(), Some("/x/prog"));
    }

    #[test_id(teqwpo9)]
    #[test]
    fn exec_start_program_skips_exec_start_pre_and_is_none_without_exec_start() {
        assert_eq!(exec_start_program("ExecStartPre=/x/pre\nExecStart=/x/prog\n").as_deref(), Some("/x/prog"));
        assert_eq!(exec_start_program("ExecStartPre=/x/pre\n"), None);
    }

    #[test_id(twta34p)]
    #[test]
    fn a_process_running_the_programs_bytes_has_not_outlived_it() {
        let t = testkit::TempDir::new("release-outlived-same");
        let (exe, prog) = (t.path().join("exe"), t.path().join("prog"));
        fs::write(&exe, "V1").unwrap();
        fs::write(&prog, "V1").unwrap();
        assert_eq!(outlived(&exe, &prog), None);
    }

    #[test_id(tjjrqba)]
    #[test]
    fn a_process_running_other_bytes_has_outlived_the_program() {
        let t = testkit::TempDir::new("release-outlived-differs");
        let (exe, prog) = (t.path().join("exe"), t.path().join("prog"));
        fs::write(&exe, "V1").unwrap();
        fs::write(&prog, "V2").unwrap();
        assert_eq!(outlived(&exe, &prog), Some(format!("its running process's executable is not {}", prog.display())));
    }

    #[test_id(tko2r03)]
    #[test]
    fn an_unreadable_executable_or_program_counts_as_changed() {
        let t = testkit::TempDir::new("release-outlived-unreadable");
        let (there, gone) = (t.path().join("there"), t.path().join("gone"));
        fs::write(&there, "V1").unwrap();
        assert_eq!(outlived(&gone, &there), Some(format!("cannot compare its running executable with {}", there.display())));
        assert_eq!(outlived(&there, &gone), Some(format!("cannot compare its running executable with {}", gone.display())));
    }

    #[test_id(tteh5db)]
    #[test]
    fn a_script_is_not_compared_with_its_interpreter() {
        let t = testkit::TempDir::new("release-outlived-script");
        let (exe, prog) = (t.path().join("exe"), t.path().join("prog"));
        fs::write(&exe, "INTERPRETER").unwrap();
        fs::write(&prog, "#!/bin/sh\n").unwrap();
        assert_eq!(outlived(&exe, &prog), None);
    }
}
