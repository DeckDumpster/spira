//! The non-disruptive unit installer (`systemd/unit-ensure.sh`, in Rust): safe while dwarves
//! are active — installs missing or changed unit files and daemon-reloads, but never
//! restarts a running service. `landing-pass` runs this after every pass.

use crate::install_units::{execstart_target_ok, is_masked, render_all, retire_dropped_units, write_unit, Ctx};
use std::fs;

#[derive(Debug, Default)]
pub struct Report {
    pub installed: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: u32,
    pub retired: Vec<String>,
    pub missing_target: Vec<String>,
    pub enabled_started: Vec<String>,
    pub errors: Vec<String>,
}

pub fn run(ctx: &Ctx, retire: bool) -> Report {
    let mut r = Report::default();
    let rendered = match render_all(ctx) {
        Ok(v) => v,
        Err(e) => {
            r.errors.push(e);
            return r;
        }
    };

    let mut newly_installed: std::collections::BTreeSet<String> = Default::default();
    for (inst, _tpl, text) in &rendered {
        let dest = ctx.unit_dir.join(inst);
        if is_masked(&dest) {
            continue;
        }
        match fs::read_to_string(&dest) {
            Ok(installed) if installed == *text => r.unchanged += 1,
            existing => {
                if existing.is_err() {
                    newly_installed.insert(inst.clone());
                }
                if let Err(e) = write_unit(ctx.unit_dir, inst, text) {
                    r.errors.push(format!("{inst}: {e}"));
                    continue;
                }
                if existing.is_err() {
                    r.installed.push(inst.clone());
                } else {
                    r.updated.push(inst.clone());
                }
            }
        }
    }

    if !r.installed.is_empty() || !r.updated.is_empty() {
        let _ = ctx.systemctl.daemon_reload();
    }

    if retire {
        let mut ir = crate::install_units::Report::default();
        retire_dropped_units(ctx, &rendered, &mut ir);
        r.retired = ir.retired;
    }

    // Enable+start only newly installed units in the ENABLE set — updated (DIFFERS) units
    // are not restarted here; that is the operator's call (`units-install`'s job instead).
    for u in ctx.manifest.enable(&ctx.host.instance) {
        if !newly_installed.contains(&u) {
            continue;
        }
        let dest = ctx.unit_dir.join(&u);
        let text = fs::read_to_string(&dest).unwrap_or_default();
        if let Err(e) = execstart_target_ok(&text) {
            r.missing_target.push(format!("{u} ({e})"));
            continue;
        }
        if ctx.systemctl.enable(&u).is_ok() {
            let _ = ctx.systemctl.enable_now(&u);
            r.enabled_started.push(u);
        }
    }

    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{build, Inputs};
    use crate::systemctl::FakeSystemctl;
    use crate::values::HostValues;

    fn host() -> HostValues {
        HostValues { toml: "/h/cfg.toml".into(), home: "/h".into(), repo: "/h".into(), run: "/run".into(), db: "/db".into(), cockpit: "/h/cockpit".into(), dolt_data: "".into(), dolt: "/usr/bin/dolt".into(), prod: "".into(), instance: "prod".into(), snap_stale_s: "600".into(), watchtower_start_timeout_s: "360".into(), path_tail: "".into(), env_bin: "/h/.local/lib/spira-env/bin".into(), sccache_dav_addr: "".into(), repo_map: "".into(), lc_password_file: "/h/lc.credential".into() }
    }

    fn tiny_manifest() -> crate::manifest::Manifest {
        let mut m = build(&Inputs { instance: "prod".into(), dolt_data_set: false, dolt_durable_set: false, inotify_present: true, sccache_dav_addr_set: false, repo_is_git_checkout: true, lc_system_mode: false, watch_names: Ok(vec![]) }).unwrap();
        m.units.retain(|u| u.name == "spira-sentinel.service" || u.name == "spira-sentinel.timer");
        m
    }

    fn td() -> testkit::TempDir {
        testkit::TempDir::new("ensure-test")
    }

    #[test]
    fn ensure_installs_missing_units_and_enables_only_new_ones_but_never_restarts() {
        let base = td();
        let unit_dir = base.join("units");
        let tmpl_dir = base.join("templates");
        std::fs::create_dir_all(&unit_dir).unwrap();
        std::fs::create_dir_all(&tmpl_dir).unwrap();
        std::fs::write(tmpl_dir.join("spira-sentinel.service"), "[Service]\nExecStart=/bin/true\n").unwrap();
        std::fs::write(tmpl_dir.join("spira-sentinel.timer"), "[Unit]\nUnit=spira-sentinel.service\n").unwrap();

        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };

        let r = run(&ctx, false);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.installed.len(), 2);
        assert!(r.enabled_started.contains(&"spira-sentinel-prod.timer".to_string()));
        assert_eq!(sc.restarts.borrow().len(), 0);

        // A second run with the template unchanged: no-op.
        let r2 = run(&ctx, false);
        assert_eq!(r2.installed.len(), 0);
        assert_eq!(r2.updated.len(), 0);
        assert_eq!(r2.unchanged, 2);

        // A changed template installs but does NOT restart or re-enable (operator's call).
        std::fs::write(tmpl_dir.join("spira-sentinel.service"), "[Service]\nExecStart=/bin/false\n").unwrap();
        let r3 = run(&ctx, false);
        assert_eq!(r3.updated, vec!["spira-sentinel-prod.service".to_string()]);
        assert_eq!(sc.restarts.borrow().len(), 0);
    }

    fn retire_fixture(retire: bool) -> (testkit::TempDir, std::path::PathBuf, FakeSystemctl, Report) {
        let base = td();
        let unit_dir = base.join("units");
        let tmpl_dir = base.join("templates");
        std::fs::create_dir_all(&unit_dir).unwrap();
        std::fs::create_dir_all(&tmpl_dir).unwrap();
        std::fs::write(tmpl_dir.join("spira-sentinel.service"), "[Service]\nExecStart=/bin/true\n").unwrap();
        std::fs::write(tmpl_dir.join("spira-sentinel.timer"), "[Unit]\nUnit=spira-sentinel.service\n").unwrap();
        std::fs::write(unit_dir.join("spira-dropped-prod.service"), "[Service]\nExecStart=/bin/true\n").unwrap();
        let manifest = tiny_manifest();
        let h = host();
        let sc = FakeSystemctl::default();
        let no_suspend = |_: &str| false;
        let ctx = Ctx { unit_dir: &unit_dir, templates_dir: &tmpl_dir, host: &h, manifest: &manifest, systemctl: &sc, suspended: &no_suspend, world_halted: false, skip_migrate_watchers: false };
        let r = run(&ctx, retire);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        (base, unit_dir, sc, r)
    }

    #[test]
    fn retire_removes_an_instance_unit_the_release_dropped_and_keeps_the_shipped_ones() {
        let (_base, unit_dir, _sc, r) = retire_fixture(true);
        assert_eq!(r.retired, vec!["spira-dropped-prod.service".to_string()]);
        assert!(!unit_dir.join("spira-dropped-prod.service").exists());
        assert!(unit_dir.join("spira-sentinel-prod.service").exists());
        assert!(unit_dir.join("spira-sentinel-prod.timer").exists());
    }

    #[test]
    fn without_retire_a_dropped_unit_is_left_alone() {
        let (_base, unit_dir, _sc, r) = retire_fixture(false);
        assert!(r.retired.is_empty());
        assert!(unit_dir.join("spira-dropped-prod.service").exists());
    }
}
