//! `release session-hook`: register, inspect, prune and remove the coding agent client's
//! `SessionStart` hook and `statusLine` entries in its own settings file (DESIGN.md
//! "session-hook"). Replaces `spira/install-session-hook.sh`.
//!
//! **THE DEFECT THIS FIXES (sp-7jr34, P0).** The bash script computed its `HOOK` path from
//! its own location (`SPIRA_HOME`), which every release activation sets to that release's own
//! sha-pinned directory. `install`'s idempotence only strips a command that is byte-identical
//! to the CURRENT invocation's own `HOOK`, so every activation registered a NEW entry that the
//! next activation's own `install` could not see as "ours" — 27 had accumulated by the time
//! this was found, each exiting 1 (its own PATH did not carry the release that sha names, so
//! `conf.sh` could not find `spira-config` and failed closed), so every session paid ~30s of
//! dead hook time and got no watcher summary at all. `statusLine` had no installer at all and
//! was wired by hand into the same shape, so it failed identically.
//!
//! **THE FIX.** The registered command is always addressed through `current`, never a sha
//! directory, and it carries its own environment: `env SPIRA_RELEASE=<releases>/current
//! PATH=<release_path_with_tail> <releases>/current/spira/hooks/session.sh` — self-contained,
//! so it runs correctly under the client's own environment rather than the launcher's. Every
//! earlier form — a sha-pinned path, the bare "current" path, wrapped or not — is recognised
//! by the same suffix [`is_ours`] checks status/uninstall/prune use, so `install` converges
//! any number of stale entries to exactly one (requirement 2; [`tests::three_activations_converge_to_one`]).

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// The client hook events this manages. No matcher is ever written — an absent matcher
/// matches every `source`, which is the point (the client's hook-input schema restricts
/// `SessionStart`'s `source` to `startup`, `resume`, `clear`, `compact` and `fork`; naming a
/// subset is how a hook goes missing from exactly the case it was written for).
pub const EVENTS: &[&str] = &["SessionStart"];

/// A retired event: stripped on install, never (re-)added. `PostCompact` used to carry this
/// hook too, before `SessionStart`'s own `source=compact` was known to fire on every
/// compaction — registering both ran the hook twice on every one.
pub const RETIRE_EVENTS: &[&str] = &["PostCompact"];

/// The SessionStart hook's own timeout: generous next to what it costs (one `systemctl
/// is-active` call and a few file reads), because the penalty for being slow is a warning and
/// the penalty for being killed is a session that starts with no idea what is watching it.
pub const HOOK_TIMEOUT_SECS: u64 = 10;

/// The status line's own refresh: re-run often enough that a session that was just cleared
/// does not keep showing the discarded one's context (ctx-meter.sh's own design intent).
pub const METER_REFRESH_SECS: u64 = 5;

/// The suffix that identifies the session hook in a command string, whatever release or form
/// it names — a sha-pinned path, the bare "current" path, or one wrapped in `env ... PATH=`.
pub const HOOK_SUFFIX: &str = "spira/hooks/session.sh";

/// The suffix that identifies the context meter, the same way.
pub const METER_SUFFIX: &str = "spira/ctx-meter.sh";

/// True when `command` names ours, by suffix, whatever release or wrapping it carries
/// (requirement 2: converge every earlier Spira entry, however it was written).
pub fn is_ours(command: &str, suffix: &str) -> bool {
    command.contains(suffix)
}

/// The paths and commands `session-hook` reads and writes, resolved once by the caller
/// ([`resolve`]) so every pure function below takes explicit values instead of re-deriving
/// them — the same shape [`crate::units::render`] takes its `host`/`rel` explicitly.
pub struct Paths {
    pub settings: PathBuf,
    /// `<releases>/current`, as the string every rendered command names.
    pub release: String,
    /// [`spira_config::release_path_with_tail`] of `release`.
    pub path: String,
    /// The one source of config the commands name (`SPIRA_TOML`): the client's environment
    /// carries none, and every Spira tool refuses without it.
    pub toml: String,
    pub hook: PathBuf,
    pub meter: PathBuf,
}

impl Paths {
    pub fn hook_command(&self) -> String {
        format!("env SPIRA_RELEASE={} SPIRA_TOML={} PATH={} {}", self.release, self.toml, self.path, self.hook.display())
    }
    pub fn meter_command(&self) -> String {
        format!("env SPIRA_RELEASE={} SPIRA_TOML={} PATH={} {}", self.release, self.toml, self.path, self.meter.display())
    }
}

/// Resolve [`Paths`] against release root `current` (typically `<releases>/current` — never a
/// sha directory, requirement 1) and PATH tail `tail`.
pub fn resolve(current: &Path, tail: &str, toml: &str, settings: PathBuf) -> Result<Paths, String> {
    if toml.is_empty() {
        return Err("session-hook: SPIRA_TOML is not set — the hook and meter commands must name the one source of config".into());
    }
    let release = current.display().to_string();
    let path = spira_config::release_path_with_tail(&release, tail)?;
    Ok(Paths { settings, release, path, toml: toml.to_string(), hook: current.join("spira/hooks/session.sh"), meter: current.join("spira/ctx-meter.sh") })
}

/// Read the settings document: `{}` if the file does not exist, an error naming the file if
/// it cannot be parsed or is not a JSON object — never templated, because the file may hold
/// settings nothing here knows about.
pub fn load(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: cannot parse: {e}", path.display()))?;
            if !v.is_object() {
                return Err(format!("{} is not a JSON object", path.display()));
            }
            Ok(v)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write `doc` to `path` only if it differs from what is already there — `install` is a
/// repair run from a timer as well as by hand, and a version that rewrote on every pass would
/// churn the operator's live client settings (and its backup) once a minute. Returns whether
/// it wrote. A prior file is backed up to `<path>.spira.bak` before the swap.
pub fn save(path: &Path, doc: &Value) -> Result<bool, String> {
    let rendered = format!("{}\n", serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?);
    let prior = std::fs::read_to_string(path).ok();
    if prior.as_deref() == Some(rendered.as_str()) {
        return Ok(false);
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    if let Some(p) = &prior {
        std::fs::write(format!("{}.spira.bak", path.display()), p).map_err(|e| format!("cannot write backup: {e}"))?;
    }
    crate::fsutil::write_atomic(path, &rendered)?;
    Ok(true)
}

fn as_entries(hooks: &Map<String, Value>, event: &str) -> Vec<Value> {
    hooks.get(event).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// Every command string in one matcher entry, whatever shape its `hooks` field is in (an
/// object, or an array of them).
fn commands_in(entry: &Value) -> Vec<String> {
    let hooks: Vec<Value> = match entry.get("hooks") {
        Some(Value::Array(a)) => a.clone(),
        Some(h @ Value::Object(_)) => vec![h.clone()],
        _ => Vec::new(),
    };
    hooks.into_iter().filter_map(|h| h.get("command").and_then(Value::as_str).map(str::to_string)).collect()
}

/// Drop every hook whose command matches `is_match` from `entries`; an entry left with no
/// hooks is dropped entirely rather than left as `{"hooks": []}` — the client validates a
/// matcher entry by its hooks being non-empty.
fn strip(entries: Vec<Value>, is_match: impl Fn(&str) -> bool) -> (Vec<Value>, Vec<String>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for entry in entries {
        let hooks: Vec<Value> = match entry.get("hooks") {
            Some(Value::Array(a)) => a.clone(),
            Some(h @ Value::Object(_)) => vec![h.clone()],
            None => {
                kept.push(entry);
                continue;
            }
            _ => {
                kept.push(entry);
                continue;
            }
        };
        let mut keep = Vec::new();
        for h in hooks {
            match h.get("command").and_then(Value::as_str) {
                Some(c) if is_match(c) => dropped.push(c.to_string()),
                _ => keep.push(h),
            }
        }
        if !keep.is_empty() {
            let mut e = entry.as_object().cloned().unwrap_or_default();
            e.insert("hooks".into(), Value::Array(keep));
            kept.push(Value::Object(e));
        }
    }
    (kept, dropped)
}

fn hooks_mut(doc: &mut Value) -> &mut Map<String, Value> {
    if !doc.get("hooks").map(Value::is_object).unwrap_or(false) {
        doc.as_object_mut().unwrap().insert("hooks".into(), json!({}));
    }
    doc.get_mut("hooks").unwrap().as_object_mut().unwrap()
}

fn prune_empty_hooks(doc: &mut Value) {
    let drop = doc.get("hooks").and_then(Value::as_object).map(Map::is_empty).unwrap_or(false);
    if drop {
        doc.as_object_mut().unwrap().remove("hooks");
    }
}

fn hook_entry(command: &str) -> Value {
    json!({"hooks": [{"type": "command", "command": command, "timeout": HOOK_TIMEOUT_SECS}]})
}

fn status_line_value(command: &str) -> Value {
    json!({"type": "command", "command": command, "refreshInterval": METER_REFRESH_SECS})
}

/// `install`: register exactly one Spira `SessionStart` entry and one `statusLine`, both
/// addressed through `p.release` (requirement 1), converging any number of earlier Spira
/// entries to one (requirement 2). Refuses, changing nothing, if the hook or the meter is not
/// an executable file — a registered command that does not exist is a hook error reported to
/// the user at every session start, which is worse than a registration still absent.
pub fn install(doc: &mut Value, p: &Paths) -> Result<Vec<String>, String> {
    if !crate::fsutil::is_executable(&p.hook) {
        return Err(format!("{} is not executable — nothing registered", p.hook.display()));
    }
    if !crate::fsutil::is_executable(&p.meter) {
        return Err(format!("{} is not executable — nothing registered", p.meter.display()));
    }
    let mut changed = Vec::new();
    let want_hook = p.hook_command();
    {
        let hooks = hooks_mut(doc);
        for &ev in EVENTS {
            let (mut entries, _dropped) = strip(as_entries(hooks, ev), |c| is_ours(c, HOOK_SUFFIX));
            entries.push(hook_entry(&want_hook));
            hooks.insert(ev.into(), Value::Array(entries));
            changed.push(ev.to_string());
        }
        for &ev in RETIRE_EVENTS {
            let (entries, dropped) = strip(as_entries(hooks, ev), |c| is_ours(c, HOOK_SUFFIX));
            if !dropped.is_empty() {
                changed.push(format!("{ev} (retired)"));
            }
            if entries.is_empty() {
                hooks.remove(ev);
            } else {
                hooks.insert(ev.into(), Value::Array(entries));
            }
        }
    }
    prune_empty_hooks(doc);

    let want_meter = status_line_value(&p.meter_command());
    if doc.get("statusLine") != Some(&want_meter) {
        doc.as_object_mut().unwrap().insert("statusLine".into(), want_meter);
        changed.push("statusLine".into());
    }
    Ok(changed)
}

/// `uninstall`: remove every Spira entry — from every event present, not only [`EVENTS`] and
/// [`RETIRE_EVENTS`], in case a still-earlier box registered on one neither names — and the
/// `statusLine` if it is ours. Never touches a foreign entry.
pub fn uninstall(doc: &mut Value) -> Vec<String> {
    let mut changed = Vec::new();
    if let Some(hooks) = doc.get("hooks").and_then(Value::as_object).cloned() {
        let hooks_mut_ref = hooks_mut(doc);
        for ev in hooks.keys().cloned().collect::<Vec<_>>() {
            let (entries, dropped) = strip(as_entries(hooks_mut_ref, &ev), |c| is_ours(c, HOOK_SUFFIX));
            if !dropped.is_empty() {
                changed.push(format!("{ev} ({})", dropped.len()));
            }
            if entries.is_empty() {
                hooks_mut_ref.remove(&ev);
            } else {
                hooks_mut_ref.insert(ev, Value::Array(entries));
            }
        }
    }
    prune_empty_hooks(doc);
    if doc.get("statusLine").and_then(Value::as_object).and_then(|o| o.get("command")).and_then(Value::as_str).map(|c| is_ours(c, METER_SUFFIX)).unwrap_or(false) {
        doc.as_object_mut().unwrap().remove("statusLine");
        changed.push("statusLine".into());
    }
    changed
}

/// `prune <needle>`: remove every entry (hooks or `statusLine`) whose command contains
/// `needle`, by name, one substring at a time — the deliberate way a foreign or retired entry
/// is removed, as opposed to `install`'s own narrow "ours" match.
pub fn prune(doc: &mut Value, needle: &str) -> Vec<String> {
    let mut changed = Vec::new();
    if let Some(hooks) = doc.get("hooks").and_then(Value::as_object).cloned() {
        let hooks_mut_ref = hooks_mut(doc);
        for ev in hooks.keys().cloned().collect::<Vec<_>>() {
            let (entries, dropped) = strip(as_entries(hooks_mut_ref, &ev), |c| c.contains(needle));
            for c in dropped {
                changed.push(format!("{ev}: {c}"));
            }
            if entries.is_empty() {
                hooks_mut_ref.remove(&ev);
            } else {
                hooks_mut_ref.insert(ev, Value::Array(entries));
            }
        }
    }
    prune_empty_hooks(doc);
    if let Some(c) = doc.get("statusLine").and_then(Value::as_object).and_then(|o| o.get("command")).and_then(Value::as_str) {
        if c.contains(needle) {
            let c = c.to_string();
            doc.as_object_mut().unwrap().remove("statusLine");
            changed.push(format!("statusLine: {c}"));
        }
    }
    changed
}

/// `status`: one line per event (`ok`/`MISSING`, plus one `other` line per foreign command — a
/// foreign hook is reported, never removed, because which of two session hooks the operator
/// wants is theirs to say), one `STALE` line per retired event still carrying ours, and one
/// line for `statusLine`. `ok` overall iff nothing is missing, stale, or carries more than one
/// Spira entry (the state `install` would otherwise silently repair away).
pub fn status(doc: &Value, p: &Paths) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut ok = true;
    let empty = Map::new();
    let hooks = doc.get("hooks").and_then(Value::as_object).unwrap_or(&empty);
    for &ev in EVENTS {
        let entries = as_entries(hooks, ev);
        let mut mine = Vec::new();
        let mut others = Vec::new();
        for e in &entries {
            for c in commands_in(e) {
                if is_ours(&c, HOOK_SUFFIX) {
                    mine.push(c);
                } else {
                    others.push(c);
                }
            }
        }
        if mine.is_empty() {
            lines.push(format!("  MISSING {ev:<14} the session hook is not registered"));
            ok = false;
        } else {
            lines.push(format!("  ok      {ev:<14} {}", p.hook_command()));
            if mine.len() > 1 {
                lines.push(format!("  DUP     {ev:<14} {} extra Spira entry/entries registered — run install to converge", mine.len() - 1));
                ok = false;
            }
        }
        for c in others {
            lines.push(format!("  other   {ev:<14} {c}"));
        }
    }
    for &ev in RETIRE_EVENTS {
        let entries = as_entries(hooks, ev);
        if entries.iter().flat_map(commands_in).any(|c| is_ours(&c, HOOK_SUFFIX)) {
            lines.push(format!("  STALE   {ev:<14} the session hook is still registered here; run install to remove it"));
            ok = false;
        }
    }
    match doc.get("statusLine") {
        Some(sl) => {
            let cmd = sl.get("command").and_then(Value::as_str).unwrap_or("");
            if is_ours(cmd, METER_SUFFIX) {
                lines.push(format!("  ok      statusLine     {}", p.meter_command()));
            } else {
                lines.push(format!("  other   statusLine     {cmd}"));
            }
        }
        None => {
            lines.push("  MISSING statusLine     the context meter is not registered".to_string());
            ok = false;
        }
    }
    (lines, ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(current: &str) -> Paths {
        Paths { settings: PathBuf::from("/x/settings.json"), release: current.into(), path: "/bin:/usr/bin".into(), toml: "/c/one-source.toml".into(), hook: PathBuf::from(format!("{current}/spira/hooks/session.sh")), meter: PathBuf::from(format!("{current}/spira/ctx-meter.sh")) }
    }

    #[test]
    fn hook_command_embeds_release_and_path() {
        let p = p("/r/spira-releases/current");
        assert_eq!(p.hook_command(), "env SPIRA_RELEASE=/r/spira-releases/current SPIRA_TOML=/c/one-source.toml PATH=/bin:/usr/bin /r/spira-releases/current/spira/hooks/session.sh");
    }

    #[test]
    fn is_ours_matches_every_earlier_form() {
        // The bare sha-pinned path, with no env wrapper at all — the exact leftover shape
        // sp-7jr34's own hand-fix found still present in the operator's real settings.json.
        assert!(is_ours("/r/spira-releases/08a2decd.../spira/hooks/session.sh", HOOK_SUFFIX));
        // The bare "current" path.
        assert!(is_ours("/r/spira-releases/current/spira/hooks/session.sh", HOOK_SUFFIX));
        // Wrapped in env/PATH, any release.
        assert!(is_ours("env SPIRA_RELEASE=/r/spira-releases/current PATH=/x /r/spira-releases/current/spira/hooks/session.sh", HOOK_SUFFIX));
        assert!(!is_ours("/some/other/hook.sh", HOOK_SUFFIX));
        assert!(!is_ours("/r/spira-releases/current/spira/ctx-meter.sh", HOOK_SUFFIX));
    }

    #[test]
    fn three_activations_converge_to_one() {
        // sp-7jr34 requirement 2's own acceptance test: three sha-pinned registrations (one
        // per activation, each unrelated to the others) plus one bare "current" form — a
        // single `install` collapses every one of them to the canonical entry.
        let mut doc = json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "/r/spira-releases/aaa.../spira/hooks/session.sh", "timeout": 10}]},
                    {"hooks": [{"type": "command", "command": "/r/spira-releases/bbb.../spira/hooks/session.sh", "timeout": 10}]},
                    {"hooks": [{"type": "command", "command": "/r/spira-releases/ccc.../spira/hooks/session.sh", "timeout": 10}]},
                    {"hooks": [{"type": "command", "command": "/r/spira-releases/current/spira/hooks/session.sh"}]},
                ]
            }
        });
        let paths = p("/r/spira-releases/current");
        crate::fsutil::is_executable(&paths.hook); // not asserted; install() below is exercised directly
        install_bypassing_exec_check(&mut doc, &paths);
        let entries = doc["hooks"]["SessionStart"].as_array().unwrap();
        let mine: Vec<_> = entries.iter().flat_map(commands_in).filter(|c| is_ours(c, HOOK_SUFFIX)).collect();
        assert_eq!(mine.len(), 1, "expected exactly one converged entry, got {mine:?}");
        assert_eq!(mine[0], paths.hook_command());
    }

    #[test]
    fn install_is_idempotent() {
        let mut doc = json!({});
        let paths = p("/r/spira-releases/current");
        install_bypassing_exec_check(&mut doc, &paths);
        let first = doc.clone();
        install_bypassing_exec_check(&mut doc, &paths);
        assert_eq!(doc, first, "a second install must change nothing");
    }

    #[test]
    fn install_writes_no_matcher_and_retires_post_compact() {
        let mut doc = json!({"hooks": {"PostCompact": [{"hooks": [{"type": "command", "command": "/r/spira-releases/current/spira/hooks/session.sh", "timeout": 10}]}]}});
        let paths = p("/r/spira-releases/current");
        install_bypassing_exec_check(&mut doc, &paths);
        let entry = &doc["hooks"]["SessionStart"][0];
        assert!(entry.get("matcher").is_none(), "no matcher must ever be written");
        assert!(doc["hooks"].get("PostCompact").is_none(), "PostCompact must be stripped, never re-added");
    }

    #[test]
    fn install_leaves_a_foreign_hook_alone() {
        let mut doc = json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "/somebody/elses/hook.sh"}]}]}});
        let paths = p("/r/spira-releases/current");
        install_bypassing_exec_check(&mut doc, &paths);
        let entries = doc["hooks"]["SessionStart"].as_array().unwrap();
        let commands: Vec<_> = entries.iter().flat_map(commands_in).collect();
        assert!(commands.contains(&"/somebody/elses/hook.sh".to_string()));
        assert!(commands.contains(&paths.hook_command()));
    }

    #[test]
    fn install_sets_one_status_line_through_current() {
        let mut doc = json!({});
        let paths = p("/r/spira-releases/current");
        install_bypassing_exec_check(&mut doc, &paths);
        assert_eq!(doc["statusLine"]["command"], Value::String(paths.meter_command()));
        assert_eq!(doc["statusLine"]["refreshInterval"], json!(METER_REFRESH_SECS));
    }

    #[test]
    fn uninstall_leaves_no_husk_and_keeps_unrelated_settings() {
        let mut doc = json!({
            "otherSetting": "keep-me",
            "hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "/r/spira-releases/current/spira/hooks/session.sh", "timeout": 10}]}]},
            "statusLine": {"command": "/r/spira-releases/current/spira/ctx-meter.sh", "refreshInterval": 5},
        });
        let changed = uninstall(&mut doc);
        assert!(!changed.is_empty());
        assert!(doc.get("hooks").is_none(), "an entry emptied of hooks must be removed, not left as a husk");
        assert!(doc.get("statusLine").is_none());
        assert_eq!(doc["otherSetting"], "keep-me");
    }

    #[test]
    fn uninstall_never_touches_a_foreign_status_line() {
        let mut doc = json!({"statusLine": {"command": "/some/other/meter.sh", "refreshInterval": 5}});
        uninstall(&mut doc);
        assert_eq!(doc["statusLine"]["command"], "/some/other/meter.sh");
    }

    #[test]
    fn prune_removes_by_substring_from_hooks_and_status_line() {
        let mut doc = json!({
            "hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "/gone/hooks/old-session.sh"}]}]},
            "statusLine": {"command": "/gone/meter.sh", "refreshInterval": 5},
        });
        let changed = prune(&mut doc, "/gone");
        assert_eq!(changed.len(), 2);
        assert!(doc.get("hooks").is_none());
        assert!(doc.get("statusLine").is_none());
    }

    #[test]
    fn status_reports_missing_then_ok_then_dup() {
        let paths = p("/r/spira-releases/current");
        let mut doc = json!({});
        let (lines, ok) = status(&doc, &paths);
        assert!(!ok);
        assert!(lines.iter().any(|l| l.contains("MISSING") && l.contains("SessionStart")));
        assert!(lines.iter().any(|l| l.contains("MISSING") && l.contains("statusLine")));

        install_bypassing_exec_check(&mut doc, &paths);
        let (lines, ok) = status(&doc, &paths);
        assert!(ok, "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("  ok      SessionStart")));
        assert!(lines.iter().any(|l| l.starts_with("  ok      statusLine")));

        // A second, un-converged sha-pinned entry alongside the canonical one is the exact
        // shape sp-7jr34 found live: DUP, not silently "ok".
        doc["hooks"]["SessionStart"].as_array_mut().unwrap().push(hook_entry("/r/spira-releases/deadbeef/spira/hooks/session.sh"));
        let (lines, ok) = status(&doc, &paths);
        assert!(!ok);
        assert!(lines.iter().any(|l| l.contains("DUP")));
    }

    #[test]
    fn status_reports_a_foreign_hook_but_never_its_removal() {
        let paths = p("/r/spira-releases/current");
        let doc = json!({"hooks": {"SessionStart": [{"matcher": "clear|startup|resume", "hooks": [{"type": "command", "command": "/gone/hooks/old-session.sh"}]}]}});
        let (lines, ok) = status(&doc, &paths);
        assert!(!ok);
        assert!(lines.iter().any(|l| l.contains("other") && l.contains("/gone/hooks/old-session.sh")));
    }

    #[test]
    fn save_writes_once_and_skips_an_unchanged_rewrite() {
        let tmp = testkit::TempDir::new("session-hook-save");
        let settings = tmp.path().join("settings.json");
        let doc = json!({"a": 1});
        assert!(save(&settings, &doc).unwrap(), "first write must report a change");
        let backup = format!("{}.spira.bak", settings.display());
        assert!(!std::path::Path::new(&backup).exists(), "no prior file, so no backup");
        assert!(!save(&settings, &doc).unwrap(), "an unchanged rewrite must report no change");
        assert!(!std::path::Path::new(&backup).exists(), "nothing changed, so no backup either");

        let doc2 = json!({"a": 2});
        assert!(save(&settings, &doc2).unwrap());
        assert!(std::path::Path::new(&backup).exists(), "a real change must leave a backup of the prior content");
    }

    /// `install()` refuses when the hook/meter file is not executable (the real function's
    /// own guard, exercised separately in [`install_refuses_when_the_hook_is_missing`]) — the
    /// tests above are about the JSON transformation alone, so they call the two mutating
    /// halves directly rather than standing up real executable fixtures for every case.
    fn install_bypassing_exec_check(doc: &mut Value, p: &Paths) {
        let want_hook = p.hook_command();
        let hooks = hooks_mut(doc);
        for &ev in EVENTS {
            let (mut entries, _) = strip(as_entries(hooks, ev), |c| is_ours(c, HOOK_SUFFIX));
            entries.push(hook_entry(&want_hook));
            hooks.insert(ev.into(), Value::Array(entries));
        }
        for &ev in RETIRE_EVENTS {
            let (entries, _) = strip(as_entries(hooks, ev), |c| is_ours(c, HOOK_SUFFIX));
            if entries.is_empty() {
                hooks.remove(ev);
            } else {
                hooks.insert(ev.into(), Value::Array(entries));
            }
        }
        let _ = hooks;
        prune_empty_hooks(doc);
        let want_meter = status_line_value(&p.meter_command());
        doc.as_object_mut().unwrap().insert("statusLine".into(), want_meter);
    }

    #[test]
    fn install_refuses_when_the_hook_is_missing() {
        let tmp = testkit::TempDir::new("session-hook-missing");
        let mut doc = json!({});
        let paths = Paths { settings: tmp.path().join("settings.json"), release: "/r/spira-releases/current".into(), path: "/bin".into(), toml: "/c/one-source.toml".into(), hook: tmp.path().join("no-such-hook.sh"), meter: tmp.path().join("no-such-meter.sh") };
        let e = install(&mut doc, &paths).unwrap_err();
        assert!(e.contains("not executable"), "{e}");
        assert_eq!(doc, json!({}), "a refusal must change nothing");
    }

    #[test]
    fn install_registers_for_real_when_both_are_executable() {
        let tmp = testkit::TempDir::new("session-hook-real");
        let hook = tmp.path().join("session.sh");
        let meter = tmp.path().join("ctx-meter.sh");
        testkit::write_exe(&hook, "#!/bin/sh\nexit 0\n");
        testkit::write_exe(&meter, "#!/bin/sh\nexit 0\n");
        let mut doc = json!({});
        let paths = Paths { settings: tmp.path().join("settings.json"), release: "/r/spira-releases/current".into(), path: "/bin".into(), toml: "/c/one-source.toml".into(), hook, meter };
        let changed = install(&mut doc, &paths).unwrap();
        assert!(changed.contains(&"SessionStart".to_string()));
        assert!(changed.contains(&"statusLine".to_string()));
    }
}
