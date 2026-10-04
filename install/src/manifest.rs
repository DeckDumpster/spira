//! The unit manifest: which templates this box runs and which of their installed names get
//! enabled (systemd/units.sh, in Rust). Pure data + naming, so it is unit-testable without a
//! filesystem or `watchd.sh` — callers resolve [`Inputs::watch_names`] and the two presence
//! flags themselves and hand them in.

/// One template this box installs. `enable` is the template-level default; a template that
/// should never be auto-enabled (the `.service` half of a timer pair) carries `enable: false`
/// and the pairing timer carries `enable: true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    pub enable: bool,
}

fn t(name: &str, enable: bool) -> Template {
    Template { name: name.into(), enable }
}

/// What the box tells the manifest builder about itself — everything units.sh either reads
/// from the environment via conf.sh, or discovers by probing.
#[derive(Debug, Clone)]
pub struct Inputs {
    pub instance: String,
    /// `SPIRA_DOLT_DATA` is non-empty.
    pub dolt_data_set: bool,
    /// `SPIRA_TESTDB_DATA` is non-empty.
    pub testdb_data_set: bool,
    /// `SPIRA_BROKER_ENABLE=1`.
    pub broker_enable: bool,
    /// `inotifywait` is on `PATH`.
    pub inotify_present: bool,
    /// `SPIRA_SCCACHE_DAV_ADDR` is non-empty (sp-xtdqi): this box names its own LAN address
    /// for the shared compilation cache.
    pub sccache_dav_addr_set: bool,
    /// The root installer's `--system-user` phase has installed the SYSTEM spira-lc unit
    /// (`spira_config::resolve::lc_system_mode`). Off is same-user mode, the default: this
    /// manifest then installs the operator's own `lc-serve.service` (sp-xfqnr).
    pub lc_system_mode: bool,
    /// Plain watcher names from the manifest (`watchd.sh units`, `spira-watch@<name>.service`
    /// with the wrapper stripped) — `Err` when the manifest itself is malformed, matching
    /// units.sh's `return 1` when `watchd.sh units` fails.
    pub watch_names: Result<Vec<String>, String>,
}

impl Default for Inputs {
    fn default() -> Self {
        Inputs { instance: String::new(), dolt_data_set: false, testdb_data_set: false, broker_enable: false, inotify_present: false, sccache_dav_addr_set: false, lc_system_mode: false, watch_names: Ok(Vec::new()) }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    /// Every template this box installs (excluding the watcher template itself; watcher
    /// instances are named individually below).
    pub units: Vec<Template>,
    /// Templates this box deliberately declined (an absent unit that IS listed here is a
    /// choice; one that is in neither `units` nor `optional` is a forgotten row).
    pub optional: Vec<String>,
    /// The subset of `optional` that is absent only because something buildable is missing
    /// right now (e.g. `inotifywait`) — an uninstall must still remove these if they were
    /// installed by an earlier run that had the dependency.
    pub unbuilt: Vec<String>,
    /// Plain watcher names (e.g. "testview"), one per manifest row.
    pub watch_names: Vec<String>,
    /// Notes worth printing (units.sh's informational `echo ... >&2` lines) — never fatal.
    pub notes: Vec<String>,
}

/// Build the manifest for this box (units.sh's top-level body, in Rust). `Err` exactly when
/// `inputs.watch_names` is `Err` — a malformed watcher manifest stops the install before a
/// single unit is rendered, matching units.sh's `return 1`.
pub fn build(inputs: &Inputs) -> Result<Manifest, String> {
    let mut m = Manifest::default();

    m.units.push(t("spira-sentinel.service", false));
    m.units.push(t("spira-sentinel.timer", true));
    m.units.push(t("spira-summon.service", false));
    m.units.push(t("spira-summon.timer", true));
    m.units.push(t("spira-ops.service", false));
    m.units.push(t("spira-ops.timer", true));
    m.units.push(t("spira-auron.service", false));
    m.units.push(t("spira-auron.timer", true));
    m.units.push(t("spira-watchtower.service", false));
    m.units.push(t("spira-watchtower.timer", true));
    m.units.push(t("spira-skew.service", false));
    m.units.push(t("spira-skew.timer", true));
    m.units.push(t("spira-cert-sweep-full.service", false));
    m.units.push(t("spira-cert-sweep-full.timer", true));
    m.units.push(t("spira-cert-sweep-sample.service", false));
    m.units.push(t("spira-cert-sweep-sample.timer", true));
    m.units.push(t("spira-archivist.service", false));
    m.units.push(t("spira-archivist.timer", true));
    m.units.push(t("spira-czar-pass.service", false));
    m.units.push(t("spira-czar-pass.timer", true));
    m.units.push(t("spira-reconciler.service", false));
    m.units.push(t("spira-reconciler.timer", true));
    m.units.push(t("spira-cockpit.service", true));
    // spira-watch@.service is the template, not an installed unit — never enabled directly.
    m.units.push(t("spira-watch@.service", false));
    m.units.push(t("spira-notify.service", false));
    m.units.push(t("spira-notify.timer", true));
    m.units.push(t("spira-refresh.service", false));
    m.units.push(t("spira-refresh.timer", true));
    m.units.push(t("cockpit-ensure.service", false));
    m.units.push(t("cockpit-ensure.timer", true));
    m.units.push(t("concierge.service", false));
    m.units.push(t("concierge.timer", true));
    m.units.push(t("beads-push.service", false));
    m.units.push(t("beads-push.timer", true));
    m.units.push(t("spira-archive.service", false));
    m.units.push(t("spira-archive.timer", true));
    m.units.push(t("spira-groom.service", false));
    m.units.push(t("spira-groom.timer", true));
    m.units.push(t("spira-maechen.service", false));
    m.units.push(t("spira-maechen.timer", true));
    m.units.push(t("spira-warden.service", false));
    m.units.push(t("spira-warden.timer", true));
    m.units.push(t("spira-moot-sweep.service", false));
    m.units.push(t("spira-moot-sweep.timer", true));
    m.units.push(t("spira-verify-asks.service", false));
    m.units.push(t("spira-verify-asks.timer", true));
    m.units.push(t("spira-gate-check.service", false));
    m.units.push(t("spira-gate-check.timer", true));
    m.units.push(t("spira-mail-tidy.service", false));
    m.units.push(t("spira-mail-tidy.timer", true));
    m.units.push(t("spira-gh-intake.service", false));
    m.units.push(t("spira-gh-intake.timer", true));
    m.units.push(t("spira-verdict.service", false));
    m.units.push(t("spira-verdict.timer", true));
    m.units.push(t("spira-publish.service", false));
    m.units.push(t("spira-publish.timer", true));
    m.units.push(t("spira-straggler-sweep.service", false));
    m.units.push(t("spira-straggler-sweep.timer", true));
    m.units.push(t("spira-sop-lint.service", false));
    m.units.push(t("spira-sop-lint.timer", true));
    m.units.push(t("spira-escape-census.service", false));
    m.units.push(t("spira-escape-census.timer", true));

    // promote.sh / spira-promote.*: retired by deploy.sh's split-checkout replacement.
    m.optional.push("spira-promote.service".into());
    m.optional.push("spira-promote.timer".into());
    // spira-lc.service: a SYSTEM unit installed by the root installer's --system-user phase,
    // never by the per-instance flow this manifest drives.
    m.optional.push("spira-lc.service".into());

    // lc-serve.service (sp-xfqnr): the SAME-USER answer to the socket an aeon's `work`
    // reaches spira-lc through — `spira-lc serve` as the operator on %t/spira-lc/sock, the
    // path spira_config::resolve::lc_socket_default gives every caller in this mode. In
    // system mode spira-lc.socket already answers /run/spira-lc/sock as its own Unix user,
    // and a second, operator-run copy would hand the store to the credential the privilege
    // split exists to keep away from the operator, so it is declined there.
    if inputs.lc_system_mode {
        m.optional.push("lc-serve.service".into());
        m.notes.push("spira-lc runs as a system service (--system-user) — not installing lc-serve.service.".into());
    } else {
        m.units.push(t("lc-serve.service", true));
    }

    if inputs.inotify_present {
        m.units.push(t("spira-mail-deliver.service", true));
    } else {
        m.optional.push("spira-mail-deliver.service".into());
        m.unbuilt.push("spira-mail-deliver.service".into());
        m.notes.push("inotifywait not found — not installing spira-mail-deliver.service.".into());
        m.notes.push("Install inotify-tools and re-run install to enable mail delivery.".into());
    }

    if inputs.dolt_data_set {
        m.units.push(t("dolt-beads.service", true));
        m.units.push(t("dolt-tmp-prune.service", false));
        m.units.push(t("dolt-tmp-prune.timer", true));
    } else {
        m.optional.push("dolt-beads.service".into());
        m.optional.push("dolt-tmp-prune.service".into());
        m.optional.push("dolt-tmp-prune.timer".into());
        m.notes.push("SPIRA_DOLT_DATA is empty — not installing dolt-beads.service.".into());
        m.notes.push("Start your Dolt server yourself, or set it in spira.conf.".into());
    }

    if inputs.testdb_data_set {
        // Installed, not enabled: testdb.sh starts it on demand.
        m.units.push(t("dolt-beads-test.service", false));
    } else {
        m.optional.push("dolt-beads-test.service".into());
    }

    m.units.push(t("spira-loom.service", true));

    // sccache-dav.service: a template whose Environment= lines bind one box's own LAN
    // address — operator inventory, never a literal in the public harness
    // (law-harness-ships-mechanism-not-inventory). `SPIRA_SCCACHE_DAV_ADDR` carries that
    // address as a conf.d key (sp-xtdqi, reversing sp-xjnzl's original "installed by hand"
    // call): set, this renders and installs the unit like any other; unset, it is declined
    // here like spira-lc.service — never installed on a fresh box, and never inside a test
    // fixture's isolated network namespace, which has no address to set.
    if inputs.sccache_dav_addr_set {
        m.units.push(t("sccache-dav.service", true));
    } else {
        m.optional.push("sccache-dav.service".into());
        m.notes.push("SPIRA_SCCACHE_DAV_ADDR is empty — not installing sccache-dav.service.".into());
        m.notes.push("Set it in spira.conf (this box's own LAN address, e.g. 192.168.1.56:9431) and re-run install.".into());
    }

    m.units.push(t("spira-broker.service", false));
    m.units.push(t("spira-broker.timer", inputs.broker_enable));

    m.units.push(t("spira-landing-pass.service", false));
    m.units.push(t("spira-landing-pass.timer", true));

    m.units.push(t("spira-gate-worker.service", false));
    m.units.push(t("spira-gate-worker.timer", true));

    m.units.push(t("spira-reconciler-flow.service", false));
    m.units.push(t("spira-reconciler-flow.timer", true));

    m.watch_names = inputs.watch_names.clone()?;

    Ok(m)
}

impl Manifest {
    /// The installed unit names that should be enabled, per-instance, in manifest order
    /// followed by watchers in manifest order — matching units.sh's `ENABLE` array build
    /// order (templates first, watchers appended by the loop after `watchd.sh units`).
    pub fn enable(&self, instance: &str) -> Vec<String> {
        let mut out: Vec<String> = self.units.iter().filter(|u| u.enable).map(|u| inst_name(&u.name, instance)).collect();
        out.extend(self.watch_names.iter().map(|w| inst_watch_name(w, instance)));
        out
    }

    /// Every template name this box installs, excluding the watcher template itself (callers
    /// render it once per watcher name instead).
    pub fn template_names(&self) -> Vec<&str> {
        self.units.iter().map(|u| u.name.as_str()).filter(|n| *n != "spira-watch@.service").collect()
    }

    /// A unit file name present on disk that this manifest neither installs nor declined —
    /// units.sh's `unlisted`/`--diff`'s `UNLISTED` check, generalised over an arbitrary set of
    /// on-disk `.service`/`.timer` basenames in the templates directory.
    ///
    /// Checks membership in `self.units` directly, not `template_names()`: the original
    /// bash's `UNITS` array carries the literal `spira-watch@.service` entry (excluded only
    /// from the per-instance render loop, via its own `[ "$u" = "spira-watch@.service" ] &&
    /// continue`), so that entry's presence in `UNITS` is what keeps `unlisted()` from ever
    /// flagging it — `template_names()` filters it out for a different, render-only reason
    /// and would wrongly make it look forgotten here.
    pub fn unlisted<'a>(&self, on_disk: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let known: std::collections::BTreeSet<&str> = self.units.iter().map(|u| u.name.as_str()).chain(self.optional.iter().map(|s| s.as_str())).collect();
        on_disk.into_iter().filter(|n| !known.contains(n)).map(str::to_string).collect()
    }
}

/// `inst_name` (units.sh): a `spira-*.service`/`.timer` template gets the instance suffix
/// appended before its extension; everything else (shared units, and the watcher template
/// itself, which is never installed directly) keeps its plain name.
pub fn inst_name(template: &str, instance: &str) -> String {
    if template == "spira-watch@.service" {
        return template.to_string();
    }
    if let Some(base) = template.strip_suffix(".service") {
        if base.starts_with("spira-") {
            return format!("{base}-{instance}.service");
        }
    }
    if let Some(base) = template.strip_suffix(".timer") {
        if base.starts_with("spira-") {
            return format!("{base}-{instance}.timer");
        }
    }
    template.to_string()
}

/// `inst_watch_name` (units.sh): `spira-watch@<name>.service`'s per-instance installed name.
pub fn inst_watch_name(name: &str, instance: &str) -> String {
    format!("spira-watch-{name}-{instance}.service")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Inputs {
        Inputs {
            instance: "prod".into(),
            dolt_data_set: true,
            testdb_data_set: false,
            broker_enable: false,
            inotify_present: true,
            sccache_dav_addr_set: false,
            lc_system_mode: false,
            watch_names: Ok(vec!["testview".into(), "notify".into()]),
        }
    }

    /// THE POSITIVE CONTROL (sp-xtdqi): `sccache-dav.service` was unconditionally declined
    /// before this bead — reversing that is the whole point of part (a), so this must fail
    /// against the old manifest.rs and pass only once the `if inputs.sccache_dav_addr_set`
    /// branch exists.
    #[test]
    fn sccache_dav_is_installed_and_enabled_only_when_its_address_is_set() {
        let m = build(&inputs()).unwrap();
        assert!(!m.units.iter().any(|u| u.name == "sccache-dav.service"), "no address: not installed");
        assert!(m.optional.contains(&"sccache-dav.service".to_string()));
        assert!(m.notes.iter().any(|n| n.contains("SPIRA_SCCACHE_DAV_ADDR")), "{:?}", m.notes);

        let mut i = inputs();
        i.sccache_dav_addr_set = true;
        let m2 = build(&i).unwrap();
        let u = m2.units.iter().find(|u| u.name == "sccache-dav.service").expect("installed once the address is set");
        assert!(u.enable, "a plain long-running service, enabled like spira-loom.service");
        assert!(!m2.optional.contains(&"sccache-dav.service".to_string()));
    }

    /// sp-xfqnr: same-user mode (no --system-user) installs and enables the operator's own
    /// serve unit, shared across instances (no suffix); system mode declines it.
    #[test]
    fn lc_serve_is_installed_and_enabled_only_in_same_user_mode() {
        let m = build(&inputs()).unwrap();
        let u = m.units.iter().find(|u| u.name == "lc-serve.service").expect("same-user mode installs it");
        assert!(u.enable);
        assert!(m.enable("prod").contains(&"lc-serve.service".to_string()), "shared name, never instance-suffixed");
        assert!(!m.optional.contains(&"lc-serve.service".to_string()));

        let mut i = inputs();
        i.lc_system_mode = true;
        let m2 = build(&i).unwrap();
        assert!(!m2.units.iter().any(|u| u.name == "lc-serve.service"), "system mode: not installed");
        assert!(m2.optional.contains(&"lc-serve.service".to_string()), "declined, so never UNLISTED");
        assert!(!m2.enable("prod").iter().any(|u| u.starts_with("lc-serve")));
        assert!(m2.unlisted(["lc-serve.service"]).is_empty());
    }

    #[test]
    fn a_malformed_watcher_manifest_stops_before_any_unit_is_built() {
        let mut i = inputs();
        i.watch_names = Err("the watcher manifest is malformed".into());
        assert!(build(&i).is_err());
    }

    #[test]
    fn dolt_beads_is_included_and_enabled_only_when_dolt_data_is_set() {
        let m = build(&inputs()).unwrap();
        assert!(m.units.iter().any(|u| u.name == "dolt-beads.service" && u.enable));
        assert!(!m.optional.contains(&"dolt-beads.service".to_string()));
        assert!(m.units.iter().any(|u| u.name == "dolt-tmp-prune.timer" && u.enable));
        assert!(m.units.iter().any(|u| u.name == "dolt-tmp-prune.service" && !u.enable));

        let mut i = inputs();
        i.dolt_data_set = false;
        let m = build(&i).unwrap();
        assert!(!m.units.iter().any(|u| u.name == "dolt-beads.service"));
        assert!(m.optional.contains(&"dolt-beads.service".to_string()));
        assert!(!m.units.iter().any(|u| u.name.starts_with("dolt-tmp-prune")));
        assert!(m.optional.contains(&"dolt-tmp-prune.timer".to_string()));
    }

    #[test]
    fn dolt_beads_test_is_installed_but_never_enabled() {
        let mut i = inputs();
        i.testdb_data_set = true;
        let m = build(&i).unwrap();
        let u = m.units.iter().find(|u| u.name == "dolt-beads-test.service").unwrap();
        assert!(!u.enable);
    }

    #[test]
    fn mail_deliver_is_unbuilt_without_inotifywait() {
        let mut i = inputs();
        i.inotify_present = false;
        let m = build(&i).unwrap();
        assert!(!m.units.iter().any(|u| u.name == "spira-mail-deliver.service"));
        assert!(m.optional.contains(&"spira-mail-deliver.service".to_string()));
        assert!(m.unbuilt.contains(&"spira-mail-deliver.service".to_string()));
    }

    #[test]
    fn the_broker_timer_is_present_but_enabled_only_with_the_operators_opt_in() {
        let m = build(&inputs()).unwrap();
        let timer = m.units.iter().find(|u| u.name == "spira-broker.timer").unwrap();
        assert!(!timer.enable);
        let mut i = inputs();
        i.broker_enable = true;
        let m2 = build(&i).unwrap();
        assert!(m2.units.iter().find(|u| u.name == "spira-broker.timer").unwrap().enable);
    }

    #[test]
    fn only_watchers_live_in_the_watcher_namespace() {
        let mut i = inputs();
        i.watch_names = Ok(vec![]);
        let m = build(&i).unwrap();
        let strays: Vec<&str> = m.template_names().into_iter().filter(|n| n.starts_with("spira-watch-")).collect();
        assert!(strays.is_empty(), "manifest units the watcher glob would match: {strays:?}");
        assert!(m.template_names().contains(&"spira-refresh.timer"));
        assert!(m.template_names().contains(&"spira-notify.timer"));
    }

    #[test]
    fn enable_lists_per_instance_names_then_watchers_in_manifest_order() {
        let m = build(&inputs()).unwrap();
        let en = m.enable("prod");
        assert!(en.contains(&"spira-sentinel-prod.timer".to_string()));
        assert!(en.contains(&"cockpit-ensure.timer".to_string())); // shared unit, no suffix
        assert!(en.contains(&"spira-watch-testview-prod.service".to_string()));
        assert!(!en.iter().any(|u| u == "spira-sentinel.timer")); // never the un-suffixed name
    }

    #[test]
    fn inst_name_leaves_shared_and_watcher_template_names_alone() {
        assert_eq!(inst_name("spira-sentinel.timer", "test"), "spira-sentinel-test.timer");
        assert_eq!(inst_name("concierge.service", "test"), "concierge.service");
        assert_eq!(inst_name("spira-watch@.service", "test"), "spira-watch@.service");
        assert_eq!(inst_watch_name("testview", "test"), "spira-watch-testview-test.service");
    }

    #[test]
    fn unlisted_finds_a_forgotten_unit_file_but_not_a_declined_one() {
        let m = build(&inputs()).unwrap();
        let on_disk = ["spira-sentinel.service", "spira-cockpit-new.service", "spira-lc.service"];
        let u = m.unlisted(on_disk);
        assert_eq!(u, vec!["spira-cockpit-new.service".to_string()]);
    }

    #[test]
    fn unlisted_never_flags_the_watcher_template_itself() {
        // spira-watch@.service is in `self.units` (enable: false) but excluded from
        // `template_names()` — a real on-disk copy of the raw template must not be reported
        // UNLISTED just because the render-only view doesn't carry it.
        let m = build(&inputs()).unwrap();
        let on_disk = ["spira-watch@.service"];
        assert_eq!(m.unlisted(on_disk), Vec::<String>::new());
    }
}
