//! Unit names (wave4-decomposition.md row C7 "deps / units"; bead sp-wqj3o, "wave 4.10:
//! small conf.sh families") — `conf.sh`'s own `spira_unit` and `watch_unit_name`, ported so
//! every caller that queries or restarts a watcher or daemon unit agrees on which name to
//! address rather than drifting into two formulas. `spira_unit`'s own scar (sp-smbq0,
//! 2026-09-09): strand.sh and doctor.sh addressed units by their un-suffixed plain name
//! while the running units on a post-migration box carried the instance suffix
//! (`spira-sentinel-prod.timer`), which made strand.sh report "sentinel timer: inactive"
//! while the sentinel was firing every two minutes.
//!
//! Both functions are side-effecting (`resolve_unit` queries systemd) or read ambient
//! per-copy facts (`SPIRA_INSTANCE`), so — like [`crate::env_bootstrap`] — neither lives in
//! [`crate::resolve`], which stays a pure function of its [`crate::resolve::ResolveInput`].

use std::process::Stdio;

/// `watch_unit_name <name> <instance>` — MIRRORS `inst_watch_name` in `systemd/units.sh`
/// (one formula, two callers; conf.sh's own comment on `watch_unit_name`). Pure string
/// formatting: no systemctl call, so this never fails and never queries anything live. An
/// empty `instance` defaults to `"prod"`, matching bash's own `${SPIRA_INSTANCE:-prod}`.
pub fn watch_unit_name(name: &str, instance: &str) -> String {
    let inst = if instance.is_empty() { "prod" } else { instance };
    format!("spira-watch-{name}-{inst}.service")
}

/// `spira_unit <base> <kind> <instance> <systemctl>` -> the unit name this installation
/// actually has loaded, or `"?"` when neither form is known to systemd
/// (law-absence-needs-a-positive-control: a unit that cannot be found must not be queried
/// for health, which would report "inactive" about an unrelated subject). Tries the
/// instance-qualified form first (`spira-<base>-<instance>.<kind>`); falls back to the
/// plain form (`spira-<base>.<kind>`) only when the qualified one is neither enabled nor
/// active. An empty `instance` makes the qualified form IDENTICAL to the plain one —
/// matching bash's `${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}` (no default here, unlike
/// [`watch_unit_name`]'s own `:-prod`), so the two systemctl queries below collapse to one.
pub fn resolve_unit(base: &str, kind: &str, instance: &str, systemctl: &str) -> String {
    let qualified = if instance.is_empty() {
        format!("spira-{base}.{kind}")
    } else {
        format!("spira-{base}-{instance}.{kind}")
    };
    let plain = format!("spira-{base}.{kind}");
    if loaded(systemctl, &qualified) {
        qualified
    } else if loaded(systemctl, &plain) {
        plain
    } else {
        "?".to_string()
    }
}

/// `systemctl --user is-enabled <unit> || systemctl --user is-active <unit>` — a unit is
/// "known" the moment either check exits 0, exactly as bash's own `if ... || ...; then`
/// reads it. Neither subcommand's stdout is read; only the exit status matters.
fn loaded(systemctl: &str, unit: &str) -> bool {
    let ok = |args: &[&str]| -> bool {
        crate::bounded::bounded(systemctl)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    ok(&["--user", "is-enabled", unit]) || ok(&["--user", "is-active", unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_systemctl(dir: &std::path::Path, enabled: &str, active: &str) -> String {
        let p = dir.join("systemctl");
        let body = format!(
            "#!/usr/bin/env bash\ncmd=\"\" unit=\"\"\nfor a; do\n    case \"$a\" in --user|--quiet) ;; *) [ -z \"$cmd\" ] && cmd=\"$a\" || unit=\"$a\" ;; esac\ndone\ncase \"$cmd\" in\n    is-enabled) [ \"$unit\" = \"{enabled}\" ] && exit 0 || exit 1 ;;\n    is-active) [ \"$unit\" = \"{active}\" ] && exit 0 || exit 3 ;;\n    *) exit 0 ;;\nesac\n"
        );
        testkit::write_exe(&p, &body);
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn watch_unit_name_formats_with_the_given_instance() {
        assert_eq!(watch_unit_name("answers", "prod"), "spira-watch-answers-prod.service");
        assert_eq!(watch_unit_name("testview", "test"), "spira-watch-testview-test.service");
    }

    #[test]
    fn watch_unit_name_defaults_empty_instance_to_prod() {
        assert_eq!(watch_unit_name("cockpit", ""), "spira-watch-cockpit-prod.service");
    }

    #[test]
    fn watch_unit_name_never_produces_the_template_form() {
        // Positive control (test-watchd-unit-name.sh's own one): the old template form
        // (spira-watch@name.service) must never come out of this function.
        assert_ne!(watch_unit_name("answers", "prod"), "spira-watch@answers.service");
    }

    #[test]
    fn resolve_unit_prefers_the_instance_qualified_form_when_it_is_known() {
        let dir = testkit::TempDir::new("spira-config-unit-a");
        let sc = stub_systemctl(dir.path(), "", "spira-sentinel-prod.timer");
        assert_eq!(resolve_unit("sentinel", "timer", "prod", &sc), "spira-sentinel-prod.timer");
    }

    #[test]
    fn resolve_unit_falls_back_to_the_plain_form() {
        let dir = testkit::TempDir::new("spira-config-unit-b");
        let sc = stub_systemctl(dir.path(), "", "spira-loom.service");
        assert_eq!(resolve_unit("loom", "service", "prod", &sc), "spira-loom.service");
    }

    #[test]
    fn resolve_unit_is_unknown_when_neither_form_is_loaded() {
        let dir = testkit::TempDir::new("spira-config-unit-c");
        let sc = stub_systemctl(dir.path(), "", "");
        assert_eq!(resolve_unit("sentinel", "timer", "prod", &sc), "?");
        assert_eq!(resolve_unit("loom", "service", "prod", &sc), "?");
    }

    #[test]
    fn resolve_unit_with_no_instance_collapses_to_one_query() {
        let dir = testkit::TempDir::new("spira-config-unit-d");
        let sc = stub_systemctl(dir.path(), "", "spira-sentinel.timer");
        assert_eq!(resolve_unit("sentinel", "timer", "", &sc), "spira-sentinel.timer");
    }
}
