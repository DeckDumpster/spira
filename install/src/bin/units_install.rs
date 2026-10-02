//! `units-install` — the per-instance unit renderer/writer/pruner (`systemd/install.sh`, in
//! Rust). `deploy.sh`'s re-render step and `install`'s own phase 4 are its two callers;
//! DESIGN.md "Callers".
//!
//! usage: units-install [<instance>] [--diff | --render | --laptop | --list-manifest]
//!                       [--no-migrate-watchers]
//!
//! `--list-manifest` prints `unit <installed-name>` for every unit this box installs
//! (watchers included, the watcher template itself excluded) and `unbuilt <installed-name>`
//! for a unit declined only because something buildable is missing right now — the read
//! `owned.sh` needs instead of sourcing the retired `systemd/units.sh` itself.
//!
//! `--list-enable` prints the `ENABLE` set alone, one installed name per line, in the same
//! build order `Manifest::enable` produces — what `units-manifest.sh` (still bash, outside
//! this bead) needs for the reconciler's Units invariant.
//!
//! `--list-optional` prints, as bare template names, the UNION of every template any
//! combination of this box's three conditional inputs (dolt data, testdb data,
//! `inotifywait`) can ever push into `OPTIONAL` — not just this box's own current
//! resolution. `test-tarball-bins.sh` (still bash) reads this instead of grepping
//! `OPTIONAL+=` lines out of the retired `systemd/units.sh`.
//!
//! `--list-templates` prints this box's *current* resolution as `UNITS <template>` (bare
//! template name) and `OPTIONAL <template>` lines, plus `ENABLE <installed-name>` (the same
//! set `--list-enable` alone prints) — the three arrays `test-units-optional.sh` (still
//! bash) used to read by sourcing `systemd/units.sh` directly.
//!
//! `--list-union` prints `UNITS <template>`, `ENABLE <template>` (bare, unsuffixed — which
//! templates get enabled, not their per-instance names) and `OPTIONAL <template>` as the
//! UNION across every combination of this box's four conditional inputs (dolt data, testdb
//! data, `inotifywait`, `SPIRA_BROKER_ENABLE`) — the "is this template listed ANYWHERE,
//! regardless of which branch a given box takes" completeness check
//! `test-timer-templates.sh` (still bash) used static text parsing of `systemd/units.sh`
//! for.

use install::bootstrap::{self, nonempty_env};
use install::install_units::{self, Ctx};
use install::systemctl::{RealSystemctl, Systemctl};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // --seed-prod-instance <conf> <toml> <instance> <home> — the underlying function alone,
    // for test-install-conf-seed.sh (docs/test-plan/instance-lifecycle.md UC-29); the real
    // call site is seed_prod_instance_if_split, wired into the ordinary run below.
    if args.first().map(String::as_str) == Some("--seed-prod-instance") {
        let (Some(conf), Some(toml), Some(instance), Some(home)) = (args.get(1), args.get(2), args.get(3), args.get(4)) else {
            eprintln!("usage: units-install --seed-prod-instance <conf> <toml> <instance> <home>");
            return ExitCode::from(2);
        };
        if let Some(msg) = install::seed_instance::seed_prod_instance(Path::new(conf), Path::new(toml), instance, Path::new(home)) {
            println!("{msg}");
        }
        return ExitCode::SUCCESS;
    }

    let mut instance_arg: Option<String> = None;
    let mut mode: Option<&str> = None;
    let mut skip_migrate_watchers = false;
    for a in &args {
        match a.as_str() {
            "--diff" | "--render" | "--laptop" | "--list-manifest" | "--list-enable" | "--list-optional" | "--list-templates" | "--list-union" => mode = Some(Box::leak(a.clone().into_boxed_str())),
            "--no-migrate-watchers" => skip_migrate_watchers = true,
            _ if !a.starts_with("--") && instance_arg.is_none() => instance_arg = Some(a.clone()),
            _ => {}
        }
    }
    if let Some(i) = &instance_arg {
        std::env::set_var("SPIRA_INSTANCE", i);
    }
    let instance = instance_arg.or_else(|| nonempty_env("SPIRA_INSTANCE")).unwrap_or_else(|| "prod".into());

    if mode == Some("--laptop") {
        let home = nonempty_env("SPIRA_HOME").unwrap_or_default();
        let dialer = Path::new(&home).parent().map(|p| p.join("cockpit/remote/cockpit")).unwrap_or_default();
        if !dialer.is_file() {
            eprintln!("units-install: --laptop: cockpit dialer not found at {}", dialer.display());
            eprintln!("units-install: --laptop: run the full install on the host; only the server needs systemd units");
            return ExitCode::from(1);
        }
        let home_dir = nonempty_env("HOME").unwrap_or_default();
        let link = PathBuf::from(&home_dir).join(".local/bin/cockpit");
        let _ = std::fs::create_dir_all(link.parent().unwrap());
        let _ = std::fs::remove_file(&link);
        if std::os::unix::fs::symlink(&dialer, &link).is_err() {
            eprintln!("units-install: could not link {}", link.display());
            return ExitCode::from(1);
        }
        println!("install: linked cockpit dialer: {} -> {}", link.display(), dialer.display());
        return ExitCode::SUCCESS;
    }

    if mode == Some("--list-optional") {
        let mut union: std::collections::BTreeSet<String> = Default::default();
        for dolt in [false, true] {
            for testdb in [false, true] {
                for inotify in [false, true] {
                    for sccache in [false, true] {
                        if let Ok(m) = install::manifest::build(&install::manifest::Inputs { instance: instance.clone(), dolt_data_set: dolt, testdb_data_set: testdb, broker_enable: false, inotify_present: inotify, sccache_dav_addr_set: sccache, watch_names: Ok(Vec::new()) }) {
                            union.extend(m.optional);
                        }
                    }
                }
            }
        }
        for name in union {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }

    if mode == Some("--list-union") {
        let mut units: std::collections::BTreeSet<String> = Default::default();
        let mut enable: std::collections::BTreeSet<String> = Default::default();
        let mut optional: std::collections::BTreeSet<String> = Default::default();
        for dolt in [false, true] {
            for testdb in [false, true] {
                for inotify in [false, true] {
                    for broker in [false, true] {
                        for sccache in [false, true] {
                            if let Ok(m) = install::manifest::build(&install::manifest::Inputs { instance: instance.clone(), dolt_data_set: dolt, testdb_data_set: testdb, broker_enable: broker, inotify_present: inotify, sccache_dav_addr_set: sccache, watch_names: Ok(Vec::new()) }) {
                                for u in &m.units {
                                    units.insert(u.name.clone());
                                    if u.enable {
                                        enable.insert(u.name.clone());
                                    }
                                }
                                optional.extend(m.optional);
                            }
                        }
                    }
                }
            }
        }
        for u in units {
            println!("UNITS {u}");
        }
        for e in enable {
            println!("ENABLE {e}");
        }
        for o in optional {
            println!("OPTIONAL {o}");
        }
        return ExitCode::SUCCESS;
    }

    let templates_dir = bootstrap::templates_dir();

    let manifest = match bootstrap::manifest_from_env(&instance) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("install: {e}");
            return ExitCode::from(1);
        }
    };

    if mode == Some("--list-manifest") {
        for name in manifest.template_names() {
            println!("unit {}", install::manifest::inst_name(name, &instance));
        }
        for w in &manifest.watch_names {
            println!("unit {}", install::manifest::inst_watch_name(w, &instance));
        }
        for name in &manifest.unbuilt {
            println!("unbuilt {}", install::manifest::inst_name(name, &instance));
        }
        return ExitCode::SUCCESS;
    }

    if mode == Some("--list-enable") {
        for u in manifest.enable(&instance) {
            println!("{u}");
        }
        return ExitCode::SUCCESS;
    }

    if mode == Some("--list-templates") {
        for u in manifest.template_names() {
            println!("UNITS {u}");
        }
        for o in &manifest.optional {
            println!("OPTIONAL {o}");
        }
        for e in manifest.enable(&instance) {
            println!("ENABLE {e}");
        }
        return ExitCode::SUCCESS;
    }

    let host = match bootstrap::host_from_env(&instance) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("units-install: {e}");
            return ExitCode::from(1);
        }
    };
    let dir = bootstrap::unit_dir();
    let systemctl = RealSystemctl::from_env();
    let suspended = bootstrap::suspended_set();
    let suspended_fn = |s: &str| suspended.contains(s);
    let world_halted = bootstrap::world_halted();

    if mode == Some("--render") {
        let ctx = Ctx { unit_dir: &dir, templates_dir: &templates_dir, host: &host, manifest: &manifest, systemctl: &systemctl, suspended: &suspended_fn, world_halted, skip_migrate_watchers };
        match install_units::render_all(&ctx) {
            Ok(rendered) => {
                for (inst, _tpl, text) in rendered {
                    println!("===== {inst} =====");
                    print!("{text}");
                }
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("install: {e}");
                return ExitCode::from(1);
            }
        }
    }

    if mode == Some("--diff") {
        let ctx = Ctx { unit_dir: &dir, templates_dir: &templates_dir, host: &host, manifest: &manifest, systemctl: &systemctl, suspended: &suspended_fn, world_halted, skip_migrate_watchers };
        match install_units::diff(&ctx) {
            Ok(lines) => {
                if lines.is_empty() {
                    println!("installed units match what this box renders");
                    return ExitCode::SUCCESS;
                }
                for l in lines {
                    println!("{l}");
                }
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("install: {e}");
                return ExitCode::from(1);
            }
        }
    }

    if let Some(msg) = bootstrap::refuse_if_stale_release("units-install") {
        eprintln!("{msg}");
        return ExitCode::from(1);
    }

    // Path collisions, landref currency and live aeons — refuse before touching anything,
    // unless overridden.
    if nonempty_env("SPIRA_INSTALL_FORCE").is_none() {
        if let Err(lines) = install::checks::preflight(&instance, &host, &systemctl) {
            for l in lines {
                eprintln!("install: {l}");
            }
            eprintln!("install: override: SPIRA_INSTALL_FORCE=1");
            return ExitCode::from(1);
        }
    }

    // Seed a split dev/prod checkout's own instance (install::seed_instance — sp-31dm0).
    if let Some(msg) = install::seed_instance::seed_prod_instance_if_split(&instance, &host.prod, &host.home) {
        println!("{msg}");
    }

    let _ = std::fs::create_dir_all(&dir);
    if let Some(run) = nonempty_env("SPIRA_RUN") {
        let _ = std::fs::create_dir_all(&run);
        let _ = std::fs::create_dir_all(Path::new(&run).join("watchd"));
    }

    let ctx = Ctx { unit_dir: &dir, templates_dir: &templates_dir, host: &host, manifest: &manifest, systemctl: &systemctl, suspended: &suspended_fn, world_halted, skip_migrate_watchers };
    let report = install_units::run(&ctx);
    for e in &report.errors {
        eprintln!("install: {e}");
    }
    if !report.errors.is_empty() {
        return ExitCode::from(1);
    }
    for u in &report.written {
        println!("installed {u}");
    }
    println!("install: {} unit file(s) changed, {} unchanged{}", report.written.len(), report.unchanged, if report.masked.is_empty() { String::new() } else { format!(", {} masked (skipped)", report.masked.len()) });
    for m in &report.migrated_legacy {
        println!("migrated  {m} (disabled and removed; superseded by the per-instance unit)");
    }

    // Linger: unconditional here (systemd/install.sh's own call; the root installer's phase
    // also records a stamp for uninstall — both calls kept, DESIGN.md "Decisions").
    let _ = Command::new("loginctl").arg("enable-linger").arg(nonempty_env("USER").unwrap_or_else(whoami)).status();

    if let Some(cockpit) = nonempty_env("SPIRA_COCKPIT") {
        if let Some(run) = nonempty_env("SPIRA_RUN") {
            for f in install_units::migrate_cockpit_state(Path::new(&cockpit), Path::new(&run)) {
                println!("install: migrated cockpit state {f}");
            }
        }
    }

    // Prune watcher and non-watcher spira-* units this manifest no longer names.
    //
    // DEST SCAN, ALONGSIDE systemctl's OWN LISTS, not instead of them: a unit file here
    // that systemd has not indexed yet (before this run's own daemon-reload, or when the
    // daemon's HOME differs from this process's — per-suite test isolation gives each
    // suite its own HOME, but a real daemon was started against the operator's) is an
    // orphan regardless of what the daemon's search path currently includes. `dir` is
    // where this binary writes unit files, so it is the ground truth a mocked or
    // not-yet-reloaded `systemctl` cannot be.
    let expected = install_units::expected_installed_names(&manifest, &instance);
    let watch_glob = format!("spira-watch-*-{instance}.service");
    let mut candidates = systemctl.list_matching(&watch_glob);
    candidates.extend(glob_dir(&dir, &watch_glob));
    for u in install_units::prune_targets(candidates.iter().map(|s| s.as_str()), &expected, |_| false) {
        // A mask is the operator's own answer; deleting it would unmask the unit.
        if install_units::is_masked(&dir.join(&u)) {
            continue;
        }
        if systemctl.disable_now(&u).is_ok() {
            println!("disabled  {u} (no row in the manifest)");
        }
        // A unit file left behind is re-listed as a candidate each run and blocks `mask`.
        if std::fs::remove_file(dir.join(&u)).is_ok() {
            println!("removed   {u} (no row in the manifest)");
        }
    }
    for glob in [format!("spira-*-{instance}.service"), format!("spira-*-{instance}.timer")] {
        let mut candidates = systemctl.list_matching(&glob);
        candidates.extend(glob_dir(&dir, &glob));
        for u in install_units::prune_targets(candidates.iter().map(|s| s.as_str()), &expected, |n| n.starts_with("spira-watch-") || n.starts_with("spira-aeon-")) {
            let _ = systemctl.disable_now(&u);
            let _ = std::fs::remove_file(dir.join(&u));
            println!("pruned    {u} (no longer in the manifest)");
        }
    }

    // `release session-hook install` (sp-7jr34: replaces spira/install-session-hook.sh,
    // which is gone) — this call site is preserved unconditionally, matching
    // systemd/install.sh's own unconditional call.
    let hook_ok = Command::new("release").args(["session-hook", "install"]).status().map(|s| s.success()).unwrap_or(false);
    if !hook_ok {
        eprintln!("note: the session hook was not registered — run release session-hook install");
    }

    if let Some(home) = nonempty_env("SPIRA_HOME") {
        let cr_src = Path::new(&home).parent().map(|p| p.join("cockpit/remote/cockpit-remote")).unwrap_or_default();
        if cr_src.is_file() {
            if let Some(h) = nonempty_env("HOME") {
                let dst = PathBuf::from(&h).join(".local/bin/cockpit-remote");
                let _ = std::fs::create_dir_all(dst.parent().unwrap());
                let current = std::fs::read_link(&dst).ok();
                if current.as_deref() != Some(cr_src.as_path()) {
                    let _ = std::fs::remove_file(&dst);
                    if std::os::unix::fs::symlink(&cr_src, &dst).is_ok() {
                        println!("install: linked cockpit-remote: {} -> {}", dst.display(), cr_src.display());
                    }
                }
            }
        }
    }

    // End-state check: every ENABLE-set unit not masked/disabled/suspended must be active,
    // unless the world is deliberately halted.
    //
    // BOUNDED WAIT, NOT ONE IMMEDIATE POLL: a `Type=notify` unit (spira-cockpit.service)
    // only reports active once its own first pass signals READY=1, which can take a few
    // real seconds (a cold collect.sh run) — comfortably inside systemd's own
    // `TimeoutStartSec`, but not necessarily inside the handful of milliseconds this
    // process takes to reach this check. `systemd/install.sh`'s original bash reached the
    // same check only after dozens of its own subprocess-spawning steps, which happened to
    // leave enough wall-clock time for this to never matter in practice; this port does the
    // same steps faster, so the wait that was accidental there is explicit here instead.
    if !world_halted {
        let candidates: Vec<String> = manifest
            .enable(&instance)
            .into_iter()
            .filter(|u| {
                if report.masked.contains(u) {
                    return false;
                }
                let subject = u.strip_suffix(&format!("-{instance}.service")).or_else(|| u.strip_suffix(&format!("-{instance}.timer"))).or_else(|| u.strip_suffix(".service")).or_else(|| u.strip_suffix(".timer")).unwrap_or(u.as_str());
                if suspended.contains(subject) {
                    return false;
                }
                !(systemctl.is_enabled(u).as_deref() == Some("disabled") && !report.written.contains(u))
            })
            .collect();
        let max_wait = nonempty_env("SPIRA_INSTALL_ACTIVE_WAIT").and_then(|v| v.parse().ok()).unwrap_or(45u64);
        let start = std::time::Instant::now();
        let mut not_active: Vec<String> = candidates.iter().filter(|u| !systemctl.is_active(u)).cloned().collect();
        while !not_active.is_empty() && start.elapsed().as_secs() < max_wait {
            std::thread::sleep(std::time::Duration::from_secs(1));
            not_active = candidates.iter().filter(|u| !systemctl.is_active(u)).cloned().collect();
        }
        if !not_active.is_empty() {
            // sp-e5v53-4: inside a test fixture, a watcher (every `spira-watch-*` instance
            // of the one template, DESIGN.md "the watcher template") reaches outside this
            // box entirely — GitHub, mail, the forge — and a sandboxed container's point is
            // exactly that it cannot. It genuinely not reaching active there says nothing about
            // whether the units this run actually needs (the suite's own dependencies) are
            // fine, so it is named — "exactly why not", never silently dropped — but does
            // not fault a run that tests no watcher. Outside a test fixture this still
            // faults: a watcher that cannot start in production is still worth knowing.
            let in_testenv = nonempty_env("SPIRA_IN_TESTENV").is_some();
            let split = install_units::split_not_active(&not_active, in_testenv);
            if !split.warn_only.is_empty() {
                eprintln!(
                    "\ninstall: {} watcher unit(s) did not reach active inside this test \
                     fixture — expected: a watcher reaches outside the container \
                     (GitHub, mail, the forge), which a sandboxed fixture cannot, and no \
                     suite here is what tests a watcher:",
                    split.warn_only.len()
                );
                for u in &split.warn_only {
                    eprintln!("    {u}");
                }
            }
            if !split.fatal.is_empty() {
                eprintln!("\ninstall: ERROR — these units are enabled but not active:");
                for u in &split.fatal {
                    eprintln!("    {u}");
                }
                eprintln!("install: check journalctl --user -xe for details.");
                return ExitCode::from(1);
            }
        }
    }

    ExitCode::SUCCESS
}

fn whoami() -> String {
    Command::new("id").arg("-un").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

/// Every file directly under `dir` whose name matches `glob` (a single `*` wildcard, the
/// only shape `systemctl --user list-unit-files <glob>` is ever called with here).
fn glob_dir(dir: &Path, glob: &str) -> Vec<String> {
    let Some((pre, suf)) = glob.split_once('*') else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            (e.path().is_file() && n.starts_with(pre) && n.ends_with(suf) && n.len() >= pre.len() + suf.len()).then_some(n)
        })
        .collect()
}
