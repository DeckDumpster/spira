//! `release activate`, `release rollback`, `release status`, and the hotfix rule
//! (DESIGN.md "activate", "Hotfix", "rollback").

use crate::config::Config;
use crate::config_delta::{self, Txn};
use crate::fsutil;
use crate::git::Git;
use crate::systemctl::Systemctl;
use crate::units;
use crate::verify;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const CURRENT: &str = "current";
const HISTORY: &str = "history";
const HOTFIX: &str = "hotfix";
/// History is a rollback stack, not an audit log: the oldest entries go past this depth.
const HISTORY_DEPTH: usize = 50;

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub sc: &'a dyn Systemctl,
    pub git: &'a dyn Git,
    /// The repository the hotfix rule asks about ancestry in.
    pub repo: Option<PathBuf>,
    /// The ref a hotfix commit must be contained in to count as landed.
    pub landed_ref: String,
    /// How long restarted units get to come up before they are checked.
    pub settle: Duration,
    /// How long a running oneshot whose unit changed gets to finish its in-flight work and
    /// exit before activation stops waiting for it.
    pub drain: Duration,
}

/// A standing hotfix: the running system is on a commit that has not landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotfix {
    pub sha: String,
    pub reason: String,
    pub at: String,
}

/// One activation on the rollback stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistEntry {
    pub sha: String,
    /// `Some(reason)` when it was activated as a hotfix.
    pub hotfix: Option<String>,
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn read_hotfix(state: &Path) -> Result<Option<Hotfix>, String> {
    let p = state.join(HOTFIX);
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", p.display())),
    };
    let mut h = Hotfix { sha: String::new(), reason: String::new(), at: String::new() };
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("sha", v)) => h.sha = v.to_string(),
            Some(("reason", v)) => h.reason = v.to_string(),
            Some(("at", v)) => h.at = v.to_string(),
            _ => {}
        }
    }
    if !crate::is_sha(&h.sha) {
        return Err(format!("{} names no hotfix sha; refusing to guess whether one stands", p.display()));
    }
    Ok(Some(h))
}

fn write_hotfix(state: &Path, h: Option<&Hotfix>) -> Result<(), String> {
    let p = state.join(HOTFIX);
    match h {
        None => match fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot clear {}: {e}", p.display())),
        },
        Some(h) => fsutil::write_atomic(&p, &format!("sha {}\nreason {}\nat {}\n", h.sha, one_line(&h.reason), h.at)),
    }
}

pub fn read_history(state: &Path) -> Result<Vec<HistEntry>, String> {
    let p = state.join(HISTORY);
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", p.display())),
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, ' ');
        let sha = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("");
        let reason = parts.next().unwrap_or("");
        let hotfix = match kind {
            "landed" => None,
            "hotfix" => Some(reason.to_string()),
            _ => return Err(format!("{} line {}: cannot parse {line:?}", p.display(), i + 1)),
        };
        if !crate::is_sha(sha) {
            return Err(format!("{} line {}: {sha:?} is not a sha", p.display(), i + 1));
        }
        out.push(HistEntry { sha: sha.to_string(), hotfix });
    }
    Ok(out)
}

fn write_history(state: &Path, h: &[HistEntry]) -> Result<(), String> {
    let start = h.len().saturating_sub(HISTORY_DEPTH);
    let text: String = h[start..]
        .iter()
        .map(|e| match &e.hotfix {
            None => format!("{} landed\n", e.sha),
            Some(r) => format!("{} hotfix {}\n", e.sha, one_line(r)),
        })
        .collect();
    fsutil::write_atomic(&state.join(HISTORY), &text)
}

/// The release `current` names, if any.
pub fn current(cfg: &Config) -> Option<String> {
    fs::read_link(cfg.releases.join(CURRENT)).ok().map(|t| t.to_string_lossy().trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string())
}

/// What the hotfix record should become after an activation.
#[derive(Debug, PartialEq, Eq)]
pub enum HotfixAfter {
    /// No hotfix before, none after.
    Unchanged,
    /// This activation is a hotfix; record it (replacing any standing one).
    Record(Hotfix),
    /// A standing hotfix is superseded: its commit has landed and is in the new release.
    Superseded(Hotfix),
}

/// THE supersede rule (DESIGN.md "Hotfix"). A refusal is `Err`, and nothing has changed.
pub fn hotfix_rule(ctx: &Ctx, standing: Option<Hotfix>, sha: &str, hotfix_reason: Option<&str>) -> Result<HotfixAfter, String> {
    if let Some(reason) = hotfix_reason {
        if one_line(reason).is_empty() {
            return Err("--hotfix needs a reason: it is what doctor and the ops pane show while it runs".into());
        }
        return Ok(HotfixAfter::Record(Hotfix { sha: sha.to_string(), reason: one_line(reason), at: fsutil::now_rfc3339() }));
    }
    let Some(h) = standing else { return Ok(HotfixAfter::Unchanged) };
    let refuse = |why: String| {
        Err(format!(
            "refusing to activate {sha} over RUNNING UNLANDED {}: {} — {why}. Land the fix or `release rollback` first.",
            h.sha, h.reason
        ))
    };
    let Some(repo) = &ctx.repo else {
        return refuse("no --repo to check whether it has landed".into());
    };
    let in_new = ctx.git.is_ancestor(repo, &h.sha, sha)?;
    if !in_new {
        return refuse(format!("{sha} does not contain it"));
    }
    let landed = ctx.git.is_ancestor(repo, &h.sha, &ctx.landed_ref)?;
    if !landed {
        return refuse(format!("{} does not contain it", ctx.landed_ref));
    }
    Ok(HotfixAfter::Superseded(h))
}

/// A unit file this switch rewrote.
struct Change {
    unit: String,
    path: PathBuf,
    old: String,
    new: String,
}

/// What a successful switch did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Switched {
    pub rewritten: Vec<String>,
    pub restarted: Vec<String>,
    /// Services whose Exec lines changed but were not restarted (inactive, or a oneshot
    /// mid-run): they pick up the new file on their next start.
    pub deferred: Vec<String>,
    /// An already-installed unit whose template's gate (`units::gate_open`) is now closed —
    /// disabled, stopped and removed rather than re-rendered (sp-xtdqi-2): the same thing
    /// `install`'s manifest does for a template it declines, applied to a copy that was
    /// already on disk before the gate existed. Best-effort and never rolled back: a
    /// disable/stop/remove failure here is printed to stderr (named, not silent) but never
    /// fails the activation or undoes an otherwise-successful one — the box not wanting this
    /// unit is a fact about its config, not about whether this activation's own restarts
    /// came up.
    pub retired: Vec<String>,
}

fn unit_files(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default()
}

fn is_up(active: &str) -> bool {
    matches!(active, "active" | "activating" | "reloading")
}

/// Switch the running system onto release `sha`: render and install units, swap
/// `current`, daemon-reload, restart what changed, check it came up, and undo all of it
/// when something did not.
pub fn switch(ctx: &Ctx, sha: &str, install_new: bool) -> Result<Switched, String> {
    let cfg = ctx.cfg;
    let rel = verify::release_dir(cfg, sha)?;
    let problems = verify::check_files(&rel, sha);
    if !problems.is_empty() {
        return Err(format!("release {sha} does not match its MANIFEST; not activating:\n  {}", problems.join("\n  ")));
    }
    let instance = cfg.instance();
    let host = cfg.host_values()?;
    let mut changes = Vec::new();
    let mut errors = Vec::new();
    let mut retiring = Vec::new();
    for m in units::installed(&cfg.unit_dir, &rel, &instance)? {
        // sp-xtdqi-2: a template whose gate has closed (its installing key is unset) is
        // never rendered — not even to discover it is unchanged. The unit was installed
        // before the gate existed (a hand-render, or an earlier release that had no gate at
        // all); the new release's own answer is that it should not exist, the same answer
        // `install`'s manifest already gives a fresh box.
        if !units::gate_open(&m.template, &host) {
            retiring.push(m);
            continue;
        }
        let tp = rel.join("systemd").join(&m.template);
        let text = fs::read_to_string(&tp).map_err(|e| format!("cannot read {}: {e}", tp.display()))?;
        let new = match units::render(&m.template, &text, &rel, &host, m.watcher.as_deref(), &instance) {
            Ok(n) => n,
            Err(e) => {
                errors.push(format!("{}: {e}", m.installed));
                continue;
            }
        };
        let path = cfg.unit_dir.join(&m.installed);
        let old = fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if units::normalize(&old) != new {
            changes.push(Change { unit: m.installed, path, old, new });
        }
    }
    if !errors.is_empty() {
        return Err(format!("cannot render units against {sha}; nothing changed:\n  {}", errors.join("\n  ")));
    }

    let cur_link = cfg.releases.join(CURRENT);
    let prev = fs::read_link(&cur_link).ok().map(|t| t.to_string_lossy().to_string());

    for (written, c) in changes.iter().enumerate() {
        if let Err(e) = fsutil::write_atomic(&c.path, &c.new) {
            restore_files(&changes[..written]);
            return Err(e);
        }
    }
    if let Err(e) = fsutil::atomic_symlink(&cur_link, sha) {
        restore_files(&changes);
        return Err(e);
    }
    eprintln!("release: current -> {sha} ({} unit file(s) rewritten)", changes.len());
    let mut retired = Vec::new();
    for m in &retiring {
        if let Err(e) = ctx.sc.disable_now(&m.installed) {
            eprintln!("release: {} is gated off by {sha} but could not be disabled: {e}", m.installed);
            continue;
        }
        match fs::remove_file(cfg.unit_dir.join(&m.installed)) {
            Ok(()) => {
                eprintln!("release: retired {} (its gate key is unset)", m.installed);
                retired.push(m.installed.clone());
            }
            Err(e) => eprintln!("release: disabled {} but could not remove its file: {e}", m.installed),
        }
    }
    if let Err(e) = ctx.sc.daemon_reload() {
        let undo = undo(ctx, &changes, prev.as_deref(), &[]);
        return Err(format!("daemon-reload failed: {e}; rolled back{undo}"));
    }

    if install_new {
        let before = unit_files(&cfg.unit_dir);
        if let Err(e) = ensure_new_units(cfg, sha) {
            let added: Vec<String> = unit_files(&cfg.unit_dir).difference(&before).cloned().collect();
            let undo = undo(ctx, &changes, prev.as_deref(), &[]);
            let shipped = if added.is_empty() { String::new() } else { format!(" (units new in {sha}: {})", added.join(", ")) };
            return Err(format!("{e}{shipped}; nothing was restarted; rolled back{undo}"));
        }
    }

    let mut out = Switched { rewritten: changes.iter().map(|c| c.unit.clone()).collect(), retired, ..Default::default() };
    let mut draining = Vec::new();
    for c in &changes {
        if !c.unit.ends_with(".service") || !units::exec_changed(&c.old, &c.new) {
            continue;
        }
        match ctx.sc.state(&c.unit) {
            Ok(st) if is_up(&st.active) && st.kind != "oneshot" => out.restarted.push(c.unit.clone()),
            Ok(st) => {
                if st.kind == "oneshot" && is_up(&st.active) {
                    draining.push(c.unit.clone());
                }
                out.deferred.push(c.unit.clone());
            }
            Err(e) => {
                let undo = undo(ctx, &changes, prev.as_deref(), &[]);
                return Err(format!("cannot read the state of {}: {e}; rolled back{undo}", c.unit));
            }
        }
    }
    let mut failed = Vec::new();
    let mut attempted = Vec::new();
    for u in &out.restarted {
        eprintln!("release: restarting {u}");
        attempted.push(u.clone());
        if let Err(e) = ctx.sc.restart(u) {
            failed.push(format!("{u}: restart failed: {e}"));
            break;
        }
    }
    if failed.is_empty() && !out.restarted.is_empty() {
        if !ctx.settle.is_zero() {
            std::thread::sleep(ctx.settle);
        }
        for u in &out.restarted {
            match ctx.sc.state(u) {
                Ok(st) if st.active == "active" => {}
                Ok(st) => failed.push(format!("{u}: did not come up (ActiveState={}, Result={})", st.active, st.result)),
                Err(e) => failed.push(format!("{u}: cannot read its state: {e}")),
            }
        }
    }
    if !failed.is_empty() {
        let undo = undo(ctx, &changes, prev.as_deref(), &attempted);
        return Err(format!(
            "activating {sha} failed, rolled back to {}:\n  {}{undo}",
            prev.as_deref().unwrap_or("no current release"),
            failed.join("\n  ")
        ));
    }
    drain(ctx, &draining);
    Ok(out)
}

/// Wait for each running oneshot of a changed unit to exit, so its next start runs the new
/// release. The worker's own release check ends its pass between jobs; this only bounds how
/// long the deploy waits for that, and a unit still running at the bound is named, not fatal.
fn drain(ctx: &Ctx, units: &[String]) {
    let deadline = std::time::Instant::now() + ctx.drain;
    let mut next_note = std::time::Instant::now() + Duration::from_secs(30);
    for u in units {
        eprintln!("release: waiting for {u} to finish its pass on the previous release");
        loop {
            match ctx.sc.state(u) {
                Ok(st) if is_up(&st.active) => {}
                _ => break,
            }
            let now = std::time::Instant::now();
            if now >= next_note {
                eprintln!("release: still waiting for {u}; {}s of the {}s drain wait left", deadline.saturating_duration_since(now).as_secs(), ctx.drain.as_secs());
                next_note = now + Duration::from_secs(30);
            }
            if now >= deadline {
                eprintln!("release: {u} still running its previous release after {}s; continuing", ctx.drain.as_secs());
                break;
            }
            std::thread::sleep((ctx.drain / 20).clamp(Duration::from_millis(10), Duration::from_secs(1)));
        }
    }
}

fn restore_files(changes: &[Change]) -> Vec<String> {
    let mut errs = Vec::new();
    for c in changes {
        if let Err(e) = fsutil::write_atomic(&c.path, &c.old) {
            errs.push(e);
        }
    }
    errs
}

/// Put everything back: unit files, `current`, daemon-reload, and restart what was
/// restarted so it runs the old release again. Returns "" or the problems undoing hit.
fn undo(ctx: &Ctx, changes: &[Change], prev: Option<&str>, restarted: &[String]) -> String {
    let mut errs = restore_files(changes);
    let cur_link = ctx.cfg.releases.join(CURRENT);
    match prev {
        Some(p) => {
            if let Err(e) = fsutil::atomic_symlink(&cur_link, p) {
                errs.push(e);
            }
        }
        None => {
            if let Err(e) = fs::remove_file(&cur_link) {
                errs.push(format!("cannot remove {}: {e}", cur_link.display()));
            }
        }
    }
    if let Err(e) = ctx.sc.daemon_reload() {
        errs.push(format!("daemon-reload: {e}"));
    }
    for u in restarted {
        if let Err(e) = ctx.sc.restart(u) {
            errs.push(format!("restart {u} on the previous release: {e}"));
        }
    }
    if errs.is_empty() {
        String::new()
    } else {
        format!("\n  UNDO INCOMPLETE:\n    {}", errs.join("\n    "))
    }
}

/// Switch onto `sha` with the config changes `delta` carries: validated by `sha`'s own
/// `spira-config` before anything changes, added keys written before the flip, dropped keys
/// removed after it, and the config put back when the switch fails. `Ok` carries the undo
/// record and any problem dropping keys hit after the flip (the switch itself stood).
fn switch_with_config(ctx: &Ctx, sha: &str, txn: Option<&Txn>, install_new: bool) -> Result<(Switched, Option<String>), String> {
    if let Some(t) = txn {
        t.apply_pre()?;
    }
    let out = match switch(ctx, sha, install_new) {
        Ok(o) => o,
        Err(e) => {
            let undo = txn.map(Txn::restore).unwrap_or_default();
            return Err(if undo.is_empty() { e } else { format!("{e}\n  CONFIG RESTORE INCOMPLETE:\n    {}", undo.join("\n    ")) });
        }
    };
    let late = txn.and_then(|t| t.apply_post().err()).map(|e| format!("{sha} is active but the config keys it drops could not be removed: {e}"));
    Ok((out, late))
}

/// Install and enable the units the release at `sha` ships that the box does not have yet,
/// through the release's own `unit-ensure` (which owns the manifest: gates, enable flags,
/// watchers). `switch` only rewrites units already on disk. A release with no `unit-ensure`
/// has nothing to install them with and is skipped.
fn ensure_new_units(cfg: &Config, sha: &str) -> Result<(), String> {
    let rel = verify::release_dir(cfg, sha)?;
    let bin = rel.join("bin/unit-ensure");
    if !fsutil::is_executable(&bin) {
        return Ok(());
    }
    // batch-job: installs and starts whatever units are new, bounded at 120 s by timeout(1)
    let mut cmd = Command::new("timeout");
    cmd.arg("120").arg(&bin).env("SPIRA_HOME", rel.join("spira")).env_remove("SPIRA_REPO");
    for (k, v) in verify::pre_activate_env(cfg, &rel)? {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    for l in String::from_utf8_lossy(&out.stdout).lines().chain(String::from_utf8_lossy(&out.stderr).lines()) {
        eprintln!("release: {l}");
    }
    if out.status.success() {
        return Ok(());
    }
    Err(format!("{sha}: unit-ensure could not install its new units ({})", out.status))
}

/// `release activate <sha> [--hotfix <reason>]`.
pub fn activate(ctx: &Ctx, sha: &str, hotfix_reason: Option<&str>) -> Result<Switched, String> {
    let state = ctx.cfg.state_dir()?;
    fs::create_dir_all(&state).map_err(|e| format!("cannot create {}: {e}", state.display()))?;
    let rel = verify::release_dir(ctx.cfg, sha)?;
    let standing = read_hotfix(&state)?;
    let after = hotfix_rule(ctx, standing, sha, hotfix_reason)?;
    let mut history = read_history(&state)?;
    let delta = config_delta::load(&rel)?;
    let txn = delta.as_ref().map(|d| config_delta::prepare(ctx.cfg, &rel, d)).transpose()?;
    let (out, late) = switch_with_config(ctx, sha, txn.as_ref(), true)?;
    if let Some(t) = &txn {
        config_delta::save_undo(&state, sha, &t.undo)?;
    }
    let entry = HistEntry { sha: sha.to_string(), hotfix: hotfix_reason.map(one_line) };
    if history.last() != Some(&entry) {
        history.push(entry);
    }
    write_history(&state, &history)?;
    match after {
        HotfixAfter::Unchanged => {}
        HotfixAfter::Record(h) => {
            eprintln!("release: RUNNING UNLANDED {}: {}", h.sha, h.reason);
            write_hotfix(&state, Some(&h))?;
        }
        HotfixAfter::Superseded(h) => {
            eprintln!("release: hotfix {} has landed and is in {sha}; superseded", h.sha);
            write_hotfix(&state, None)?;
        }
    }
    if let Some(e) = late {
        return Err(e);
    }
    Ok(out)
}

/// `release rollback`: activate the release below the top of the history stack.
pub fn rollback(ctx: &Ctx) -> Result<String, String> {
    let state = ctx.cfg.state_dir()?;
    let mut history = read_history(&state)?;
    let Some(top) = history.last().cloned() else {
        return Err("no activation history; nothing to roll back".into());
    };
    let cur = current(ctx.cfg);
    if cur.as_deref() != Some(top.sha.as_str()) {
        return Err(format!(
            "current is {} but the last activation recorded is {}; the record and the system disagree, refusing to guess",
            cur.as_deref().unwrap_or("unset"),
            top.sha
        ));
    }
    if history.len() < 2 {
        return Err(format!("{} is the only release ever activated; nothing to roll back to", top.sha));
    }
    let prev = history[history.len() - 2].clone();
    let undo = config_delta::load_undo(&state, &top.sha)?;
    let txn = match &undo {
        Some(d) => {
            let rel = verify::release_dir(ctx.cfg, &prev.sha)?;
            Some(config_delta::prepare(ctx.cfg, &rel, d)?)
        }
        None => None,
    };
    let (_, late) = switch_with_config(ctx, &prev.sha, txn.as_ref(), false)?;
    config_delta::clear_undo(&state, &top.sha);
    history.pop();
    write_history(&state, &history)?;
    let rec = prev.hotfix.as_ref().map(|r| Hotfix { sha: prev.sha.clone(), reason: r.clone(), at: fsutil::now_rfc3339() });
    write_hotfix(&state, rec.as_ref())?;
    match late {
        Some(e) => Err(e),
        None => Ok(prev.sha),
    }
}

/// `release status`. Reads the hotfix record itself, never re-derived elsewhere: doctor,
/// the ops pane and watchtower all key off THIS text (`RUNNING UNLANDED` / `ALERT`) instead
/// of reading `$SPIRA_RUN/release/hotfix` or computing an age of their own (sp-6p20x).
pub fn status(cfg: &Config) -> Result<String, String> {
    let mut s = format!("current {}\n", current(cfg).unwrap_or_else(|| "none".into()));
    if let Ok(state) = cfg.state_dir() {
        let h = read_history(&state)?;
        if h.len() >= 2 {
            s.push_str(&format!("previous {}\n", h[h.len() - 2].sha));
        }
        if let Some(hf) = read_hotfix(&state)? {
            s.push_str(&format!("RUNNING UNLANDED {}: {} (since {})\n", hf.sha, hf.reason, hf.at));
            // ALERT once the hotfix has stood at least `hotfix_alert_hours` (default 4h,
            // DESIGN.md "Hotfix: visibility"). A `since` this crate itself did not write
            // (corrupt or hand-edited) never alerts silently-as-zero: it is simply not aged.
            if let Some(started) = fsutil::parse_rfc3339(&hf.at) {
                let threshold = cfg.hotfix_alert_hours()?;
                let age_hours = fsutil::now_secs().saturating_sub(started) / 3600;
                if age_hours >= threshold {
                    s.push_str(&format!("ALERT hotfix {} standing {age_hours}h >= threshold {threshold}h\n", hf.sha));
                }
            }
        }
    }
    Ok(s)
}
