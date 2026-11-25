//! Use-case cells of the matrix area `instance-lifecycle` whose behaviour the installer owns.
//! Each test states what the use case in `docs/test-plan/instance-lifecycle.toml` says must hold.

use crate::install_units::{render_all, Ctx};
use crate::manifest::{build, Inputs, Manifest};
use crate::systemctl::FakeSystemctl;
use crate::values::{render, HostValues};
use std::path::PathBuf;

fn templates() -> PathBuf {
    spira_config::resolve::checkout_root_for_process().unwrap().join("systemd")
}

fn host(instance: &str) -> HostValues {
    HostValues {
        toml: "h/cfg.toml".into(),
        home: "h/spira".into(),
        repo: "h/spira".into(),
        run: "run".into(),
        db: "db".into(),
        cockpit: "h/cockpit".into(),
        dolt_data: "d".into(),
        dolt: "usr/bin/dolt".into(),
        prod: "r/releases/current".into(),
        instance: instance.into(),
        snap_stale_s: "600".into(),
        watchtower_start_timeout_s: "360".into(),
        path_tail: "".into(),
        env_bin: "h/.local/lib/spira-env/bin".into(),
        sccache_dav_addr: "".into(),
        sccache_dav_root: "run/sccache-dav".into(),
        repo_map: "".into(),
        lc_password_file: "h/lc.credential".into(),
        watchdog_every_s: "60".into(),
    }
}

fn inputs(instance: &str) -> Inputs {
    Inputs { instance: instance.into(), dolt_data_set: true, dolt_durable_set: false, sccache_dav_addr_set: false, repo_is_git_checkout: true, lc_system_mode: false, watch_names: Ok(vec!["testview".into()]) }
}

fn rendered(instance: &str, manifest: &Manifest) -> Vec<(String, String, String)> {
    let (h, tmpl) = (host(instance), templates());
    let sc = FakeSystemctl::default();
    let none = |_: &str| false;
    let units = testkit::TempDir::new("cells-units");
    let ctx = Ctx { unit_dir: units.path(), templates_dir: &tmpl, host: &h, manifest, systemctl: &sc, suspended: &none, world_halted: false, skip_migrate_watchers: false };
    render_all(&ctx).unwrap()
}

#[test_id(tehq0eg)]
#[test]
fn the_scheduler_unit_loads_the_backup_key_and_the_store_unit_does_not() {
    let out = rendered("beta", &build(&inputs("beta")).unwrap());
    let text = |n: &str| out.iter().find(|(name, _, _)| name == n).unwrap_or_else(|| panic!("{n} not rendered")).2.clone();
    let loads = |t: &str| t.lines().any(|l| l.starts_with("EnvironmentFile=") && l.contains("lc-backup.env"));
    assert!(loads(&text("spira-runtime-beta.service")), "the scheduler's lc.ship signs with this key");
    assert!(!loads(&text("lc-serve.service")), "lc-serve no longer uploads");
}

// covers: UC-instance-lifecycle-24
#[test_id(ttxxoni)]
#[test]
fn rendering_names_every_unit_for_its_instance_and_leaves_no_placeholder() {
    let m = build(&inputs("beta")).unwrap();
    let out = rendered("beta", &m);
    let names: Vec<&str> = out.iter().map(|(n, _, _)| n.as_str()).collect();
    for want in ["spira-runtime-beta.service", "spira-watchdog-beta.timer", "spira-watch-testview-beta.service", "cockpit-ensure.service", "concierge.service"] {
        assert!(names.contains(&want), "{want} not rendered: {names:?}");
    }
    for (name, _, text) in &out {
        if name.starts_with("spira-") {
            let stem = name.rsplit_once('.').unwrap().0;
            assert!(stem.ends_with("-beta"), "{name} is not instance-suffixed");
        }
        assert!(!name.contains('@'), "{name}: a watcher is never named with the template's `@`");
        assert!(!text.contains("@SPIRA_") && !text.contains("@DOLT@"), "{name} keeps a placeholder");
    }
    let enabled = m.enable("beta");
    assert!(enabled.iter().any(|u| u == "spira-watchdog-beta.timer"), "{enabled:?}");
    assert!(!enabled.iter().any(|u| u == "spira-watchdog.timer"), "{enabled:?}");
    let timer = out.iter().find(|(n, _, _)| n == "spira-watchdog-beta.timer").expect("a periodic timer is rendered");
    assert!(timer.2.contains("Unit=spira-watchdog-beta.service"), "a timer fires its suffixed service:\n{}", timer.2);
}

// covers: UC-instance-lifecycle-25
#[test_id(twsxgjk)]
#[test]
fn an_optional_unit_is_declined_with_a_note_until_its_prerequisite_is_present() {
    let without = build(&Inputs { dolt_data_set: false, ..inputs("beta") }).unwrap();
    let enabled = without.enable("beta");
    for u in ["dolt-beads.service", "sccache-dav.service"] {
        assert!(without.optional.iter().any(|o| o == u), "{u} is optional without its prerequisite");
        assert!(!without.units.iter().any(|t| t.name == u), "{u} is not installed");
        assert!(!enabled.iter().any(|e| e == u), "{u} is not enabled");
    }
    assert!(!without.notes.is_empty(), "the declined units come with a note");

    let with = build(&Inputs { dolt_data_set: true, sccache_dav_addr_set: true, ..inputs("beta") }).unwrap();
    for u in ["dolt-beads.service", "sccache-dav.service"] {
        assert!(with.units.iter().any(|t| t.name == u && t.enable), "{u} is installed and enabled once present");
        assert!(!with.optional.iter().any(|o| o == u));
    }
    assert!(with.enable("beta").iter().any(|e| e == "spira-loom-beta.service"), "loom is unconditional");
}

// covers: UC-instance-lifecycle-26
#[test_id(tfuc1vx)]
#[test]
fn a_dolt_template_is_refused_without_dolt_and_an_empty_prod_falls_back_to_home() {
    let tmpl = std::fs::read_to_string(templates().join("dolt-beads.service")).unwrap();
    let mut h = host("beta");
    h.dolt = String::new();
    let err = render("dolt-beads.service", &tmpl, &h, None).unwrap_err();
    assert!(err.contains("dolt"), "{err}");

    h.dolt = "opt/dolt/bin/dolt".into();
    let ok = render("dolt-beads.service", &tmpl, &h, None).unwrap();
    assert!(ok.lines().any(|l| l.starts_with("ExecStart=opt/dolt/bin/dolt")), "no empty ExecStart:\n{ok}");

    let plain = std::fs::read_to_string(templates().join("spira-runtime.service")).unwrap();
    h.dolt = String::new();
    render("spira-runtime.service", &plain, &h, None).expect("a template that never names dolt renders without it");

    let cockpit = std::fs::read_to_string(templates().join("cockpit-ensure.service")).unwrap();
    h.prod = "p/rel/current".into();
    assert!(render("cockpit-ensure.service", &cockpit, &h, None).unwrap().contains("ExecStart=p/rel/bin/layout ensure"), "derives from dirname(SPIRA_PROD)");
    h.prod = String::new();
    h.home = "h/spira".into();
    assert!(render("cockpit-ensure.service", &cockpit, &h, None).unwrap().contains("ExecStart=h/bin/layout ensure"), "an empty SPIRA_PROD is SPIRA_HOME");
}

use crate::checks::preflight_unless_forced;
use crate::ensure;
use crate::install_units::{run, split_not_active};
use crate::systemctl::FakeUnit;

struct Box_ {
    _root: testkit::TempDir,
    units: PathBuf,
    tmpl: PathBuf,
}

fn box_with(service: &str, watcher_tmpl: bool) -> Box_ {
    let root = testkit::TempDir::new("cells-box");
    let (units, tmpl) = (root.join("units"), root.join("templates"));
    std::fs::create_dir_all(&units).unwrap();
    std::fs::create_dir_all(&tmpl).unwrap();
    std::fs::write(tmpl.join("spira-example.service"), service).unwrap();
    std::fs::write(tmpl.join("spira-example.timer"), "[Unit]\nUnit=spira-example.service\n[Timer]\nOnCalendar=*:0/5\n").unwrap();
    if watcher_tmpl {
        std::fs::write(tmpl.join("spira-watch@.service"), "[Service]\nExecStart=/bin/true %i\n").unwrap();
    }
    Box_ { _root: root, units, tmpl }
}

fn example_manifest(watchers: &[&str]) -> Manifest {
    let t = |n: &str, e| crate::manifest::Template { name: n.into(), enable: e };
    Manifest { units: vec![t("spira-watch@.service", false), t("spira-example.service", false), t("spira-example.timer", true)], watch_names: watchers.iter().map(|w| w.to_string()).collect(), ..Default::default() }
}

fn ctx<'a>(b: &'a Box_, h: &'a HostValues, m: &'a Manifest, sc: &'a FakeSystemctl, none: &'a dyn Fn(&str) -> bool) -> Ctx<'a> {
    Ctx { unit_dir: &b.units, templates_dir: &b.tmpl, host: h, manifest: m, systemctl: sc, suspended: none, world_halted: false, skip_migrate_watchers: false }
}

// covers: UC-instance-lifecycle-27
#[test_id(ta1qlw4)]
#[test]
fn prune_retires_an_orphaned_unit_and_watcher_and_spares_the_manifest_and_other_instances() {
    let b = box_with("[Service]\nExecStart=/bin/true\n", true);
    let (h, m, sc, none) = (host("beta"), example_manifest(&["kept"]), FakeSystemctl::default(), |_: &str| false);
    let c = ctx(&b, &h, &m, &sc, &none);
    for orphan in ["spira-gone-beta.service", "spira-gone-beta.timer", "spira-watch-gone-beta.service"] {
        std::fs::write(b.units.join(orphan), "[Service]\nExecStart=/bin/true\n").unwrap();
    }
    std::fs::write(b.units.join("spira-other-alpha.service"), "[Service]\nExecStart=/bin/true\n").unwrap();
    let r = run(&c);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    for orphan in ["spira-gone-beta.service", "spira-gone-beta.timer", "spira-watch-gone-beta.service"] {
        assert!(r.retired.iter().any(|u| u == orphan), "{orphan} retired: {:?}", r.retired);
        assert!(!b.units.join(orphan).exists(), "{orphan} removed");
        assert!(sc.disabled_now.borrow().iter().any(|u| u == orphan), "{orphan} disabled and stopped");
    }
    assert!(b.units.join("spira-watch-kept-beta.service").exists(), "a manifest watcher is never pruned");
    assert!(b.units.join("spira-example-beta.timer").exists(), "a manifest unit is never pruned");
    assert!(b.units.join("spira-other-alpha.service").exists(), "another instance's unit is not ours to prune");
    assert!(!r.retired.iter().any(|u| u == "spira-other-alpha.service"));
}

// covers: UC-instance-lifecycle-21
#[test_id(t94e3m8)]
#[test]
fn install_refuses_while_dwarves_are_live_and_when_an_exec_target_is_not_executable() {
    let sc = FakeSystemctl::default();
    sc.set("spira-dwarf-builder-1-beta.service", FakeUnit { active: true, ..Default::default() });
    let h = host("beta");
    let refused = preflight_unless_forced(false, "beta", &h, &sc).unwrap_err();
    assert!(refused.iter().any(|l| l.contains("spira-dwarf-builder-1-beta.service")), "{refused:?}");
    assert!(preflight_unless_forced(true, "beta", &h, &sc).is_ok(), "SPIRA_INSTALL_FORCE overrides");
    assert!(preflight_unless_forced(false, "beta", &h, &FakeSystemctl::default()).is_ok(), "no dwarves: no refusal");
    assert!(preflight_unless_forced(false, "alpha", &h, &sc).is_ok(), "another instance's dwarves are not this install's");

    let scratch = testkit::TempDir::new("cells-exec");
    let target = scratch.join("tool");
    std::fs::write(&target, "#!/bin/sh\n").unwrap();
    let b = box_with(&format!("[Service]\nExecStart={} run\n", target.display()), false);
    let (m, sc, none) = (example_manifest(&[]), FakeSystemctl::default(), |_: &str| false);
    let c = ctx(&b, &h, &m, &sc, &none);
    let r = run(&c);
    assert!(r.errors.iter().any(|e| e.contains("not executable")), "{:?}", r.errors);
    assert!(r.written.is_empty() && sc.reloads.get() == 0, "nothing is written or reloaded for a refused install");
    std::fs::remove_file(&target).unwrap();
    testkit::write_exe(&target, "#!/bin/sh\n");
    assert!(run(&c).errors.is_empty(), "an executable target installs");
}

// covers: UC-instance-lifecycle-23
#[test_id(t5ghgdo)]
#[test]
fn a_changed_timer_waits_for_its_running_oneshot_and_a_dead_enabled_unit_fails_the_install() {
    let b = box_with("[Service]\nType=oneshot\nExecStart=/bin/true\n", false);
    let (h, m, none) = (host("beta"), example_manifest(&[]), |_: &str| false);
    let sc = FakeSystemctl::default();
    assert!(run(&ctx(&b, &h, &m, &sc, &none)).errors.is_empty());
    std::fs::write(b.tmpl.join("spira-example.timer"), "[Unit]\nUnit=spira-example.service\n[Timer]\nOnCalendar=*:0/10\n").unwrap();
    sc.set("spira-example-beta.service", FakeUnit { active: true, enabled: Some("enabled".into()), kind: "oneshot".into(), file_listed: true });
    sc.set("spira-example-beta.timer", FakeUnit { active: true, enabled: Some("enabled".into()), kind: "".into(), file_listed: true });
    sc.finishes_after_polls.borrow_mut().insert("spira-example-beta.service".into(), 2);
    let _env = testkit::env(&[("SPIRA_DRAIN_INTERVAL", Some("0"))]);
    let r = run(&ctx(&b, &h, &m, &sc, &none));
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(sc.restarts.borrow().as_slice(), ["spira-example-beta.timer"], "the changed timer is applied");
    assert!(!sc.units.borrow()["spira-example-beta.service"].active, "…only after the oneshot drained");
    assert!(sc.order.borrow().iter().all(|o| !o.contains("still running")), "{:?}", sc.order.borrow());
    assert!(sc.finishes_after_polls.borrow()["spira-example-beta.service"] == 0, "the installer polled the oneshot until it finished");

    let dead = vec!["spira-example-beta.timer".to_string()];
    assert!(!split_not_active(&dead, false, "beta").fatal.is_empty(), "an enabled unit that is not active fails the install");
    assert!(split_not_active(&[], false, "beta").fatal.is_empty(), "all active: the install passes");
}

// covers: UC-instance-lifecycle-30
#[test_id(t0vmm7m)]
#[test]
fn ensure_reloads_only_when_a_unit_changed_and_reports_a_missing_target_without_enabling() {
    let scratch = testkit::TempDir::new("cells-ensure");
    let missing = scratch.join("absent-tool");
    let b = box_with(&format!("[Service]\nExecStart={} run\n", missing.display()), false);
    let (h, mut m, none) = (host("beta"), example_manifest(&[]), |_: &str| false);
    m.units.iter_mut().for_each(|u| u.enable = u.name == "spira-example.service");
    let sc = FakeSystemctl::default();
    let c = ctx(&b, &h, &m, &sc, &none);

    let first = ensure::run(&c, false);
    assert_eq!(first.installed.len(), 2, "missing units are installed");
    assert_eq!(sc.reloads.get(), 1);
    assert!(first.missing_target.iter().any(|t| t.contains("spira-example-beta.service")), "{:?}", first.missing_target);
    assert!(first.enabled_started.is_empty() && sc.enabled_now.borrow().is_empty(), "a unit whose target is missing is not enabled");

    let second = ensure::run(&c, false);
    assert_eq!((second.installed.len(), second.updated.len(), second.unchanged), (0, 0, 2));
    assert_eq!(sc.reloads.get(), 1, "an unchanged tree is a no-op: no daemon-reload");

    std::fs::write(b.tmpl.join("spira-example.timer"), "[Unit]\nUnit=spira-example.service\n[Timer]\nOnCalendar=daily\n").unwrap();
    let third = ensure::run(&c, false);
    assert_eq!(third.updated, vec!["spira-example-beta.timer".to_string()], "a changed unit is reinstalled");
    assert_eq!(sc.reloads.get(), 2);
}

fn unit_texts() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(templates())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".service") || n.ends_with(".timer"))
        .map(|n| (n.clone(), std::fs::read_to_string(templates().join(&n)).unwrap()))
        .collect();
    v.sort();
    v
}

fn code_lines(text: &str) -> Vec<&str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect()
}

fn setting<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    code_lines(text).into_iter().rev().find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
}

fn leading_number(v: Option<&str>) -> Option<u64> {
    let v = v?;
    let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// `Err` names why a restarting unit's start limiter can never trip.
fn limiter_problem(text: &str) -> Result<(), String> {
    if !matches!(setting(text, "Restart"), Some("always" | "on-failure")) {
        return Ok(());
    }
    let (Some(rsec), Some(burst), Some(window)) = (leading_number(setting(text, "RestartSec")), leading_number(setting(text, "StartLimitBurst")), leading_number(setting(text, "StartLimitIntervalSec"))) else {
        return Err("RestartSec, StartLimitBurst and StartLimitIntervalSec must all be set".into());
    };
    if window <= rsec * burst.saturating_sub(1) {
        return Err(format!("{burst} starts at RestartSec={rsec}s span {}s but the window is {window}s", rsec * burst.saturating_sub(1)));
    }
    let mut section = "";
    for l in code_lines(text) {
        if l.starts_with('[') {
            section = l;
        } else if l.starts_with("StartLimit") && section != "[Unit]" {
            return Err(format!("{l} sits under {section}; systemd ignores it outside [Unit]"));
        }
    }
    Ok(())
}

fn execstart_target(text: &str) -> String {
    setting(text, "ExecStart").map(|v| v.trim_start_matches('-').split(' ').next().unwrap_or("").to_string()).unwrap_or_default()
}

// covers: UC-instance-lifecycle-31
#[test_id(tjkgppm)]
#[test]
fn shipped_unit_files_are_listed_enabled_periodic_limited_and_watchdogged_correctly() {
    let units = unit_texts();
    let mut union: Vec<(String, bool)> = Vec::new();
    let mut declined: Vec<String> = Vec::new();
    for dolt in [false, true] {
        for sccache in [false, true] {
            for lc_system in [false, true] {
                let m = build(&Inputs { dolt_data_set: dolt, dolt_durable_set: dolt, sccache_dav_addr_set: sccache, lc_system_mode: lc_system, ..inputs("t") }).unwrap();
                union.extend(m.units.iter().map(|u| (u.name.clone(), u.enable)));
                declined.extend(m.optional.iter().cloned());
            }
        }
    }
    let listed = |n: &str| union.iter().any(|(u, _)| u == n) || declined.iter().any(|o| o == n);
    let enabled = |n: &str| union.iter().any(|(u, e)| u == n && *e);

    let timers: Vec<&(String, String)> = units.iter().filter(|(n, _)| n.ends_with(".timer")).collect();
    assert!(timers.len() > 3, "positive control: the timer templates were found");
    assert!(timers.iter().any(|(n, _)| n == "concierge.timer") && enabled("concierge.timer"), "positive control: a known timer is parsed as listed and enabled");
    for (n, text) in &timers {
        assert!(listed(n), "{n} is in no manifest row: the installer would never write it");
        assert!(enabled(n) || declined.contains(n), "{n} is installed but never enabled: it would never fire");
        assert!(setting(text, "OnUnitActiveSec").is_some() || setting(text, "OnCalendar").is_some(), "{n} fires once at boot and never again");
    }

    let unreachable = "[Unit]\nStartLimitIntervalSec=10\nStartLimitBurst=5\n[Service]\nRestart=always\nRestartSec=15\n";
    assert!(limiter_problem(unreachable).is_err(), "positive control: an unreachable limiter is caught");
    assert!(limiter_problem("[Service]\nRestart=always\nRestartSec=1\n[Unit]\nStartLimitBurst=3\nStartLimitIntervalSec=60\n").is_ok());
    assert!(limiter_problem("[Unit]\nStartLimitBurst=3\n[Service]\nRestart=always\nRestartSec=1\nStartLimitIntervalSec=60\n").is_err(), "limit directives under [Service] are ignored");
    let restarting: Vec<&(String, String)> = units.iter().filter(|(_, t)| matches!(setting(t, "Restart"), Some("always" | "on-failure"))).collect();
    assert!(!restarting.is_empty(), "positive control: a restarting unit exists");
    for (n, text) in restarting {
        assert_eq!(limiter_problem(text), Ok(()), "{n}");
    }

    assert!(execstart_target("[Service]\nExecStart=/usr/bin/bash /x.sh\nWatchdogSec=60\n").ends_with("bash"), "positive control: a shell target is extracted");
    assert_eq!(execstart_target("[Service]\nExecStart=bin/spira-supervise x/collect.sh loop\n"), "bin/spira-supervise", "arguments are not the target");
    let concierge = std::fs::read_to_string(templates().join("concierge.service")).unwrap();
    let mut h = host("t");
    h.repo = "the/git/checkout".into();
    let exec = render("concierge.service", &concierge, &h, None).unwrap();
    let exec = exec.lines().find(|l| l.starts_with("ExecStart=")).unwrap().to_string();
    assert!(exec.contains("r/") && !exec.contains("the/git/checkout"), "a shared unit runs from the release root, not the git checkout: {exec}");
    let shell: Vec<&(String, String)> = units.iter().filter(|(_, t)| {
        let target = execstart_target(t);
        target.contains("bash") || target.ends_with(".sh")
    }).collect();
    for (n, text) in shell {
        assert!(setting(text, "WatchdogSec").is_none(), "{n}: a shell main process cannot deliver the watchdog heartbeat");
    }
}

fn workspace_binaries() -> std::collections::BTreeSet<String> {
    let root = spira_config::resolve::checkout_root_for_process().unwrap();
    let members: Vec<String> = std::fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .lines()
        .skip_while(|l| !l.starts_with("members"))
        .skip(1)
        .take_while(|l| !l.starts_with(']'))
        .filter_map(|l| l.trim().trim_end_matches(',').trim_matches('"').to_string().into())
        .filter(|m| !m.is_empty())
        .collect();
    let mut bins = std::collections::BTreeSet::new();
    for m in members {
        let dir = root.join(&m);
        let Ok(toml) = std::fs::read_to_string(dir.join("Cargo.toml")) else { continue };
        let mut section = String::new();
        let mut package = String::new();
        for l in toml.lines().map(str::trim) {
            if l.starts_with('[') {
                section = l.to_string();
                if section == "[[bin]]" {
                    package.push('\0');
                }
            } else if let Some(v) = l.strip_prefix("name = ") {
                let v = v.trim_matches('"').to_string();
                match section.as_str() {
                    "[package]" => package = v,
                    "[[bin]]" => {
                        bins.insert(v);
                    }
                    _ => {}
                }
            }
        }
        let pkg = package.trim_end_matches('\0');
        if dir.join("src/main.rs").exists() && !pkg.is_empty() {
            bins.insert(pkg.to_string());
        }
        if let Ok(rd) = std::fs::read_dir(dir.join("src/bin")) {
            bins.extend(rd.flatten().filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned())));
        }
    }
    bins
}

// covers: UC-instance-lifecycle-03
#[test_id(t8ainv2)]
#[test]
fn every_release_binary_a_non_optional_unit_runs_is_built_by_the_workspace() {
    let always = build(&Inputs { dolt_data_set: false, ..inputs("t") }).unwrap();
    let bins = workspace_binaries();
    assert!(bins.contains("spira-install") && bins.contains("loom"), "positive control: the workspace scan finds known binaries ({} found)", bins.len());
    let mut referenced = 0;
    for (name, text) in unit_texts() {
        if !always.units.iter().any(|u| u.name == name) || name == "spira-watchdog.service" {
            continue;
        }
        for l in text.lines().filter(|l| l.starts_with("ExecStart=")) {
            for tok in l.trim_start_matches("ExecStart=").split_whitespace().filter_map(|w| w.strip_prefix("@SPIRA_PROD_ROOT@/bin/")) {
                referenced += 1;
                assert!(bins.contains(tok), "{name} runs bin/{tok}, which no workspace crate builds: the tarball would not ship it");
            }
        }
    }
    assert!(referenced > 0, "positive control: some non-optional unit runs a release binary");
}

// covers: UC-instance-lifecycle-34
#[test_id(t7es1mn)]
#[test]
fn diff_is_empty_when_installed_units_match_and_names_missing_and_differing_ones() {
    let b = box_with("[Service]\nExecStart=/bin/true\n", false);
    let (h, m, sc, none) = (host("beta"), example_manifest(&[]), FakeSystemctl::default(), |_: &str| false);
    let c = ctx(&b, &h, &m, &sc, &none);
    let missing = crate::install_units::diff(&c).unwrap();
    assert!(missing.iter().any(|l| l.starts_with("MISSING") && l.contains("spira-example-beta.service")), "{missing:?}");

    for (name, _, text) in render_all(&c).unwrap() {
        std::fs::write(b.units.join(name), text).unwrap();
    }
    assert!(crate::install_units::diff(&c).unwrap().is_empty(), "matching units report clean");

    let stale = b.units.join("spira-example-beta.service");
    std::fs::write(&stale, "[Service]\nExecStart=/bin/true\n# stale\n").unwrap();
    let differs = crate::install_units::diff(&c).unwrap();
    assert!(differs.iter().any(|l| l.starts_with("DIFFERS") && l.contains("spira-example-beta.service")), "{differs:?}");
}
