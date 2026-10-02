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
use std::process::Command;

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

/// Render every template and watcher instance this manifest installs. Used by `--render` and
/// `--diff`, and by [`run`] itself so both paths use the same set.
pub fn render_all(ctx: &Ctx) -> Result<Vec<(String, String, String)>, String> {
    // (installed name, template basename, rendered text)
    let mut out = Vec::new();
    for name in ctx.manifest.template_names() {
        let text = render_template(ctx, name, None)?;
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
    let out = Command::new("diff").arg("-u").arg(&tmp).arg(installed_path).output();
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
            Ok(installed) if installed == *text => r.unchanged += 1,
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
    // Cockpit `.runtime` state migration is a filesystem concern the caller resolves paths
    // for; left to the orchestrator (root installer) since it needs SPIRA_COCKPIT/SPIRA_RUN
    // as real directories, not just the host-values map.

    let mut ordered: Vec<String> = ctx.manifest.enable(&ctx.host.instance);
    ordered.sort_by_key(|u| if u == "dolt-beads.service" { 0 } else { 1 });

    let new_by_inst: std::collections::BTreeSet<&String> = new_units.iter().collect();
    for u in &ordered {
        if masked.contains(u) {
            r.enabled_actions.push((u.clone(), Action::Masked));
            continue;
        }
        let subject = ctrl_subject(u, &ctx.host.instance);
        let suspended = (ctx.suspended)(&subject);
        let enabled_str = ctx.systemctl.is_enabled(u);
        let disabled = enabled_str.as_deref() == Some("disabled") && !new_by_inst.contains(u);
        // A changed template (spira-watch@.service) counts as a change to every instance.
        let tmpl_of_u = rendered.iter().find(|(inst, _, _)| inst == u).map(|(_, t, _)| t.clone());
        let changed = tmpl_of_u.as_deref().map(|t| changed_templates.contains(t)).unwrap_or(false);
        let active = ctx.systemctl.is_active(u);
        let action = decide(State { changed, masked: false, suspended, disabled, halted: ctx.world_halted, active });
        apply(ctx, u, action);
        r.enabled_actions.push((u.clone(), action));
        if u == "dolt-beads.service" && !ctx.world_halted {
            db_server_wait(ctx);
        }
    }

    r
}

fn apply(ctx: &Ctx, unit: &str, action: Action) {
    match action {
        Action::Masked | Action::Suspended | Action::OperatorDisabled | Action::Skip => {}
        Action::Enable => {
            let _ = ctx.systemctl.enable(unit);
        }
        Action::Restart => {
            drain_oneshot(ctx, unit);
            let _ = ctx.systemctl.enable(unit);
            restart_active(ctx, unit);
        }
        Action::EnableNow => {
            drain_oneshot(ctx, unit);
            let _ = ctx.systemctl.enable_now(unit);
        }
    }
}

/// `dolt-beads.service` ships `RefuseManualStop=yes`; a restart job is an explicit stop+start,
/// which that denies outright. `kill` sends the signal directly, bypassing job control, and
/// `Restart=always` brings the process back under the just-reloaded unit file.
fn restart_active(ctx: &Ctx, unit: &str) {
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
fn drain_oneshot(ctx: &Ctx, unit: &str) {
    let svc = if unit.ends_with(".timer") { format!("{}.service", unit.trim_end_matches(".timer")) } else { unit.to_string() };
    if !ctx.systemctl.is_active(&svc) || ctx.systemctl.unit_type(&svc) != "oneshot" {
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
        match Command::new(&bd).args(["-C", db, "sql", "select 1"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status() {
            Ok(s) if s.success() => {
                if waited > 0 {
                    println!("install: beads database answering after {waited}s");
                }
                return;
            }
            Err(_) if !printed_waiting && waited == 0 => {
                // bd cannot even be run (command -v would have failed) — nothing to wait for.
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

pub fn split_not_active(not_active: &[String], in_testenv: bool) -> NotActiveSplit {
    if !in_testenv {
        return NotActiveSplit { warn_only: Vec::new(), fatal: not_active.to_vec() };
    }
    let (watchers, other): (Vec<String>, Vec<String>) =
        not_active.iter().cloned().partition(|u| u.starts_with("spira-watch-"));
    NotActiveSplit { warn_only: watchers, fatal: other }
}

#[allow(dead_code)]
fn _unused(_: &BTreeMap<String, String>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{build, Inputs};
    use crate::systemctl::{FakeSystemctl, FakeUnit};
    use std::fs;

    fn host() -> HostValues {
        HostValues { home: "/h".into(), repo: "/h".into(), run: "/run".into(), db: "/db".into(), cockpit: "/h/cockpit".into(), dolt_data: "".into(), testdb_data: "".into(), dolt: "/usr/bin/dolt".into(), prod: "".into(), instance: "prod".into(), testdb_port: "3308".into(), snap_stale_s: "600".into(), watchtower_start_timeout_s: "360".into(), path_tail: "".into(), sccache_dav_addr: "".into(), repo_map: "".into() }
    }

    fn tiny_manifest() -> Manifest {
        let mut m = build(&Inputs { instance: "prod".into(), dolt_data_set: false, testdb_data_set: false, broker_enable: false, inotify_present: true, sccache_dav_addr_set: false, watch_names: Ok(vec![]) }).unwrap();
        m.units.retain(|u| u.name == "spira-sentinel.service" || u.name == "spira-sentinel.timer");
        m
    }

    fn write_templates(dir: &Path) {
        fs::write(dir.join("spira-sentinel.service"), "[Service]\nExecStart=/bin/true\n").unwrap();
        fs::write(dir.join("spira-sentinel.timer"), "[Unit]\nUnit=spira-sentinel.service\n[Timer]\nOnCalendar=*:0/5\n").unwrap();
    }

    #[test]
    fn execstart_ok_skips_system_dirs_and_refuses_a_missing_target() {
        assert!(execstart_target_ok("ExecStart=/usr/bin/true\n").is_ok());
        assert!(execstart_target_ok("ExecStart=-/does/not/exist\n").is_ok()); // "-" ignores failure
        assert!(execstart_target_ok("ExecStart=/does/not/exist\n").is_err());
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
        assert!(unit_dir.join("spira-sentinel-prod.service").exists());
        assert!(sc.enabled_now.borrow().contains(&"spira-sentinel-prod.timer".to_string()));

        let r2 = run(&ctx);
        assert!(r2.errors.is_empty());
        assert_eq!(r2.written.len(), 0);
        assert_eq!(r2.unchanged, 2);
        // Already active and unchanged: Skip, no second enable/restart call recorded.
        assert!(matches!(r2.enabled_actions.iter().find(|(u, _)| u == "spira-sentinel-prod.timer").unwrap().1, Action::Skip));
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
        std::os::unix::fs::symlink("/dev/null", unit_dir.join("spira-sentinel-prod.service")).unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty());
        assert!(r.masked.contains(&"spira-sentinel-prod.service".to_string()));
        assert!(!r.written.contains(&"spira-sentinel-prod.service".to_string()));
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
        fs::write(unit_dir.join("spira-sentinel-prod.service"), "[Service]\nExecStart=/bin/true\n").unwrap();
        fs::write(unit_dir.join("spira-sentinel-prod.timer"), "[Unit]\nUnit=spira-sentinel-prod.service\n[Timer]\nOnCalendar=*:0/1\n").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        sc.set("spira-sentinel-prod.timer", FakeUnit { active: true, enabled: Some("enabled".into()), kind: "oneshot".into(), file_listed: true });
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(sc.restarts.borrow().contains(&"spira-sentinel-prod.timer".to_string()));
        assert!(matches!(r.enabled_actions.iter().find(|(u, _)| u == "spira-sentinel-prod.timer").unwrap().1, Action::Restart));
    }

    /// `--diff` must show WHAT changed, not just that it did (sp-al35q, caught by testenv's
    /// test-unit-drift.sh): systemd/install.sh's own `diff -u ... | sed 's/^/    /'` carried
    /// the actual line-level change; this crate's rewrite dropped it, leaving an operator
    /// staring at a bare "DIFFERS some.service" with no way to see what moved.
    #[test]
    fn diff_shows_the_changed_line_not_just_that_something_differs() {
        let td = tempdir();
        let unit_dir = td.join("units");
        let tmpl_dir = td.join("templates");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::create_dir_all(&tmpl_dir).unwrap();
        write_templates(&tmpl_dir);
        // Rendered content will be "[Service]\nExecStart=/bin/true\n"; the installed copy
        // carries a planted line the render never produces.
        fs::write(unit_dir.join("spira-sentinel-prod.service"), "[Service]\nExecStart=/bin/true\n# stale modification by test fixture\n").unwrap();
        fs::write(unit_dir.join("spira-sentinel-prod.timer"), "[Unit]\nUnit=spira-sentinel-prod.service\n[Timer]\nOnCalendar=*:0/5\n").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };

        let lines = diff(&ctx).unwrap();
        assert!(lines.iter().any(|l| l.contains("DIFFERS") && l.contains("spira-sentinel-prod.service")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("stale modification")), "{lines:?}");
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
        let split = split_not_active(&not_active, true);
        assert_eq!(split.warn_only, not_active, "named — never silently dropped");
        assert!(split.fatal.is_empty(), "no non-watcher unit, so nothing faults the run");
    }

    #[test]
    fn inside_a_test_fixture_a_non_watcher_unit_still_faults() {
        let not_active = vec![
            "spira-watch-pr-notify-abc123.service".to_string(),
            "spira-sentinel-abc123.service".to_string(),
        ];
        let split = split_not_active(&not_active, true);
        assert_eq!(split.warn_only, vec!["spira-watch-pr-notify-abc123.service".to_string()]);
        assert_eq!(split.fatal, vec!["spira-sentinel-abc123.service".to_string()]);
    }

    #[test]
    fn outside_a_test_fixture_a_watcher_not_active_still_faults_as_it_always_did() {
        let not_active = vec!["spira-watch-pr-notify-prod.service".to_string()];
        let split = split_not_active(&not_active, false);
        assert!(split.warn_only.is_empty(), "production's own strictness is unchanged");
        assert_eq!(split.fatal, not_active);
    }

    #[test]
    fn with_nothing_not_active_both_lists_are_empty() {
        let split = split_not_active(&[], true);
        assert!(split.warn_only.is_empty() && split.fatal.is_empty());
    }
}
