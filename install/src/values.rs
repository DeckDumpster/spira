//! Host values for rendering a unit template (systemd/render.py's argument set, in Rust) and
//! the render call itself, which delegates to `release::units::render_from_values` — the same
//! substitution, `%i`, timer-`Unit=` rewrite and one-trailing-newline rules `release activate`
//! and `release install-tarball` already use, so there is exactly one renderer, not two that
//! can drift the way `install.sh`'s and `unit-ensure.sh`'s hand-copied heredocs once did.
//!
//! DECISION: render.py refused only on an unfilled placeholder and specially refused `@DOLT@`
//! when empty; every other key rendered empty silently (e.g. an unset `SPIRA_TESTDB_DATA` in
//! a template that happened to use it). `release::units::render_from_values` is stricter — any
//! placeholder used with an empty value refuses by name, `SPIRA_PATH_TAIL` excepted — matching
//! the render `release activate`/`install-tarball` already ship. This crate adopts that
//! stricter rule rather than porting the looser one: FAIL CLOSED (wave brief, 2026-09-29).
//! Checked against every template in `systemd/` (DESIGN.md "Parity"): no template uses a key
//! that is empty on the path that renders it, so the stricter rule changes no real verdict —
//! it only turns a hypothetical silent-empty render into a named refusal.

use std::collections::BTreeMap;
use std::path::Path;

/// Everything systemd/render.py's `--home/--repo/.../--path-tail` flags carried, plus the
/// three derived keys it computed (`SPIRA_PROD_COCK`, `SPIRA_PROD_ROOT`, `SPIRA_RELEASE`).
#[derive(Debug, Clone, Default)]
pub struct HostValues {
    pub home: String,
    pub repo: String,
    pub run: String,
    pub db: String,
    pub cockpit: String,
    pub dolt_data: String,
    pub testdb_data: String,
    pub dolt: String,
    pub prod: String,
    pub instance: String,
    pub testdb_port: String,
    pub snap_stale_s: String,
    pub path_tail: String,
}

impl HostValues {
    /// The `@KEY@` -> value map render.py built, including the `SPIRA_PROD` empty-to-`SPIRA_HOME`
    /// fallback and the three derived keys.
    pub fn to_map(&self) -> BTreeMap<String, String> {
        let prod = if self.prod.is_empty() { self.home.clone() } else { self.prod.clone() };
        let prod_root = dirname(&prod);
        let mut m = BTreeMap::new();
        m.insert("SPIRA_HOME".into(), self.home.clone());
        m.insert("SPIRA_REPO".into(), self.repo.clone());
        m.insert("SPIRA_RUN".into(), self.run.clone());
        m.insert("SPIRA_DB".into(), self.db.clone());
        m.insert("SPIRA_COCKPIT".into(), self.cockpit.clone());
        m.insert("SPIRA_DOLT_DATA".into(), self.dolt_data.clone());
        m.insert("SPIRA_TESTDB_DATA".into(), self.testdb_data.clone());
        m.insert("DOLT".into(), self.dolt.clone());
        m.insert("SPIRA_PROD".into(), prod.clone());
        m.insert("SPIRA_INSTANCE".into(), self.instance.clone());
        m.insert("SPIRA_TESTDB_PORT".into(), self.testdb_port.clone());
        m.insert("SPIRA_SNAP_STALE_S".into(), self.snap_stale_s.clone());
        m.insert("SPIRA_PATH_TAIL".into(), if self.path_tail.is_empty() { String::new() } else { format!(":{}", self.path_tail) });
        m.insert("SPIRA_PROD_COCK".into(), format!("{prod_root}/cockpit"));
        m.insert("SPIRA_PROD_ROOT".into(), prod_root.clone());
        m.insert("SPIRA_RELEASE".into(), prod_root);
        m
    }
}

/// `dirname(prod)`, matching Python's `os.path.dirname` (no trailing-slash trimming beyond
/// one level, no symlink resolution — render.py ran this on a string, not a real path).
fn dirname(p: &str) -> String {
    match p.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => p[..i].to_string(),
        None => String::new(),
    }
}

/// Render one template (systemd/render.py's rules). `template_name` is used only for error
/// messages and the `spira-*.timer` `Unit=` rewrite — it need not match `path`'s basename.
pub fn render(template_name: &str, text: &str, host: &HostValues, watcher: Option<&str>) -> Result<String, String> {
    release::units::render_from_values(template_name, text, &host.to_map(), watcher, &host.instance)
}

/// Render the template at `path`, naming it by its own basename.
pub fn render_file(path: &Path, host: &HostValues, watcher: Option<&str>) -> Result<String, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    render(&name, &text, host, watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hv() -> HostValues {
        HostValues {
            home: "/h/spira".into(),
            repo: "/h".into(),
            run: "/run".into(),
            db: "/db".into(),
            cockpit: "/h/cockpit".into(),
            dolt_data: "".into(),
            testdb_data: "".into(),
            dolt: "/usr/bin/dolt".into(),
            prod: "".into(),
            instance: "prod".into(),
            testdb_port: "3308".into(),
            snap_stale_s: "600".into(),
            path_tail: "".into(),
        }
    }

    #[test]
    fn prod_falls_back_to_home_and_derives_release_keys() {
        let m = hv().to_map();
        assert_eq!(m["SPIRA_PROD"], "/h/spira");
        assert_eq!(m["SPIRA_PROD_ROOT"], "/h");
        assert_eq!(m["SPIRA_RELEASE"], "/h");
        assert_eq!(m["SPIRA_PROD_COCK"], "/h/cockpit");
        assert_eq!(m["SPIRA_PATH_TAIL"], "");
    }

    #[test]
    fn an_explicit_prod_is_kept_and_path_tail_gets_a_leading_colon() {
        let mut h = hv();
        h.prod = "/rel/current/spira".into();
        h.path_tail = "/opt/extra/bin".into();
        let m = h.to_map();
        assert_eq!(m["SPIRA_PROD"], "/rel/current/spira");
        assert_eq!(m["SPIRA_PROD_ROOT"], "/rel/current");
        assert_eq!(m["SPIRA_RELEASE"], "/rel/current");
        assert_eq!(m["SPIRA_PROD_COCK"], "/rel/current/cockpit");
        assert_eq!(m["SPIRA_PATH_TAIL"], ":/opt/extra/bin");
    }

    #[test]
    fn a_service_renders_and_an_unfilled_placeholder_refuses() {
        let out = render("x.service", "ExecStart=@SPIRA_HOME@/x\n", &hv(), None).unwrap();
        assert_eq!(out, "ExecStart=/h/spira/x\n");
        let e = render("y.service", "ExecStart=@NOPE@\n", &hv(), None).unwrap_err();
        assert!(e.contains("NOPE"), "{e}");
    }

    #[test]
    fn a_key_used_with_an_empty_value_refuses_except_path_tail() {
        let e = render("z.service", "Environment=SPIRA_DOLT_DATA=@SPIRA_DOLT_DATA@\n", &hv(), None).unwrap_err();
        assert!(e.contains("SPIRA_DOLT_DATA"), "{e}");
        // SPIRA_PATH_TAIL is the one OPTIONAL_EMPTY key — empty is the ordinary case.
        let out = render("w.service", "Environment=PATH=/bin@SPIRA_PATH_TAIL@\n", &hv(), None).unwrap();
        assert_eq!(out, "Environment=PATH=/bin\n");
    }

    #[test]
    fn a_watcher_template_substitutes_percent_i() {
        let out = render("spira-watch@.service", "ExecStart=/w %i\n", &hv(), Some("testview")).unwrap();
        assert_eq!(out, "ExecStart=/w testview\n");
    }

    #[test]
    fn a_timer_template_gains_the_instance_suffix_on_its_unit_line() {
        let out = render("spira-sentinel.timer", "[Unit]\nUnit=spira-sentinel.service\n", &hv(), None).unwrap();
        assert_eq!(out, "[Unit]\nUnit=spira-sentinel-prod.service\n");
    }
}
