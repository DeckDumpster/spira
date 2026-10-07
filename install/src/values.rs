//! Host values for rendering a unit template (systemd/render.py's argument set, in Rust) and
//! the render call itself — render.py's own rules, not `release::units::render_from_values`
//! (`release activate`/`install-tarball`'s renderer, which is stricter: any placeholder used
//! with an empty value refuses). That strictness was tried here first and reverted (DESIGN.md
//! "Decisions"): a live `testenv` batch container never sets `SPIRA_DB`/`SPIRA_SNAP_STALE_S`
//! before running this crate's binaries — `install.sh` got them from sourcing conf.sh, which
//! this crate does not — and render.py's original, permissive rule (substitute a known key
//! even when it is empty; refuse only an entirely-unfilled placeholder, plus one special case
//! for `@DOLT@`) is what every caller has actually been relying on. This file is therefore
//! its own renderer, using only [`release::units::placeholders`] and
//! [`release::units::normalize`] (structural parsing that carries no behaviour of its own)
//! from the shared module.

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
    pub watchtower_start_timeout_s: String,
    pub path_tail: String,
    /// `SPIRA_SCCACHE_DAV_ADDR` (sp-xtdqi) — this box's own LAN address for the shared
    /// compilation cache, if any. Empty is a legal value (the template is never installed
    /// when it is, per `manifest::Inputs::sccache_dav_addr_set`), substituted as empty like
    /// `SPIRA_DOLT_DATA` rather than refused, in case a caller ever renders the template
    /// directly (`--render`) without going through the manifest's own gate.
    pub sccache_dav_addr: String,
    pub repo_map: String,
    pub lc_password_file: String,
    /// The spec SPIRA_TOML names — the one source every rendered unit runs under.
    pub toml: String,
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
        m.insert("SPIRA_REPO_MAP".into(), self.repo_map.clone());
        m.insert("SPIRA_LC_PASSWORD_FILE".into(), self.lc_password_file.clone());
        m.insert("SPIRA_TOML".into(), self.toml.clone());
        m.insert("SPIRA_COCKPIT".into(), self.cockpit.clone());
        m.insert("SPIRA_DOLT_DATA".into(), self.dolt_data.clone());
        m.insert("SPIRA_SCCACHE_DAV_ADDR".into(), self.sccache_dav_addr.clone());
        m.insert("SPIRA_TESTDB_DATA".into(), self.testdb_data.clone());
        m.insert("DOLT".into(), self.dolt.clone());
        m.insert("SPIRA_PROD".into(), prod.clone());
        m.insert("SPIRA_INSTANCE".into(), self.instance.clone());
        m.insert("SPIRA_TESTDB_PORT".into(), self.testdb_port.clone());
        m.insert("SPIRA_SNAP_STALE_S".into(), self.snap_stale_s.clone());
        m.insert("SPIRA_WATCHTOWER_START_TIMEOUT_S".into(), self.watchtower_start_timeout_s.clone());
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
    let map = host.to_map();
    // render.py's own special case, checked against the RAW template before substitution:
    // an empty DOLT is fine for a template that never names it, but a refusal — not a
    // silent empty ExecStart — for one that does.
    if text.contains("@DOLT@") && map.get("DOLT").is_none_or(|v| v.is_empty()) {
        return Err(format!("{template_name}: dolt is not on PATH; install dolt before rendering units that need it"));
    }
    let mut out = text.to_string();
    for k in release::units::placeholders(text) {
        if let Some(v) = map.get(&k) {
            out = out.replace(&format!("@{k}@"), v);
        }
    }
    if let Some(w) = watcher {
        out = out.replace("%i", w);
    }
    if template_name.starts_with("spira-") && template_name.ends_with(".timer") {
        out = out
            .lines()
            .map(|l| match l.strip_prefix("Unit=spira-").and_then(|r| r.strip_suffix(".service")) {
                Some(name) if !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') => {
                    format!("Unit=spira-{name}-{}.service", host.instance)
                }
                _ => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let left = release::units::placeholders(&out);
    if !left.is_empty() {
        return Err(format!("{template_name} has placeholders nothing fills: {}", left.join(", ")));
    }
    Ok(release::units::normalize(&out))
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
            watchtower_start_timeout_s: "360".into(),
            path_tail: "".into(),
            lc_password_file: "/h/lc.credential".into(),
            toml: "/h/cfg.toml".into(),
            sccache_dav_addr: "".into(),
            repo_map: "".into(),
        }
    }

    /// sp-xfqnr: the same-user serve unit renders completely, serves the user runtime-dir
    /// socket the resolver's same-user default names, and authenticates with the same-user
    /// credential.
    #[test]
    fn the_same_user_serve_unit_renders_onto_the_user_socket() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd/lc-serve.service");
        let out = render_file(&p, &hv(), None).unwrap();
        assert!(!out.contains('@'), "every placeholder substituted:\n{out}");
        assert!(out.contains("\nExecStart=/h/bin/spira-lc serve %t/spira-lc/sock\n"), "{out}");
        assert!(out.contains("\nEnvironment=SPIRA_LC_SOCKET=%t/spira-lc/sock\n"));
        assert!(out.contains("\nEnvironment=SPIRA_LC_PASSWORD_FILE=/h/lc.credential\n"));
        assert!(out.contains("\nRequires=lc-serve.socket\n"));
        assert!(!out.contains("RuntimeDirectory"), "the socket unit owns the runtime directory");
        let sock = render_file(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd/lc-serve.socket"), &hv(), None).unwrap();
        assert!(sock.contains("\nListenStream=%t/spira-lc/sock\n"), "{sock}");
        for k in ["SPIRA_RELEASE=/h", "SPIRA_HOME=/h/spira", "SPIRA_REPO=/h", "SPIRA_DB=/db", "SPIRA_RUN=/run"] {
            assert!(out.contains(&format!("\nEnvironment={k}\n")), "{k}");
        }
        // PATH is the standard unit form (release, system dirs, then the configured tail, which
        // is where ~/.local/bin — dolt and bd — comes from); the release crate enforces the shape.
        assert!(out.lines().any(|l| l.starts_with("Environment=PATH=/h/bin:/h/spira:/usr/local/bin:/usr/bin:/bin")), "standard unit PATH:\n{out}");
        assert!(!out.contains("\nUser="), "a user unit runs as the operator");
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
    fn a_known_key_substitutes_even_when_its_value_is_empty() {
        // render.py's own permissive rule: SPIRA_DOLT_DATA is a known key, just an empty one
        // here — substituted as empty, not refused. Real callers never hit this in practice
        // (units.sh only ever installs a template that uses it when SPIRA_DOLT_DATA is set),
        // but a batch container that has not sourced conf.sh leaves other keys
        // (SPIRA_DB, SPIRA_SNAP_STALE_S) genuinely empty and still needs this to render.
        let out = render("z.service", "Environment=SPIRA_DOLT_DATA=@SPIRA_DOLT_DATA@\n", &hv(), None).unwrap();
        assert_eq!(out, "Environment=SPIRA_DOLT_DATA=\n");
        let out = render("w.service", "Environment=PATH=/bin@SPIRA_PATH_TAIL@\n", &hv(), None).unwrap();
        assert_eq!(out, "Environment=PATH=/bin\n");
    }

    #[test]
    fn dolt_is_the_one_key_refused_when_empty_and_used() {
        let mut h = hv();
        h.dolt = "".into();
        let e = render("dolt-beads.service", "ExecStart=@DOLT@ sql-server\n", &h, None).unwrap_err();
        assert!(e.contains("dolt"), "{e}");
        // A template that never names @DOLT@ is unaffected by DOLT being empty.
        let out = render("other.service", "ExecStart=/bin/true\n", &h, None).unwrap();
        assert_eq!(out, "ExecStart=/bin/true\n");
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
