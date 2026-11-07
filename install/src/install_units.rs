//! The per-instance unit installer (`systemd/install.sh`, in Rust): render every template,
//! write what changed, enable/restart/prune per [`crate::decide`], migrate legacy unit names
//! and cockpit state, and report the end state. `deploy.sh`'s re-render step and `install`'s
//! own phase 4 both call [`run`] — one restart per changed unit, from here alone (sp-r15cf:
//! `release install-tarball --skip-restart` leaves this call as the one that matters).

use crate::decide::{decide, Action, State};
use crate::manifest::{inst_name, inst_watch_name, Manifest};
use crate::systemctl::Systemctl;
use crate::values::{render, HostValues};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// What the caller must supply beyond the manifest and host values: the control-plane
/// suspension predicate (`ctrl.sh`, read once by the caller) and whether the world is halted.
pub struct Ctx<'a> {
    pub unit_dir: &'a Path,
    pub templates_dir: &'a Path,
    pub host: &'a HostValues,
    pub manifest: &'a Manifest,
    pub systemctl: &'a dyn Systemctl,
    pub suspended: &'a dyn Fn(&str) -> bool,
    pub world_halted: bool,
    pub skip_migrate_watchers: bool,
}

#[derive(Debug, Default)]
pub struct Report {
    pub written: Vec<String>,
    pub unchanged: u32,
    pub masked: Vec<String>,
    pub enabled_actions: Vec<(String, Action)>,
    pub migrated_legacy: Vec<String>,
    pub migrated_cockpit_state: Vec<String>,
    pub pruned: Vec<String>,
    pub retired: Vec<String>,
    pub drift: Vec<String>,
    pub errors: Vec<String>,
}

/// Strip the unit's instance suffix and extension back to `ctrl.sh`'s subject name — e.g.
/// `spira-groom-prod.timer` -> `spira-groom`.
fn ctrl_subject(unit: &str, instance: &str) -> String {
    let s = unit.to_string();
    for suf in [format!("-{instance}.service"), format!("-{instance}.timer"), ".service".to_string(), ".timer".to_string()] {
        if let Some(stripped) = s.strip_suffix(&suf) {
            return stripped.to_string();
        }
    }
    s
}

/// Units whose absence disables the convergence that would re-enable them: a hand `disable`
/// on one is not honoured, only `ctrl suspend` is. The runtime hosts the reconciler.
fn is_convergence_root(unit: &str, instance: &str) -> bool {
    unit.ends_with(".service") && ctrl_subject(unit, instance) == "spira-runtime"
}

/// A mask is a symlink to `/dev/null` — detect without following it into a read that would
/// fail (a character device fails a plain existence check the way a masked unit must not).
pub fn is_masked(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false) && fs::read_link(path).map(|t| t == Path::new("/dev/null")).unwrap_or(false)
}

pub(crate) fn write_unit(dir: &Path, inst: &str, text: &str) -> io::Result<()> {
    let tmp = dir.join(format!("{inst}.new"));
    fs::write(&tmp, text)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    fs::rename(&tmp, dir.join(inst))
}

/// Refuse an unexecutable `ExecStart=`/`ExecStartPre=` target before writing a single byte —
/// the failure this catches is `203/EXEC`: systemd accepts the unit, a timer reports active,
/// and the service never runs. System binaries (`/usr/*`, `/bin/*`, `/sbin/*`) are the OS's
/// responsibility, not ours; an empty or `-`-prefixed (ignore-failure) target is skipped too.
pub fn execstart_target_ok(text: &str) -> Result<(), String> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("ExecStart=").or_else(|| line.strip_prefix("ExecStartPre=")) else { continue };
        let exec_path = rest.split_whitespace().next().unwrap_or("");
        if exec_path.is_empty() || exec_path.starts_with('-') || exec_path.starts_with("/usr/") || exec_path.starts_with("/bin/") || exec_path.starts_with("/sbin/") {
            continue;
        }
        let ok = fs::metadata(exec_path).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false);
        if !ok {
            return Err(format!("ExecStart target is not executable: {exec_path}"));
        }
    }
    Ok(())
}

/// The manifest rows rendered from [`watchdog_files`] rather than from a template file.
const WATCHDOG_ROWS: [&str; 2] = ["spira-watchdog.service", "spira-watchdog.timer"];

/// The watchdog's timer files for `platform` (S15): one pass of `spira-watchdog` every
/// `SPIRA_WATCHDOG_EVERY_SECS`, under the environment the runtime's own unit carries, named
/// per instance like every other `spira-*` unit.
pub fn watchdog_files(platform: watchdog::render::Platform, host: &HostValues) -> Result<Vec<(String, String)>, String> {
    let every: u64 = host.watchdog_every_s.trim().parse().map_err(|_| format!("{} is {:?}, not a whole number of seconds — refusing to render the watchdog's timer", watchdog::KEY_EVERY, host.watchdog_every_s))?;
    let v = host.to_map();
    let (root, prod) = (&v["SPIRA_PROD_ROOT"], &v["SPIRA_PROD"]);
    let env = vec![
        ("SPIRA_RELEASE".to_string(), v["SPIRA_RELEASE"].clone()),
        ("SPIRA_TOML".to_string(), host.toml.clone()),
        ("SPIRA_HOME".to_string(), prod.clone()),
        ("SPIRA_RUN".to_string(), host.run.clone()),
        ("PATH".to_string(), format!("{root}/bin:{root}/spira:{}:/usr/local/bin:/usr/bin:/bin{}", host.env_bin, v["SPIRA_PATH_TAIL"])),
    ];
    let spec = watchdog::render::TimerSpec { label: format!("{}-{}", watchdog::render::LABEL, host.instance), ..watchdog::render::TimerSpec::watchdog(std::path::PathBuf::from(format!("{root}/bin/spira-watchdog")), every, env) };
    watchdog::render::render(platform, &spec)
}

/// One watchdog row's file for this host's platform. This installer writes systemd units, so a
/// platform whose files carry other names (a launchd plist, a crontab line) is refused here.
fn render_watchdog_row(ctx: &Ctx, row: &str) -> Result<String, String> {
    let platform = watchdog::render::Platform::for_os(std::env::consts::OS);
    let inst = inst_name(row, &ctx.host.instance);
    let files = watchdog_files(platform, ctx.host)?;
    files.into_iter().find(|(n, _)| *n == inst).map(|(_, t)| t).ok_or_else(|| format!("{inst}: the watchdog renders {platform:?} files on {}, which this systemd installer does not install", std::env::consts::OS))
}

/// Render every template and watcher instance this manifest installs. Used by `--render` and
/// `--diff`, and by [`run`] itself so both paths use the same set.
pub fn render_all(ctx: &Ctx) -> Result<Vec<(String, String, String)>, String> {
    // (installed name, template basename, rendered text)
    let mut out = Vec::new();
    for name in ctx.manifest.template_names() {
        let text = if WATCHDOG_ROWS.contains(&name) { render_watchdog_row(ctx, name)? } else { render_template(ctx, name, None)? };
        out.push((inst_name(name, &ctx.host.instance), name.to_string(), text));
    }
    for w in &ctx.manifest.watch_names {
        let text = render_template(ctx, "spira-watch@.service", Some(w))?;
        out.push((inst_watch_name(w, &ctx.host.instance), "spira-watch@.service".to_string(), text));
    }
    Ok(out)
}

fn render_template(ctx: &Ctx, template: &str, watcher: Option<&str>) -> Result<String, String> {
    let text = fs::read_to_string(ctx.templates_dir.join(template)).map_err(|e| format!("{template}: {e}"))?;
    render(template, &text, ctx.host, watcher)
}

/// `--diff`: which installed units are missing or differ from what this box would render,
/// plus any `.service`/`.timer` file in the templates directory that no template lists
/// (`UNLISTED` — the manifest's own "forgotten unit" check).
pub fn diff(ctx: &Ctx) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let mut on_disk = Vec::new();
    for e in fs::read_dir(ctx.templates_dir).map_err(|e| e.to_string())?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if (n.ends_with(".service") || n.ends_with(".timer")) && e.path().is_file() {
            on_disk.push(n);
        }
    }
    for u in ctx.manifest.unlisted(on_disk.iter().map(|s| s.as_str())) {
        lines.push(format!("UNLISTED {u} (in this directory but absent from the manifest — it will never be installed)"));
    }
    for (inst, _tpl, text) in render_all(ctx)? {
        let dest = ctx.unit_dir.join(&inst);
        match fs::read_to_string(&dest) {
            Err(_) => lines.push(format!("MISSING  {inst} (not installed)")),
            Ok(installed) if installed != text => {
                lines.push(format!("DIFFERS  {inst}"));
                lines.extend(unified_diff(&text, &dest));
            }
            Ok(_) => {}
        }
    }
    Ok(lines)
}

/// `diff -u <rendered> <installed>`, indented four spaces — systemd/install.sh's own
/// `diff -u "$TMP/$inst" "$DEST/$inst" | sed 's/^/    /'`, dropped by this crate's rewrite
/// (sp-31dm0): `--diff` reported which unit differed but never what changed, so an operator
/// staring at "DIFFERS some.service" had to go find both files and diff them by hand.
/// Caught live by testenv's test-unit-drift.sh (sp-al35q). Best-effort: a `diff` failure
/// (binary missing, write failure) is swallowed — the bare DIFFERS line above already said
/// the one thing that must never be silent.
fn unified_diff(rendered: &str, installed_path: &Path) -> Vec<String> {
    let tmp = std::env::temp_dir().join(format!("units-install-diff-{}-{}", std::process::id(), installed_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    if fs::write(&tmp, rendered).is_err() {
        return Vec::new();
    }
    let out = spira_config::bounded::bounded("diff").arg("-u").arg(&tmp).arg(installed_path).output();
    let _ = fs::remove_file(&tmp);
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).lines().map(|l| format!("    {l}")).collect(),
        Err(_) => Vec::new(),
    }
}

/// The full install: render, write what changed, migrate legacy names and cockpit state,
/// enable/restart/prune per unit, report the end state. Mirrors `systemd/install.sh`'s main
/// body exactly, including its two accreted, harmless double-ups this port keeps rather than
/// silently "fixing" mid-rewrite (DESIGN.md "Decisions"): `loginctl enable-linger` is called
/// here AND again by the root installer's own phase, and `release session-hook install` is
/// called here unconditionally on every run.
pub fn run(ctx: &Ctx) -> Report {
    let mut r = Report::default();
    let rendered = match render_all(ctx) {
        Ok(v) => v,
        Err(e) => {
            r.errors.push(e);
            return r;
        }
    };

    let mut changed_templates: std::collections::BTreeSet<String> = Default::default();
    let mut new_units: std::collections::BTreeSet<String> = Default::default();
    let mut masked: std::collections::BTreeSet<String> = Default::default();

    for (inst, tpl, text) in &rendered {
        let dest = ctx.unit_dir.join(inst);
        if is_masked(&dest) {
            masked.insert(inst.clone());
            r.masked.push(inst.clone());
            continue;
        }
        let suspended = (ctx.suspended)(&ctrl_subject(inst, &ctx.host.instance));
        if !suspended {
            if let Err(e) = execstart_target_ok(text) {
                r.errors.push(format!("{inst}: {e}"));
                return r;
            }
        }
        match fs::read_to_string(&dest) {
            Ok(installed) if release::units::without_documentation(&installed) == release::units::without_documentation(text) => r.unchanged += 1,
            existing => {
                if existing.is_err() {
                    new_units.insert(inst.clone());
                }
                changed_templates.insert(tpl.clone());
                if let Err(e) = write_unit(ctx.unit_dir, inst, text) {
                    r.errors.push(format!("{inst}: {e}"));
                    return r;
                }
                r.written.push(inst.clone());
            }
        }
    }
    if !r.errors.is_empty() {
        return r;
    }

    if !r.written.is_empty() {
        let _ = ctx.systemctl.daemon_reload();
    }

    migrate_legacy(ctx, &mut r);
    retire_dropped_units(ctx, &rendered, &mut r);
    // Cockpit `.runtime` state migration is a filesystem concern the caller resolves paths
    // for; left to the orchestrator (root installer) since it needs SPIRA_COCKPIT/SPIRA_RUN
    // as real directories, not just the host-values map.

    let mut ordered: Vec<String> = ctx.manifest.enable(&ctx.host.instance);
    ordered.sort_by_key(|u| if u == "dolt-beads.service" { 0 } else { 1 });

    let new_by_inst: std::collections::BTreeSet<&String> = new_units.iter().collect();
    let live: Vec<String> = ordered.iter().filter(|u| !masked.contains(*u)).cloned().collect();
    let enabled_all: BTreeMap<&String, Option<String>> = live.iter().zip(ctx.systemctl.is_enabled_many(&live)).collect();
    // The service behind each timer rides along: `drain_oneshot` asks whether it is running.
    let mut active_names: Vec<String> = live.iter().flat_map(|u| [Some(u.clone()), u.strip_suffix(".timer").map(|b| format!("{b}.service"))]).flatten().collect();
    active_names.sort();
    active_names.dedup();
    let active_all: BTreeMap<&String, bool> = active_names.iter().zip(ctx.systemctl.is_active_many(&active_names)).collect();

    for u in &ordered {
        if masked.contains(u) {
            r.enabled_actions.push((u.clone(), Action::Masked));
            continue;
        }
        let subject = ctrl_subject(u, &ctx.host.instance);
        let suspended = (ctx.suspended)(&subject);
        let disabled = enabled_all[u].as_deref() == Some("disabled") && !new_by_inst.contains(u) && !is_convergence_root(u, &ctx.host.instance);
        // A changed template (spira-watch@.service) counts as a change to every instance.
        let tmpl_of_u = rendered.iter().find(|(inst, _, _)| inst == u).map(|(_, t, _)| t.clone());
        let changed = tmpl_of_u.as_deref().map(|t| changed_templates.contains(t)).unwrap_or(false);
        let action = decide(State { changed, masked: false, suspended, disabled, halted: ctx.world_halted, active: active_all[u] });
        apply(ctx, u, action, &active_all);
        r.enabled_actions.push((u.clone(), action));
        if u == "dolt-beads.service" && !ctx.world_halted {
            db_server_wait(ctx);
        }
    }

    r
}

fn apply(ctx: &Ctx, unit: &str, action: Action, was_active: &BTreeMap<&String, bool>) {
    match action {
        Action::Masked | Action::Suspended | Action::OperatorDisabled | Action::Skip => {}
        Action::Enable => {
            let _ = ctx.systemctl.enable(unit);
        }
        Action::Restart => {
            drain_oneshot(ctx, unit, was_active);
            let _ = ctx.systemctl.enable(unit);
            restart_active(ctx, unit);
        }
        Action::EnableNow => {
            drain_oneshot(ctx, unit, was_active);
            let _ = ctx.systemctl.enable_now(unit);
        }
    }
}

/// `dolt-beads.service` ships `RefuseManualStop=yes`; a restart job is an explicit stop+start,
/// which that denies outright. `kill` sends the signal directly, bypassing job control, and
/// `Restart=always` brings the process back under the just-reloaded unit file.
fn restart_active(ctx: &Ctx, unit: &str) {
    if unit.ends_with(".socket") {
        println!("install: {unit} changed — left listening; its service restarts onto it");
        return;
    }
    let base = unit.strip_suffix(".timer").unwrap_or(unit);
    if base == "dolt-beads.service" || unit == "dolt-beads.service" {
        if ctx.systemctl.kill(unit).is_ok() {
            println!("restarted {unit} (kill — RefuseManualStop=yes)");
        }
    } else {
        let _ = ctx.systemctl.restart(unit);
    }
}

/// `_drain_oneshot` (systemd/install.sh, now gone with sp-31dm0's port): a changed unit whose
/// backing service is a running oneshot is drained — polled until it finishes — before the
/// restart that follows in [`apply`], rather than restarted out from under itself mid-pass.
/// A long-running (non-oneshot) service is not drained; restarted directly. `SPIRA_DRAIN_INTERVAL`
/// overrides the 2-second poll interval (tests set it to 0 to poll without sleeping); bounded
/// at 300s, after which this proceeds anyway and says so.
fn drain_oneshot(ctx: &Ctx, unit: &str, was_active: &BTreeMap<&String, bool>) {
    let svc = if unit.ends_with(".timer") { format!("{}.service", unit.trim_end_matches(".timer")) } else { unit.to_string() };
    let running = was_active.get(&svc).copied().unwrap_or_else(|| ctx.systemctl.is_active(&svc));
    if !running || ctx.systemctl.unit_type(&svc) != "oneshot" {
        return;
    }
    println!("install: {svc} is mid-pass — waiting for it to finish");
    let interval = crate::bootstrap::nonempty_env("SPIRA_DRAIN_INTERVAL").and_then(|v| v.parse::<u64>().ok()).unwrap_or(2);
    let max = 300u64;
    let mut waited = 0u64;
    while waited < max {
        if !ctx.systemctl.is_active(&svc) {
            return;
        }
        if interval > 0 {
            std::thread::sleep(std::time::Duration::from_secs(interval));
        }
        waited += interval;
    }
    eprintln!("install: warning — {svc} did not finish within {max}s; proceeding");
}

/// `_db_server_wait` (systemd/install.sh, gone with sp-31dm0's port): after dolt-beads.service
/// is applied, hold every timer behind it in the same `ordered` pass until `bd` can read the
/// database — so a timer whose elapsed-since-boot condition is already met does not fire into
/// a server still starting (acceptance phase B, 2026-09-26: units.sh listed dolt-beads.service
/// near the end of ENABLE, after every timer; the sentinel ran first, logged DATABASE
/// UNREADABLE, exited 1 and was left failed). No database yet (a first install before phase
/// 3's seed) or no `bd` at all: nothing to wait for. A server that never answers is reported,
/// not fatal — the end-state check and the units' own failures say what is wrong more
/// precisely than a refusal here would. `SPIRA_INSTALL_DB_READY_WAIT` bounds the wait
/// (seconds, default 60); `SPIRA_DRAIN_INTERVAL` paces it (default 2, tests set it to 0).
fn db_server_wait(ctx: &Ctx) {
    let db = ctx.host.db.as_str();
    if db.is_empty() || !Path::new(db).join(".beads").is_dir() {
        return;
    }
    let bd = crate::bootstrap::nonempty_env("SPIRA_BD").unwrap_or_else(|| "bd".to_string());
    let interval = crate::bootstrap::nonempty_env("SPIRA_DRAIN_INTERVAL").and_then(|v| v.parse::<u64>().ok()).unwrap_or(2);
    let max = crate::bootstrap::nonempty_env("SPIRA_INSTALL_DB_READY_WAIT").and_then(|v| v.parse::<u64>().ok()).unwrap_or(60);
    let mut waited = 0u64;
    let mut printed_waiting = false;
    loop {
        match spira_config::bounded::bounded(&bd).args(["-C", db, "sql", "select 1"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status() {
            Ok(s) if s.success() => {
                if waited > 0 {
                    println!("install: beads database answering after {waited}s");
                }
                return;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !printed_waiting && waited == 0 => {
                // No bd on the path: nothing to wait for. Any other spawn failure (ETXTBSY on a
                // just-written script, EAGAIN under load) is transient and is retried as a miss.
                return;
            }
            _ => {}
        }
        if waited >= max {
            break;
        }
        if !printed_waiting {
            println!("install: waiting for the beads database before arming timers");
            printed_waiting = true;
        }
        if interval > 0 {
            std::thread::sleep(std::time::Duration::from_secs(interval));
        }
        waited += interval.max(1);
    }
    eprintln!("install: warning — the beads database did not answer within {max}s; arming timers anyway");
}

/// Manifest units once named `spira-watch-<x>-<instance>`, a name the watcher glob also matched.
const RENAMED_OUT_OF_WATCHER_NAMESPACE: [&str; 2] = ["refresh", "notify"];

/// Disable and delete any surviving un-suffixed `spira-*` unit — the naming scheme that
/// predates per-instance suffixes. Enumerated from the manifest's own templates, so a
/// template added here is automatically covered without a second edit.
const LEDGER: &str = ".spira-installed-units";

/// Units a previous run installed and this release no longer ships. The ledger, not a name
/// pattern, decides what is Spira's to remove.
pub(crate) fn retire_dropped_units(ctx: &Ctx, rendered: &[(String, String, String)], r: &mut Report) {
    let ledger = ctx.unit_dir.join(LEDGER);
    let now: std::collections::BTreeSet<String> = rendered.iter().map(|(inst, _, _)| inst.clone()).collect();
    let mut keep = now.clone();
    if let Ok(prev) = fs::read_to_string(&ledger) {
        for u in prev.lines().map(str::trim).filter(|u| !u.is_empty() && !u.contains('/') && !now.contains(*u)) {
            if is_masked(&ctx.unit_dir.join(u)) {
                continue;
            }
            let _ = ctx.systemctl.disable_now(u);
            let _ = fs::remove_dir_all(ctx.unit_dir.join(format!("{u}.d")));
            match fs::remove_file(ctx.unit_dir.join(u)) {
                Ok(()) => r.retired.push(u.to_string()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => r.retired.push(u.to_string()),
                Err(_) => {
                    keep.insert(u.to_string());
                }
            }
        }
    }
    retire_unledgered_units(ctx, &now, &mut keep, r);
    retire_named_units(ctx, &now, r);
    if !r.retired.is_empty() {
        let _ = ctx.systemctl.daemon_reload();
    }
    let body: String = keep.iter().map(|u| format!("{u}\n")).collect();
    if fs::read_to_string(&ledger).ok().as_deref() != Some(body.as_str()) {
        let _ = fs::write(&ledger, body);
    }
}

/// Units outside the `spira-*` namespace that an earlier release shipped and none does now; the
/// ledger may never have listed them, and no name pattern marks them as Spira's.
const RETIRED_UNITS: [&str; 1] = ["dolt-beads-test.service"];

fn retire_named_units(ctx: &Ctx, now: &std::collections::BTreeSet<String>, r: &mut Report) {
    for u in RETIRED_UNITS {
        let path = ctx.unit_dir.join(u);
        if now.contains(u) || r.retired.iter().any(|x| x == u) || is_masked(&path) || !path.exists() {
            continue;
        }
        let _ = ctx.systemctl.disable_now(u);
        let _ = fs::remove_dir_all(ctx.unit_dir.join(format!("{u}.d")));
        if fs::remove_file(&path).is_ok() {
            r.retired.push(u.to_string());
        }
    }
}

/// A unit installed before the ledger existed is absent from it. A `spira-*` unit carrying this
/// instance's suffix is Spira's by name and is retired like a ledgered one; any other `spira-*`
/// unit the manifest does not ship is reported as drift and left alone.
fn retire_unledgered_units(ctx: &Ctx, now: &std::collections::BTreeSet<String>, keep: &mut std::collections::BTreeSet<String>, r: &mut Report) {
    const EXTS: [&str; 4] = [".service", ".timer", ".socket", ".path"];
    let Ok(entries) = fs::read_dir(ctx.unit_dir) else { return };
    let mut found: std::collections::BTreeSet<String> = Default::default();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let stem = name.strip_suffix(".d").unwrap_or(&name);
        if stem.starts_with("spira-") && EXTS.iter().any(|x| stem.ends_with(x)) && !stem.contains('@') {
            found.insert(stem.to_string());
        }
    }
    for u in found {
        if now.contains(&u) || r.retired.contains(&u) || is_masked(&ctx.unit_dir.join(&u)) {
            continue;
        }
        if !EXTS.iter().any(|x| u.strip_suffix(x).is_some_and(|b| b.ends_with(&format!("-{}", ctx.host.instance)))) {
            r.drift.push(u);
            continue;
        }
        let _ = ctx.systemctl.disable_now(&u);
        let _ = fs::remove_dir_all(ctx.unit_dir.join(format!("{u}.d")));
        match fs::remove_file(ctx.unit_dir.join(&u)) {
            Ok(()) => r.retired.push(u),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => r.retired.push(u),
            Err(_) => {
                keep.insert(u);
            }
        }
    }
}

fn migrate_legacy(ctx: &Ctx, r: &mut Report) {
    for name in ctx.manifest.template_names() {
        if name == "spira-watch@.service" {
            continue;
        }
        if !(name.starts_with("spira-") && (name.ends_with(".service") || name.ends_with(".timer"))) {
            continue;
        }
        let installed = inst_name(name, &ctx.host.instance);
        if installed == name {
            continue; // a shared unit (its plain name IS the installed name)
        }
        if ctx.systemctl.disable_now(name).is_ok() {
            let _ = fs::remove_file(ctx.unit_dir.join(name));
            r.migrated_legacy.push(name.to_string());
        }
    }
    for base in RENAMED_OUT_OF_WATCHER_NAMESPACE {
        for ext in ["service", "timer"] {
            let old = format!("spira-watch-{base}-{}.{ext}", ctx.host.instance);
            if ctx.systemctl.disable_now(&old).is_ok() {
                let _ = fs::remove_file(ctx.unit_dir.join(&old));
                r.migrated_legacy.push(old);
            }
        }
    }
    if ctx.skip_migrate_watchers {
        println!("install: --no-migrate-watchers: sparing watcher legacy units (spira-watch-* and spira-watch@*)");
    } else {
        for w in &ctx.manifest.watch_names {
            for old in [format!("spira-watch-{w}.service"), format!("spira-watch@{w}.service")] {
                if ctx.systemctl.disable_now(&old).is_ok() {
                    let _ = fs::remove_file(ctx.unit_dir.join(&old));
                    r.migrated_legacy.push(old);
                }
            }
        }
    }
}

/// Migrate cockpit's pre-`SPIRA_RUN` state files from `<cockpit>/.runtime` into `<run>`.
/// Never overwrites; a no-op once the old location is gone or the new one is already populated.
pub fn migrate_cockpit_state(cockpit: &Path, run: &Path) -> Vec<String> {
    let old_dir = cockpit.join(".runtime");
    if !old_dir.is_dir() {
        return Vec::new();
    }
    let _ = fs::create_dir_all(run);
    let mut moved = Vec::new();
    for f in ["answered-seen.json", "self-closed"] {
        let old = old_dir.join(f);
        let new = run.join(f);
        if !old.is_file() || new.exists() {
            continue;
        }
        if fs::copy(&old, &new).is_ok() {
            moved.push(f.to_string());
        }
    }
    moved
}

/// Units installed and matching `pattern` (a `spira-watch-*-<instance>` or
/// `spira-<x>-<instance>` glob, already expanded by the caller into concrete names found on
/// disk) that the manifest's `expected` set does not name — units.sh's two prune passes,
/// generalised over a plain name list so they are testable without real `systemctl`.
pub fn prune_targets<'a>(on_disk_or_known: impl IntoIterator<Item = &'a str>, expected: &std::collections::BTreeSet<String>, never_prune: impl Fn(&str) -> bool) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for u in on_disk_or_known {
        if !seen.insert(u.to_string()) {
            continue;
        }
        if never_prune(u) || expected.contains(u) {
            continue;
        }
        out.push(u.to_string());
    }
    out
}

/// Every installed name the manifest expects (`ENABLE` set unioned with every non-enabled
/// template's own installed name) — the prune passes' "a manifest unit, not a stray" set.
pub fn expected_installed_names(manifest: &Manifest, instance: &str) -> std::collections::BTreeSet<String> {
    let mut s: std::collections::BTreeSet<String> = manifest.template_names().iter().map(|n| inst_name(n, instance)).collect();
    s.extend(manifest.watch_names.iter().map(|w| inst_watch_name(w, instance)));
    s
}

/// The end-state check's own split of what did not reach active (sp-e5v53-4): a watcher
/// (every `spira-watch-*` instance of the one template, DESIGN.md "the watcher template")
/// reaches outside the box it runs on — GitHub, mail, the forge — which a test fixture's
/// container, by design, cannot; it genuinely not reaching active there says nothing about
/// whether the units a suite actually needs are fine, so `in_testenv` names it in
/// `warn_only` rather than `fatal`, and the run does not fault for it alone. Outside a test
/// fixture, or for any unit that is not a watcher, not active is the same hard failure it
/// always was — every one of those lands in `fatal`.
pub struct NotActiveSplit {
    pub warn_only: Vec<String>,
    pub fatal: Vec<String>,
}

pub fn split_not_active(not_active: &[String], in_testenv: bool, instance: &str) -> NotActiveSplit {
    if !in_testenv {
        return NotActiveSplit { warn_only: Vec::new(), fatal: not_active.to_vec() };
    }
    // lc-serve.service serves the lifecycle store, which a test fixture's install never builds
    // (that is spira-install's phase 4.5, against a real Dolt server): in a fixture it cannot
    // reach active either, and is named, never silently dropped (sp-xfqnr). spira-runtime opens
    // its journal on that store at start, so it shares the exemption.
    let runtime = crate::manifest::inst_name("spira-runtime.service", instance);
    let (watchers, other): (Vec<String>, Vec<String>) = not_active
        .iter()
        .cloned()
        .partition(|u| u.starts_with("spira-watch-") || u == "lc-serve.service" || u == "lc-serve.socket" || *u == runtime);
    NotActiveSplit { warn_only: watchers, fatal: other }
}

#[allow(dead_code)]
fn _unused(_: &BTreeMap<String, String>) {}

#[cfg(test)]
mod lc_serve_split_tests {
    use super::*;
    #[test]
    fn lc_serve_not_active_is_warn_only_in_a_fixture_and_fatal_outside() {
        let na = vec!["lc-serve.service".to_string(), "spira-sentinel-prod.service".to_string()];
        let f = split_not_active(&na, true, "571fae10b413");
        assert_eq!(f.warn_only, vec!["lc-serve.service".to_string()]);
        assert_eq!(f.fatal, vec!["spira-sentinel-prod.service".to_string()]);
        let p = split_not_active(&na, false, "571fae10b413");
        assert!(p.warn_only.is_empty());
        assert_eq!(p.fatal.len(), 2);
    }

    #[test]
    fn spira_runtime_instance_is_warn_only_in_a_fixture_but_its_neighbours_are_not() {
        let na = vec!["spira-runtime-571fae10b413.service".to_string(), "spira-runtime-watch-571fae10b413.service".to_string(), "spira-runtime-e48b6112ddef-sim.service".to_string()];
        let f = split_not_active(&na, true, "571fae10b413");
        assert_eq!(f.warn_only, vec![na[0].clone()]);
        assert_eq!(f.fatal, vec![na[1].clone(), na[2].clone()]);
        let sim = split_not_active(&na, true, "e48b6112ddef-sim");
        assert_eq!(sim.warn_only, vec![na[2].clone()]);
        assert_eq!(split_not_active(&na, false, "571fae10b413").fatal.len(), 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{build, Inputs};
    use crate::systemctl::{FakeSystemctl, FakeUnit};
    #[test]
    fn the_enable_loop_asks_systemd_about_every_unit_in_two_calls_not_two_per_unit() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };

        let r = run(&ctx);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(!r.enabled_actions.is_empty(), "positive control: units were walked");
        assert_eq!(sc.batched_queries.get(), 2);
        assert_eq!(sc.single_queries.get(), 0);
    }

    #[test]
    fn the_drain_of_a_running_oneshot_reads_the_batched_answer_then_polls_live() {
        let td = tempdir();
        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        sc.set("spira-sentinel-prod.service", FakeUnit { active: false, enabled: None, kind: "oneshot".into(), file_listed: true });
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &td, templates_dir: &td, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let svc = "spira-sentinel-prod.service".to_string();
        let _env = testkit::env(&[("SPIRA_DRAIN_INTERVAL", Some("0"))]);

        drain_oneshot(&ctx, "spira-sentinel-prod.timer", &BTreeMap::from([(&svc, true)]));
        assert_eq!(sc.single_queries.get(), 1, "one live poll to see it finished; the was-running answer came from the batch");

        drain_oneshot(&ctx, "spira-sentinel-prod.timer", &BTreeMap::from([(&svc, false)]));
        assert_eq!(sc.single_queries.get(), 1, "a service the batch saw idle is not polled");
    }

    use std::fs;

    fn host() -> HostValues {
        HostValues { toml: "/h/cfg.toml".into(), home: "/h".into(), repo: "/h".into(), run: "/run".into(), db: "/db".into(), cockpit: "/h/cockpit".into(), dolt_data: "".into(), dolt: "/usr/bin/dolt".into(), prod: "".into(), instance: "prod".into(), snap_stale_s: "600".into(), watchtower_start_timeout_s: "360".into(), path_tail: "".into(), env_bin: "/h/.local/lib/spira-env/bin".into(), sccache_dav_addr: "".into(), repo_map: "".into(), lc_password_file: "/h/lc.credential".into(), watchdog_every_s: "60".into() }
    }

    fn tiny_manifest() -> Manifest {
        crate::manifest::Manifest { units: vec![crate::manifest::Template { name: "spira-example.service".into(), enable: false }, crate::manifest::Template { name: "spira-example.timer".into(), enable: true }], ..Default::default() }
    }

    fn write_templates(dir: &Path) {
        fs::write(dir.join("spira-example.service"), "[Service]\nExecStart=/bin/true\n").unwrap();
        fs::write(dir.join("spira-example.timer"), "[Unit]\nUnit=spira-example.service\n[Timer]\nOnCalendar=*:0/5\n").unwrap();
    }

    #[test]
    fn execstart_ok_skips_system_dirs_and_refuses_a_missing_target() {
        assert!(execstart_target_ok("ExecStart=/usr/bin/true\n").is_ok());
        assert!(execstart_target_ok("ExecStart=-/does/not/exist\n").is_ok()); // "-" ignores failure
        assert!(execstart_target_ok("ExecStart=/does/not/exist\n").is_err());
    }

    #[test]
    fn a_unit_a_previous_release_installed_and_this_one_drops_is_stopped_and_removed() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        fs::write(unit_dir.join("beads-push.service"), "x").unwrap();
        fs::write(unit_dir.join("beads-push.timer"), "x").unwrap();
        fs::write(unit_dir.join("unrelated.service"), "x").unwrap();
        fs::write(unit_dir.join(LEDGER), "beads-push.service\nbeads-push.timer\n").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.retired, vec!["beads-push.service".to_string(), "beads-push.timer".to_string()]);
        assert!(!unit_dir.join("beads-push.service").exists());
        assert!(!unit_dir.join("beads-push.timer").exists());
        assert!(sc.disabled_now.borrow().contains(&"beads-push.timer".to_string()));
        assert!(unit_dir.join("unrelated.service").exists());
        assert!(!sc.disabled_now.borrow().contains(&"unrelated.service".to_string()));
        let ledger = fs::read_to_string(unit_dir.join(LEDGER)).unwrap();
        assert!(ledger.contains("spira-example-prod.timer") && !ledger.contains("beads-push"));
        assert!(run(&ctx).retired.is_empty());
    }

    #[test]
    fn an_upgrade_removes_a_retired_round_template_unit_and_its_dropin() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(unit_dir.join("spira-round-template-prod.service.d")).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        fs::write(unit_dir.join("spira-round-template-prod.service"), "x").unwrap();
        fs::write(unit_dir.join("spira-round-template-prod.timer"), "x").unwrap();
        fs::write(unit_dir.join("spira-round-template-prod.service.d/retired.conf"), "[Unit]\n").unwrap();
        fs::write(unit_dir.join(LEDGER), "spira-round-template-prod.service\nspira-round-template-prod.timer\n").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.retired.contains(&"spira-round-template-prod.timer".to_string()), "{:?}", r.retired);
        assert!(!unit_dir.join("spira-round-template-prod.service").exists());
        assert!(!unit_dir.join("spira-round-template-prod.timer").exists());
        assert!(!unit_dir.join("spira-round-template-prod.service.d").exists());
        assert!(sc.disabled_now.borrow().contains(&"spira-round-template-prod.timer".to_string()));
    }

    #[test]
    fn a_dropped_unit_is_retired_though_the_ledger_never_listed_it() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(unit_dir.join("spira-gone-prod.service.d")).unwrap();
        fs::create_dir_all(unit_dir.join("spira-orphan-prod.timer.d")).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        fs::write(unit_dir.join("spira-gone-prod.service"), "x").unwrap();
        fs::write(unit_dir.join("spira-gone-prod.timer"), "x").unwrap();
        fs::write(unit_dir.join("spira-gone-prod.service.d/retired.conf"), "[Unit]\n").unwrap();
        fs::write(unit_dir.join("spira-orphan-prod.timer.d/x.conf"), "[Unit]\n").unwrap();
        fs::write(unit_dir.join("spira-other-test.timer"), "x").unwrap();
        fs::write(unit_dir.join("mine.service"), "x").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        for u in ["spira-gone-prod.service", "spira-gone-prod.timer", "spira-orphan-prod.timer"] {
            assert!(r.retired.contains(&u.to_string()), "{u}: {:?}", r.retired);
            assert!(!unit_dir.join(u).exists() && !unit_dir.join(format!("{u}.d")).exists());
            assert!(sc.disabled_now.borrow().contains(&u.to_string()));
        }
        assert_eq!(r.drift, vec!["spira-other-test.timer".to_string()]);
        assert!(unit_dir.join("spira-other-test.timer").exists() && unit_dir.join("mine.service").exists());
    }

    #[test]
    fn the_shared_test_dolt_unit_is_stopped_and_removed_though_no_ledger_names_it() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        fs::write(unit_dir.join("dolt-beads-test.service"), "x").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(!unit_dir.join("dolt-beads-test.service").exists());
        assert!(r.retired.contains(&"dolt-beads-test.service".to_string()), "{:?}", r.retired);
        assert!(sc.disabled_now.borrow().contains(&"dolt-beads-test.service".to_string()));
        assert!(run(&ctx).retired.is_empty(), "a second run has nothing left to retire");
    }

    #[test]
    fn a_fresh_install_writes_and_enables_and_a_clean_rerun_changes_nothing() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };

        let r1 = run(&ctx);
        assert!(r1.errors.is_empty(), "{:?}", r1.errors);
        assert_eq!(r1.written.len(), 2);
        assert!(unit_dir.join("spira-example-prod.service").exists());
        assert!(sc.enabled_now.borrow().contains(&"spira-example-prod.timer".to_string()));

        let r2 = run(&ctx);
        assert!(r2.errors.is_empty());
        assert_eq!(r2.written.len(), 0);
        assert_eq!(r2.unchanged, 2);
        // Already active and unchanged: Skip, no second enable/restart call recorded.
        assert!(matches!(r2.enabled_actions.iter().find(|(u, _)| u == "spira-example-prod.timer").unwrap().1, Action::Skip));
    }

    #[test]
    fn a_hand_disabled_runtime_is_re_enabled_but_other_timers_and_suspends_hold() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        fs::write(tmpl_dir.join("spira-runtime.service"), "[Service]\nExecStart=/bin/true\n").unwrap();

        let mut manifest = build(&Inputs { instance: "prod".into(), dolt_data_set: false, dolt_durable_set: false, sccache_dav_addr_set: false, repo_is_git_checkout: true, lc_system_mode: false, watch_names: Ok(vec![]) }).unwrap();
        manifest.units.retain(|u| u.name == "spira-runtime.service");
        manifest.units.extend(tiny_manifest().units);
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        assert!(run(&ctx).errors.is_empty());

        for (t, kind) in [("spira-example-prod.timer", "oneshot"), ("spira-runtime-prod.service", "notify")] {
            sc.set(t, FakeUnit { active: false, enabled: Some("disabled".into()), kind: kind.into(), file_listed: true });
        }
        let r = run(&ctx);
        let act = |r: &Report, u: &str| r.enabled_actions.iter().find(|(n, _)| n == u).unwrap().1;
        assert!(matches!(act(&r, "spira-runtime-prod.service"), Action::EnableNow), "{:?}", act(&r, "spira-runtime-prod.service"));
        assert!(matches!(act(&r, "spira-example-prod.timer"), Action::OperatorDisabled));

        let only_runtime = |s: &str| s == "spira-runtime";
        let ctx2 = Ctx { suspended: &only_runtime, ..ctx };
        let r = run(&ctx2);
        assert!(matches!(act(&r, "spira-runtime-prod.service"), Action::Suspended));
    }

    fn watchdog_host(prod: &str) -> HostValues {
        HostValues { prod: prod.into(), watchdog_every_s: "60".into(), ..host() }
    }

    #[test]
    fn the_watchdog_renders_a_service_and_timer_on_linux_and_a_plist_on_macos() {
        let h = watchdog_host("/rel/current/spira");
        let env = "Environment=SPIRA_RELEASE=/rel/current\nEnvironment=SPIRA_TOML=/h/cfg.toml\nEnvironment=SPIRA_HOME=/rel/current/spira\nEnvironment=SPIRA_RUN=/run\nEnvironment=PATH=/rel/current/bin:/rel/current/spira:/h/.local/lib/spira-env/bin:/usr/local/bin:/usr/bin:/bin\n";
        assert_eq!(
            watchdog_files(watchdog::render::Platform::Systemd, &h).unwrap(),
            vec![
                ("spira-watchdog-prod.service".to_string(), format!("[Unit]\nDescription=spira-watchdog-prod — one pass: restart and page when the scheduler is wedged or absent\n\n[Service]\nType=oneshot\n{env}ExecStart=/rel/current/bin/spira-watchdog pass\nTimeoutStartSec=60\n")),
                ("spira-watchdog-prod.timer".to_string(), "[Unit]\nDescription=spira-watchdog-prod timer\n\n[Timer]\nOnBootSec=60s\nOnUnitActiveSec=60s\nAccuracySec=1s\nUnit=spira-watchdog-prod.service\n\n[Install]\nWantedBy=timers.target\n".to_string()),
            ]
        );
        let plist = watchdog_files(watchdog::render::Platform::Launchd, &h).unwrap();
        assert_eq!(plist.len(), 1);
        assert_eq!(plist[0].0, "spira-watchdog-prod.plist");
        assert_eq!(
            plist[0].1,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>spira-watchdog-prod</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>/rel/current/bin/spira-watchdog</string>\n    <string>pass</string>\n  </array>\n  <key>EnvironmentVariables</key>\n  <dict>\n    <key>SPIRA_RELEASE</key>\n    <string>/rel/current</string>\n    <key>SPIRA_TOML</key>\n    <string>/h/cfg.toml</string>\n    <key>SPIRA_HOME</key>\n    <string>/rel/current/spira</string>\n    <key>SPIRA_RUN</key>\n    <string>/run</string>\n    <key>PATH</key>\n    <string>/rel/current/bin:/rel/current/spira:/h/.local/lib/spira-env/bin:/usr/local/bin:/usr/bin:/bin</string>\n  </dict>\n  <key>StartInterval</key>\n  <integer>60</integer>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n"
        );
        let unset = HostValues { watchdog_every_s: String::new(), ..h };
        assert!(watchdog_files(watchdog::render::Platform::Systemd, &unset).unwrap_err().contains("SPIRA_WATCHDOG_EVERY_SECS"), "no interval is a refusal, never a default");
    }

    /// law-periodic-tool-needs-a-tested-timer: the watchdog's timer is in the manifest, enabled,
    /// written beside the other units and enabled by the install; this fails if the
    /// registration is removed.
    #[test]
    fn the_watchdog_timer_is_registered_written_and_enabled() {
        let m = build(&Inputs { instance: "prod".into(), dolt_data_set: false, dolt_durable_set: false, sccache_dav_addr_set: false, repo_is_git_checkout: true, lc_system_mode: false, watch_names: Ok(vec![]) }).unwrap();
        assert!(m.units.iter().any(|u| u.name == "spira-watchdog.timer" && u.enable), "the watchdog timer is registered and enabled");
        assert!(m.units.iter().any(|u| u.name == "spira-watchdog.service" && !u.enable), "its service is installed, started by the timer");
        assert!(m.enable("prod").contains(&"spira-watchdog-prod.timer".to_string()));

        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(td.join("rel/bin")).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        testkit::write_exe(td.join("rel/bin/spira-watchdog"), "exit 0\n");
        let mut manifest = m.clone();
        manifest.units.retain(|u| u.name.starts_with("spira-watchdog."));
        manifest.units.extend(tiny_manifest().units);
        let h = watchdog_host(&td.join("rel/spira").to_string_lossy());
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        for (name, text) in watchdog_files(watchdog::render::Platform::Systemd, &h).unwrap() {
            assert!(r.written.contains(&name), "{name} written: {:?}", r.written);
            assert_eq!(fs::read_to_string(unit_dir.join(&name)).unwrap(), text);
        }
        assert!(r.enabled_actions.iter().any(|(u, a)| u == "spira-watchdog-prod.timer" && matches!(a, Action::EnableNow)), "{:?}", r.enabled_actions);
        assert!(fs::read_to_string(unit_dir.join(LEDGER)).unwrap().contains("spira-watchdog-prod.timer"), "a later release that drops it retires it");
    }

    /// The watcher names a box installs from the shipped manifest's `daemon` rows plus `overlay`.
    fn shipped_daemon_rows(overlay: &[&str]) -> Vec<String> {
        let text = fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/watchers")).unwrap();
        let mut names: Vec<String> = text.lines().filter(|l| !l.trim_start().starts_with('#')).filter_map(|l| l.split('|').collect::<Vec<_>>().get(..2).filter(|f| f[1].trim() == "daemon").map(|f| f[0].trim().trim_start_matches('?').to_string())).collect();
        names.extend(overlay.iter().map(|s| s.to_string()));
        names
    }

    /// The `spira-*-<instance>.timer` and `spira-watch-*` units `rendered` holds outside the
    /// scheduler's keep list (design W2): its watchdog, the store's boot check, and the
    /// operator's repo-events overlay row.
    fn outside_the_keep_list(rendered: &[(String, String, String)]) -> Vec<String> {
        const KEEP: [&str; 3] = ["spira-watchdog-prod.timer", "spira-dolt-boot-check-prod.timer", "spira-watch-repo-events-prod.service"];
        rendered.iter().map(|(u, _, _)| u.as_str()).filter(|u| (u.starts_with("spira-") && u.ends_with("-prod.timer")) || u.starts_with("spira-watch-")).filter(|u| !KEEP.contains(u)).map(str::to_string).collect()
    }

    /// W2 of the scheduler consolidation: rendering the manifest, with every conditional input
    /// on, the shipped watchers and the operator's one overlay row, yields no loop timer and no
    /// watcher unit outside the keep list. The loop runs in `spira-runtime serve`.
    #[test]
    fn rendering_the_manifest_yields_no_loop_timer_and_no_watcher_outside_the_keep_list() {
        let tmpl_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd");
        let td = tempdir();
        let unit_dir = td.join("units");
        let manifest = build(&Inputs { instance: "prod".into(), dolt_data_set: true, dolt_durable_set: true, sccache_dav_addr_set: true, repo_is_git_checkout: true, lc_system_mode: false, watch_names: Ok(shipped_daemon_rows(&["repo-events"])) }).unwrap();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let rendered = render_all(&ctx).unwrap();
        assert!(rendered.iter().any(|(u, _, _)| u == "spira-runtime-prod.service"), "positive control: the scheduler renders");
        assert!(rendered.iter().any(|(u, _, _)| u == "spira-watchdog-prod.timer"), "positive control: a kept timer renders");
        let mut planted = rendered.clone();
        planted.push(("spira-sentinel-prod.timer".into(), "spira-sentinel.timer".into(), String::new()));
        planted.push(("spira-watch-queue-watch-prod.service".into(), "spira-watch@.service".into(), String::new()));
        assert_eq!(outside_the_keep_list(&planted).len(), outside_the_keep_list(&rendered).len() + 2, "positive control: the check sees a loop timer and a watcher");
        let strays = outside_the_keep_list(&rendered);
        assert!(strays.is_empty(), "units the scheduler replaces are still rendered: {strays:?}");
    }

    #[test]
    fn the_reconciler_has_no_timer_in_the_manifest() {
        let m = build(&Inputs { instance: "prod".into(), dolt_data_set: false, dolt_durable_set: false, sccache_dav_addr_set: false, repo_is_git_checkout: true, lc_system_mode: false, watch_names: Ok(vec![]) }).unwrap();
        assert!(!m.units.iter().any(|u| u.name.starts_with("spira-reconciler.")), "the runtime hosts the pass; its timer and service are retired");
        assert!(m.units.iter().any(|u| u.name == "spira-watchdog.timer"), "positive control: the manifest still ships timers");
        assert!(!m.enable("prod").iter().any(|u| u == "spira-reconciler-prod.timer"));
    }

    #[test]
    fn an_installed_reconciler_timer_and_service_are_retired_by_the_release_that_drops_them() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        for u in ["spira-reconciler-prod.service", "spira-reconciler-prod.timer"] {
            fs::write(unit_dir.join(u), "x").unwrap();
        }
        fs::write(unit_dir.join(LEDGER), "spira-reconciler-prod.service\nspira-reconciler-prod.timer\n").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.retired.contains(&"spira-reconciler-prod.timer".to_string()), "{:?}", r.retired);
        assert!(!unit_dir.join("spira-reconciler-prod.timer").exists() && !unit_dir.join("spira-reconciler-prod.service").exists());
        assert!(sc.disabled_now.borrow().contains(&"spira-reconciler-prod.timer".to_string()));
    }

    #[test]
    fn install_migrates_the_old_watcher_namespaced_units_away() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        for old in ["spira-watch-refresh-prod.service", "spira-watch-refresh-prod.timer", "spira-watch-notify-prod.service", "spira-watch-notify-prod.timer"] {
            fs::write(unit_dir.join(old), "[Service]\n").unwrap();
        }
        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        for old in ["spira-watch-refresh-prod.service", "spira-watch-refresh-prod.timer", "spira-watch-notify-prod.service", "spira-watch-notify-prod.timer"] {
            assert!(!unit_dir.join(old).exists(), "{old} survived");
            assert!(sc.disabled_now.borrow().contains(&old.to_string()), "{old} not disabled");
        }
    }

    #[test]
    fn a_masked_unit_is_never_written_or_restarted() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        std::os::unix::fs::symlink("/dev/null", unit_dir.join("spira-example-prod.service")).unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty());
        assert!(r.masked.contains(&"spira-example-prod.service".to_string()));
        assert!(!r.written.contains(&"spira-example-prod.service".to_string()));
    }

    #[test]
    fn a_changed_and_active_unit_restarts_rather_than_enables_fresh() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        // The SERVICE matches what will be rendered; only the TIMER's own installed content
        // differs (a different OnCalendar). `_CHANGED` in the original bash is keyed by
        // installed unit name, not by template — a changed `.service` does not, by itself,
        // mark its paired `.timer` changed (the enable loop's own `$tmpl` lookup key can
        // never match a per-instance name, which never contains '@' — a dead check the
        // original bash carried after the per-instance naming migration; this port drops it
        // rather than preserving a check that can no longer fire).
        fs::write(unit_dir.join("spira-example-prod.service"), "[Service]\nExecStart=/bin/true\n").unwrap();
        fs::write(unit_dir.join("spira-example-prod.timer"), "[Unit]\nUnit=spira-example-prod.service\n[Timer]\nOnCalendar=*:0/1\n").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        sc.set("spira-example-prod.timer", FakeUnit { active: true, enabled: Some("enabled".into()), kind: "oneshot".into(), file_listed: true });
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(sc.restarts.borrow().contains(&"spira-example-prod.timer".to_string()));
        assert!(matches!(r.enabled_actions.iter().find(|(u, _)| u == "spira-example-prod.timer").unwrap().1, Action::Restart));
    }

    fn tempdir() -> testkit::TempDir {
        testkit::TempDir::new("install-units-test")
    }

    // ---- split_not_active (sp-e5v53-4) ------------------------------------------------

    /// THE PRODUCTION BUG: concierge/sp-kfimz, sp-0ffox (x2) — the base trial's install
    /// step faulted on three watcher units (pr-notify, inbox-keeper, publish-backlog —
    /// every one that reaches outside the box: GitHub, mail, the forge) never reaching
    /// active inside the test container, faulting a run whose suite tests none of them.
    /// A fourth, purely local watcher (queue-watch) was never in that list — this is why
    /// the split is by name, not "every watcher, unconditionally".
    #[test]
    fn inside_a_test_fixture_only_watchers_are_named_and_the_run_does_not_fault_for_them() {
        let not_active = vec![
            "spira-watch-pr-notify-abc123.service".to_string(),
            "spira-watch-inbox-keeper-abc123.service".to_string(),
            "spira-watch-publish-backlog-abc123.service".to_string(),
        ];
        let split = split_not_active(&not_active, true, "t");
        assert_eq!(split.warn_only, not_active, "named — never silently dropped");
        assert!(split.fatal.is_empty(), "no non-watcher unit, so nothing faults the run");
    }

    #[test]
    fn inside_a_test_fixture_a_non_watcher_unit_still_faults() {
        let not_active = vec![
            "spira-watch-pr-notify-abc123.service".to_string(),
            "spira-example-abc123.service".to_string(),
        ];
        let split = split_not_active(&not_active, true, "t");
        assert_eq!(split.warn_only, vec!["spira-watch-pr-notify-abc123.service".to_string()]);
        assert_eq!(split.fatal, vec!["spira-example-abc123.service".to_string()]);
    }

    #[test]
    fn outside_a_test_fixture_a_watcher_not_active_still_faults_as_it_always_did() {
        let not_active = vec!["spira-watch-pr-notify-prod.service".to_string()];
        let split = split_not_active(&not_active, false, "t");
        assert!(split.warn_only.is_empty(), "production's own strictness is unchanged");
        assert_eq!(split.fatal, not_active);
    }

    #[test]
    fn with_nothing_not_active_both_lists_are_empty() {
        let split = split_not_active(&[], true, "t");
        assert!(split.warn_only.is_empty() && split.fatal.is_empty());
    }
}
