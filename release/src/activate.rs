//! `release activate`, `release rollback`, `release status`, and the hotfix rule
//! (DESIGN.md "activate", "Hotfix", "rollback").

use crate::broker;
use crate::config::Config;
use crate::config_delta::{self, Txn};
use crate::fsutil;
use crate::git::Git;
use crate::machine::{self, ReleaseState};
use crate::spira_env;
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

/// The reads activation makes through the new release's own `spira-lc`, once its units are up.
pub trait Reads {
    /// `spira-lc <args>` from the release at `rel`: its stdout, or the refusal in its words.
    fn lc(&self, rel: &Path, args: &[&str]) -> Result<String, String>;
}

/// Runs the release's own `spira-lc` under the environment its services get.
pub struct RealReads<'a>(pub &'a Config);

impl Reads for RealReads<'_> {
    fn lc(&self, rel: &Path, args: &[&str]) -> Result<String, String> {
        let bin = rel.join("bin/spira-lc");
        // batch-job: one bounded read through the new release; a hung store is a refusal
        let mut cmd = Command::new("timeout");
        cmd.arg("20").arg(&bin).args(args).env_remove("SPIRA_REPO");
        for (k, v) in verify::pre_activate_env(self.0, rel)? {
            cmd.env(k, v);
        }
        let out = cmd.output().map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
        let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
        if out.status.success() {
            return Ok(text(&out.stdout));
        }
        let said = [text(&out.stderr), text(&out.stdout)].join(" ");
        Err(format!("{} ({}): {}", args.join(" "), out.status, one_line(&said)))
    }
}

/// One lifecycle read and one content read through the release that was just started: a
/// release whose store layout or client cannot answer them serves no caller. An empty
/// listing passes; there is nothing to show.
fn serves_reads(reads: &dyn Reads, rel: &Path) -> Result<(), String> {
    let listed = reads.lc(rel, &["list", "--state", "READY"]).map_err(|e| format!("lifecycle read refused: spira-lc {e}"))?;
    eprintln!("release: read through {}: spira-lc list --state READY", rel.display());
    let rows: Vec<serde_json::Value> = serde_json::from_str(&listed).map_err(|e| format!("lifecycle read unreadable: spira-lc list --state READY: {e}"))?;
    let Some(id) = rows.first().and_then(|r| r["bead_id"].as_str()) else { return Ok(()) };
    reads.lc(rel, &["content", "show", id]).map_err(|e| format!("content read refused: spira-lc {e}"))?;
    eprintln!("release: read through {}: spira-lc content show {id}", rel.display());
    Ok(())
}

/// The reads watchers make first after a store server restart, each of which must answer inside
/// `Ctx::steady` before activation is done: the server's first reads pay for its cold build.
const WARM_READS: [&[&str]; 2] = [&["dwell"], &["list", "--state", "READY"]];

/// The unit that serves the control store.
const STORE_SERVER: &str = "lc-serve.service";

/// Asks each of `WARM_READS` until it answers inside `steady`, up to `wait` in all. A server that
/// stays slow is reported, not rolled back: the release served its reads, and another restart
/// would only make it cold again. `restarted` is when the store's restart was issued, if it was.
fn warmed(ctx: &Ctx, rel: &Path, restarted: Option<std::time::Instant>) -> Result<(), String> {
    let give_up = std::time::Instant::now() + ctx.warm_wait;
    for (i, args) in WARM_READS.into_iter().enumerate() {
        loop {
            let began = std::time::Instant::now();
            let said = ctx.reads.lc(rel, args);
            let took = began.elapsed();
            if said.is_ok() && took <= ctx.steady {
                eprintln!("release: warm read through {}: spira-lc {} in {} ms", rel.display(), args.join(" "), took.as_millis());
                if let (0, Some(at)) = (i, restarted) {
                    (ctx.report)(&format!("release: the store answered {} ms after its restart (bound {} s)", at.elapsed().as_millis(), ctx.warm_wait.as_secs()));
                }
                break;
            }
            if std::time::Instant::now() >= give_up {
                if let (0, Some(_)) = (i, restarted) {
                    (ctx.report)(&format!("release: WARN: the store did not answer inside {} s of its restart", ctx.warm_wait.as_secs()));
                }
                return Err(format!("spira-lc {} still not answering inside {} ms after {} s ({})", args.join(" "), ctx.steady.as_millis(), ctx.warm_wait.as_secs(), said.err().unwrap_or_else(|| format!("took {} ms", took.as_millis()))));
            }
            std::thread::sleep(ctx.warm_pause);
        }
    }
    Ok(())
}

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub reads: &'a dyn Reads,
    pub sc: &'a dyn Systemctl,
    pub nix: &'a dyn spira_env::Nix,
    pub git: &'a dyn Git,
    /// The repository the hotfix rule asks about ancestry in.
    pub repo: Option<PathBuf>,
    /// The ref a hotfix commit must be contained in to count as landed.
    pub landed_ref: String,
    /// The cap on waiting for restarted units to reach `active` (or `failed`).
    pub settle: Duration,
    /// How long a running oneshot whose unit changed gets to finish its in-flight work and
    /// exit before activation stops waiting for it.
    pub drain: Duration,
    /// The steady read limit a warm-up read must answer inside.
    pub steady: Duration,
    /// The cap on waiting for the store server's reads to answer inside `steady`.
    pub warm_wait: Duration,
    /// The pause between warm-up attempts.
    pub warm_pause: Duration,
    /// Where the lines that state a measurement go.
    pub report: &'a dyn Fn(&str),
    /// Collects the envs a switch supersedes; `None` collects nothing.
    pub gc: Option<spira_env::Collector<'a>>,
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
    /// When the store server's restart was issued; `None` when it was not restarted.
    pub store_restarted_at: Option<std::time::Instant>,
    pub rewritten: Vec<String>,
    pub restarted: Vec<String>,
    /// Running services whose unit text and programs match the outgoing release's: untouched.
    pub left_running: Vec<String>,
    /// Installed services found failed that this activation did not restart: reset and
    /// started. One that fails again is reported, never a reason to roll back.
    pub revived: Vec<String>,
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
    /// Running services left on the previous release's code, each with why (sp-m9rruh): only
    /// one whose template gives a reason for never being restarted by activation
    /// (`X-Spira-NoRestart=<reason>`), or whose restart failed after the drain wait ran out.
    /// Never silent: `release activate` names every one as STALE.
    pub stale: Vec<String>,
    /// Running services on the previous release's code whose restart would cut in-flight work,
    /// each with why: they restart themselves at their next idle point. Reported, not STALE.
    pub when_idle: Vec<String>,
    /// Units this activation started because it changed a path they list
    /// (`X-Spira-StartWhenChanged`), each with why. A start that failed is reported to stderr
    /// and never undoes the activation.
    pub started: Vec<String>,
    /// A rollback only: units still not up after their second restart under the target. The
    /// target stays current; the caller names each one rather than undoing onto the release
    /// being left.
    pub down: Vec<Down>,
}

/// A unit a rollback could not bring up, and how it stood.
#[derive(Debug, PartialEq, Eq)]
pub struct Down {
    pub unit: String,
    pub state: String,
}

fn unit_files(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default()
}

fn is_up(active: &str) -> bool {
    matches!(active, "active" | "activating" | "reloading")
}

/// Builds `rel`'s tool env and links it; `None` for a release that carries no flake.
fn build_env(ctx: &Ctx, rel: &Path, sha: &str) -> Result<Option<spira_env::Built>, String> {
    if !spira_env::has_flake(rel) {
        eprintln!("release: spira-env not built: {sha} has no flake.nix and flake.lock");
        return Ok(None);
    }
    let home = ctx.cfg.home()?;
    let link = home.join(spira_config::SPIRA_ENV_REL);
    let held = fs::canonicalize(&link).ok();
    let built = spira_env::build(ctx.nix, rel, &home).map_err(|e| {
        // Claimed only when true: a root that failed may also have failed to put the link back.
        let unchanged = if fs::canonicalize(&link).ok() == held { "; nothing changed" } else { "" };
        format!("cannot build spira-env from {sha}: {e}{unchanged}")
    })?;
    match &built.before {
        Some(before) if *before == built.after => eprintln!("release: spira-env unchanged ({})", built.after.display()),
        before => eprintln!("release: spira-env {} -> {}", before.as_deref().map_or("(none)".into(), |b| b.display().to_string()), built.after.display()),
    }
    Ok(Some(built))
}

/// Starts the background collection when the switch replaced the env. Never fails the switch.
fn collect_superseded_env(ctx: &Ctx, env: Option<&spira_env::Built>, rel: &Path) {
    let replaced = env.and_then(spira_env::Built::replaced).is_some();
    let (true, Some(gc)) = (replaced, ctx.gc.as_ref()) else { return };
    let home = match ctx.cfg.home() {
        Ok(h) => h,
        Err(e) => return eprintln!("release: WARN: nix garbage will not be collected: {e}"),
    };
    let plan = spira_env::GcPlan::for_release(&gc.profile_bin, &gc.run_dir, rel);
    match spira_env::collect_nix_garbage(gc.units, &plan, &home.join(spira_config::SPIRA_ENV_REL)) {
        Ok(name) => eprintln!("release: started nix garbage collection as {name}"),
        Err(w) => eprintln!("release: WARN: {w}"),
    }
}

/// Switch the running system onto release `sha`: render and install units, swap
/// `current`, daemon-reload, restart what changed, check it came up, and undo all of it
/// when something did not.
pub fn switch(ctx: &Ctx, sha: &str, install_new: bool, live: &dyn Fn(&[String]) -> Result<(), String>) -> Result<Switched, String> {
    let cfg = ctx.cfg;
    let rel = verify::release_dir(cfg, sha)?;
    let problems = verify::check_files(&rel, sha);
    if !problems.is_empty() {
        return Err(format!("release {sha} does not match its MANIFEST; not activating:\n  {}", problems.join("\n  ")));
    }
    let instance = cfg.instance();
    let host = cfg.host_values()?;
    let mut changes = Vec::new();
    let mut rendered = Vec::new();
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
        let changed = units::without_documentation(&old) != units::without_documentation(&new);
        rendered.push((m.installed.clone(), old.clone(), new.clone(), changed));
        if changed {
            changes.push(Change { unit: m.installed, path, old, new });
        }
    }
    if !errors.is_empty() {
        return Err(format!("cannot render units against {sha}; nothing changed:\n  {}", errors.join("\n  ")));
    }

    // Before any unit changes: units resolve their tools through the env this links.
    let env = build_env(ctx, &rel, sha)?;

    let cur_link = cfg.releases.join(CURRENT);
    let prev = fs::read_link(&cur_link).ok().map(|t| t.to_string_lossy().to_string());
    let prev_root = prev.as_ref().map(|p| cfg.releases.join(p));
    let roots = Roots { releases: &cfg.releases, old: prev_root.as_deref(), new: &rel };

    for (written, c) in changes.iter().enumerate() {
        if let Err(e) = fsutil::write_atomic(&c.path, &c.new) {
            restore_files(&changes[..written]);
            return Err(format!("{e}{}", incomplete(restore_env(ctx, env.as_ref()).err())));
        }
    }
    if let Err(e) = fsutil::atomic_symlink(&cur_link, sha) {
        restore_files(&changes);
        return Err(format!("{e}{}", incomplete(restore_env(ctx, env.as_ref()).err())));
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
        let undo = undo(ctx, env.as_ref(), &changes, None, prev.as_deref(), &[]);
        return Err(format!("daemon-reload failed: {e}; rolled back{undo}"));
    }

    // Before the new release's unit-ensure: it writes the same file, and a write made there
    // would never be seen here and so never restart the broker.
    let broker_unit = broker::UNIT.replace(".service", &format!("-{instance}.service"));
    let conf = if rendered.iter().any(|r| r.0 == broker_unit) {
        match follow_broker_conf(cfg, &rel, &host) {
            Ok(replaced) => replaced,
            Err(e) => {
                let undo = undo(ctx, env.as_ref(), &changes, None, prev.as_deref(), &[]);
                return Err(format!("{e}; nothing was restarted; rolled back{undo}"));
            }
        }
    } else {
        None
    };

    if install_new {
        let before = unit_files(&cfg.unit_dir);
        if let Err(e) = ensure_new_units(cfg, sha) {
            let added: Vec<String> = unit_files(&cfg.unit_dir).difference(&before).cloned().collect();
            let undo = undo(ctx, env.as_ref(), &changes, conf.as_ref(), prev.as_deref(), &[]);
            let shipped = if added.is_empty() { String::new() } else { format!(" (units new in {sha}: {})", added.join(", ")) };
            return Err(format!("{e}{shipped}; nothing was restarted; rolled back{undo}"));
        }
    }

    let mut out = Switched { rewritten: changes.iter().map(|c| c.unit.clone()).collect(), retired, ..Default::default() };
    let mut draining = Vec::new();
    for (unit, old, new, changed) in &rendered {
        if !unit.ends_with(".service") {
            continue;
        }
        let fixed = restart_reason(unit, old, new, &roots).or_else(|| (conf.is_some() && *unit == broker_unit).then(|| "its config changed".to_string()));
        let st = match ctx.sc.state(unit) {
            Ok(st) => st,
            Err(e) if fixed.is_some() => {
                let undo = undo(ctx, env.as_ref(), &changes, conf.as_ref(), prev.as_deref(), &[]);
                return Err(format!("cannot read the state of {unit}: {e}; rolled back{undo}"));
            }
            Err(e) => {
                eprintln!("release: WARN: cannot read the state of {unit}: {e}; not checked for running stale code");
                continue;
            }
        };
        let long_running = is_up(&st.active) && st.kind != "oneshot";
        let why = fixed.or_else(|| (long_running && unit != KILL_RESTARTED && unit != COMPILER_CACHE_BACKEND).then(|| live_reason(old, new, &roots, ctx.sc.running_exe(unit).as_deref(), ctx.sc.running_release(unit).as_deref())).flatten());
        let Some(why) = why else {
            if long_running {
                out.left_running.push(unit.clone());
            }
            if *changed {
                eprintln!("release: {unit} differs only in the release root and runs the same bytes; left running");
            }
            continue;
        };
        eprintln!("release: {unit}: {why}");
        if long_running {
            match (units::never_restart(new), units::restarts_itself_when_idle(old, new)) {
                (Some(reason), _) => out.stale.push(left_running(unit, &why, &reason)),
                (None, Some(how)) => out.when_idle.push(format!("{unit} ({why}; restarts itself when idle: {how})")),
                (None, None) => out.restarted.push(unit.clone()),
            }
        } else {
            if st.kind == "oneshot" && is_up(&st.active) && !units::undrained(new) {
                draining.push(unit.clone());
            }
            out.deferred.push(unit.clone());
        }
    }
    let mut failed = Vec::new();
    let mut attempted = Vec::new();
    let mut logs = OwnLogs::default();
    let store_first = out.restarted.iter().filter(|u| *u == STORE_SERVER).chain(out.restarted.iter().filter(|u| *u != STORE_SERVER));
    for u in store_first {
        eprintln!("release: restarting {u}");
        if let Some(text) = rendered.iter().find(|r| &r.0 == u).map(|r| &r.2) {
            logs.mark(u, text);
        }
        attempted.push(u.clone());
        if u == STORE_SERVER {
            out.store_restarted_at = Some(std::time::Instant::now());
        }
        if let Err(e) = ctx.sc.reset_failed(u).and_then(|()| restart_unit(ctx.sc, u)) {
            let bound = (u == STORE_SERVER).then(|| ctx.sc.start_budget(u)).flatten().map(|b| format!(" (its start bound is TimeoutStartSec={}s)", b.as_secs())).unwrap_or_default();
            failed.push(format!("{u}: restart failed: {e}{bound}{}", logs.since_restart(u)));
            break;
        }
    }
    if failed.is_empty() && !out.restarted.is_empty() {
        for (u, st) in crate::systemctl::await_settled(ctx.sc, &out.restarted, ctx.settle) {
            match st {
                Ok(st) if st.active == "active" => {}
                Ok(st) => failed.push(format!("{u}: did not come up (ActiveState={}, Result={}){}", st.active, st.result, logs.since_restart(&u))),
                Err(e) => failed.push(format!("{u}: cannot read its state: {e}")),
            }
        }
    }
    if failed.is_empty() {
        failed.extend(stayed_up(ctx, &out.restarted, &logs));
    }
    if failed.is_empty() {
        failed.extend(heal_stale_executables(ctx, &rendered, &rel, &mut out, &mut attempted));
    }
    // A rollback returns to a release that served before; refusing it would strand the box.
    if failed.is_empty() && install_new {
        match serves_reads(ctx.reads, &rel) {
            Err(e) => failed.push(e),
            Ok(()) => {
                if let Err(e) = warmed(ctx, &rel, out.store_restarted_at) {
                    eprintln!("release: WARN: the store server is still cold: {e}");
                }
            }
        }
    }
    if !failed.is_empty() && !install_new {
        for f in &failed {
            eprintln!("release: {f}");
        }
        out.down = restart_again_under_target(ctx, &out.restarted, &attempted, &failed);
        failed.clear();
    }
    if failed.is_empty() {
        if let Err(e) = live(&out.restarted) {
            failed.push(format!("cannot record the activation of {sha}: {e}"));
        }
    }
    if !failed.is_empty() {
        let undo = undo(ctx, env.as_ref(), &changes, conf.as_ref(), prev.as_deref(), &attempted);
        return Err(format!(
            "activating {sha} failed, rolled back to {}:\n  {}{undo}",
            prev.as_deref().unwrap_or("no current release"),
            failed.join("\n  ")
        ));
    }
    // Past the last undo: a collection started sooner could remove an env an undo links back to.
    collect_superseded_env(ctx, env.as_ref(), &rel);
    revive_failed(ctx, &mut out);
    for s in units::path_shadows(&cfg.unit_dir, &rel, &cfg.releases) {
        eprintln!("release: WARN: {s}");
    }
    let still_running = drain(ctx, &draining);
    let (to_restart, left) = split_by_exemption(&still_running, |u| rendered.iter().find(|r| &r.0 == u).and_then(|r| units::never_restart(&r.2)));
    let why = format!("still on its previous pass after the {}s drain wait", ctx.drain.as_secs());
    for (u, reason) in left {
        out.stale.push(left_running(&u, &why, &reason));
    }
    for u in to_restart {
        eprintln!("release: restarting {u}: {why}");
        match ctx.sc.reset_failed(&u).and_then(|()| restart_unit(ctx.sc, &u)) {
            Ok(()) => out.restarted.push(u),
            Err(e) => out.stale.push(format!("{u} ({why}; restarting it failed: {e}; its next start runs {sha})")),
        }
    }
    for s in &out.stale {
        eprintln!("release: STALE: {s}");
    }
    for s in &out.when_idle {
        eprintln!("release: WHEN IDLE: {s}");
    }
    if install_new {
        start_changed_triggers(ctx, &rendered, &roots, &mut out);
    }
    Ok(out)
}

/// Starts each installed unit whose `X-Spira-StartWhenChanged` paths differ between the old and
/// new release. Runs after the restarts have settled and reads are served, so what it starts
/// sees the new release; a failed start is named and never undoes the activation.
fn start_changed_triggers(ctx: &Ctx, rendered: &[(String, String, String, bool)], roots: &Roots, out: &mut Switched) {
    for (unit, _, new, _) in rendered {
        let paths = units::start_when_changed(new);
        if paths.is_empty() {
            continue;
        }
        let Some(why) = listed_paths_changed(&paths, roots) else { continue };
        eprintln!("release: starting {unit}: {why}");
        match ctx.sc.start_no_block(unit) {
            Ok(()) => out.started.push(format!("{unit} ({why})")),
            Err(e) => eprintln!("release: WARN: could not start {unit} ({why}): {e}"),
        }
    }
}

/// The first of `paths` (files, or directories taken whole) whose bytes differ between the old
/// and new release root, as a reason; `None` when none does. No old root counts as changed.
fn listed_paths_changed(paths: &[String], roots: &Roots) -> Option<String> {
    let Some(old) = roots.old else { return Some("no previous release to compare against".into()) };
    for p in paths {
        let rel = Path::new(p);
        match (tree_of(old, rel), tree_of(roots.new, rel)) {
            (Some(o), Some(n)) if o == n => {}
            (Some(_), Some(_)) => return Some(format!("{p} changed")),
            _ => return Some(format!("cannot compare {p} between releases")),
        }
    }
    None
}

/// Every file at `rel` under `root` by path (the file itself, or all under a directory); empty
/// when it does not exist; `None` when it cannot be read.
fn tree_of(root: &Path, rel: &Path) -> Option<std::collections::BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = std::collections::BTreeMap::new();
    let full = root.join(rel);
    if full.is_dir() {
        collect_files(root, rel, &mut files)?;
    } else if full.exists() {
        files.insert(rel.to_path_buf(), std::fs::read(&full).ok()?);
    }
    Some(files)
}

/// The path of a program in a release other than `new`, as `/proc/<pid>/exe` shows it: the
/// release it runs from and its path inside it. `None` for a program outside the releases
/// directory or inside `new`.
fn superseded_program(exe: &Path, releases: &Path, new: &Path) -> Option<PathBuf> {
    let shown = exe.to_string_lossy();
    let shown = shown.strip_suffix(" (deleted)").unwrap_or(&shown);
    let mut parts = Path::new(shown).strip_prefix(releases).ok()?.components();
    let release = parts.next()?.as_os_str();
    (release != new.file_name()? && release != CURRENT).then(|| parts.as_path().to_path_buf())
}

/// Up, long-lived units with a process executing a binary from a superseded release whose
/// bytes differ from the same program in `new` (identical bytes are no reason to cycle a
/// service), each with that binary. A unit that never restarts, or restarts itself when idle,
/// is returned too: the caller names it rather than restarting it.
fn stale_executables(ctx: &Ctx, rendered: &[(String, String, String, bool)], new: &Path) -> Vec<(String, PathBuf)> {
    let mut stale = Vec::new();
    for (unit, _, _, _) in rendered {
        if !unit.ends_with(".service") || !matches!(ctx.sc.state(unit), Ok(st) if is_up(&st.active) && st.kind != "oneshot") {
            continue;
        }
        for exe in ctx.sc.executables(unit) {
            let Some(rel_path) = superseded_program(&exe, &ctx.cfg.releases, new) else { continue };
            if matches!((fs::read(&exe), fs::read(new.join(&rel_path))), (Ok(a), Ok(b)) if a == b) {
                continue;
            }
            stale.push((unit.clone(), exe));
            break;
        }
    }
    stale
}

/// Activation does not finish while a long-lived service still executes a superseded
/// release's binary: restart it, say so, and fail (rolling back) if it is still there.
fn heal_stale_executables(ctx: &Ctx, rendered: &[(String, String, String, bool)], new: &Path, out: &mut Switched, attempted: &mut Vec<String>) -> Vec<String> {
    let exempt = |u: &str| rendered.iter().find(|r| r.0 == u).is_some_and(|r| units::never_restart(&r.2).is_some() || units::restarts_itself_when_idle(&r.1, &r.2).is_some());
    let (left, first): (Vec<_>, Vec<_>) = stale_executables(ctx, rendered, new).into_iter().partition(|(u, _)| exempt(u));
    for (unit, exe) in left {
        let line = format!("{unit} (a process runs {}; not restarted by activation, by its own declaration)", exe.display());
        if !out.stale.iter().chain(&out.when_idle).any(|s| s.starts_with(&unit)) {
            out.stale.push(line);
        }
    }
    let mut failed = Vec::new();
    for (unit, exe) in &first {
        eprintln!("release: HEALED: {unit}: a process runs {}; restarting it onto {}", exe.display(), new.display());
        attempted.push(unit.clone());
        if unit == STORE_SERVER {
            out.store_restarted_at = Some(std::time::Instant::now());
        }
        if let Err(e) = ctx.sc.reset_failed(unit).and_then(|()| restart_unit(ctx.sc, unit)) {
            failed.push(format!("{unit}: still executes a superseded release and restarting it failed: {e}"));
        } else if !out.restarted.contains(unit) {
            out.restarted.push(unit.clone());
        }
    }
    if first.is_empty() || !failed.is_empty() {
        return failed;
    }
    let healed: Vec<String> = first.into_iter().map(|(u, _)| u).collect();
    for (u, st) in crate::systemctl::await_settled(ctx.sc, &healed, ctx.settle) {
        if !matches!(&st, Ok(s) if s.active == "active") {
            failed.push(format!("{u}: did not come up after the restart that cleared its superseded executable"));
        }
    }
    failed.extend(stale_executables(ctx, rendered, new).into_iter().filter(|(u, _)| healed.contains(u)).map(|(u, e)| format!("{u}: a process still runs {} after its restart", e.display())));
    failed
}

/// A rollback's second chance: the target release is the one that served before, so a unit
/// that is slow to start is restarted once more under it and reported if still down, never
/// undone onto the release being left. Units the first pass never reached are restarted too.
fn restart_again_under_target(ctx: &Ctx, restarted: &[String], attempted: &[String], failed: &[String]) -> Vec<Down> {
    let mut down = Vec::new();
    let mut waiting = Vec::new();
    let mut seen = BTreeSet::new();
    for u in restarted.iter().chain(attempted) {
        if !seen.insert(u.clone()) {
            continue;
        }
        let named = failed.iter().any(|f| f.starts_with(&format!("{u}: ")));
        let up = attempted.contains(u) && !named && matches!(ctx.sc.state(u), Ok(st) if st.active == "active");
        if up {
            continue;
        }
        eprintln!("release: restarting {u} once more under the rollback target");
        match ctx.sc.reset_failed(u).and_then(|()| restart_unit(ctx.sc, u)) {
            Ok(()) => waiting.push(u.clone()),
            Err(e) => down.push(Down { unit: u.clone(), state: format!("restart failed: {e}") }),
        }
    }
    for (u, st) in crate::systemctl::await_settled(ctx.sc, &waiting, ctx.settle) {
        match st {
            Ok(st) if st.active == "active" => {}
            Ok(st) => down.push(Down { unit: u, state: format!("ActiveState={}, Result={}", st.active, st.result) }),
            Err(e) => down.push(Down { unit: u, state: format!("cannot read its state: {e}") }),
        }
    }
    let settled: Vec<String> = waiting.into_iter().filter(|u| !down.iter().any(|d| &d.unit == u)).collect();
    for f in stayed_up(ctx, &settled, &OwnLogs::default()) {
        if let Some((unit, state)) = f.split_once(": ") {
            down.push(Down { unit: unit.into(), state: state.into() });
        }
    }
    down
}

const OWN_LINE_CHARS: usize = 300;
const OWN_LOG_TAIL_BYTES: u64 = 64 * 1024;

/// Each restarted unit's own append log and its length when the restart was issued, so a
/// failure can quote only what the unit wrote afterwards.
#[derive(Default)]
struct OwnLogs(std::collections::BTreeMap<String, (PathBuf, u64)>);

impl OwnLogs {
    fn mark(&mut self, unit: &str, rendered: &str) {
        if let Some(path) = units::own_log(rendered) {
            let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            self.0.insert(unit.into(), (path, len));
        }
    }

    /// The suffix for a failure line: the unit's last line since its restart, or that it wrote none.
    fn since_restart(&self, unit: &str) -> String {
        let Some((path, from)) = self.0.get(unit) else { return String::new() };
        match last_line_since(path, *from) {
            Some(line) => format!(" — its own last line: \"{}\"", line.chars().take(OWN_LINE_CHARS).collect::<String>()),
            None => format!(" — its own log {} got no line after its restart", path.display()),
        }
    }
}

fn last_line_since(path: &Path, from: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = if len < from { 0 } else { from }.max(len.saturating_sub(OWN_LOG_TAIL_BYTES));
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(OWN_LOG_TAIL_BYTES).read_to_end(&mut buf).ok()?;
    String::from_utf8_lossy(&buf).lines().rev().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// A restarted unit that reached `active` and then left it did not come up: a service that
/// fails after its start job is reported ready is caught by watching it for the settle time,
/// not by one read.
fn stayed_up(ctx: &Ctx, units: &[String], logs: &OwnLogs) -> Vec<String> {
    const POLLS: u32 = 5;
    let mut gone: BTreeSet<String> = BTreeSet::new();
    let mut failed = Vec::new();
    for _ in 0..POLLS {
        std::thread::sleep(ctx.settle / POLLS);
        for u in units {
            if gone.contains(u) {
                continue;
            }
            let why = match ctx.sc.state(u) {
                Ok(st) if st.active == "active" => continue,
                Ok(st) => format!("did not stay up after its restart (ActiveState={}, Result={}){}", st.active, st.result, logs.since_restart(u)),
                Err(e) => format!("cannot read its state: {e}"),
            };
            gone.insert(u.clone());
            failed.push(format!("{u}: {why}"));
        }
    }
    failed
}

fn left_running(unit: &str, why: &str, reason: &str) -> String {
    format!("{unit} ({why}; never restarted by activation: {reason})")
}

/// Units split into those activation restarts and those it leaves, each left unit with the
/// reason its template gives.
fn split_by_exemption(units: &[String], reason: impl Fn(&String) -> Option<String>) -> (Vec<String>, Vec<(String, String)>) {
    let mut restart = Vec::new();
    let mut left = Vec::new();
    for u in units {
        match reason(u) {
            Some(r) => left.push((u.clone(), r)),
            None => restart.push(u.clone()),
        }
    }
    (restart, left)
}

fn revive_failed(ctx: &Ctx, out: &mut Switched) {
    let failed = match ctx.sc.list_failed("spira-*.service") {
        Ok(f) => f,
        Err(e) => return eprintln!("release: WARN: cannot list failed units: {e}"),
    };
    for u in failed {
        if out.restarted.contains(&u) || !ctx.cfg.unit_dir.join(&u).exists() {
            continue;
        }
        eprintln!("release: {u} is failed; resetting and starting it under the new release");
        match ctx.sc.reset_failed(&u).and_then(|()| restart_unit(ctx.sc, &u)) {
            Ok(()) => out.revived.push(u),
            Err(e) => eprintln!("release: WARN: {u} is still failed: {e}"),
        }
    }
}

/// Wait for each running oneshot of a changed unit to exit, so its next start runs the new
/// release. The worker's own release check ends its pass between jobs; this only bounds how
/// long the deploy waits for that, and a unit still running at the bound is named, not fatal:
/// it is returned, for the caller to report as stale.
fn drain(ctx: &Ctx, units: &[String]) -> Vec<String> {
    let mut left = Vec::new();
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
                left.push(u.clone());
                break;
            }
            std::thread::sleep((ctx.drain / 20).clamp(Duration::from_millis(10), Duration::from_secs(1)));
        }
    }
    left
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

/// Points the env link back at what it held before this switch built over it.
fn restore_env(ctx: &Ctx, env: Option<&spira_env::Built>) -> Result<(), String> {
    match env.and_then(spira_env::Built::replaced) {
        Some(before) => spira_env::restore(&ctx.cfg.home()?, before),
        None => Ok(()),
    }
}

/// The broker config a switch rewrote, as it was before; `None` when there was none.
struct ReplacedConf {
    path: PathBuf,
    before: Option<String>,
}

impl ReplacedConf {
    fn restore(&self) -> Result<(), String> {
        match &self.before {
            Some(text) => fsutil::write_atomic(&self.path, text),
            None => fs::remove_file(&self.path).map_err(|e| format!("cannot remove {}: {e}", self.path.display())),
        }
    }
}

/// Places the broker config `rel` ships, so the broker runs the one of the release it is on;
/// `Some` when that rewrote the file. A release without the template places nothing.
fn follow_broker_conf(cfg: &Config, rel: &Path, host: &std::collections::BTreeMap<String, String>) -> Result<Option<ReplacedConf>, String> {
    let templates = rel.join("systemd");
    if !templates.join(broker::CONF_FILE).is_file() {
        return Ok(None);
    }
    let run = cfg.run.as_ref().ok_or_else(|| format!("no run directory to place {} in", broker::CONF_FILE))?;
    let path = run.join(broker::CONF_FILE);
    let before = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let socket = host.get(spira_config::broker::SOCKET_KEY).map(String::as_str).unwrap_or_default();
    let wrote = broker::place_conf(&templates, socket, run)?;
    if wrote {
        eprintln!("release: wrote {}", path.display());
    }
    Ok(wrote.then_some(ReplacedConf { path, before }))
}

/// Put everything back: the tool env, unit files, the broker config, `current`, daemon-reload, and restart what was
/// restarted so it runs the old release again. Returns "" or the problems undoing hit.
fn undo(ctx: &Ctx, env: Option<&spira_env::Built>, changes: &[Change], conf: Option<&ReplacedConf>, prev: Option<&str>, restarted: &[String]) -> String {
    // The env goes first: what is restarted below must resolve the old release's tools.
    let mut errs: Vec<String> = restore_env(ctx, env).err().into_iter().collect();
    errs.extend(restore_files(changes));
    errs.extend(conf.and_then(|c| c.restore().err()));
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
        if let Err(e) = restart_unit(ctx.sc, u) {
            errs.push(format!("restart {u} on the previous release: {e}"));
        }
    }
    incomplete(errs)
}

fn incomplete(errs: impl IntoIterator<Item = String>) -> String {
    let errs: Vec<String> = errs.into_iter().collect();
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
fn switch_with_config(ctx: &Ctx, sha: &str, txn: Option<&Txn>, install_new: bool, live: &dyn Fn(&[String]) -> Result<(), String>) -> Result<(Switched, Option<String>), String> {
    if let Some(t) = txn {
        t.apply_pre()?;
    }
    let out = match switch(ctx, sha, install_new, live) {
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
    cmd.arg("120").arg(&bin).arg("--retire").env_remove("SPIRA_REPO");
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

/// The unit whose `RefuseManualStop=yes` denies a restart job; `systemctl kill` plus its
/// `Restart=always` is the sanctioned way to cycle it.
const KILL_RESTARTED: &str = "dolt-beads.service";

/// The compiler cache's storage backend. Every restart leaves the sccache server holding dead
/// connections and wedges the next compile, so it restarts only when its unit text, its launcher
/// or its source changes. A rebuild that changed the binary's bytes but not the source it is
/// compiled from (`backend_source`) leaves it running.
const COMPILER_CACHE_BACKEND: &str = "sccache-dav.service";
const BACKEND_CRATE_DIR: &str = "sccache-dav";

/// The two release roots a unit's paths resolve under (sp-m9rruh): the one `current` named
/// before this switch, which the running process started from, and the one it switches to.
/// Installed units name their programs through `<releases>/current/`, and the switch compares
/// after `current` has moved, so a path through the link must be resolved per side; reading it
/// as written read the NEW binary for both and left every such service on its old code.
struct Roots<'a> {
    releases: &'a Path,
    /// `None` when no release was current: nothing names what the old side ran.
    old: Option<&'a Path>,
    new: &'a Path,
}

impl Roots<'_> {
    fn current_prefix(&self) -> String {
        format!("{}/{CURRENT}/", self.releases.display())
    }

    /// `path` with a leading `<releases>/current/` resolved under `root`; `None` when it runs
    /// through the link and `root` is unknown.
    fn resolve(&self, path: &str, root: Option<&Path>) -> Option<PathBuf> {
        match path.strip_prefix(&self.current_prefix()) {
            Some(rest) => root.map(|r| r.join(rest)),
            None => Some(PathBuf::from(path)),
        }
    }
}

/// Why a rendered service must be restarted whatever its run state (over-broad-scopes #4,
/// sp-ipwlic), or `None`: its unit text, with the release root collapsed, changed, or a program
/// one of its `Exec*=` lines runs differs in bytes between the release it ran and the one it
/// switches to. Every unit names the release root, so comparing exec lines verbatim restarted
/// 80 of 82 services on every release — a single changed binary cycled loom, watchd and the
/// scheduler and drained every running oneshot. A program that cannot be read, or one that is
/// a script (it reads siblings such as `lib.sh` that its bytes do not show), is a change.
///
/// The store server restarts like any other service: when its unit text or `bin/spira-lc`'s
/// bytes change, and a restart is an outage for every caller.
fn restart_reason(unit: &str, old: &str, new: &str, roots: &Roots) -> Option<String> {
    let text_changed = unrooted(&units::without_documentation(old), roots.new) != unrooted(&units::without_documentation(new), roots.new);
    if text_changed {
        return Some("its unit changed".into());
    }
    if unit == KILL_RESTARTED {
        return None;
    }
    if unit == COMPILER_CACHE_BACKEND {
        return backend_binary_changed(roots);
    }
    program_changed(old, new, roots)
}

fn backend_binary_changed(roots: &Roots) -> Option<String> {
    let Some(old) = roots.old else { return Some("cannot tell which release its backend ran before; restarted".into()) };
    let files = |root: &Path| (std::fs::read(root.join("bin/sccache-dav")), std::fs::read(root.join("spira/sccache-dav.sh")));
    let unreadable = || Some("cannot compare its backend binaries; restarted".to_string());
    let ((Ok(ob), Ok(ol)), (Ok(nb), Ok(nl))) = (files(old), files(roots.new)) else { return unreadable() };
    if ol != nl {
        return Some("its backend launcher changed".into());
    }
    if ob == nb {
        return None;
    }
    match (backend_source(old), backend_source(roots.new)) {
        (Some(o), Some(n)) => {
            let first = o.keys().chain(n.keys()).filter(|p| o.get(*p) != n.get(*p)).min();
            first.map(|p| format!("its backend's source changed ({})", p.display()))
        }
        _ => unreadable(),
    }
}

/// Every file the backend is compiled from, by path under `root`: the crates reached from
/// `sccache-dav/Cargo.toml` through `path =` entries of `[dependencies]` (not dev-dependencies),
/// plus `Cargo.lock` and `rust-toolchain.toml` when present. `None` when any cannot be read.
fn backend_source(root: &Path) -> Option<std::collections::BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = std::collections::BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut pending = vec![PathBuf::from(BACKEND_CRATE_DIR)];
    while let Some(dir) = pending.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        let manifest: toml::Table = std::fs::read_to_string(root.join(&dir).join("Cargo.toml")).ok()?.parse().ok()?;
        for dep in manifest.get("dependencies").and_then(|d| d.as_table()).into_iter().flat_map(|t| t.values()) {
            if let Some(p) = dep.get("path").and_then(|p| p.as_str()) {
                pending.push(lexical_join(&dir, p)?);
            }
        }
        collect_files(root, &dir, &mut files)?;
    }
    files.insert(PathBuf::from("Cargo.lock"), std::fs::read(root.join("Cargo.lock")).ok()?);
    if let Ok(b) = std::fs::read(root.join("rust-toolchain.toml")) {
        files.insert(PathBuf::from("rust-toolchain.toml"), b);
    }
    Some(files)
}

fn lexical_join(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    for part in Path::new(rel).components() {
        match part {
            std::path::Component::Normal(p) => out.push(p),
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn collect_files(root: &Path, dir: &Path, files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) -> Option<()> {
    for entry in std::fs::read_dir(root.join(dir)).ok()? {
        let rel = dir.join(entry.ok()?.file_name());
        let full = root.join(&rel);
        if full.is_dir() {
            collect_files(root, &rel, files)?;
        } else {
            files.insert(rel, std::fs::read(&full).ok()?);
        }
    }
    Some(())
}

/// The first program an `Exec*=` line runs whose content differs between the two roots (or
/// cannot be shown to match), as a reason; `None` when every one matches.
fn program_changed(old: &str, new: &str, roots: &Roots) -> Option<String> {
    let path_of = |w: &str| w.find('/').map(|i| w[i..].trim_end_matches(['"', '\'', ';']).to_string());
    for (o, n) in units::exec_lines(old).iter().zip(units::exec_lines(new).iter()) {
        let (ov, nv) = (o.split_once('=').map_or("", |x| x.1), n.split_once('=').map_or("", |x| x.1));
        for (ow, nw) in ov.split_whitespace().zip(nv.split_whitespace()) {
            let (op, np) = match (path_of(ow), path_of(nw)) {
                (None, None) if ow == nw => continue,
                (Some(op), Some(np)) => (op, np),
                _ => return Some(format!("{ow} became {nw}")),
            };
            let Some(or) = roots.resolve(&op, roots.old) else {
                return Some(format!("cannot tell which release {op} ran before; restarted"));
            };
            let nr = roots.resolve(&np, Some(roots.new)).unwrap_or_else(|| PathBuf::from(&np));
            if or == nr {
                continue;
            }
            match (std::fs::read(&or), std::fs::read(&nr)) {
                (Ok(a), Ok(b)) if a == b && !b.starts_with(b"#!") => {}
                (Ok(_), Ok(b)) if b.starts_with(b"#!") => return Some(format!("{} is a script; restarted", nr.display())),
                (Ok(_), Ok(_)) => return Some(format!("{} differs from {}", nr.display(), or.display())),
                _ => return Some(format!("cannot compare {} with {}; restarted", or.display(), nr.display())),
            }
        }
    }
    None
}

/// Why a RUNNING long-lived service is on old code although its unit's programs match
/// (sp-m9rruh), or `None`: an `Environment=` line pins a release by name other than the one it
/// switches to (lc-serve's `SPIRA_HOME=<sha>/spira` kept its broker resolving the old
/// release's registry for hours), or the executable its main process runs (`exe`, from
/// [`Systemctl::running_exe`]) differs in bytes from the program its `ExecStart=` names — a
/// process left behind by an earlier activation, whatever the unit file now says.
fn live_reason(old: &str, new: &str, roots: &Roots, exe: Option<&Path>, started_under: Option<&str>) -> Option<String> {
    if let Some(r) = started_under.map(|r| r.trim_end_matches('/').rsplit('/').next().unwrap_or(r)) {
        if r != CURRENT && Some(r) != roots.new.file_name().and_then(|n| n.to_str()) {
            return Some(format!("its process started under release {r}"));
        }
    }
    let cur = roots.current_prefix();
    let now = format!("{}/", roots.new.display());
    let env = |t: &str| t.lines().filter(|l| l.starts_with("Environment=")).map(|l| l.replace(&cur, &now)).collect::<Vec<_>>();
    let (oe, ne) = (env(old), env(new));
    if let Some((o, n)) = oe.iter().zip(ne.iter()).find(|(o, n)| o != n) {
        return Some(format!("its environment pins the previous release ({o}; now {n})"));
    }
    let exe = exe?;
    let line = units::exec_lines(new).into_iter().find(|l| l.starts_with("ExecStart="))?;
    let prog = line["ExecStart=".len()..].trim_start_matches(['-', '@', '+', '!', ':']).split_whitespace().next()?.to_string();
    if !prog.starts_with(&format!("{}/", roots.releases.display())) {
        return None;
    }
    let path = roots.resolve(&prog, Some(roots.new))?;
    let (have, want) = match (std::fs::read(exe), std::fs::read(&path)) {
        (Ok(h), Ok(w)) => (h, w),
        _ => return Some(format!("cannot compare its running executable with {}; restarted", path.display())),
    };
    if want.starts_with(b"#!") {
        return None;
    }
    (have != want).then(|| format!("its running process's executable is not {}", path.display()))
}

/// The text with every `<releases>/<name>` prefix collapsed, whatever `<name>` is: an installed
/// unit may name a release by sha or by the `current` link, and neither is a change to the unit.
fn unrooted(text: &str, root: &Path) -> String {
    let Some(parent) = root.parent() else { return text.to_string() };
    let prefix = format!("{}/", parent.display());
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(&prefix) {
        out.push_str(&rest[..i]);
        out.push_str("ROOT");
        let tail = &rest[i + prefix.len()..];
        let end = tail.find(|c: char| c == '/' || c.is_whitespace() || c == ':').unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn restart_unit(sc: &dyn Systemctl, unit: &str) -> Result<(), String> {
    if unit == KILL_RESTARTED {
        return sc.kill(unit);
    }
    sc.restart(unit)?;
    if unit == COMPILER_CACHE_BACKEND {
        sc.stop_compiler_cache_server();
    }
    Ok(())
}

/// The binary that must perform `release activate <sha>`: the target release's own, so an
/// older activator never renders templates it predates. `None` when `me` already is that
/// binary or the release carries none; an error when its files fail the manifest check.
pub fn delegate_target(cfg: &Config, sha: &str, me: &Path) -> Result<Option<PathBuf>, String> {
    delegate_target_in(&cfg.releases, sha, me)
}

/// [`delegate_target`] from the releases directory alone: deciding whether to hand over must not
/// resolve config, because the process-wide config resolves once and a resolution before the
/// release's delta is staged caches its refusal of every key the delta adds.
pub fn delegate_target_in(releases: &Path, sha: &str, me: &Path) -> Result<Option<PathBuf>, String> {
    if !crate::is_sha(sha) {
        return Err(format!("{sha:?} is not a full commit sha (40 hex digits); releases are named by exactly that"));
    }
    let rel = releases.join(sha);
    if !rel.is_dir() {
        return Err(format!("no release {sha} in {}", releases.display()));
    }
    let bin = rel.join("bin").join("release");
    let (Ok(bin_real), Ok(me_real)) = (fs::canonicalize(&bin), fs::canonicalize(me)) else {
        return Ok(None);
    };
    if bin_real == me_real {
        return Ok(None);
    }
    let problems = verify::check_files(&rel, sha);
    if !problems.is_empty() {
        return Err(format!("release {sha} fails verify; not handing activation to its binary:\n  {}", problems.join("\n  ")));
    }
    Ok(Some(bin))
}

/// The home and repo a release's own `spira-config resolve` is pointed at: the release itself.
pub(crate) fn resolve_env(rel: &Path) -> [(&'static str, PathBuf); 2] {
    [("SPIRA_HOME", rel.join("spira")), ("SPIRA_REPO", rel.to_path_buf())]
}

/// `release activate <sha> [--hotfix <reason>]`.
pub fn activate(ctx: &Ctx, sha: &str, hotfix_reason: Option<&str>) -> Result<Switched, String> {
    let state = ctx.cfg.state_dir()?;
    fs::create_dir_all(&state).map_err(|e| format!("cannot create {}: {e}", state.display()))?;
    let rel = verify::release_dir(ctx.cfg, sha)?;
    let standing = read_hotfix(&state)?;
    let after = hotfix_rule(ctx, standing.clone(), sha, hotfix_reason)?;
    let history = read_history(&state)?;
    let delta = config_delta::complete_for(ctx.cfg, &rel, config_delta::load(&rel)?)?;
    let delta = delta.or_else(|| (!config_delta::load_retired(&state).is_empty() || config_delta::must_resolve(ctx.cfg, &rel)).then(config_delta::Delta::default));
    let txn = delta.as_ref().map(|d| config_delta::prepare(ctx.cfg, &rel, d, true)).transpose()?;
    let entry = HistEntry { sha: sha.to_string(), hotfix: hotfix_reason.map(one_line) };
    let mut activated = history.clone();
    if activated.last() != Some(&entry) {
        activated.push(entry);
    }
    let recorded = std::cell::Cell::new(false);
    let live = |restarted: &[String]| {
        write_history(&state, &activated)?;
        if let Err(e) = machine::enter_with(&state, sha, ReleaseState::Activated, "current switched", restarted) {
            return write_history(&state, &history).and(Err(e));
        }
        recorded.set(true);
        match &after {
            HotfixAfter::Unchanged | HotfixAfter::Superseded(_) => {}
            HotfixAfter::Record(h) => {
                eprintln!("release: RUNNING UNLANDED {}: {}", h.sha, h.reason);
                write_hotfix(&state, Some(h))?;
            }
        }
        if let Some(t) = &txn {
            config_delta::save_undo(&state, sha, &t.undo)?;
            config_delta::save_retired(&state, &t.deferred)?;
        }
        Ok(())
    };
    let mut cleared = false;
    if let HotfixAfter::Superseded(h) = &after {
        eprintln!("release: hotfix {} has landed and is in {sha}; superseded", h.sha);
        write_hotfix(&state, None)?;
        cleared = true;
    }
    let (out, late) = match switch_with_config(ctx, sha, txn.as_ref(), true, &live) {
        Ok(done) => done,
        Err(e) => {
            let unrecorded = recorded
                .get()
                .then(|| write_history(&state, &history))
                .and_then(Result::err)
                .or_else(|| (cleared || recorded.get()).then(|| write_hotfix(&state, standing.as_ref())).and_then(Result::err))
                .or_else(|| undone_in_record(&state, sha).err());
            return Err(match unrecorded {
                Some(why) => format!("{e}\n  THE RECORD OF THE UNDONE ACTIVATION IS STILL THERE: {why}"),
                None => e,
            });
        }
    };
    if let Some(e) = late {
        return Err(e);
    }
    Ok(out)
}

/// A failed activation leaves its release `rolled_back`, whether or not it got as far as
/// `activated`. A release that never reached `published` has no activation to undo.
fn undone_in_record(state: &Path, sha: &str) -> Result<(), String> {
    match machine::load(state, sha)?.and_then(|r| r.state()) {
        None | Some(ReleaseState::Published | ReleaseState::Activated) => machine::enter(state, sha, ReleaseState::RolledBack, "activation failed and was undone"),
        Some(_) => Ok(()),
    }
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
            Some(config_delta::prepare(ctx.cfg, &rel, d, false)?)
        }
        None => None,
    };
    let (switched, late) = switch_with_config(ctx, &prev.sha, txn.as_ref(), false, &|_| Ok(()))?;
    config_delta::clear_undo(&state, &top.sha);
    config_delta::save_retired(&state, &[])?;
    history.pop();
    write_history(&state, &history)?;
    let rec = prev.hotfix.as_ref().map(|r| Hotfix { sha: prev.sha.clone(), reason: r.clone(), at: fsutil::now_rfc3339() });
    write_hotfix(&state, rec.as_ref())?;
    if !switched.down.is_empty() {
        let units: Vec<String> = switched.down.iter().map(|d| format!("{} ({}): systemctl --user start {}", d.unit, d.state, d.unit)).collect();
        let config = late.map(|e| format!("\n  {e}")).unwrap_or_default();
        return Err(format!("rolled back to {}; {} unit(s) not up:\n  {}{config}", prev.sha, units.len(), units.join("\n  ")));
    }
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

#[cfg(test)]
mod restart_tests {
    use super::{left_running, live_reason, restart_reason, split_by_exemption, Roots};
    use crate::units;
    use std::fs;
    use std::path::Path;

    fn restart_needed(unit: &str, old: &str, new: &str, releases: &Path, from: &str, to: &str) -> bool {
        let (o, n) = (releases.join(from), releases.join(to));
        restart_reason(unit, old, new, &Roots { releases, old: Some(&o), new: &n }).is_some()
    }

    /// Over-broad-scopes #4 (sp-ipwlic): a release that changed only another binary leaves a
    /// service running; one that changed this service's binary, its unit text, or the script it
    /// runs restarts it.
    #[test_id(twverpw)]
    #[test]
    fn a_service_restarts_only_when_its_program_bytes_or_unit_text_changed() {
        let t = testkit::TempDir::new("release-restart-needed");
        let rel = t.path().join("releases");
        for (sha, loom, other, script) in [("a1", "LOOM1", "X1", "#!/bin/sh\n"), ("b2", "LOOM1", "X2", "#!/bin/sh\n"), ("c3", "LOOM2", "X2", "#!/bin/sh\n")] {
            fs::create_dir_all(rel.join(sha).join("bin")).unwrap();
            fs::write(rel.join(sha).join("bin/loom"), loom).unwrap();
            fs::write(rel.join(sha).join("bin/other"), other).unwrap();
            fs::write(rel.join(sha).join("job.sh"), script).unwrap();
        }
        let unit = |sha: &str, extra: &str| {
            let r = rel.join(sha).display().to_string();
            format!("[Service]\nEnvironment=SPIRA_RELEASE={r}\nExecStart={r}/bin/loom --serve{extra}\n")
        };
        assert!(!restart_needed("spira-loom.service", &unit("a1", ""), &unit("b2", ""), &rel, "a1", "b2"), "only bin/other changed");
        assert!(restart_needed("spira-loom.service", &unit("b2", ""), &unit("c3", ""), &rel, "b2", "c3"), "bin/loom changed");
        assert!(restart_needed("spira-loom.service", &unit("a1", ""), &unit("b2", " --fast"), &rel, "a1", "b2"), "the unit text changed");
        let job = |sha: &str| format!("[Service]\nType=oneshot\nExecStart={}/job.sh\n", rel.join(sha).display());
        assert!(restart_needed("spira-job.service", &job("a1"), &job("b2"), &rel, "a1", "b2"), "a script reads siblings its bytes do not show");
    }

    /// sp-m9rruh: an installed unit names its programs through the `current` link, and
    /// activation compares after `current` has moved, so reading the old unit's path read the
    /// NEW binary and every such service was "left running" on its old code. Each side's
    /// program resolves under its own release root; identical bytes still leave it running.
    #[test_id(tphqwqe)]
    #[test]
    fn a_unit_naming_current_restarts_when_the_binary_under_the_old_root_differs() {
        let t = testkit::TempDir::new("release-restart-current");
        let rel = t.path().join("releases");
        for (sha, rt) in [("a1", "RUNTIME1"), ("b2", "RUNTIME2"), ("c3", "RUNTIME2")] {
            fs::create_dir_all(rel.join(sha).join("bin")).unwrap();
            fs::write(rel.join(sha).join("bin/spira-runtime"), rt).unwrap();
        }
        std::os::unix::fs::symlink("b2", rel.join("current")).unwrap();
        let cur = rel.join("current").display().to_string();
        let at = |sha: &str| rel.join(sha).display().to_string();
        let old = format!("[Service]\nEnvironment=SPIRA_HOME={cur}/spira\nExecStart={cur}/bin/spira-runtime serve\n");
        let new = |sha: &str| format!("[Service]\nEnvironment=SPIRA_HOME={}/spira\nExecStart={}/bin/spira-runtime serve\n", at(sha), at(sha));
        assert!(restart_needed("spira-runtime-prod.service", &old, &new("b2"), &rel, "a1", "b2"), "a1's binary differs from b2's; the unit runs a1's");
        assert!(!restart_needed("spira-runtime-prod.service", &old, &new("c3"), &rel, "b2", "c3"), "b2's and c3's binaries match: left running");
        let both = |sha: &str| format!("[Service]\nEnvironment=SPIRA_HOME={}/spira\nExecStart={cur}/bin/spira-runtime serve\n", at(sha));
        assert!(restart_needed("lc-serve.service", &both("a1"), &both("b2"), &rel, "a1", "b2"), "the same current path names a1's binary before the flip");
        let no_prev = Roots { releases: &rel, old: None, new: &rel.join("b2") };
        assert!(restart_reason("lc-serve.service", &both("a1"), &both("b2"), &no_prev).is_some(), "no previous release: cannot show the bytes match");
    }

    /// sp-m9rruh: a running server is on old code when its environment pins the previous
    /// release by name, or when its process runs an executable other than the one its unit now
    /// names (left behind by an earlier activation); not when both match.
    #[test_id(tz5b8da)]
    #[test]
    fn a_running_server_pinned_to_an_old_release_or_running_an_old_executable_is_stale() {
        let t = testkit::TempDir::new("release-live-reason");
        let rel = t.path().join("releases");
        for (sha, lc) in [("a1", "LC1"), ("b2", "LC2")] {
            fs::create_dir_all(rel.join(sha).join("bin")).unwrap();
            fs::write(rel.join(sha).join("bin/spira-lc"), lc).unwrap();
        }
        let cur = rel.join("current").display().to_string();
        let (a1, b2) = (rel.join("a1"), rel.join("b2"));
        let roots = Roots { releases: &rel, old: Some(&a1), new: &b2 };
        let unit = |home: &str| format!("[Service]\nEnvironment=SPIRA_HOME={home}/spira\nExecStart={cur}/bin/spira-lc serve\n");
        let pinned = unit(&a1.display().to_string());
        let now = unit(&b2.display().to_string());
        assert!(live_reason(&pinned, &now, &roots, None, None).is_some_and(|w| w.contains("environment")), "SPIRA_HOME names a1");
        assert_eq!(live_reason(&unit(&cur), &now, &roots, None, None), None, "current follows the flip");
        let exe = a1.join("bin/spira-lc");
        assert!(live_reason(&now, &now, &roots, Some(&exe), None).is_some_and(|w| w.contains("executable")), "the process runs a1's spira-lc");
        assert_eq!(live_reason(&now, &now, &roots, Some(&b2.join("bin/spira-lc")), None), None, "the process runs b2's bytes");
    }

    /// sp-r1gmno: a running process whose executable cannot be read, or a program the unit
    /// names that cannot be read, is not shown to match, so the service restarts.
    #[test_id(tmwlbfa)]
    #[test]
    fn a_running_executable_that_cannot_be_compared_restarts_the_service() {
        let t = testkit::TempDir::new("release-live-unreadable");
        let rel = t.path().join("releases");
        fs::create_dir_all(rel.join("b2").join("bin")).unwrap();
        fs::write(rel.join("b2/bin/spira-lc"), "LC2").unwrap();
        let (a1, b2) = (rel.join("a1"), rel.join("b2"));
        let roots = Roots { releases: &rel, old: Some(&a1), new: &b2 };
        let unit = |sha: &str| format!("[Service]\nExecStart={}/{sha}/bin/spira-lc serve\n", rel.display());
        let gone = t.path().join("no-such-exe");
        assert!(live_reason(&unit("b2"), &unit("b2"), &roots, Some(&gone), None).is_some_and(|w| w.contains("cannot")), "unreadable process executable");
        let pruned = Roots { releases: &rel, old: Some(&a1), new: &rel.join("c3") };
        let exe = b2.join("bin/spira-lc");
        assert!(live_reason(&unit("c3"), &unit("c3"), &pruned, Some(&exe), None).is_some_and(|w| w.contains("cannot")), "unreadable new program");
    }

    /// A release restarts no subscriber: neither restart judgement finds a reason to restart a
    /// running broker when only the release changes.
    #[test_id(txumw36)]
    #[test]
    fn the_shipped_broker_unit_is_left_running_across_releases() {
        let t = testkit::TempDir::new("release-broker-unit");
        let rel = t.path().join("releases");
        let (a1, b2) = (rel.join("a1"), rel.join("b2"));
        let template = spira_config::resolve::checkout_root_for_process().unwrap().join("systemd/spira-broker.service");
        let text = fs::read_to_string(template).unwrap();
        let env_bin = t.path().join("env-bin");
        let host = [("SPIRA_ENV_PATH", env_bin.clone()), ("SPIRA_RUN", t.path().join("run")), ("SPIRA_TOML", t.path().join("cfg.toml"))].map(|(k, v)| (k.to_string(), v.display().to_string())).into();
        let render = |r: &Path| units::render("spira-broker.service", &text, r, &host, None, "prod").unwrap();
        let (old, new) = (render(&a1), render(&b2));
        let roots = Roots { releases: &rel, old: Some(&a1), new: &b2 };
        let started: Vec<u8> = old.lines().filter_map(|l| l.strip_prefix("Environment=")).flat_map(|l| format!("{l}\0").into_bytes()).collect();
        let started = crate::systemctl::release_of_environ(&started);
        assert_eq!(restart_reason("spira-broker-prod.service", &old, &new, &roots), None);
        assert_eq!(live_reason(&old, &new, &roots, Some(&env_bin.join("mosquitto")), started.as_deref()), None);
    }

    /// sp-sv2lw2: a stale unit is restarted unless its template states a reason for never
    /// being restarted; a bare boolean is no reason, and a left unit is named with its reason.
    #[test_id(tk7gu1x)]
    #[test]
    fn a_stale_unit_is_restarted_unless_its_template_gives_a_reason_to_leave_it() {
        let texts = [
            ("plain.service", "[Unit]\nDescription=x\n"),
            ("cockpit.service", "[Unit]\nX-Spira-NoRestart=a restart disturbs the operator's panes\n"),
            ("bare.service", "[Unit]\nX-Spira-NoRestart=true\n"),
        ];
        let stale: Vec<String> = texts.iter().map(|t| t.0.to_string()).collect();
        let (restart, left) = split_by_exemption(&stale, |u| texts.iter().find(|t| t.0 == u).and_then(|t| units::never_restart(t.1)));
        assert_eq!(restart, vec!["plain.service", "bare.service"]);
        assert_eq!(left, vec![("cockpit.service".to_string(), "a restart disturbs the operator's panes".to_string())]);
        let named = left_running(&left[0].0, "its unit changed", &left[0].1);
        assert!(named.contains("cockpit.service") && named.contains("operator's panes"), "{named}");
    }

    /// sp-sv2lw2: an earlier activation rewrote the unit file and left the process; the file
    /// now names the current release but the process started under another, so it is stale.
    #[test_id(tacgx3a)]
    #[test]
    fn a_process_started_under_another_release_is_stale_though_its_unit_file_is_current() {
        let t = testkit::TempDir::new("release-started-under");
        let rel = t.path().join("releases");
        let (a1, b2) = (rel.join("a1"), rel.join("b2"));
        let roots = Roots { releases: &rel, old: Some(&b2), new: &b2 };
        let unit = format!("[Service]\nEnvironment=SPIRA_RELEASE={}\nExecStart=/prod/bin/unit-run watch\n", b2.display());
        let started = |r: &Path| crate::systemctl::release_of_environ(format!("A=1\0SPIRA_RELEASE={}\0B=2\0", r.display()).as_bytes());
        assert!(live_reason(&unit, &unit, &roots, None, started(&a1).as_deref()).is_some_and(|w| w.contains("a1")), "started under a1");
        assert_eq!(live_reason(&unit, &unit, &roots, None, started(&b2).as_deref()), None, "started under b2");
        assert_eq!(live_reason(&unit, &unit, &roots, None, started(&rel.join("current")).as_deref()), None, "current is judged by the executable");
    }
}
