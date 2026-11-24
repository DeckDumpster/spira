//! Unit tests over a `Fake` [`crate::ports::World`] — one scenario per decision point
//! doctor.sh's own dedicated suites exercised (test-doctor-concierge-singleton.sh,
//! test-doctor-config-files.sh, test-doctor-duckdb.sh, test-doctor-events-probe.sh,
//! test-doctor-failed-units.sh, test-doctor-gate-compile-check.sh, test-doctor-hotfix.sh,
//! test-doctor-operator-channel.sh, test-doctor-orphan-units.sh, test-doctor-snap-fresh.sh,
//! test-doctor-store.sh — all retired by this bead, their coverage moved here).

use crate::ports::{StoreMeta, World};
use crate::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Fake {
    pub env: RefCell<BTreeMap<String, String>>,
    pub which: RefCell<BTreeMap<String, PathBuf>>,
    pub exec_files: RefCell<Vec<String>>,
    pub deps_release: RefCell<Vec<String>>,
    pub release_status: RefCell<String>,
    pub config_valid: RefCell<Result<(), String>>,
    pub config_migrate: RefCell<String>,
    pub md_files: RefCell<BTreeMap<PathBuf, Vec<String>>>,
    pub overrides: RefCell<Result<String, String>>,
    pub repo_names: RefCell<Vec<String>>,
    pub repo_fields: RefCell<BTreeMap<(String, String), String>>,
    pub cargo_dirs: RefCell<Vec<PathBuf>>,
    pub gate_defs: RefCell<BTreeMap<String, Result<String, String>>>,
    pub dirs: RefCell<Vec<PathBuf>>,
    pub files: RefCell<Vec<PathBuf>>,
    pub contents: RefCell<BTreeMap<PathBuf, String>>,
    pub bd_list: RefCell<Result<String, String>>,
    pub dolt_metrics_off: RefCell<bool>,
    pub dolt_events: RefCell<usize>,
    pub store_meta: RefCell<BTreeMap<PathBuf, StoreMeta>>,
    pub active_units: RefCell<Vec<String>>,
    pub tcp_open: RefCell<bool>,
    pub root_passwordless: RefCell<Option<bool>>,
    pub bd_first_id: RefCell<Option<String>>,
    pub bump_ok: RefCell<bool>,
    pub counter: RefCell<String>,
    pub failed_units: RefCell<Result<Vec<String>, String>>,
    pub watchd: RefCell<Result<String, String>>,
    #[allow(clippy::type_complexity)]
    pub unit_execs: RefCell<Result<Vec<(String, String, String)>, String>>,
    pub enabled_units: RefCell<Result<Vec<String>, String>>,
    pub spira_unit: RefCell<String>,
    pub spira_unit_asked: RefCell<Vec<(String, String)>>,
    pub file_ages: RefCell<BTreeMap<PathBuf, u64>>,
    pub bd_version: RefCell<Option<String>>,
    pub arch: RefCell<String>,
    pub bin_purpose: RefCell<String>,
    pub stray: RefCell<Vec<String>>,
    pub stdout: RefCell<Vec<String>>,
    pub sccache_help: RefCell<Option<String>>,
    pub sccache_show_stats: RefCell<Option<String>>,
    pub sccache_dav_addr: RefCell<Option<String>>,
    pub build_base: RefCell<Vec<PathBuf>>,
    pub tmpfs: RefCell<Vec<PathBuf>>,
    pub unnamed_roots: RefCell<Vec<String>>,
    pub placements: RefCell<Vec<(String, PathBuf, crate::ports::Placement)>>,
    pub daemon_bases: RefCell<Vec<String>>,
    pub hooks_path: RefCell<Option<String>>,
    pub env_provided: RefCell<Vec<String>>,
    pub env_installed: RefCell<Option<String>>,
    pub env_locked: RefCell<Result<String, String>>,
    pub tool_versions: RefCell<BTreeMap<PathBuf, String>>,
    pub submission: RefCell<Option<String>>,
    pub root_scan: RefCell<crate::ports::RootScan>,
    pub root_targets: RefCell<Vec<PathBuf>>,
}

impl Default for Fake {
    fn default() -> Self {
        Fake {
            env: RefCell::new(BTreeMap::new()),
            which: RefCell::new(BTreeMap::new()),
            exec_files: RefCell::new(Vec::new()),
            deps_release: RefCell::new(Vec::new()),
            release_status: RefCell::new(String::new()),
            config_valid: RefCell::new(Ok(())),
            config_migrate: RefCell::new(String::new()),
            md_files: RefCell::new(BTreeMap::new()),
            overrides: RefCell::new(Ok(String::new())),
            repo_names: RefCell::new(Vec::new()),
            repo_fields: RefCell::new(BTreeMap::new()),
            cargo_dirs: RefCell::new(Vec::new()),
            gate_defs: RefCell::new(BTreeMap::new()),
            dirs: RefCell::new(Vec::new()),
            files: RefCell::new(Vec::new()),
            contents: RefCell::new(BTreeMap::new()),
            bd_list: RefCell::new(Err("no fixture".into())),
            dolt_metrics_off: RefCell::new(true),
            dolt_events: RefCell::new(0),
            store_meta: RefCell::new(BTreeMap::new()),
            active_units: RefCell::new(Vec::new()),
            tcp_open: RefCell::new(false),
            root_passwordless: RefCell::new(None),
            bd_first_id: RefCell::new(None),
            bump_ok: RefCell::new(false),
            counter: RefCell::new("0".into()),
            failed_units: RefCell::new(Ok(Vec::new())),
            watchd: RefCell::new(Ok(String::new())),
            unit_execs: RefCell::new(Ok(Vec::new())),
            enabled_units: RefCell::new(Ok(Vec::new())),
            spira_unit: RefCell::new(String::new()),
            spira_unit_asked: RefCell::new(Vec::new()),
            file_ages: RefCell::new(BTreeMap::new()),
            bd_version: RefCell::new(None),
            arch: RefCell::new(String::new()),
            bin_purpose: RefCell::new(String::new()),
            stray: RefCell::new(Vec::new()),
            stdout: RefCell::new(Vec::new()),
            sccache_help: RefCell::new(None),
            sccache_show_stats: RefCell::new(None),
            sccache_dav_addr: RefCell::new(None),
            build_base: RefCell::new(Vec::new()),
            tmpfs: RefCell::new(Vec::new()),
            placements: RefCell::new(Vec::new()),
            unnamed_roots: RefCell::new(Vec::new()),
            daemon_bases: RefCell::new(Vec::new()),
            hooks_path: RefCell::new(None),
            env_provided: RefCell::new(Vec::new()),
            env_installed: RefCell::new(None),
            env_locked: RefCell::new(Err(String::new())),
            tool_versions: RefCell::new(BTreeMap::new()),
            submission: RefCell::new(None),
            root_scan: RefCell::new(crate::ports::RootScan::Seen(Vec::new())),
            root_targets: RefCell::new(Vec::new()),
        }
    }
}

impl Fake {
    fn set(&self, k: &str, v: &str) {
        self.env.borrow_mut().insert(k.to_string(), v.to_string());
    }
}

impl World for Fake {
    fn env(&self, k: &str) -> Option<String> {
        self.env.borrow().get(k).cloned()
    }
    fn which(&self, name: &str) -> Option<PathBuf> {
        self.which.borrow().get(name).cloned()
    }
    fn is_executable_file(&self, p: &str) -> bool {
        self.exec_files.borrow().iter().any(|f| f == p)
    }
    fn deps_list_release(&self) -> Vec<String> {
        self.deps_release.borrow().clone()
    }
    fn release_status(&self) -> String {
        self.release_status.borrow().clone()
    }
    fn spira_config_validate(&self, _toml_path: &Path) -> Result<(), String> {
        self.config_valid.borrow().clone()
    }
    fn spira_config_migrate(&self, _toml_path: &Path) -> String {
        self.config_migrate.borrow().clone()
    }
    fn find_md_files(&self, dir: &Path) -> Vec<String> {
        self.md_files.borrow().get(dir).cloned().unwrap_or_default()
    }
    fn overrides_doctor(&self) -> Result<String, String> {
        self.overrides.borrow().clone()
    }
    fn repo_names(&self) -> Vec<String> {
        self.repo_names.borrow().clone()
    }
    fn repo_field(&self, name: &str, field: &str) -> Option<String> {
        self.repo_fields.borrow().get(&(name.to_string(), field.to_string())).cloned()
    }
    fn dir_has_cargo_toml_within(&self, path: &Path) -> bool {
        self.cargo_dirs.borrow().iter().any(|d| d == path)
    }
    fn gate_definition(&self, _home: &Path, name: &str) -> Result<String, String> {
        self.gate_defs.borrow().get(name).cloned().unwrap_or_else(|| Err("no fixture".into()))
    }
    fn dir_exists(&self, p: &Path) -> bool {
        self.dirs.borrow().contains(&p.to_path_buf())
    }
    fn file_exists(&self, p: &Path) -> bool {
        self.files.borrow().contains(&p.to_path_buf())
    }
    fn read_file(&self, p: &Path) -> Option<String> {
        self.contents.borrow().get(p).cloned()
    }
    fn bd_list(&self, _timeout_secs: u64) -> Result<String, String> {
        self.bd_list.borrow().clone()
    }
    fn read_store_meta(&self, metadata_json: &Path) -> Option<StoreMeta> {
        self.store_meta.borrow().get(metadata_json).cloned()
    }
    fn systemd_user_is_active(&self, unit: &str) -> bool {
        self.active_units.borrow().iter().any(|u| u == unit)
    }
    fn tcp_connect(&self, _host: &str, _port: u16) -> bool {
        *self.tcp_open.borrow()
    }
    fn dolt_metrics_disabled(&self) -> bool {
        *self.dolt_metrics_off.borrow()
    }
    fn dolt_root_passwordless(&self, _host: &str, _port: u16) -> Option<bool> {
        *self.root_passwordless.borrow()
    }
    fn dolt_events_count(&self) -> usize {
        *self.dolt_events.borrow()
    }
    fn bd_first_id(&self) -> Option<String> {
        self.bd_first_id.borrow().clone()
    }
    fn bump_write_event_try(&self, _id: &str, _etype: &str, _actor: &str) -> bool {
        *self.bump_ok.borrow()
    }
    fn counter_events_query(&self, _id: &str, _etype: &str) -> String {
        self.counter.borrow().clone()
    }
    fn systemd_failed_units(&self, _pattern: &str) -> Result<Vec<String>, String> {
        self.failed_units.borrow().clone()
    }
    fn watchd_manifest(&self) -> Result<String, String> {
        self.watchd.borrow().clone()
    }
    fn systemd_enabled_unit_files(&self, _pattern: &str) -> Result<Vec<String>, String> {
        self.enabled_units.borrow().clone()
    }
    fn systemd_installed_unit_execs(&self) -> Result<Vec<(String, String, String)>, String> {
        self.unit_execs.borrow().clone()
    }
    fn spira_unit(&self, kind: &str, subkind: &str) -> String {
        self.spira_unit_asked.borrow_mut().push((kind.into(), subkind.into()));
        self.spira_unit.borrow().clone()
    }
    fn file_age_secs(&self, p: &Path) -> Option<u64> {
        self.file_ages.borrow().get(p).copied()
    }
    fn bd_version(&self) -> Option<String> {
        self.bd_version.borrow().clone()
    }
    fn arch(&self) -> String {
        self.arch.borrow().clone()
    }
    fn spira_bin_purpose(&self, _name: &str) -> String {
        self.bin_purpose.borrow().clone()
    }
    fn concierge_stray_holders(&self, _concierge_sh: &Path) -> Vec<String> {
        self.stray.borrow().clone()
    }
    fn sccache_help(&self) -> Option<String> {
        self.sccache_help.borrow().clone()
    }
    fn sccache_show_stats(&self) -> Option<String> {
        self.sccache_show_stats.borrow().clone()
    }
    fn git_daemon_base_paths(&self, _port: u16) -> Vec<String> {
        self.daemon_bases.borrow().clone()
    }
    fn sccache_dav_addr(&self) -> Option<String> {
        self.sccache_dav_addr.borrow().clone()
    }
    fn dwarf_build_bases(&self) -> Vec<PathBuf> {
        self.build_base.borrow().clone()
    }
    fn placement_roots(&self) -> Vec<(String, Option<PathBuf>)> {
        self.placements.borrow().iter().map(|(k, p, _)| (k.clone(), Some(p.clone()))).chain(self.unnamed_roots.borrow().iter().map(|k| (k.clone(), None))).collect()
    }
    fn root_writers(&self) -> crate::ports::RootScan {
        self.root_scan.borrow().clone()
    }
    fn root_targets(&self) -> Vec<PathBuf> {
        self.root_targets.borrow().clone()
    }
    fn placement_of(&self, p: &Path) -> crate::ports::Placement {
        self.placements.borrow().iter().find(|(_, q, _)| q == p).map(|(_, _, v)| v.clone()).unwrap_or_else(|| crate::ports::Placement::Unresolved("no fixture".into()))
    }
    fn on_tmpfs(&self, p: &Path) -> bool {
        self.tmpfs.borrow().iter().any(|t| p.starts_with(t))
    }
    fn deps_provided_by_spira_env(&self) -> Vec<String> {
        self.env_provided.borrow().clone()
    }
    fn spira_env_installed(&self) -> Option<String> {
        self.env_installed.borrow().clone()
    }
    fn spira_env_locked(&self) -> Result<String, String> {
        self.env_locked.borrow().clone()
    }
    fn tool_version_line(&self, path: &Path) -> Option<String> {
        self.tool_versions.borrow().get(path).cloned()
    }
    fn git_hooks_path(&self, _repo: &Path) -> Option<String> {
        self.hooks_path.borrow().clone()
    }
    fn submission_status(&self) -> Option<String> {
        self.submission.borrow().clone()
    }
    fn out(&self, s: &str) {
        self.stdout.borrow_mut().push(s.to_string());
    }
}

fn levels(lines: &[Line]) -> Vec<Level> {
    lines.iter().map(|l| l.level).collect()
}

// ============================================================================ release tools

#[test_id(tewiv0e)]
#[test]
fn release_tools_all_resolve() {
    let f = Fake::default();
    // world.sh is the `world` binary now (sp-6onps) and mail.sh is the `mail` binary now
    // (sp-ooh1k) -- both come through deps_list_release, not a hand-added suffix; gate
    // comes through deps_list_release too, per the existing fixture below.
    f.deps_release.borrow_mut().push("gate".into());
    f.deps_release.borrow_mut().push("world".into());
    f.deps_release.borrow_mut().push("mail".into());
    f.which.borrow_mut().insert("gate".into(), "bin/gate".into());
    f.which.borrow_mut().insert("world".into(), "bin/world".into());
    f.which.borrow_mut().insert("mail".into(), "bin/mail".into());
    f.which.borrow_mut().insert("gate.sh".into(), "bin/gate.sh".into());
    let out = check_release_tools(&f);
    assert_eq!(levels(&out), vec![Level::Ok]);
}

/// world.sh and mail.sh are NOT hand-added suffixes any more (sp-6onps, sp-ooh1k) -- a box
/// whose deps.toml release tier forgets to name `world` or `mail` must FAIL on it missing,
/// never silently pass because an old hand-written "*.sh" entry papered over it.
#[test_id(tip6clg)]
#[test]
fn release_tools_world_and_mail_binaries_come_through_deps_list_not_a_hand_added_suffix() {
    let f = Fake::default();
    f.which.borrow_mut().insert("gate.sh".into(), "bin/gate.sh".into());
    // deps_release is empty -- "world" and "mail" are deliberately absent from it, and
    // nothing else in check_release_tools should supply either.
    let out = check_release_tools(&f);
    assert_eq!(levels(&out), vec![Level::Ok], "empty deps_release + gate.sh resolved must be clean");
    assert!(!out.iter().any(|l| l.msg.contains("world")), "{:?}", out);
    assert!(!out.iter().any(|l| l.msg.contains("mail")), "{:?}", out);
}

#[test_id(tjcxhjl)]
#[test]
fn release_tools_missing_fails() {
    let f = Fake::default();
    f.deps_release.borrow_mut().push("gate".into());
    let out = check_release_tools(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("gate"));
}

// ============================================================================ hotfix

#[test_id(t2bo9e4)]
#[test]
fn hotfix_none_standing() {
    let f = Fake::default();
    assert_eq!(levels(&check_hotfix(&f)), vec![Level::Ok]);
}

#[test_id(tmtplhc)]
#[test]
fn hotfix_below_threshold_warns() {
    let f = Fake::default();
    *f.release_status.borrow_mut() = "RUNNING UNLANDED abc123: testing a hotfix\n".into();
    let out = check_hotfix(&f);
    assert_eq!(out[0].level, Level::Warn);
}

#[test_id(to24w1j)]
#[test]
fn hotfix_past_alert_threshold_fails() {
    let f = Fake::default();
    *f.release_status.borrow_mut() = "RUNNING UNLANDED abc123: testing\nALERT stood 5h, past 4h threshold\n".into();
    let out = check_hotfix(&f);
    assert_eq!(out[0].level, Level::Fail);
}

// ============================================================================ config files
//
// `check_config_files` no longer goes through `World` at all (per Ryan 2026-10-05: one
// source of config) — it calls `spira_config::locate`/`load`/`process::cfg` directly, so
// these are real-environment tests, not `Fake`-driven ones. `Fake::default()` is passed
// only to satisfy the function's `&dyn World` parameter, which the function itself ignores.

#[test_id(tt36xkz)]
#[test]
fn config_files_fails_when_spira_toml_is_unset() {
    let _env = testkit::env(&[("SPIRA_TOML", None), ("SPIRA_CONF", None)]);
    let f = Fake::default();
    let out = check_config_files(&f);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("SPIRA_TOML is not set"), "{:?}", out[0].msg);
}

#[test_id(teq5xpd)]
#[test]
fn config_files_fails_when_a_named_layer_is_missing() {
    let d = testkit::TempDir::new("doctor-config-files-missing-layer");
    let missing = d.join("no-such-file.toml");
    let _env = testkit::env(&[("SPIRA_TOML", missing.to_str())]);
    let f = Fake::default();
    let out = check_config_files(&f);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("does not exist"), "{:?}", out[0].msg);
    assert!(out[0].msg.contains("no-such-file.toml"), "{:?}", out[0].msg);
}

#[test_id(tjuj1sn)]
#[test]
fn config_files_fails_when_the_document_does_not_validate() {
    let d = testkit::TempDir::new("doctor-config-files-malformed");
    let bad = d.join("bad-config.toml");
    std::fs::write(&bad, "this is not [valid toml").unwrap();
    let _env = testkit::env(&[("SPIRA_TOML", bad.to_str())]);
    let f = Fake::default();
    let out = check_config_files(&f);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0].level, Level::Ok, "the layer itself exists, so this line still passes");
    assert_eq!(out[1].level, Level::Fail);
    assert!(out[1].msg.contains("fails validation"), "{:?}", out[1].msg);
}

// ============================================================================ runes overlays

#[test_id(tgq19jc)]
#[test]
fn runes_overlays_none_active() {
    let f = Fake::default();
    f.set("SPIRA_RUNES_OVERLAY", "overlay");
    assert_eq!(levels(&check_runes_overlays(&f)), vec![Level::Ok]);
}

#[test_id(tfpsrz0)]
#[test]
fn runes_overlays_active_named() {
    let f = Fake::default();
    f.set("SPIRA_RUNES_OVERLAY", "overlay");
    f.md_files.borrow_mut().insert(PathBuf::from("overlay"), vec!["builder/note.md".into()]);
    let out = check_runes_overlays(&f);
    assert_eq!(out[0].level, Level::Ok);
    assert!(out[0].msg.contains("builder/note.md"));
}

// ============================================================================ overrides

#[test_id(tpenmo7)]
#[test]
fn overrides_no_problems() {
    let f = Fake::default();
    *f.overrides.borrow_mut() = Ok(String::new());
    assert_eq!(levels(&check_overrides(&f)), vec![Level::Ok]);
}

#[test_id(tgej74s)]
#[test]
fn overrides_problems_are_failures() {
    let f = Fake::default();
    *f.overrides.borrow_mut() = Err("spec-X: failed to apply\nspec-Y: stale-closed\n".into());
    let out = check_overrides(&f);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|l| l.level == Level::Fail));
}

// ============================================================================ gate compile check

#[test_id(tsbeaq6)]
#[test]
fn gate_compile_check_no_rust_repos() {
    let f = Fake::default();
    let out = check_gate_compile_check(&f);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].level, Level::Ok);
    assert!(out[0].msg.contains("no repository"));
}

#[test_id(tux4ul2)]
#[test]
fn gate_compile_check_reaches_build_fence() {
    let f = Fake::default();
    f.repo_names.borrow_mut().push("spira".into());
    f.repo_fields.borrow_mut().insert(("spira".into(), "path".into()), "repo".into());
    f.cargo_dirs.borrow_mut().push(PathBuf::from("repo"));
    f.gate_defs.borrow_mut().insert("spira".into(), Ok("fence1.sh && bash spira/build-fence.sh && fence2.sh".into()));
    let out = check_gate_compile_check(&f);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].level, Level::Ok);
}

#[test_id(tsfc6w1)]
#[test]
fn gate_compile_check_missing_build_fence_fails() {
    let f = Fake::default();
    f.repo_names.borrow_mut().push("spira".into());
    f.repo_fields.borrow_mut().insert(("spira".into(), "path".into()), "repo".into());
    f.cargo_dirs.borrow_mut().push(PathBuf::from("repo"));
    f.gate_defs.borrow_mut().insert("spira".into(), Ok("fence1.sh && fence2.sh".into()));
    let out = check_gate_compile_check(&f);
    assert_eq!(out[0].level, Level::Fail);
}

#[test_id(ts4vys3)]
#[test]
fn gate_compile_check_unresolved_gate_fails() {
    let f = Fake::default();
    f.repo_names.borrow_mut().push("spira".into());
    f.repo_fields.borrow_mut().insert(("spira".into(), "path".into()), "repo".into());
    f.cargo_dirs.borrow_mut().push(PathBuf::from("repo"));
    f.gate_defs.borrow_mut().insert("spira".into(), Err("no gate.steps".into()));
    let out = check_gate_compile_check(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].detail.as_deref().unwrap_or("").contains("<unresolved: no gate.steps>"));
}

// ============================================================================ store

#[test_id(tk2pl6f)]
#[test]
fn store_missing_beads_fails_outside_install() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    let out = check_store(&f);
    assert_eq!(out[0].level, Level::Fail);
}

#[test_id(ttxoyfl)]
#[test]
fn store_missing_beads_warns_during_install() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.set("SPIRA_DOCTOR_INSTALLING", "1");
    let out = check_store(&f);
    assert_eq!(out[0].level, Level::Warn);
}

#[test_id(t66y835)]
#[test]
fn store_bd_read_ok_and_server_mode() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.dirs.borrow_mut().push(PathBuf::from("db/.beads"));
    *f.bd_list.borrow_mut() = Ok("[]".into());
    f.files.borrow_mut().push(PathBuf::from("db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("db/.beads/metadata.json"), StoreMeta { dolt_mode: Some("server".into()), ..Default::default() });
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.msg.contains("has a .beads")));
    assert!(out.iter().any(|l| l.msg == "bd can read it"));
    assert!(out.iter().any(|l| l.msg.contains("store mode: server")));
    assert!(out.iter().all(|l| l.level != Level::Fail));
}

#[test_id(t3jbxsd)]
#[test]
fn store_embedded_mode_fails() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.dirs.borrow_mut().push(PathBuf::from("db/.beads"));
    *f.bd_list.borrow_mut() = Ok("[]".into());
    f.files.borrow_mut().push(PathBuf::from("db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("db/.beads/metadata.json"), StoreMeta { dolt_mode: Some("embedded".into()), ..Default::default() });
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("embedded mode")));
}

#[test_id(tf8zcaf)]
#[test]
fn store_bd_cannot_read_softens_during_install_when_server_not_active() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.set("SPIRA_DOCTOR_INSTALLING", "1");
    f.set("SPIRA_DOLT_DATA", "dolt");
    f.dirs.borrow_mut().push(PathBuf::from("db/.beads"));
    *f.bd_list.borrow_mut() = Err("connection refused".into());
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Warn && l.msg.contains("bd cannot read")));
}

#[test_id(tydqmdj)]
#[test]
fn store_bd_cannot_read_fails_outside_install() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.dirs.borrow_mut().push(PathBuf::from("db/.beads"));
    *f.bd_list.borrow_mut() = Err("connection refused".into());
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("bd cannot read")));
}

#[test_id(tfauuse)]
#[test]
fn store_dolt_server_answering() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    *f.tcp_open.borrow_mut() = true;
    f.files.borrow_mut().push(PathBuf::from("db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("db/.beads/metadata.json"), StoreMeta { dolt_server_host: "127.0.0.1".into(), dolt_server_port: Some(3306), ..Default::default() });
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Ok && l.msg.contains("answering on 127.0.0.1:3306")));
}

#[test_id(tejmje1)]
#[test]
fn store_dolt_server_unreachable_fails() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    *f.tcp_open.borrow_mut() = false;
    f.files.borrow_mut().push(PathBuf::from("db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("db/.beads/metadata.json"), StoreMeta { dolt_server_host: "127.0.0.1".into(), dolt_server_port: Some(3306), ..Default::default() });
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("no server answers")));
}

// ============================================================================ duckdb

#[test_id(t9m35gn)]
#[test]
fn duckdb_present() {
    let f = Fake::default();
    f.which.borrow_mut().insert("duckdb".into(), "usr/bin/duckdb".into());
    assert_eq!(levels(&check_duckdb(&f)), vec![Level::Ok]);
}

#[test_id(t5437vh)]
#[test]
fn duckdb_missing_fails() {
    let f = Fake::default();
    assert_eq!(levels(&check_duckdb(&f)), vec![Level::Fail]);
}

// ============================================================================ compilation cache

#[test_id(t4n3x5r)]
#[test]
fn sccache_missing_fails_and_names_the_install_command() {
    let f = Fake::default();
    let lines = check_sccache(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].detail.as_deref().unwrap_or("").contains("cargo install sccache --locked --no-default-features --features webdav"));
}

#[test_id(tfkudgn)]
#[test]
fn sccache_missing_only_warns_off_an_operated_box() {
    // deps.toml's own waiver: a testenv fixture container sets SPIRA_BUILD_CACHE=off and
    // carries no sccache at all, on purpose — SPIRA_OPERATED=0 is how check_operator_channel
    // already tells a fixture from a real box, and this check uses the same gate.
    let f = Fake::default();
    f.set("SPIRA_OPERATED", "0");
    assert_eq!(levels(&check_sccache(&f)), vec![Level::Warn]);
}

#[test_id(t8mbd3g)]
#[test]
fn sccache_present_but_built_without_webdav_fails_rather_than_passing_on_presence_alone() {
    let f = Fake::default();
    f.which.borrow_mut().insert("sccache".into(), "opt/spira/cargo/bin/sccache".into());
    *f.sccache_help.borrow_mut() = Some(
        "Usage: sccache ...\n\nEnabled features:\n    S3:        false\n    WebDAV:    false\n    OSS:       false\n".into(),
    );
    let lines = check_sccache(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("without the webdav backend"), "{:?}", lines[0].msg);
    assert!(lines[0].detail.as_deref().unwrap_or("").contains("--features webdav"));
}

#[test_id(t5njk0z)]
#[test]
fn sccache_with_webdav_enabled_passes() {
    let f = Fake::default();
    f.which.borrow_mut().insert("sccache".into(), "opt/spira/cargo/bin/sccache".into());
    *f.sccache_help.borrow_mut() = Some(
        "Usage: sccache ...\n\nEnabled features:\n    S3:        false\n    WebDAV:    true\n    OSS:       false\n".into(),
    );
    assert_eq!(levels(&check_sccache(&f)), vec![Level::Ok]);
}

#[test_id(tonlrng)]
#[test]
fn sccache_present_but_unresponsive_to_help_fails() {
    let f = Fake::default();
    f.which.borrow_mut().insert("sccache".into(), "opt/spira/cargo/bin/sccache".into());
    // sccache_help left at its default None: the binary could not be run (permissions,
    // a wrapper script that shadows the real one, etc).
    let lines = check_sccache(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("did not answer --help"), "{:?}", lines[0].msg);
}

// -------------------------------------------------------- dwarf build directories (sp-ke0uk8)

#[test_id(t71crz0)]
#[test]
fn build_base_check_names_an_unset_key_a_base_in_ram_and_a_good_one() {
    let f = Fake::default();
    let unset = check_dwarf_build(&f);
    assert_eq!(levels(&unset), vec![Level::Fail]);
    assert!(unset[0].msg.contains("SPIRA_DWARF_BUILD_BASE"), "{:?}", unset[0].msg);
    assert!(unset[0].detail.as_deref().unwrap_or("").contains("dwarf_build_base"), "{:?}", unset[0].detail);

    *f.build_base.borrow_mut() = vec!["srv/ssd-b/dwarf-build".into()];
    assert_eq!(levels(&check_dwarf_build(&f)), vec![Level::Ok], "a base on the compile disk is good");

    f.tmpfs.borrow_mut().push("srv/ssd-b".into());
    let ram = check_dwarf_build(&f);
    assert_eq!(levels(&ram), vec![Level::Warn], "planted offender: a base in RAM must be caught");
    assert!(ram[0].msg.contains("tmpfs"), "{:?}", ram[0].msg);

    *f.build_base.borrow_mut() = vec!["srv/ssd-b/dwarf-build".into(), "srv/ssd-c/dwarf-build".into()];
    f.tmpfs.borrow_mut().clear();
    f.tmpfs.borrow_mut().push("srv/ssd-c".into());
    assert_eq!(levels(&check_dwarf_build(&f)), vec![Level::Ok, Level::Warn], "each listed base is judged on its own");
}

// -------------------------------------------------------- disk placement (sp-hpaak4)

#[test_id(t9mcnfd)]
#[test]
fn disk_placement_fails_a_root_on_the_root_fs_and_reports_every_root() {
    use crate::ports::Placement::*;
    let f = Fake::default();
    assert!(check_disk_placement(&f).is_empty());
    *f.placements.borrow_mut() = vec![
        ("podman graphroot".into(), "srv/ssd-c/containers".into(), OnRoot { canonical: "home/x/boot/storage".into(), source: "mapper-root".into() }),
        ("SPIRA_GATE_TARGET_ROOT".into(), "srv/ssd-b/gate".into(), Elsewhere),
        ("SPIRA_DOLT_DATA".into(), "srv/gone".into(), Unresolved("no such file".into())),
    ];
    f.unnamed_roots.borrow_mut().push("SPIRA_GATE_TARGET_ROOT".into());
    let lines = check_disk_placement(&f);
    assert_eq!(levels(&lines), vec![Level::Fail, Level::Ok, Level::Warn, Level::Warn]);
    assert!(lines[3].msg.contains("cannot tell"), "{:?}", lines[3].msg);
    assert!(lines[0].msg.contains("podman graphroot") && lines[0].msg.contains("home/x/boot/storage") && lines[0].msg.contains("mapper-root"), "{:?}", lines[0].msg);
    assert!(lines[0].detail.as_deref().unwrap_or("").contains("disk-model"));
    assert!(lines[2].msg.contains("cannot tell"), "{:?}", lines[2].msg);
}

#[test_id(tc2swle)]
#[test]
fn placement_roots_name_every_key_and_the_store_directory_and_keep_unset_ones() {
    let cfg = |k: &str| match k {
        "SPIRA_LC_STORE_PATH" => Some("vol/store/lc.db".to_string()),
        "SPIRA_DOLT_DATA" => Some("vol/dolt".to_string()),
        "SPIRA_GATE_TARGET_ROOT" => Some(String::new()),
        _ => None,
    };
    let roots = crate::real::placement_roots_from(cfg, vec!["vol/b1".into(), "vol/b2".into()]);
    let get = |k: &str| roots.iter().find(|(n, _)| n.starts_with(k)).map(|(_, p)| p.clone());
    assert_eq!(get("SPIRA_DOLT_DATA"), Some(Some("vol/dolt".into())));
    assert_eq!(get("SPIRA_LC_STORE_PATH"), Some(Some("vol/store".into())));
    for unset in ["SPIRA_GATE_TARGET_ROOT", "SPIRA_CARGO_HOME", "SPIRA_TESTENV_SCRATCH_ROOT", "SPIRA_TESTENV_SANDBOX_TARGET_ROOT", "SPIRA_SCCACHE_DAV_ROOT"] {
        assert_eq!(get(unset), Some(None), "{unset} must be a line even when unset");
    }
    assert_eq!(roots.iter().filter(|(n, _)| n.starts_with("SPIRA_DWARF_BUILD_BASE")).count(), 2);
}

#[test_id(ts3up4x)]
#[test]
fn judge_placement_follows_a_symlink_to_where_it_lands() {
    use crate::ports::Placement::*;
    let d = testkit::TempDir::new("doctor-placement");
    let root = d.path().join("rootfs");
    let target = d.path().join("real");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let link = d.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mounts = format!("fakedev {} ext4 rw 0 0\notherdev nowhere ext4 rw 0 0\n", d.path().display());
    match crate::real::judge_placement(&link, &root, &mounts) {
        OnRoot { canonical, source } => {
            assert_eq!(canonical, target.canonicalize().unwrap());
            assert_eq!(source, "fakedev");
        }
        other => panic!("same device as the passed root must be on root: {other:?}"),
    }
    assert_eq!(crate::real::judge_placement(&link, Path::new(system_paths::PROC), ""), Elsewhere);
    assert!(matches!(crate::real::judge_placement(&d.path().join("missing"), &root, ""), Unresolved(_)));
}

// -------------------------------------------------------- root-disk writers (sp-ohlz3o)

fn writer(pid: u32, comm: &str, path: &str, build: bool) -> crate::ports::RootWriter {
    crate::ports::RootWriter { pid, comm: comm.into(), unit: "app.slice".into(), path: path.into(), build }
}

#[test_id(tu5nzl1)]
#[test]
fn root_writers_fails_a_build_writer_and_warns_for_any_other() {
    use crate::ports::RootScan::Seen;
    let f = Fake::default();
    *f.root_scan.borrow_mut() = Seen(vec![writer(1, "cargo", "home/x/.cargo/.package-cache", true), writer(2, "syslogd", "var/log/syslog", false)]);
    let lines = check_root_writers(&f);
    assert_eq!(levels(&lines), vec![Level::Fail, Level::Warn]);
    assert!(lines[0].msg.contains("cargo") && lines[0].msg.contains("home/x/.cargo") && lines[0].msg.contains("app.slice") && lines[0].msg.contains(": 1)"), "{:?}", lines[0].msg);
    assert!(lines[0].detail.as_deref().unwrap_or("").contains("disk-model"));
}

#[test_id(tq840eq)]
#[test]
fn a_target_a_live_writer_holds_is_not_reported_as_unheld() {
    let f = Fake::default();
    *f.root_scan.borrow_mut() = crate::ports::RootScan::Seen(vec![writer(1, "rustc", "var/tmp/a-target/debug/x", true)]);
    *f.root_targets.borrow_mut() = vec!["var/tmp/a-target".into()];
    assert_eq!(levels(&check_root_writers(&f)), vec![Level::Fail]);
}

#[test_id(t0tquft)]
#[test]
fn scan_finds_a_writer_on_the_root_device_blind_without_proc_and_skips_itself() {
    use crate::ports::RootScan;
    let d = testkit::TempDir::new("doctor-scan");
    let proc = d.path().join("proc");
    let data = d.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let file = data.join("out");
    std::fs::write(&file, b"x").unwrap();
    for (pid, flags) in [("10", "0100002"), ("11", "0100000"), ("12", "0100002")] {
        let p = proc.join(pid);
        std::fs::create_dir_all(p.join("fd")).unwrap();
        std::fs::create_dir_all(p.join("fdinfo")).unwrap();
        std::os::unix::fs::symlink(&file, p.join("fd/3")).unwrap();
        std::fs::write(p.join("fdinfo/3"), format!("flags:\t{flags}\n")).unwrap();
        std::fs::write(p.join("comm"), "cargo\n").unwrap();
        std::fs::write(p.join("environ"), b"RUSTC_WRAPPER=/w\0").unwrap();
        std::fs::write(p.join("cgroup"), "0::/app.slice/u.service\n").unwrap();
    }
    std::fs::create_dir_all(proc.join("13")).unwrap();
    let RootScan::Seen(w) = crate::real::scan_root_writers(&proc, d.path(), 12) else { panic!("expected Seen") };
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!((w[0].pid, w[0].comm.as_str(), w[0].unit.as_str(), w[0].build), (10, "cargo", "u.service", true));
    assert_eq!(w[0].path, file);
    let tgt = d.path().join("tgt");
    std::fs::create_dir_all(&tgt).unwrap();
    let p = proc.join("14");
    std::fs::create_dir_all(p.join("fd")).unwrap();
    std::fs::write(p.join("environ"), format!("CARGO_TARGET_DIR={}\0", tgt.display())).unwrap();
    let RootScan::Seen(w) = crate::real::scan_root_writers(&proc, d.path(), 12) else { panic!("expected Seen") };
    assert!(w.iter().any(|x| x.pid == 14 && x.path == tgt && x.build), "{w:?}");
    assert!(matches!(crate::real::scan_root_writers(&d.path().join("none"), d.path(), 0), RootScan::Blind(_)));
    assert!(matches!(crate::real::scan_root_writers(&proc.join("13"), d.path(), 0), RootScan::Blind(_)), "no readable fd dir is blind, not empty");
}

#[test_id(tgqblrt)]
#[test]
fn target_roots_are_the_tagged_children_only() {
    let d = testkit::TempDir::new("doctor-targets");
    std::fs::create_dir_all(d.path().join("a-target")).unwrap();
    std::fs::write(d.path().join("a-target/CACHEDIR.TAG"), b"").unwrap();
    std::fs::create_dir_all(d.path().join("plain")).unwrap();
    assert_eq!(crate::real::target_roots_in(d.path(), d.path()), vec![d.path().join("a-target")]);
    assert!(crate::real::target_roots_in(d.path(), Path::new(system_paths::PROC)).is_empty());
}

#[test_id(tgeguuu)]
#[test]
fn root_writers_warns_per_root_target_blind_and_says_ok_when_empty() {
    use crate::ports::RootScan::Blind;
    let f = Fake::default();
    assert_eq!(levels(&check_root_writers(&f)), vec![Level::Ok]);
    *f.root_targets.borrow_mut() = vec!["var/tmp/a-target".into(), "var/tmp/b-target".into()];
    let lines = check_root_writers(&f);
    assert_eq!(levels(&lines), vec![Level::Warn, Level::Warn]);
    assert!(lines[0].msg.contains("var/tmp/a-target"), "{:?}", lines[0].msg);
    *f.root_scan.borrow_mut() = Blind("no /proc".into());
    let lines = check_root_writers(&f);
    assert_eq!(levels(&lines), vec![Level::Warn, Level::Warn, Level::Warn]);
    assert!(lines[0].msg.contains("cannot tell who writes") && lines[0].msg.contains("no /proc"));
}

#[test_id(ts9re04)]
#[test]
fn root_writers_folds_twenty_writers_of_one_unit_comm_and_directory_to_one_line() {
    let f = Fake::default();
    *f.root_scan.borrow_mut() = crate::ports::RootScan::Seen((0..20).map(|i| writer(100 + i, "conmon", &format!("home/x/storage/f{i}"), false)).collect());
    let lines = check_root_writers(&f);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].msg.contains("20 pids"), "{:?}", lines[0].msg);
}

#[test_id(to136lj)]
#[test]
fn fdinfo_flags_are_writable_only_for_wronly_or_rdwr() {
    use crate::real::fdinfo_writable;
    assert!(fdinfo_writable("pos:\t0\nflags:\t0100002\nmnt_id:\t1\n"));
    assert!(fdinfo_writable("flags:\t01\n"));
    assert!(!fdinfo_writable("pos:\t0\nflags:\t0100000\n"));
    assert!(!fdinfo_writable("no flags here"));
}

#[test_id(tsej3qg)]
#[test]
fn build_is_a_harness_env_key_or_a_container_scope() {
    use crate::real::{cgroup_unit, is_build};
    assert!(is_build(b"PATH=/bin\0RUSTC_WRAPPER=/x/sccache\0", "0::/app.slice/a.service\n"));
    assert!(is_build(b"CARGO_TARGET_DIR=/srv/t\0", ""));
    assert!(is_build(b"", "0::/user.slice/libpod-abc.scope/container\n"));
    assert!(is_build(b"PATH=/bin\0RUSTC_WRAPPER=\0", "0::/app.slice/a.service\n"));
    assert!(!is_build(b"PATH=/bin\0", "0::/app.slice/a.service\n"));
    assert_eq!(cgroup_unit("0::/user.slice/libpod-abc.scope/container\n"), "container");
}

#[test_id(t6vq650)]
#[test]
fn same_device_follows_a_symlink_and_reports_unstatable_as_none() {
    use crate::real::same_device;
    let d = testkit::TempDir::new("doctor-samedev");
    let file = d.path().join("f");
    std::fs::write(&file, b"x").unwrap();
    let link = d.path().join("l");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert_eq!(same_device(&file, d.path()), Some(true));
    assert_eq!(same_device(&link, d.path()), Some(true));
    assert_eq!(same_device(&file, Path::new(system_paths::PROC)), Some(false));
    assert_eq!(same_device(&d.path().join("missing"), d.path()), None);
}

// -------------------------------------------------------- compilation cache backend (sp-xtdqi)

#[test_id(teva3i6)]
#[test]
fn backend_check_is_a_no_op_when_no_store_is_configured() {
    let f = Fake::default();
    // SPIRA_SCCACHE_DAV_ADDR left unset.
    assert_eq!(levels(&check_sccache_backend(&f)), vec![Level::Ok]);
    assert!(check_sccache_backend(&f)[0].msg.contains("no shared store configured"));
}

/// THE POSITIVE CONTROL: a configured store whose live server answers with a NON-webdav
/// `Cache location` (the local-disk shape) must FAIL on an operated box — this is the exact
/// defect sp-xtdqi exists to catch (a gate restarted the daemon before anyone re-exported
/// `SCCACHE_WEBDAV_ENDPOINT`, and it silently kept answering from disk).
#[test_id(t7rlnzo)]
#[test]
fn backend_check_fails_when_the_live_server_is_on_a_different_backend() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    *f.sccache_show_stats.borrow_mut() = Some("Compile requests                      0\nCache location                  Local disk: \"/var/cache/sccache\"\nVersion (client)                0.18.0\n".into());
    let lines = check_sccache_backend(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("NOT on the shared store"), "{:?}", lines[0].msg);
    assert!(lines[0].msg.contains("Local disk"), "{:?}", lines[0].msg);
}

#[test_id(t5mp6cq)]
#[test]
fn backend_check_only_warns_off_an_operated_box() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    f.set("SPIRA_OPERATED", "0");
    *f.sccache_show_stats.borrow_mut() = Some("Cache location                  Local disk: \"/x\"\n".into());
    assert_eq!(levels(&check_sccache_backend(&f)), vec![Level::Warn]);
}

#[test_id(tz9nipg)]
#[test]
fn backend_check_passes_when_the_live_server_is_on_the_store() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    *f.sccache_show_stats.borrow_mut() = Some("Cache location                  webdav, name: , prefix: /\nVersion (client)                0.18.0\n".into());
    assert_eq!(levels(&check_sccache_backend(&f)), vec![Level::Ok]);
}

#[test_id(tq2mcs9)]
#[test]
fn backend_check_fails_when_show_stats_does_not_answer() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    // sccache_show_stats left at its default None.
    let lines = check_sccache_backend(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("did not answer"), "{:?}", lines[0].msg);
}

#[test_id(t0mh1do)]
#[test]
fn backend_check_fails_when_show_stats_answers_with_no_cache_location_line() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    *f.sccache_show_stats.borrow_mut() = Some("some unexpected garbage\n".into());
    let lines = check_sccache_backend(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("did not report a Cache location"), "{:?}", lines[0].msg);
}

/// THE FULL PIPELINE (sp-xtdqi-3): an address that lives only in the host config
/// document — never the environment — still feeds `check_sccache_backend`, which FAILS on
/// a fake "Local disk" location. This is the exact production scenario (configured,
/// unexported, server on the wrong backend) that used to report "no shared store
/// configured" blind, because `World::env` (`conf.sh`'s own bash capture) never carried a
/// NO-DEFAULT key like this one even when the host config document genuinely configured
/// it. `World::sccache_dav_addr` resolves this through `spira_config::process::cfg` alone
/// now (per Ryan 2026-10-05: one source of config) — the address below stands in for that
/// already-proven resolution; this test is about `check_sccache_backend`'s own judgement.
#[test_id(tfys02k)]
#[test]
fn backend_check_fails_on_an_address_resolved_from_a_fixture_config_with_nothing_exported() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".to_string());
    *f.sccache_show_stats.borrow_mut() = Some("Cache location                  Local disk: \"/var/cache/sccache\"\n".into());
    let lines = check_sccache_backend(&f);
    assert_eq!(levels(&lines), vec![Level::Fail], "{lines:?}");
    assert!(lines[0].msg.contains("NOT on the shared store"), "{:?}", lines[0].msg);
}

// ============================================================================ events probe

#[test_id(t8fnlt4)]
#[test]
fn events_probe_no_database_warns() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    assert_eq!(levels(&check_events_probe(&f)), vec![Level::Warn]);
}

#[test_id(tb050ck)]
#[test]
fn events_probe_no_anchor_bead_warns() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.dirs.borrow_mut().push(PathBuf::from("db/.beads"));
    let out = check_events_probe(&f);
    assert_eq!(out[0].level, Level::Warn);
}

#[test_id(taib3lc)]
#[test]
fn events_probe_round_trip_ok() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.dirs.borrow_mut().push(PathBuf::from("db/.beads"));
    *f.bd_first_id.borrow_mut() = Some("sp-123".into());
    *f.bump_ok.borrow_mut() = true;
    *f.counter.borrow_mut() = "3".into();
    assert_eq!(levels(&check_events_probe(&f)), vec![Level::Ok]);
}

#[test_id(t28don7)]
#[test]
fn events_probe_write_refused_fails() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.dirs.borrow_mut().push(PathBuf::from("db/.beads"));
    *f.bd_first_id.borrow_mut() = Some("sp-123".into());
    *f.bump_ok.borrow_mut() = false;
    *f.counter.borrow_mut() = "0".into();
    let out = check_events_probe(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("was refused"));
}

#[test_id(txs5394)]
#[test]
fn events_probe_write_discarded_fails() {
    let f = Fake::default();
    f.set("SPIRA_DB", "db");
    f.dirs.borrow_mut().push(PathBuf::from("db/.beads"));
    *f.bd_first_id.borrow_mut() = Some("sp-123".into());
    *f.bump_ok.borrow_mut() = true;
    *f.counter.borrow_mut() = "?".into();
    let out = check_events_probe(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("are discarded"));
}

// ============================================================================ failed units

#[test_id(tir9coj)]
#[test]
fn failed_units_none() {
    let f = Fake::default();
    *f.failed_units.borrow_mut() = Ok(vec![]);
    assert_eq!(levels(&check_failed_units(&f)), vec![Level::Ok]);
}

#[test_id(t5wfc7o)]
#[test]
fn failed_units_outside_install_fails() {
    let f = Fake::default();
    *f.failed_units.borrow_mut() = Ok(vec!["spira-gate-prod.service".into()]);
    let out = check_failed_units(&f);
    assert_eq!(out[0].level, Level::Fail);
}

#[test_id(t600id1)]
#[test]
fn failed_units_during_install_warns() {
    let f = Fake::default();
    f.set("SPIRA_DOCTOR_INSTALLING", "1");
    *f.failed_units.borrow_mut() = Ok(vec!["spira-call-prod.service".into()]);
    let out = check_failed_units(&f);
    assert_eq!(out[0].level, Level::Warn);
    assert!(out[0].msg.contains("predates this install"));
}

#[test_id(t5h2e0n)]
#[test]
fn failed_unit_hint_names_no_journal_read() {
    for installing in [false, true] {
        let f = Fake::default();
        if installing {
            f.set("SPIRA_DOCTOR_INSTALLING", "1");
        }
        *f.failed_units.borrow_mut() = Ok(vec!["spira-gate-prod.service".into()]);
        let hint = check_failed_units(&f)[0].detail.clone().unwrap_or_default();
        assert!(hint.contains("systemctl --user status spira-gate-prod.service"), "{hint}");
        assert!(!hint.contains("journal"), "{hint}");
    }
}

#[test_id(tx89rf5)]
#[test]
fn failed_units_query_fault_fails() {
    let f = Fake::default();
    *f.failed_units.borrow_mut() = Err("Failed to connect to bus".into());
    let out = check_failed_units(&f);
    assert_eq!(out[0].level, Level::Fail);
}

// ============================================================================ orphan units

#[test_id(trs87uu)]
#[test]
fn orphan_units_none() {
    let f = Fake::default();
    *f.watchd.borrow_mut() = Ok(String::new());
    *f.enabled_units.borrow_mut() = Ok(vec![]);
    assert_eq!(levels(&check_orphan_units(&f)), vec![Level::Ok]);
}

#[test_id(tyw2o4j)]
#[test]
fn orphan_units_known_watcher_is_fine() {
    let f = Fake::default();
    f.set("SPIRA_INSTANCE", "prod");
    *f.watchd.borrow_mut() = Ok("queue-watch|daemon|\n".into());
    *f.enabled_units.borrow_mut() = Ok(vec!["spira-watch-queue-watch-prod.service".into()]);
    assert_eq!(levels(&check_orphan_units(&f)), vec![Level::Ok]);
}

#[test_id(t7o7te7)]
#[test]
fn orphan_units_unknown_watcher_fails() {
    let f = Fake::default();
    f.set("SPIRA_INSTANCE", "prod");
    *f.watchd.borrow_mut() = Ok(String::new());
    *f.enabled_units.borrow_mut() = Ok(vec!["spira-watch-retired-thing-prod.service".into()]);
    let out = check_orphan_units(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("retired-thing"));
}

/// A watcher retired to a non-daemon kind (e.g. `log`) still orphans the old unit — a row
/// that changed kind is not the same as a row still owning that unit. (test-doctor-orphan-
/// units.sh property 3, retired to this crate's own tests, sp-yyk47.)
#[test_id(tqhfce8)]
#[test]
fn orphan_units_watcher_retired_to_non_daemon_kind_still_orphans() {
    let f = Fake::default();
    f.set("SPIRA_INSTANCE", "prod");
    *f.watchd.borrow_mut() = Ok("retired-thing|log|\n".into());
    *f.enabled_units.borrow_mut() = Ok(vec!["spira-watch-retired-thing-prod.service".into()]);
    let out = check_orphan_units(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("retired-thing"));
}

/// systemctl itself failing (unreachable manager) is a FAIL, never a silent "no orphan
/// units" — an unanswerable probe must not read as a clean bill of health. (test-doctor-
/// orphan-units.sh property 4.)
#[test_id(txhd1qe)]
#[test]
fn orphan_units_probe_failure_is_fail_not_silent_ok() {
    let f = Fake::default();
    f.set("SPIRA_INSTANCE", "prod");
    *f.watchd.borrow_mut() = Ok(String::new());
    *f.enabled_units.borrow_mut() = Err("systemctl: unreachable manager".into());
    let out = check_orphan_units(&f);
    assert_eq!(out[0].level, Level::Fail);
}

// ============================================================================ snapshot fresh

#[test_id(te3vkgc)]
#[test]
fn snapshot_missing_warns() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "run");
    assert_eq!(levels(&check_snapshot_fresh(&f)), vec![Level::Warn]);
}

#[test_id(tctc60p)]
#[test]
fn snapshot_fresh_ok() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "run");
    f.file_ages.borrow_mut().insert(PathBuf::from("run/cockpit.env"), 5);
    assert_eq!(levels(&check_snapshot_fresh(&f)), vec![Level::Ok]);
}

#[test_id(taeuarf)]
#[test]
fn snapshot_stale_fails() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "run");
    f.set("SPIRA_SNAP_STALE_S", "60");
    f.file_ages.borrow_mut().insert(PathBuf::from("run/cockpit.env"), 120);
    let out = check_snapshot_fresh(&f);
    assert_eq!(out[0].level, Level::Fail);
}

#[test_id(twh0e3s)]
#[test]
fn a_stale_snapshot_hint_names_the_scheduler_unit() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "run");
    f.file_ages.borrow_mut().insert(PathBuf::from("run/cockpit.env"), 600);
    check_snapshot_fresh(&f);
    assert_eq!(*f.spira_unit_asked.borrow(), vec![("runtime".to_string(), "service".to_string())]);
}

#[test_id(t291yzr)]
#[test]
fn snapshot_threshold_key_decides_between_fresh_and_stale() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "run");
    f.set("SPIRA_SNAP_STALE_S", "7");
    f.file_ages.borrow_mut().insert(PathBuf::from("run/cockpit.env"), 20);
    assert_eq!(levels(&check_snapshot_fresh(&f)), vec![Level::Fail]);
    f.file_ages.borrow_mut().insert(PathBuf::from("run/cockpit.env"), 0);
    assert_eq!(levels(&check_snapshot_fresh(&f)), vec![Level::Ok]);
}

// ============================================================================ operator channel

#[test_id(t2g3erj)]
#[test]
fn operator_channel_everything_present() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "bin/inotifywait".into());
    f.which.borrow_mut().insert("go".into(), "usr/bin/go".into());
    f.exec_files.borrow_mut().push("usr/bin/go".into());
    let out = check_operator_channel(&f);
    assert!(out.iter().all(|l| l.level == Level::Ok));
}

#[test_id(t787kkk)]
#[test]
fn operator_channel_reports_mail_mute_only_when_on() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "bin/inotifywait".into());
    f.which.borrow_mut().insert("go".into(), "usr/bin/go".into());
    f.exec_files.borrow_mut().push("usr/bin/go".into());
    let has = |f: &Fake| check_operator_channel(f).iter().any(|l| l.msg.contains("mail is muted"));
    assert!(!has(&f));
    f.set("SPIRA_MAIL_MUTE", "0");
    assert!(!has(&f));
    f.set("SPIRA_MAIL_MUTE", "true");
    assert!(has(&f));
    f.set("SPIRA_MAIL_MUTE", "1");
    assert!(has(&f));
    assert!(check_operator_channel(&f).iter().all(|l| l.level == Level::Ok));
}

#[test_id(tj1fldf)]
#[test]
fn operator_channel_missing_inotify_fails_when_operated() {
    let f = Fake::default();
    f.exec_files.borrow_mut().push("usr/bin/go".into());
    f.which.borrow_mut().insert("go".into(), "usr/bin/go".into());
    let out = check_operator_channel(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("inotifywait")));
}

#[test_id(t6vmao1)]
#[test]
fn operator_channel_missing_inotify_warns_when_not_operated() {
    let f = Fake::default();
    f.set("SPIRA_OPERATED", "0");
    f.exec_files.borrow_mut().push("usr/bin/go".into());
    f.which.borrow_mut().insert("go".into(), "usr/bin/go".into());
    let out = check_operator_channel(&f);
    assert!(out.iter().any(|l| l.level == Level::Warn && l.msg.contains("inotifywait")));
}

#[test_id(t4dnizj)]
#[test]
fn operator_channel_no_go_but_bd_current_and_prebuilt_is_ok() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "bin/inotifywait".into());
    f.set("SPIRA_BD_TAG", "v1.2.3");
    *f.bd_version.borrow_mut() = Some("1.2.3".into());
    *f.arch.borrow_mut() = "x86_64".into();
    let out = check_operator_channel(&f);
    assert!(out.iter().all(|l| l.level == Level::Ok));
}

#[test_id(thoomga)]
#[test]
fn operator_channel_no_go_mismatched_bd_no_prebuilt_fails() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "bin/inotifywait".into());
    f.set("SPIRA_BD_TAG", "v1.2.3");
    *f.bd_version.borrow_mut() = Some("1.0.0".into());
    *f.arch.borrow_mut() = "riscv64".into();
    let out = check_operator_channel(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("go is not on PATH")));
}

#[test_id(tp7f6jl)]
#[test]
fn operator_channel_hunk_checked_only_when_configured() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "bin/inotifywait".into());
    f.which.borrow_mut().insert("go".into(), "usr/bin/go".into());
    f.exec_files.borrow_mut().push("usr/bin/go".into());
    f.set("COCKPIT_SESSIONS", "chat hunk");
    let out = check_operator_channel(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("hunk")));
}

// ============================================================================ concierge singleton

#[test_id(tx9actj)]
#[test]
fn concierge_singleton_none_found() {
    let f = Fake::default();
    f.set("SPIRA_REPO", "repo");
    f.exec_files.borrow_mut().push("repo/concierge.sh".into());
    assert_eq!(levels(&check_concierge_singleton(&f)), vec![Level::Ok]);
}

#[test_id(tqqq53c)]
#[test]
fn concierge_singleton_stray_found_fails() {
    let f = Fake::default();
    f.set("SPIRA_REPO", "repo");
    f.exec_files.borrow_mut().push("repo/concierge.sh".into());
    f.stray.borrow_mut().push("12345".into());
    let out = check_concierge_singleton(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].detail.as_deref().unwrap_or("").contains("12345"));
}

#[test_id(te924sx)]
#[test]
fn concierge_singleton_resolves_from_release_when_repo_unset() {
    let f = Fake::default();
    f.set("SPIRA_HOME", "rel/spira");
    f.exec_files.borrow_mut().push("rel/spira/../concierge.sh".into());
    assert_eq!(levels(&check_concierge_singleton(&f)), vec![Level::Ok]);
}

#[test_id(tustlkp)]
#[test]
fn concierge_singleton_no_concierge_sh_warns() {
    let f = Fake::default();
    f.set("SPIRA_REPO", "repo");
    let out = check_concierge_singleton(&f);
    assert_eq!(out[0].level, Level::Warn);
}

// ============================================================================ run() overall tally

// ============================================================================ prod checkout

fn exec_row(unit: &str, state: &str, path: &str) -> (String, String, String) {
    (unit.into(), state.into(), path.into())
}

#[test_id(tey3b8c)]
#[test]
fn prod_checkout_unit_outside_releases_fails() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![exec_row("spira-sentinel-prod.service", "enabled", "fixture/checkout/spira/sentinel.sh")]);
    let out = check_prod_checkout(&f);
    assert_eq!(levels(&out), vec![Level::Fail]);
    assert!(out[0].msg.contains("spira-sentinel-prod.service"));
    assert!(out[0].detail.as_deref().unwrap().contains("fixture/checkout/spira/sentinel.sh"));
}

#[test_id(td9ms3r)]
#[test]
fn prod_checkout_all_under_releases_is_ok() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![exec_row("spira-x.service", "enabled", "fixture/releases/current/x.sh")]);
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Ok]);
}

#[test_id(t9c1p7r)]
#[test]
fn prod_checkout_sibling_prefix_is_outside() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![exec_row("spira-x.service", "enabled", "fixture/releases-old/x")]);
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Fail]);
}

#[test_id(tsabdta)]
#[test]
fn prod_checkout_each_offender_gets_a_line() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![
        exec_row("a.service", "enabled", "co/a"),
        exec_row("b.service", "enabled", "fixture/releases/b"),
        exec_row("c.service", "static", "co/c"),
    ]);
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Fail, Level::Fail]);
}

#[test_id(tp5zzdj)]
#[test]
fn prod_checkout_transient_and_dwarf_units_are_not_flagged() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![
        exec_row("spira-job.service", "transient", "co/job"),
        exec_row("spira-dwarf-x.service", "static", "co/dwarf"),
        exec_row("spira-nopath.service", "enabled", ""),
    ]);
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Ok]);
}

#[test_id(t94ol5h)]
#[test]
fn prod_checkout_probe_failure_fails() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "fixture/releases");
    *f.unit_execs.borrow_mut() = Err("Failed to connect to bus".into());
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Fail]);
}

#[test_id(tzd96f1)]
#[test]
fn mirror_check_warns_on_a_daemon_serving_another_state_dir() {
    let f = Fake::default();
    f.set("SPIRA_ROUND_VM_STATE_DIR", "srv/round-vm");
    *f.daemon_bases.borrow_mut() = vec!["old/place".into()];
    let lines = check_round_vm_mirror(&f);
    assert_eq!(levels(&lines), vec![Level::Warn]);
    assert!(lines[0].msg.contains("old/place") && lines[0].msg.contains("srv/round-vm"), "{:?}", lines[0].msg);
}

#[test_id(tljhnqz)]
#[test]
fn mirror_check_passes_on_the_configured_dir_and_when_no_daemon_runs() {
    let f = Fake::default();
    f.set("SPIRA_ROUND_VM_STATE_DIR", "srv/round-vm");
    assert_eq!(levels(&check_round_vm_mirror(&f)), vec![Level::Ok]);
    *f.daemon_bases.borrow_mut() = vec!["srv/round-vm/".into()];
    assert_eq!(levels(&check_round_vm_mirror(&f)), vec![Level::Ok]);
}

#[test_id(tz3vp7v)]
#[test]
fn mirror_check_defaults_the_state_dir_to_run_round_vm() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "r");
    *f.daemon_bases.borrow_mut() = vec!["r/round-vm".into()];
    assert_eq!(levels(&check_round_vm_mirror(&f)), vec![Level::Ok]);
    *f.daemon_bases.borrow_mut() = vec!["elsewhere".into()];
    assert_eq!(levels(&check_round_vm_mirror(&f)), vec![Level::Warn]);
}

#[test_id(t87x8s6)]
#[test]
fn daemon_base_path_reads_only_a_git_daemon_on_the_port() {
    let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let argv = v(&["git", "daemon", "--port=9430", "--base-path=x", "--export-all"]);
    assert_eq!(crate::real::daemon_base_path(&argv, 9430), Some("x".into()));
    assert_eq!(crate::real::daemon_base_path(&argv, 9431), None);
    assert_eq!(crate::real::daemon_base_path(&v(&["vim", "daemon", "--port=9430", "--base-path=x"]), 9430), None);
}


#[test_id(tybookw)]
#[test]
fn dolt_telemetry_check_passes_when_disabled_and_the_backlog_is_small() {
    let f = Fake::default();
    f.env.borrow_mut().insert("SPIRA_DOLT_DATA".into(), "d".into());
    let out = check_dolt_telemetry(&f);
    assert!(out.iter().all(|l| l.level == Level::Ok), "{out:?}");
}

#[test_id(tj9px7h)]
#[test]
fn dolt_telemetry_check_fails_when_metrics_are_not_disabled() {
    let f = Fake::default();
    f.env.borrow_mut().insert("SPIRA_DOLT_DATA".into(), "d".into());
    *f.dolt_metrics_off.borrow_mut() = false;
    let out = check_dolt_telemetry(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("metrics.disabled")), "{out:?}");
}

#[test_id(t97gv7d)]
#[test]
fn dolt_telemetry_check_fails_on_an_events_backlog_over_the_bound() {
    let f = Fake::default();
    f.env.borrow_mut().insert("SPIRA_DOLT_DATA".into(), "d".into());
    *f.dolt_events.borrow_mut() = 101;
    assert!(check_dolt_telemetry(&f).iter().any(|l| l.level == Level::Fail && l.msg.contains("eventsData")));
    *f.dolt_events.borrow_mut() = 100;
    assert!(check_dolt_telemetry(&f).iter().all(|l| l.level == Level::Ok));
}

#[test_id(tarift7)]
#[test]
fn dolt_telemetry_check_is_silent_on_a_host_that_runs_no_dolt_server() {
    let f = Fake::default();
    *f.dolt_metrics_off.borrow_mut() = false;
    *f.dolt_events.borrow_mut() = 500;
    assert!(check_dolt_telemetry(&f).is_empty());
}

fn root_fixture(state: Option<bool>) -> Fake {
    let f = Fake::default();
    f.env.borrow_mut().insert("SPIRA_DB".into(), "db".into());
    f.files.borrow_mut().push(PathBuf::from("db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("db/.beads/metadata.json"), StoreMeta { dolt_server_host: "127.0.0.1".into(), dolt_server_port: Some(3307), ..Default::default() });
    *f.root_passwordless.borrow_mut() = state;
    f
}

#[test_id(t1kymmo)]
#[test]
fn doctor_flags_a_passwordless_dolt_root() {
    let out = check_root_closed(&root_fixture(Some(true)));
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("passwordless")), "{out:?}");
}

#[test_id(t3rwr98)]
#[test]
fn doctor_accepts_a_closed_dolt_root_and_is_silent_when_it_cannot_probe() {
    assert!(check_root_closed(&root_fixture(Some(false))).iter().all(|l| l.level == Level::Ok));
    assert!(check_root_closed(&root_fixture(None)).is_empty());
}

// ============================================================================ git hooks

fn hooks_fixture(path: Option<&str>) -> Fake {
    hooks_fixture_at("repo", path)
}

fn hooks_fixture_at(repo: &str, path: Option<&str>) -> Fake {
    let f = Fake::default();
    f.set("SPIRA_REPO", repo);
    f.set("SPIRA_HOME", &format!("{repo}/spira"));
    *f.hooks_path.borrow_mut() = path.map(String::from);
    f
}

#[test_id(t437xd8)]
#[test]
fn hooks_path_fails_naming_a_displaced_value() {
    let out = check_hooks_path(&hooks_fixture(Some("somewhere/else")));
    assert_eq!(levels(&out), vec![Level::Fail]);
    assert!(out[0].msg.contains("somewhere/else") && out[0].msg.contains("repo/spira/hooks"), "{:?}", out[0].msg);
}

#[test_id(tinww7y)]
#[test]
fn hooks_path_fails_when_unset() {
    assert_eq!(levels(&check_hooks_path(&hooks_fixture(None))), vec![Level::Fail]);
}

#[test_id(ts3pui7)]
#[test]
fn hooks_path_passes_relative_and_absolute_forms() {
    assert_eq!(levels(&check_hooks_path(&hooks_fixture(Some("spira/hooks")))), vec![Level::Ok]);
    let root = testkit::TempDir::new("doctor-hooks-path");
    let repo = root.join("repo").display().to_string();
    let abs = format!("{repo}/spira/hooks/");
    assert_eq!(levels(&check_hooks_path(&hooks_fixture_at(&repo, Some(&abs)))), vec![Level::Ok]);
}

const STORE_A: &str = "nix/store/aaa-spira-env";
const STORE_B: &str = "nix/store/bbb-spira-env";

fn env_fixture(installed: &str, locked: &str, bd: &str) -> Fake {
    let f = Fake::default();
    f.set("HOME", "h");
    *f.env_provided.borrow_mut() = vec!["bd".into()];
    *f.env_installed.borrow_mut() = Some(installed.into());
    *f.env_locked.borrow_mut() = Ok(locked.into());
    f.which.borrow_mut().insert("bd".into(), PathBuf::from(bd));
    f
}

#[test_id(tqxutg3)]
#[test]
fn spira_env_clean_when_installed_matches_locked_and_tool_resolves_inside() {
    let f = env_fixture(STORE_A, STORE_A, "h/.local/lib/spira-env/bin/bd");
    assert_eq!(levels(&check_spira_env(&f)), vec![Level::Ok]);
}

#[test_id(tu3eqo3)]
#[test]
fn spira_env_names_both_paths_when_stale() {
    let f = env_fixture(STORE_A, STORE_B, "h/.local/lib/spira-env/bin/bd");
    let out = check_spira_env(&f);
    assert_eq!(levels(&out), vec![Level::Warn]);
    let d = out[0].detail.as_deref().unwrap();
    assert!(d.contains(STORE_A) && d.contains(STORE_B), "{d}");
}

#[test_id(tearsvw)]
#[test]
fn spira_env_flags_a_stray_binary_ahead_of_the_env() {
    let f = env_fixture(STORE_A, STORE_A, "h/.local/bin/bd");
    let out = check_spira_env(&f);
    assert_eq!(levels(&out), vec![Level::Ok, Level::Warn]);
    assert!(out[1].msg.contains("h/.local/bin/bd"), "{:?}", out[1].msg);
}

#[test_id(tgqqznb)]
#[test]
fn spira_env_flags_a_commit_stamped_dev_build() {
    let f = env_fixture(STORE_A, STORE_A, "h/.local/bin/bd");
    f.tool_versions.borrow_mut().insert(PathBuf::from("h/.local/bin/bd"), "bd version 1.3.1 (dev: abc1234)".into());
    let out = check_spira_env(&f);
    assert!(out.iter().any(|l| l.msg.contains("dev build")), "{out:?}");
}

#[test_id(tfrya0t)]
#[test]
fn spira_env_warns_when_not_installed() {
    let f = env_fixture(STORE_A, STORE_A, "h/.local/lib/spira-env/bin/bd");
    *f.env_installed.borrow_mut() = None;
    assert_eq!(levels(&check_spira_env(&f)), vec![Level::Warn]);
}

// ============================================================================ mail submission

fn submission(status: &str) -> Vec<Line> {
    let f = Fake::default();
    *f.submission.borrow_mut() = Some(status.to_string());
    check_submission(&f)
}

const HEALTHY: &str = "configured=1\ntailnet=1\nlistening=1\naddr=100.64.0.1:2587\niface=tailscale0\nfirewall=open\ncert_days=80\npassword=ok\n";

#[test_id(tpm4b9c)]
#[test]
fn submission_without_the_operator_grant_names_the_command() {
    let out = submission("configured=0\ntailnet=1\noperator=missing\n");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].level, Level::Warn);
    assert!(out[0].detail.as_deref().unwrap_or("").contains("tailscale set --operator"), "{:?}", out[0].detail);
}

#[test_id(t96o67v)]
#[test]
fn submission_absent_binary_is_silent() {
    assert!(check_submission(&Fake::default()).is_empty());
}

#[test_id(t36743d)]
#[test]
fn submission_healthy_is_all_ok() {
    assert!(submission(HEALTHY).iter().all(|l| l.level == Level::Ok));
}

#[test_id(twi0e20)]
#[test]
fn submission_not_listening_hint_points_at_the_producers_log() {
    let out = submission(&HEALTHY.replace("listening=1", "listening=0"));
    let hint = out.iter().find(|l| l.msg.contains("not listening")).and_then(|l| l.detail.clone()).unwrap_or_default();
    assert!(hint.contains("spira-submission.log"), "{hint}");
    assert!(!hint.contains("journal"), "{hint}");
}

#[test_id(t68igba)]
#[test]
fn submission_configured_but_not_listening_fails() {
    let out = submission(&HEALTHY.replace("listening=1", "listening=0"));
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("not listening")), "{out:?}");
}

#[test_id(t4kvm1m)]
#[test]
fn submission_listening_behind_a_closed_firewall_is_its_own_row() {
    let out = submission(&HEALTHY.replace("firewall=open", "firewall=closed"));
    let l = out.iter().find(|l| l.level == Level::Fail).expect("a failing row");
    assert!(l.msg.contains("unreachable on the tailnet interface"), "{l:?}");
    assert!(!l.msg.contains("not listening"));
    assert!(l.detail.as_deref().unwrap().contains("ufw allow in on tailscale0 to any port 2587"));
}

#[test_id(tjocib0)]
#[test]
fn submission_certificate_under_21_days_warns_and_expired_fails() {
    let out = submission(&HEALTHY.replace("cert_days=80", "cert_days=20"));
    assert!(out.iter().any(|l| l.level == Level::Warn && l.msg.contains("expires in 20 days")), "{out:?}");
    assert!(submission(&HEALTHY.replace("cert_days=80", "cert_days=21")).iter().all(|l| l.level == Level::Ok));
    assert!(submission(&HEALTHY.replace("cert_days=80", "cert_days=0")).iter().any(|l| l.level == Level::Fail));
}

#[test_id(tktmo96)]
#[test]
fn submission_password_drift_fails() {
    let out = submission(&HEALTHY.replace("password=ok", "password=drifted"));
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("drifted")), "{out:?}");
}

#[test_id(ta8epvn)]
#[test]
fn submission_unconfigured_warns_only_when_tailscale_is_up() {
    assert_eq!(submission("configured=0\ntailnet=1\n")[0].level, Level::Warn);
    assert_eq!(submission("configured=0\ntailnet=0\n")[0].level, Level::Ok);
}

#[test_id(tj2dcxs)]
#[test]
fn dovecot_dropin_absent_where_dovecot_is_not_installed_says_nothing() {
    assert!(check_dovecot_dropin(&Fake::default()).is_empty());
}

#[test_id(tdxbx4a)]
#[test]
fn dovecot_dropin_missing_warns_and_present_is_ok() {
    let f = Fake::default();
    f.files.borrow_mut().push(Path::new(&format!("{}/dovecot.service", system_paths::USR_LIB_SYSTEMD_SYSTEM)).to_path_buf());
    let out = check_dovecot_dropin(&f);
    assert_eq!(levels(&out), vec![Level::Warn]);
    assert!(out[0].detail.as_deref().unwrap().contains("--system-user"));
    assert!(out[0].detail.as_deref().unwrap().contains("systemctl restart dovecot"));
    f.files.borrow_mut().push(Path::new(system_paths::DOVECOT_DROPIN).to_path_buf());
    assert_eq!(levels(&check_dovecot_dropin(&f)), vec![Level::Ok]);
}

#[test_id(tdqrrqb)]
#[test]
fn doctor_without_spira_toml_refuses_naming_it() {
    let e = require_spec(None).unwrap_err();
    assert!(e.contains("SPIRA_TOML is not set"), "{e}");
    assert!(require_spec(Some("")).unwrap_err().contains("SPIRA_TOML"));
    assert_eq!(require_spec(Some("c/one-source.toml")).unwrap(), "c/one-source.toml");
}

#[test_id(t3qhj7t)]
#[test]
fn a_host_cargo_config_that_sets_what_the_harness_declares_warns_naming_file_and_key() {
    let f = Fake::default();
    f.set("CARGO_HOME", "ch");
    f.set("SPIRA_RUN", "srun");
    f.contents.borrow_mut().insert(PathBuf::from("ch/config.toml"), "[build]\njobs = 6\n".into());
    f.contents.borrow_mut().insert(PathBuf::from("srun/.cargo/config"), "[env]\nRUST_TEST_THREADS = \"4\"\n".into());
    let out = check_host_cargo_config(&f);
    assert_eq!(levels(&out), vec![Level::Warn, Level::Warn]);
    assert!(out[0].msg.contains("ch/config.toml") && out[0].msg.contains("build.jobs"), "{}", out[0].msg);
    assert!(out[1].msg.contains("srun/.cargo/config") && out[1].msg.contains("env.RUST_TEST_THREADS"), "{}", out[1].msg);
}

#[test_id(tsg0yd1)]
#[test]
fn a_host_cargo_config_setting_only_incremental_profiles_or_tmpdir_is_ok() {
    let f = Fake::default();
    f.set("CARGO_HOME", "ch");
    f.contents.borrow_mut().insert(PathBuf::from("ch/config.toml"), "[build]\nincremental = false\n[env]\nTMPDIR = \"/t\"\n[profile.dev]\ncodegen-units = 4\n".into());
    assert_eq!(levels(&check_host_cargo_config(&f)), vec![Level::Ok]);
}

#[test_id(ti3t10p)]
#[test]
fn a_clean_or_absent_host_cargo_config_is_ok() {
    let f = Fake::default();
    f.set("CARGO_HOME", "ch");
    assert_eq!(levels(&check_host_cargo_config(&f)), vec![Level::Ok]);
    f.contents.borrow_mut().insert(PathBuf::from("ch/config.toml"), "[build]\nrustc-wrapper = \"sccache\"\n[env]\nSCCACHE_DIR = \"x\"\n".into());
    assert_eq!(levels(&check_host_cargo_config(&f)), vec![Level::Ok]);
}

#[test_id(t9e7anw)]
#[test]
fn doctor_run_reports_host_cargo_drift_found_under_a_run_ancestor() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "srun");
    f.contents.borrow_mut().insert(PathBuf::from("srun/.cargo/config.toml"), "[build]\njobs = 6\n".into());
    run(&f);
    let out = f.stdout.borrow().join("\n");
    assert!(out.contains("host cargo config") && out.contains("srun/.cargo/config.toml sets build.jobs"), "{out}");
}
