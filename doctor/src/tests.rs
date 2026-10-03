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
    pub bd_list: RefCell<Result<String, String>>,
    pub role_warnings: RefCell<Option<usize>>,
    pub dolt_metrics_off: RefCell<bool>,
    pub dolt_events: RefCell<usize>,
    pub store_meta: RefCell<BTreeMap<PathBuf, StoreMeta>>,
    pub active_units: RefCell<Vec<String>>,
    pub tcp_open: RefCell<bool>,
    pub bd_first_id: RefCell<Option<String>>,
    pub bump_ok: RefCell<bool>,
    pub counter: RefCell<String>,
    pub failed_units: RefCell<Result<Vec<String>, String>>,
    pub watchd: RefCell<Result<String, String>>,
    pub unit_execs: RefCell<Result<Vec<(String, String, String)>, String>>,
    pub enabled_units: RefCell<Result<Vec<String>, String>>,
    pub spira_unit: RefCell<String>,
    pub file_ages: RefCell<BTreeMap<PathBuf, u64>>,
    pub bd_version: RefCell<Option<String>>,
    pub arch: RefCell<String>,
    pub bin_purpose: RefCell<String>,
    pub stray: RefCell<Vec<String>>,
    pub stdout: RefCell<Vec<String>>,
    pub sccache_help: RefCell<Option<String>>,
    pub sccache_show_stats: RefCell<Option<String>>,
    pub sccache_dav_addr: RefCell<Option<String>>,
    pub daemon_bases: RefCell<Vec<String>>,
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
            bd_list: RefCell::new(Err("no fixture".into())),
            role_warnings: RefCell::new(Some(0)),
            dolt_metrics_off: RefCell::new(true),
            dolt_events: RefCell::new(0),
            store_meta: RefCell::new(BTreeMap::new()),
            active_units: RefCell::new(Vec::new()),
            tcp_open: RefCell::new(false),
            bd_first_id: RefCell::new(None),
            bump_ok: RefCell::new(false),
            counter: RefCell::new("0".into()),
            failed_units: RefCell::new(Ok(Vec::new())),
            watchd: RefCell::new(Ok(String::new())),
            unit_execs: RefCell::new(Ok(Vec::new())),
            enabled_units: RefCell::new(Ok(Vec::new())),
            spira_unit: RefCell::new(String::new()),
            file_ages: RefCell::new(BTreeMap::new()),
            bd_version: RefCell::new(None),
            arch: RefCell::new(String::new()),
            bin_purpose: RefCell::new(String::new()),
            stray: RefCell::new(Vec::new()),
            stdout: RefCell::new(Vec::new()),
            sccache_help: RefCell::new(None),
            sccache_show_stats: RefCell::new(None),
            sccache_dav_addr: RefCell::new(None),
            daemon_bases: RefCell::new(Vec::new()),
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
    fn bd_list(&self, _db: &Path, _timeout_secs: u64) -> Result<String, String> {
        self.bd_list.borrow().clone()
    }
    fn bd_role_warnings(&self, _db: &Path, _cwd: &Path) -> Option<usize> {
        *self.role_warnings.borrow()
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
    fn dolt_events_count(&self) -> usize {
        *self.dolt_events.borrow()
    }
    fn bd_first_id(&self, _db: &Path) -> Option<String> {
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
    fn spira_unit(&self, _kind: &str, _subkind: &str) -> String {
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
    fn out(&self, s: &str) {
        self.stdout.borrow_mut().push(s.to_string());
    }
}

fn levels(lines: &[Line]) -> Vec<Level> {
    lines.iter().map(|l| l.level).collect()
}

// ============================================================================ release tools

#[test]
fn release_tools_all_resolve() {
    let f = Fake::default();
    // world.sh is the `world` binary now (sp-6onps) and mail.sh is the `mail` binary now
    // (sp-ooh1k) -- both come through deps_list_release, not a hand-added suffix; gate
    // comes through deps_list_release too, per the existing fixture below.
    f.deps_release.borrow_mut().push("gate".into());
    f.deps_release.borrow_mut().push("world".into());
    f.deps_release.borrow_mut().push("mail".into());
    f.which.borrow_mut().insert("gate".into(), "/bin/gate".into());
    f.which.borrow_mut().insert("world".into(), "/bin/world".into());
    f.which.borrow_mut().insert("mail".into(), "/bin/mail".into());
    f.which.borrow_mut().insert("gate.sh".into(), "/bin/gate.sh".into());
    let out = check_release_tools(&f);
    assert_eq!(levels(&out), vec![Level::Ok]);
}

/// world.sh and mail.sh are NOT hand-added suffixes any more (sp-6onps, sp-ooh1k) -- a box
/// whose deps.toml release tier forgets to name `world` or `mail` must FAIL on it missing,
/// never silently pass because an old hand-written "*.sh" entry papered over it.
#[test]
fn release_tools_world_and_mail_binaries_come_through_deps_list_not_a_hand_added_suffix() {
    let f = Fake::default();
    f.which.borrow_mut().insert("gate.sh".into(), "/bin/gate.sh".into());
    // deps_release is empty -- "world" and "mail" are deliberately absent from it, and
    // nothing else in check_release_tools should supply either.
    let out = check_release_tools(&f);
    assert_eq!(levels(&out), vec![Level::Ok], "empty deps_release + gate.sh resolved must be clean");
    assert!(!out.iter().any(|l| l.msg.contains("world")), "{:?}", out);
    assert!(!out.iter().any(|l| l.msg.contains("mail")), "{:?}", out);
}

#[test]
fn release_tools_missing_fails() {
    let f = Fake::default();
    f.deps_release.borrow_mut().push("gate".into());
    let out = check_release_tools(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("gate"));
}

// ============================================================================ hotfix

#[test]
fn hotfix_none_standing() {
    let f = Fake::default();
    assert_eq!(levels(&check_hotfix(&f)), vec![Level::Ok]);
}

#[test]
fn hotfix_below_threshold_warns() {
    let f = Fake::default();
    *f.release_status.borrow_mut() = "RUNNING UNLANDED abc123: testing a hotfix\n".into();
    let out = check_hotfix(&f);
    assert_eq!(out[0].level, Level::Warn);
}

#[test]
fn hotfix_past_alert_threshold_fails() {
    let f = Fake::default();
    *f.release_status.borrow_mut() = "RUNNING UNLANDED abc123: testing\nALERT stood 5h, past 4h threshold\n".into();
    let out = check_hotfix(&f);
    assert_eq!(out[0].level, Level::Fail);
}

// ============================================================================ config files

#[test]
fn config_files_none_present() {
    let f = Fake::default();
    assert_eq!(levels(&check_config_files(&f)), vec![Level::Ok]);
}

#[test]
fn config_files_both_present_warns() {
    let f = Fake::default();
    f.set("SPIRA_CONF_FILE", "/etc/spira.conf");
    f.set("SPIRA_TOML_FILE", "/etc/spira-cfg.toml");
    f.which.borrow_mut().insert("spira-config".into(), "/bin/spira-config".into());
    *f.config_valid.borrow_mut() = Ok(());
    let out = check_config_files(&f);
    assert_eq!(out[0].level, Level::Warn);
    assert_eq!(out[1].level, Level::Ok);
}

#[test]
fn config_files_toml_fails_validation() {
    let f = Fake::default();
    f.set("SPIRA_TOML_FILE", "/etc/spira-cfg.toml");
    f.which.borrow_mut().insert("spira-config".into(), "/bin/spira-config".into());
    *f.config_valid.borrow_mut() = Err("bad key".into());
    let out = check_config_files(&f);
    assert_eq!(out[1].level, Level::Fail);
}

#[test]
fn config_files_migrates_before_validating_when_migrate_has_output() {
    // sp-oppza: `spira-config migrate <file>` runs immediately before `validate`, in that
    // order, and its output (only when non-empty) is logged as a plain, unranked line —
    // never folded into the validate verdict, and never gating on migrate's own exit code.
    let f = Fake::default();
    f.set("SPIRA_TOML_FILE", "/etc/spira-cfg.toml");
    f.which.borrow_mut().insert("spira-config".into(), "/bin/spira-config".into());
    *f.config_migrate.borrow_mut() = "spira-config migrate: /etc/spira-cfg.toml: set spira.id_prefix = \"sp\" from goal".into();
    *f.config_valid.borrow_mut() = Ok(());

    let out = check_config_files(&f);
    // out[0]: the TOML_NAME-only line (no spira.conf); out[1]: the migrate note; out[2]: validates.
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(out[1].level, Level::Raw);
    assert_eq!(out[1].msg, "spira-config migrate: /etc/spira-cfg.toml: set spira.id_prefix = \"sp\" from goal");
    assert_eq!(out[2].level, Level::Ok);
}

#[test]
fn config_files_migrate_silent_when_it_had_nothing_to_do() {
    // The common case (a box whose config already has id_prefix, or predates nothing):
    // migrate prints nothing, so no extra line appears at all — not even an empty one.
    let f = Fake::default();
    f.set("SPIRA_TOML_FILE", "/etc/spira-cfg.toml");
    f.which.borrow_mut().insert("spira-config".into(), "/bin/spira-config".into());
    *f.config_valid.borrow_mut() = Ok(());

    let out = check_config_files(&f);
    assert!(!out.iter().any(|l| l.level == Level::Raw), "{out:?}");
}

#[test]
fn config_files_migrate_output_ignores_its_own_exit_code() {
    // Even when validate then fails, the migrate note (if any) still precedes it and is
    // still just logged, never turned into a FAIL of its own — validate is the one gate.
    let f = Fake::default();
    f.set("SPIRA_TOML_FILE", "/etc/spira-cfg.toml");
    f.which.borrow_mut().insert("spira-config".into(), "/bin/spira-config".into());
    *f.config_migrate.borrow_mut() = "spira-config migrate: /etc/spira-cfg.toml: some note".into();
    *f.config_valid.borrow_mut() = Err("bad key".into());

    let out = check_config_files(&f);
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(out[1].level, Level::Raw);
    assert_eq!(out[2].level, Level::Fail);
}

#[test]
fn config_files_toml_but_no_spira_config_binary_fails() {
    let f = Fake::default();
    f.set("SPIRA_TOML_FILE", "/etc/spira-cfg.toml");
    let out = check_config_files(&f);
    assert_eq!(out[1].level, Level::Fail);
    assert!(out[1].msg.contains("spira-config is not on PATH"));
}

// ============================================================================ chamber overlays

#[test]
fn chamber_overlays_none_active() {
    let f = Fake::default();
    f.set("SPIRA_CHAMBER_OVERLAY", "/overlay");
    assert_eq!(levels(&check_chamber_overlays(&f)), vec![Level::Ok]);
}

#[test]
fn chamber_overlays_active_named() {
    let f = Fake::default();
    f.set("SPIRA_CHAMBER_OVERLAY", "/overlay");
    f.md_files.borrow_mut().insert(PathBuf::from("/overlay"), vec!["builder/note.md".into()]);
    let out = check_chamber_overlays(&f);
    assert_eq!(out[0].level, Level::Ok);
    assert!(out[0].msg.contains("builder/note.md"));
}

// ============================================================================ overrides

#[test]
fn overrides_no_problems() {
    let f = Fake::default();
    *f.overrides.borrow_mut() = Ok(String::new());
    assert_eq!(levels(&check_overrides(&f)), vec![Level::Ok]);
}

#[test]
fn overrides_problems_are_failures() {
    let f = Fake::default();
    *f.overrides.borrow_mut() = Err("spec-X: failed to apply\nspec-Y: stale-closed\n".into());
    let out = check_overrides(&f);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|l| l.level == Level::Fail));
}

// ============================================================================ gate compile check

#[test]
fn gate_compile_check_no_rust_repos() {
    let f = Fake::default();
    let out = check_gate_compile_check(&f);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].level, Level::Ok);
    assert!(out[0].msg.contains("no repository"));
}

#[test]
fn gate_compile_check_reaches_build_fence() {
    let f = Fake::default();
    f.repo_names.borrow_mut().push("spira".into());
    f.repo_fields.borrow_mut().insert(("spira".into(), "path".into()), "/repo".into());
    f.cargo_dirs.borrow_mut().push(PathBuf::from("/repo"));
    f.gate_defs.borrow_mut().insert("spira".into(), Ok("fence1.sh && bash spira/build-fence.sh && fence2.sh".into()));
    let out = check_gate_compile_check(&f);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].level, Level::Ok);
}

#[test]
fn gate_compile_check_missing_build_fence_fails() {
    let f = Fake::default();
    f.repo_names.borrow_mut().push("spira".into());
    f.repo_fields.borrow_mut().insert(("spira".into(), "path".into()), "/repo".into());
    f.cargo_dirs.borrow_mut().push(PathBuf::from("/repo"));
    f.gate_defs.borrow_mut().insert("spira".into(), Ok("fence1.sh && fence2.sh".into()));
    let out = check_gate_compile_check(&f);
    assert_eq!(out[0].level, Level::Fail);
}

#[test]
fn gate_compile_check_unresolved_gate_fails() {
    let f = Fake::default();
    f.repo_names.borrow_mut().push("spira".into());
    f.repo_fields.borrow_mut().insert(("spira".into(), "path".into()), "/repo".into());
    f.cargo_dirs.borrow_mut().push(PathBuf::from("/repo"));
    f.gate_defs.borrow_mut().insert("spira".into(), Err("no gate.steps".into()));
    let out = check_gate_compile_check(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].detail.as_deref().unwrap_or("").contains("<unresolved: no gate.steps>"));
}

// ============================================================================ store

#[test]
fn store_missing_beads_fails_outside_install() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    let out = check_store(&f);
    assert_eq!(out[0].level, Level::Fail);
}

#[test]
fn store_missing_beads_warns_during_install() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.set("SPIRA_DOCTOR_INSTALLING", "1");
    let out = check_store(&f);
    assert_eq!(out[0].level, Level::Warn);
}

#[test]
fn store_bd_read_ok_and_server_mode() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    *f.bd_list.borrow_mut() = Ok("[]".into());
    f.files.borrow_mut().push(PathBuf::from("/db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("/db/.beads/metadata.json"), StoreMeta { dolt_mode: Some("server".into()), ..Default::default() });
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.msg.contains("has a .beads")));
    assert!(out.iter().any(|l| l.msg == "bd can read it"));
    assert!(out.iter().any(|l| l.msg.contains("store mode: server")));
    assert!(out.iter().all(|l| l.level != Level::Fail));
}

#[test]
fn store_role_warning_is_named_with_its_fix() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.set("SPIRA_HOME", "/harness");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    *f.bd_list.borrow_mut() = Ok("[]".into());
    *f.role_warnings.borrow_mut() = Some(3);
    let out = check_store(&f);
    let l = out.iter().find(|l| l.level == Level::Warn && l.msg.contains("beads.role")).expect("warn line");
    assert!(l.detail.as_deref().unwrap().contains("git config --global beads.role maintainer"));
    *f.role_warnings.borrow_mut() = Some(0);
    assert!(check_store(&f).iter().any(|l| l.level == Level::Ok && l.msg.contains("no beads.role warning")));
}

#[test]
fn store_embedded_mode_fails() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    *f.bd_list.borrow_mut() = Ok("[]".into());
    f.files.borrow_mut().push(PathBuf::from("/db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("/db/.beads/metadata.json"), StoreMeta { dolt_mode: Some("embedded".into()), ..Default::default() });
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("embedded mode")));
}

#[test]
fn store_bd_cannot_read_softens_during_install_when_server_not_active() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.set("SPIRA_DOCTOR_INSTALLING", "1");
    f.set("SPIRA_DOLT_DATA", "/dolt");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    *f.bd_list.borrow_mut() = Err("connection refused".into());
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Warn && l.msg.contains("bd cannot read")));
}

#[test]
fn store_bd_cannot_read_fails_outside_install() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    *f.bd_list.borrow_mut() = Err("connection refused".into());
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("bd cannot read")));
}

#[test]
fn store_dolt_server_answering() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    *f.tcp_open.borrow_mut() = true;
    f.files.borrow_mut().push(PathBuf::from("/db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("/db/.beads/metadata.json"), StoreMeta { dolt_server_host: "127.0.0.1".into(), dolt_server_port: Some(3306), ..Default::default() });
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Ok && l.msg.contains("answering on 127.0.0.1:3306")));
}

#[test]
fn store_dolt_server_unreachable_fails() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    *f.tcp_open.borrow_mut() = false;
    f.files.borrow_mut().push(PathBuf::from("/db/.beads/metadata.json"));
    f.store_meta.borrow_mut().insert(PathBuf::from("/db/.beads/metadata.json"), StoreMeta { dolt_server_host: "127.0.0.1".into(), dolt_server_port: Some(3306), ..Default::default() });
    let out = check_store(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("no server answers")));
}

// ============================================================================ duckdb

#[test]
fn duckdb_present() {
    let f = Fake::default();
    f.which.borrow_mut().insert("duckdb".into(), "/usr/bin/duckdb".into());
    assert_eq!(levels(&check_duckdb(&f)), vec![Level::Ok]);
}

#[test]
fn duckdb_missing_fails() {
    let f = Fake::default();
    assert_eq!(levels(&check_duckdb(&f)), vec![Level::Fail]);
}

// ============================================================================ compilation cache

#[test]
fn sccache_missing_fails_and_names_the_install_command() {
    let f = Fake::default();
    let lines = check_sccache(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].detail.as_deref().unwrap_or("").contains("cargo install sccache --locked --no-default-features --features webdav"));
}

#[test]
fn sccache_missing_only_warns_off_an_operated_box() {
    // deps.toml's own waiver: a testenv fixture container sets SPIRA_BUILD_CACHE=off and
    // carries no sccache at all, on purpose — SPIRA_OPERATED=0 is how check_operator_channel
    // already tells a fixture from a real box, and this check uses the same gate.
    let f = Fake::default();
    f.set("SPIRA_OPERATED", "0");
    assert_eq!(levels(&check_sccache(&f)), vec![Level::Warn]);
}

#[test]
fn sccache_present_but_built_without_webdav_fails_rather_than_passing_on_presence_alone() {
    let f = Fake::default();
    f.which.borrow_mut().insert("sccache".into(), "/opt/spira/cargo/bin/sccache".into());
    *f.sccache_help.borrow_mut() = Some(
        "Usage: sccache ...\n\nEnabled features:\n    S3:        false\n    WebDAV:    false\n    OSS:       false\n".into(),
    );
    let lines = check_sccache(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("without the webdav backend"), "{:?}", lines[0].msg);
    assert!(lines[0].detail.as_deref().unwrap_or("").contains("--features webdav"));
}

#[test]
fn sccache_with_webdav_enabled_passes() {
    let f = Fake::default();
    f.which.borrow_mut().insert("sccache".into(), "/opt/spira/cargo/bin/sccache".into());
    *f.sccache_help.borrow_mut() = Some(
        "Usage: sccache ...\n\nEnabled features:\n    S3:        false\n    WebDAV:    true\n    OSS:       false\n".into(),
    );
    assert_eq!(levels(&check_sccache(&f)), vec![Level::Ok]);
}

#[test]
fn sccache_present_but_unresponsive_to_help_fails() {
    let f = Fake::default();
    f.which.borrow_mut().insert("sccache".into(), "/opt/spira/cargo/bin/sccache".into());
    // sccache_help left at its default None: the binary could not be run (permissions,
    // a wrapper script that shadows the real one, etc).
    let lines = check_sccache(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("did not answer --help"), "{:?}", lines[0].msg);
}

// -------------------------------------------------------- compilation cache backend (sp-xtdqi)

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

#[test]
fn backend_check_only_warns_off_an_operated_box() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    f.set("SPIRA_OPERATED", "0");
    *f.sccache_show_stats.borrow_mut() = Some("Cache location                  Local disk: \"/x\"\n".into());
    assert_eq!(levels(&check_sccache_backend(&f)), vec![Level::Warn]);
}

#[test]
fn backend_check_passes_when_the_live_server_is_on_the_store() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    *f.sccache_show_stats.borrow_mut() = Some("Cache location                  webdav, name: , prefix: /\nVersion (client)                0.18.0\n".into());
    assert_eq!(levels(&check_sccache_backend(&f)), vec![Level::Ok]);
}

#[test]
fn backend_check_fails_when_show_stats_does_not_answer() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    // sccache_show_stats left at its default None.
    let lines = check_sccache_backend(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("did not answer"), "{:?}", lines[0].msg);
}

#[test]
fn backend_check_fails_when_show_stats_answers_with_no_cache_location_line() {
    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = Some("192.168.1.56:9431".into());
    *f.sccache_show_stats.borrow_mut() = Some("some unexpected garbage\n".into());
    let lines = check_sccache_backend(&f);
    assert_eq!(levels(&lines), vec![Level::Fail]);
    assert!(lines[0].msg.contains("did not report a Cache location"), "{:?}", lines[0].msg);
}

/// THE FULL PIPELINE (sp-xtdqi-3): an address resolved from a fixture config document
/// (the host config document's own `[spira]` table) — never the environment, which is deliberately
/// empty here — feeds `check_sccache_backend`, which FAILS on a fake "Local disk" location.
/// This is the exact production scenario (configured, unexported, server on the wrong
/// backend) that used to report "no shared store configured" blind, because `World::env`
/// (`conf.sh`'s own bash capture) never carries a NO-DEFAULT key like this one even when
/// the host config document genuinely configures it.
#[test]
fn backend_check_fails_on_an_address_resolved_from_a_fixture_config_with_nothing_exported() {
    let d = testkit::TempDir::new("doctor-backend-fixture-config");
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("conf.d")).unwrap();
    std::fs::write(
        home.join("conf.d/SPIRA_SCCACHE_DAV_ADDR"),
        "TYPE=string\nGROUP=sccache\nDOC=test fixture\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    # NO DEFAULT.\nSPIRA_CONF_DEFAULT_EOF\n",
    )
    .unwrap();
    let toml = spira_config::validate("[spira]\nid_prefix = \"sp\"\nsccache_dav_addr = \"192.168.1.56:9431\"\n").unwrap();
    let env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new(); // deliberately unexported
    let addr = crate::real::resolve_sccache_dav_addr(&home, &home, &env, Some(&toml));
    assert_eq!(addr.as_deref(), Some("192.168.1.56:9431"), "sanity: must resolve before feeding the check");

    let f = Fake::default();
    *f.sccache_dav_addr.borrow_mut() = addr;
    *f.sccache_show_stats.borrow_mut() = Some("Cache location                  Local disk: \"/var/cache/sccache\"\n".into());
    let lines = check_sccache_backend(&f);
    assert_eq!(levels(&lines), vec![Level::Fail], "{lines:?}");
    assert!(lines[0].msg.contains("NOT on the shared store"), "{:?}", lines[0].msg);
}

// ============================================================================ events probe

#[test]
fn events_probe_no_database_warns() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    assert_eq!(levels(&check_events_probe(&f)), vec![Level::Warn]);
}

#[test]
fn events_probe_no_anchor_bead_warns() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    let out = check_events_probe(&f);
    assert_eq!(out[0].level, Level::Warn);
}

#[test]
fn events_probe_round_trip_ok() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    *f.bd_first_id.borrow_mut() = Some("sp-123".into());
    *f.bump_ok.borrow_mut() = true;
    *f.counter.borrow_mut() = "3".into();
    assert_eq!(levels(&check_events_probe(&f)), vec![Level::Ok]);
}

#[test]
fn events_probe_write_refused_fails() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    *f.bd_first_id.borrow_mut() = Some("sp-123".into());
    *f.bump_ok.borrow_mut() = false;
    *f.counter.borrow_mut() = "0".into();
    let out = check_events_probe(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("was refused"));
}

#[test]
fn events_probe_write_discarded_fails() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db");
    f.dirs.borrow_mut().push(PathBuf::from("/db/.beads"));
    *f.bd_first_id.borrow_mut() = Some("sp-123".into());
    *f.bump_ok.borrow_mut() = true;
    *f.counter.borrow_mut() = "?".into();
    let out = check_events_probe(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].msg.contains("are discarded"));
}

// ============================================================================ failed units

#[test]
fn failed_units_none() {
    let f = Fake::default();
    *f.failed_units.borrow_mut() = Ok(vec![]);
    assert_eq!(levels(&check_failed_units(&f)), vec![Level::Ok]);
}

#[test]
fn failed_units_outside_install_fails() {
    let f = Fake::default();
    *f.failed_units.borrow_mut() = Ok(vec!["spira-gate-prod.service".into()]);
    let out = check_failed_units(&f);
    assert_eq!(out[0].level, Level::Fail);
}

#[test]
fn failed_units_during_install_warns() {
    let f = Fake::default();
    f.set("SPIRA_DOCTOR_INSTALLING", "1");
    *f.failed_units.borrow_mut() = Ok(vec!["spira-summon-prod.service".into()]);
    let out = check_failed_units(&f);
    assert_eq!(out[0].level, Level::Warn);
    assert!(out[0].msg.contains("predates this install"));
}

#[test]
fn failed_units_query_fault_fails() {
    let f = Fake::default();
    *f.failed_units.borrow_mut() = Err("Failed to connect to bus".into());
    let out = check_failed_units(&f);
    assert_eq!(out[0].level, Level::Fail);
}

// ============================================================================ orphan units

#[test]
fn orphan_units_none() {
    let f = Fake::default();
    *f.watchd.borrow_mut() = Ok(String::new());
    *f.enabled_units.borrow_mut() = Ok(vec![]);
    assert_eq!(levels(&check_orphan_units(&f)), vec![Level::Ok]);
}

#[test]
fn orphan_units_known_watcher_is_fine() {
    let f = Fake::default();
    f.set("SPIRA_INSTANCE", "prod");
    *f.watchd.borrow_mut() = Ok("queue-watch|daemon|\n".into());
    *f.enabled_units.borrow_mut() = Ok(vec!["spira-watch-queue-watch-prod.service".into()]);
    assert_eq!(levels(&check_orphan_units(&f)), vec![Level::Ok]);
}

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

#[test]
fn snapshot_missing_warns() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "/run");
    assert_eq!(levels(&check_snapshot_fresh(&f)), vec![Level::Warn]);
}

#[test]
fn snapshot_fresh_ok() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "/run");
    f.file_ages.borrow_mut().insert(PathBuf::from("/run/cockpit.env"), 5);
    assert_eq!(levels(&check_snapshot_fresh(&f)), vec![Level::Ok]);
}

#[test]
fn snapshot_stale_fails() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "/run");
    f.set("SPIRA_SNAP_STALE_S", "60");
    f.file_ages.borrow_mut().insert(PathBuf::from("/run/cockpit.env"), 120);
    let out = check_snapshot_fresh(&f);
    assert_eq!(out[0].level, Level::Fail);
}

#[test]
fn snapshot_threshold_key_decides_between_fresh_and_stale() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "/run");
    f.set("SPIRA_SNAP_STALE_S", "7");
    f.file_ages.borrow_mut().insert(PathBuf::from("/run/cockpit.env"), 20);
    assert_eq!(levels(&check_snapshot_fresh(&f)), vec![Level::Fail]);
    f.file_ages.borrow_mut().insert(PathBuf::from("/run/cockpit.env"), 0);
    assert_eq!(levels(&check_snapshot_fresh(&f)), vec![Level::Ok]);
}

// ============================================================================ operator channel

#[test]
fn operator_channel_everything_present() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "/bin/inotifywait".into());
    f.which.borrow_mut().insert("go".into(), "/usr/bin/go".into());
    f.exec_files.borrow_mut().push("/usr/bin/go".into());
    let out = check_operator_channel(&f);
    assert!(out.iter().all(|l| l.level == Level::Ok));
}

#[test]
fn operator_channel_reports_mail_mute_only_when_on() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "/bin/inotifywait".into());
    f.which.borrow_mut().insert("go".into(), "/usr/bin/go".into());
    f.exec_files.borrow_mut().push("/usr/bin/go".into());
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

#[test]
fn operator_channel_missing_inotify_fails_when_operated() {
    let f = Fake::default();
    f.exec_files.borrow_mut().push("/usr/bin/go".into());
    f.which.borrow_mut().insert("go".into(), "/usr/bin/go".into());
    let out = check_operator_channel(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("inotifywait")));
}

#[test]
fn operator_channel_missing_inotify_warns_when_not_operated() {
    let f = Fake::default();
    f.set("SPIRA_OPERATED", "0");
    f.exec_files.borrow_mut().push("/usr/bin/go".into());
    f.which.borrow_mut().insert("go".into(), "/usr/bin/go".into());
    let out = check_operator_channel(&f);
    assert!(out.iter().any(|l| l.level == Level::Warn && l.msg.contains("inotifywait")));
}

#[test]
fn operator_channel_no_go_but_bd_current_and_prebuilt_is_ok() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "/bin/inotifywait".into());
    f.set("SPIRA_BD_TAG", "v1.2.3");
    *f.bd_version.borrow_mut() = Some("1.2.3".into());
    *f.arch.borrow_mut() = "x86_64".into();
    let out = check_operator_channel(&f);
    assert!(out.iter().all(|l| l.level == Level::Ok));
}

#[test]
fn operator_channel_no_go_mismatched_bd_no_prebuilt_fails() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "/bin/inotifywait".into());
    f.set("SPIRA_BD_TAG", "v1.2.3");
    *f.bd_version.borrow_mut() = Some("1.0.0".into());
    *f.arch.borrow_mut() = "riscv64".into();
    let out = check_operator_channel(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("go is not on PATH")));
}

#[test]
fn operator_channel_hunk_checked_only_when_configured() {
    let f = Fake::default();
    f.which.borrow_mut().insert("inotifywait".into(), "/bin/inotifywait".into());
    f.which.borrow_mut().insert("go".into(), "/usr/bin/go".into());
    f.exec_files.borrow_mut().push("/usr/bin/go".into());
    f.set("COCKPIT_SESSIONS", "chat hunk");
    let out = check_operator_channel(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("hunk")));
}

// ============================================================================ concierge singleton

#[test]
fn concierge_singleton_none_found() {
    let f = Fake::default();
    f.set("SPIRA_REPO", "/repo");
    f.exec_files.borrow_mut().push("/repo/concierge.sh".into());
    assert_eq!(levels(&check_concierge_singleton(&f)), vec![Level::Ok]);
}

#[test]
fn concierge_singleton_stray_found_fails() {
    let f = Fake::default();
    f.set("SPIRA_REPO", "/repo");
    f.exec_files.borrow_mut().push("/repo/concierge.sh".into());
    f.stray.borrow_mut().push("12345".into());
    let out = check_concierge_singleton(&f);
    assert_eq!(out[0].level, Level::Fail);
    assert!(out[0].detail.as_deref().unwrap_or("").contains("12345"));
}

#[test]
fn concierge_singleton_resolves_from_release_when_repo_unset() {
    let f = Fake::default();
    f.set("SPIRA_HOME", "/rel/spira");
    f.exec_files.borrow_mut().push("/rel/spira/../concierge.sh".into());
    assert_eq!(levels(&check_concierge_singleton(&f)), vec![Level::Ok]);
}

#[test]
fn concierge_singleton_no_concierge_sh_warns() {
    let f = Fake::default();
    f.set("SPIRA_REPO", "/repo");
    let out = check_concierge_singleton(&f);
    assert_eq!(out[0].level, Level::Warn);
}

// ============================================================================ run() overall tally

#[test]
fn run_exit_code_reflects_fatal_count() {
    let f = Fake::default();
    f.set("SPIRA_DB", "/db"); // store -> FAIL (no .beads, not installing)
    let rc = run(&f);
    assert_eq!(rc, 1);
    assert!(f.stdout.borrow().iter().any(|l| l.contains("fatal") && l.contains("not healthy")));
}

// ============================================================================ prod checkout

fn exec_row(unit: &str, state: &str, path: &str) -> (String, String, String) {
    (unit.into(), state.into(), path.into())
}

#[test]
fn prod_checkout_unit_outside_releases_fails() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "/fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![exec_row("spira-sentinel-prod.service", "enabled", "/fixture/checkout/spira/sentinel.sh")]);
    let out = check_prod_checkout(&f);
    assert_eq!(levels(&out), vec![Level::Fail]);
    assert!(out[0].msg.contains("spira-sentinel-prod.service"));
    assert!(out[0].detail.as_deref().unwrap().contains("/fixture/checkout/spira/sentinel.sh"));
}

#[test]
fn prod_checkout_all_under_releases_is_ok() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "/fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![exec_row("beads-push.service", "enabled", "/fixture/releases/current/beads-push.sh")]);
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Ok]);
}

#[test]
fn prod_checkout_sibling_prefix_is_outside() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "/fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![exec_row("beads-push.service", "enabled", "/fixture/releases-old/x")]);
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Fail]);
}

#[test]
fn prod_checkout_each_offender_gets_a_line() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "/fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![
        exec_row("a.service", "enabled", "/co/a"),
        exec_row("b.service", "enabled", "/fixture/releases/b"),
        exec_row("c.service", "static", "/co/c"),
    ]);
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Fail, Level::Fail]);
}

#[test]
fn prod_checkout_transient_and_aeon_units_are_not_flagged() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "/fixture/releases");
    *f.unit_execs.borrow_mut() = Ok(vec![
        exec_row("spira-job.service", "transient", "/co/job"),
        exec_row("spira-aeon-x.service", "static", "/co/aeon"),
        exec_row("spira-nopath.service", "enabled", ""),
    ]);
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Ok]);
}

#[test]
fn prod_checkout_probe_failure_fails() {
    let f = Fake::default();
    f.set("SPIRA_RELEASES", "/fixture/releases");
    *f.unit_execs.borrow_mut() = Err("Failed to connect to bus".into());
    assert_eq!(levels(&check_prod_checkout(&f)), vec![Level::Fail]);
}

#[test]
fn mirror_check_warns_on_a_daemon_serving_another_state_dir() {
    let f = Fake::default();
    f.set("SPIRA_ROUND_VM_STATE_DIR", "/srv/round-vm");
    *f.daemon_bases.borrow_mut() = vec!["/old/place".into()];
    let lines = check_round_vm_mirror(&f);
    assert_eq!(levels(&lines), vec![Level::Warn]);
    assert!(lines[0].msg.contains("/old/place") && lines[0].msg.contains("/srv/round-vm"), "{:?}", lines[0].msg);
}

#[test]
fn mirror_check_passes_on_the_configured_dir_and_when_no_daemon_runs() {
    let f = Fake::default();
    f.set("SPIRA_ROUND_VM_STATE_DIR", "/srv/round-vm");
    assert_eq!(levels(&check_round_vm_mirror(&f)), vec![Level::Ok]);
    *f.daemon_bases.borrow_mut() = vec!["/srv/round-vm/".into()];
    assert_eq!(levels(&check_round_vm_mirror(&f)), vec![Level::Ok]);
}

#[test]
fn mirror_check_defaults_the_state_dir_to_run_round_vm() {
    let f = Fake::default();
    f.set("SPIRA_RUN", "/r");
    *f.daemon_bases.borrow_mut() = vec!["/r/round-vm".into()];
    assert_eq!(levels(&check_round_vm_mirror(&f)), vec![Level::Ok]);
    *f.daemon_bases.borrow_mut() = vec!["/elsewhere".into()];
    assert_eq!(levels(&check_round_vm_mirror(&f)), vec![Level::Warn]);
}

#[test]
fn daemon_base_path_reads_only_a_git_daemon_on_the_port() {
    let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let argv = v(&["git", "daemon", "--port=9430", "--base-path=/x", "--export-all"]);
    assert_eq!(crate::real::daemon_base_path(&argv, 9430), Some("/x".into()));
    assert_eq!(crate::real::daemon_base_path(&argv, 9431), None);
    assert_eq!(crate::real::daemon_base_path(&v(&["vim", "daemon", "--port=9430", "--base-path=/x"]), 9430), None);
}


#[test]
fn dolt_telemetry_check_passes_when_disabled_and_the_backlog_is_small() {
    let f = Fake::default();
    let out = check_dolt_telemetry(&f);
    assert!(out.iter().all(|l| l.level == Level::Ok), "{out:?}");
}

#[test]
fn dolt_telemetry_check_fails_when_metrics_are_not_disabled() {
    let f = Fake::default();
    *f.dolt_metrics_off.borrow_mut() = false;
    let out = check_dolt_telemetry(&f);
    assert!(out.iter().any(|l| l.level == Level::Fail && l.msg.contains("metrics.disabled")), "{out:?}");
}

#[test]
fn dolt_telemetry_check_fails_on_an_events_backlog_over_the_bound() {
    let f = Fake::default();
    *f.dolt_events.borrow_mut() = 101;
    assert!(check_dolt_telemetry(&f).iter().any(|l| l.level == Level::Fail && l.msg.contains("eventsData")));
    *f.dolt_events.borrow_mut() = 100;
    assert!(check_dolt_telemetry(&f).iter().all(|l| l.level == Level::Ok));
}
