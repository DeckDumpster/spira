//! Install's lifecycle-store phase (sp-xfqnr): build the `spira_lifecycle` database a fresh
//! same-user install never got — `lifecycle/schema.sql`, then `lifecycle/migrations/*.sql`,
//! then `lifecycle/grants.sql` with the two same-user credentials phase 1.5 wrote — so an
//! aeon can claim and submit, and `spira-lc history` answers, the moment install finishes.
//!
//! THE MECHANISM IS spira-lc's OWN, the one `spira/cutover-deploy.sh` steps 1–2 use:
//! `spira-lc admin-apply-ddl <file>` as the database admin, plus `spira-lc admin-migrate
//! <dir>` (spira-lc/src/migrate.rs), which applies the migrations in filename order and
//! skips an `ADD COLUMN` whose column is already there — schema.sql already carries every
//! migrated column, and Dolt has no `ADD COLUMN IF NOT EXISTS`. schema.sql is `IF NOT
//! EXISTS` throughout and grants.sql is `CREATE USER IF NOT EXISTS`, so the whole phase is
//! safe to re-run on every install.
//!
//! THE SECRETS NEVER REACH argv, A LOG, OR A PERSISTENT FILE. spira-lc reads its password
//! from the file `SPIRA_LC_PASSWORD_FILE` names, and admin-apply-ddl reads SQL from a path —
//! so the admin password and the substituted grants live only in a [`PrivateDir`] (0700,
//! files 0600, created fresh, removed when it drops, right after the call). Every byte of
//! spira-lc's output install relays is passed through [`redact`] first: a SQL error can
//! quote the statement it failed on, and grants.sql's statements carry the passwords.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const RW_PLACEHOLDER: &str = "@SPIRA_LC_PASSWORD@";
pub const RO_PLACEHOLDER: &str = "@SPIRA_LC_RO_PASSWORD@";

/// A credential as read from its file: trimmed exactly as spira-lc's own `password_from`
/// trims, so the database user and the service authenticating as it agree byte for byte.
pub fn read_credential(path: &Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read the spira_lc credential {}: {e}", path.display()))?;
    let v = raw.trim().to_string();
    if v.is_empty() {
        return Err(format!("the spira_lc credential {} is empty", path.display()));
    }
    Ok(v)
}

/// grants.sql with both placeholders substituted. Refuses a secret that could break out of
/// the single-quoted SQL literal it lands in (a quote, a backslash, a control character):
/// install's own credentials are base64 and never contain one, so a credential that does
/// was written by something else and is not this phase's to quote.
pub fn substitute_grants(template: &str, rw: &str, ro: &str) -> Result<String, String> {
    for (name, v) in [("spira_lc", rw), ("spira_lc_ro", ro)] {
        if v.is_empty() {
            return Err(format!("the {name} credential is empty"));
        }
        if v.chars().any(|c| c == '\'' || c == '\\' || c == '"' || c.is_control()) {
            return Err(format!("the {name} credential contains a quote, backslash or control character — refusing to place it in grants.sql"));
        }
    }
    if rw == ro {
        return Err("the spira_lc and spira_lc_ro credentials are the same secret — grants.sql requires two".into());
    }
    if !template.contains(RW_PLACEHOLDER) || !template.contains(RO_PLACEHOLDER) {
        return Err(format!("grants.sql does not carry both {RW_PLACEHOLDER} and {RO_PLACEHOLDER}"));
    }
    Ok(template.replace(RW_PLACEHOLDER, rw).replace(RO_PLACEHOLDER, ro))
}

/// `text` with every non-empty secret replaced by `<redacted>`.
pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(*s, "<redacted>");
    }
    out
}

/// A directory only this user can enter, removed (with everything in it) on drop.
pub struct PrivateDir {
    path: PathBuf,
}

impl PrivateDir {
    pub fn new(parent: &Path, tag: &str) -> Result<Self, String> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let path = parent.join(format!("spira-install-{tag}-{}-{nanos}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        Ok(PrivateDir { path })
    }

    /// Write `contents` to a new 0600 file inside; refuses to reuse an existing name.
    pub fn write(&self, name: &str, contents: &str) -> Result<PathBuf, String> {
        let p = self.path.join(name);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        f.write_all(contents.as_bytes()).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
        Ok(p)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Where and as whom the admin connection goes.
#[derive(Debug, Clone)]
pub struct Admin {
    pub user: String,
    pub password: String,
    pub host: Option<String>,
    pub port: u16,
}

/// The environment one spira-lc admin call runs under: the admin user, the PATH of the file
/// holding the admin password (never the password itself), and where the server is.
/// `SPIRA_LC_PASSWORD` is cleared (spira-lc would prefer the file anyway) and
/// `SPIRA_LC_SOCKET` is pointed nowhere so nothing is ever forwarded to a service.
pub fn admin_env(admin: &Admin, password_file: &Path) -> Vec<(String, String)> {
    let mut e = vec![
        ("SPIRA_LC_USER".to_string(), admin.user.clone()),
        ("SPIRA_LC_PASSWORD_FILE".to_string(), password_file.to_string_lossy().to_string()),
        ("SPIRA_LC_PASSWORD".to_string(), String::new()),
        ("SPIRA_LC_PORT".to_string(), admin.port.to_string()),
        ("SPIRA_LC_SOCKET".to_string(), "/nonexistent/spira-install-admin-never-forwards".to_string()),
    ];
    if let Some(h) = &admin.host {
        e.push(("SPIRA_LC_HOST".to_string(), h.clone()));
    }
    e
}

/// One step of the phase, in the order it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Schema(PathBuf),
    Migrations(PathBuf),
    Grants,
}

/// The phase's steps for a `lifecycle/` directory: schema, then migrations, then grants
/// (grants name tables, so they come after every table and column exists).
pub fn steps(lifecycle_dir: &Path) -> Result<Vec<Step>, String> {
    let schema = lifecycle_dir.join("schema.sql");
    let migrations = lifecycle_dir.join("migrations");
    let grants = lifecycle_dir.join("grants.sql");
    for p in [&schema, &grants] {
        if !p.is_file() {
            return Err(format!("{} is missing — cannot build the lifecycle store", p.display()));
        }
    }
    if !migrations.is_dir() {
        return Err(format!("{} is missing — cannot build the lifecycle store", migrations.display()));
    }
    Ok(vec![Step::Schema(schema), Step::Migrations(migrations), Step::Grants])
}

/// Run the whole phase. `run(args, env)` invokes `spira-lc <args>` under `env` and returns
/// (exit code, combined output); it is injected so tests can watch every call. Fails closed
/// on the first step that does not exit 0, naming the step; returns the per-step lines to
/// print. Every output line is redacted of all three secrets before it leaves here.
pub fn apply(
    lifecycle_dir: &Path,
    tmp_parent: &Path,
    admin: &Admin,
    rw: &str,
    ro: &str,
    mut run: impl FnMut(&[String], &[(String, String)]) -> (i32, String),
) -> Result<Vec<String>, String> {
    let secrets = [rw, ro, admin.password.as_str()];
    let plan = steps(lifecycle_dir)?;
    let grants_text = std::fs::read_to_string(lifecycle_dir.join("grants.sql")).map_err(|e| format!("cannot read grants.sql: {e}"))?;
    let substituted = substitute_grants(&grants_text, rw, ro)?;

    let dir = PrivateDir::new(tmp_parent, "lc")?;
    let pw_file = dir.write("admin", &admin.password)?;
    let env = admin_env(admin, &pw_file);
    let mut lines = Vec::new();
    for step in plan {
        let (label, args, grants_file) = match &step {
            Step::Schema(p) => ("schema.sql", vec!["admin-apply-ddl".to_string(), p.to_string_lossy().to_string()], None),
            Step::Migrations(p) => ("migrations", vec!["admin-migrate".to_string(), p.to_string_lossy().to_string()], None),
            Step::Grants => {
                let g = dir.write("grants.sql", &substituted)?;
                ("grants.sql", vec!["admin-apply-ddl".to_string(), g.to_string_lossy().to_string()], Some(g))
            }
        };
        let (rc, out) = run(&args, &env);
        // The substituted grants exist only for the one call that reads them.
        if let Some(g) = grants_file {
            let _ = std::fs::remove_file(g);
        }
        let out = redact(out.trim_end(), &secrets);
        if rc != 0 {
            return Err(format!("applying {label} as {} failed (spira-lc exit {rc}){}", admin.user, if out.is_empty() { String::new() } else { format!(": {out}") }));
        }
        lines.push(format!("applied {label} (as {})", admin.user));
        lines.extend(out.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("  {l}")));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const RW: &str = "rwSecret+/AbC123";
    const RO: &str = "roSecret+/XyZ789";

    fn lifecycle_dir(t: &testkit::TempDir) -> PathBuf {
        let d = t.path().join("lifecycle");
        std::fs::create_dir_all(d.join("migrations")).unwrap();
        std::fs::write(d.join("schema.sql"), "CREATE DATABASE IF NOT EXISTS spira_lifecycle;").unwrap();
        std::fs::write(d.join("migrations/0001-a.sql"), "ALTER TABLE bead ADD COLUMN a INT;").unwrap();
        std::fs::write(d.join("grants.sql"), "CREATE USER IF NOT EXISTS 'spira_lc'@'%' IDENTIFIED BY '@SPIRA_LC_PASSWORD@';\nCREATE USER IF NOT EXISTS 'spira_lc_ro'@'%' IDENTIFIED BY '@SPIRA_LC_RO_PASSWORD@';\n").unwrap();
        d
    }

    fn admin(pw: &str) -> Admin {
        Admin { user: "root".into(), password: pw.into(), host: None, port: 3307 }
    }

    #[test]
    fn the_real_grants_file_substitutes_both_placeholders_and_leaves_none() {
        let g = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lifecycle/grants.sql")).unwrap();
        let out = substitute_grants(&g, RW, RO).unwrap();
        assert!(!out.contains("@SPIRA_LC"), "no placeholder survives");
        assert!(out.contains(&format!("'spira_lc'@'%' IDENTIFIED BY '{RW}'")));
        assert!(out.contains(&format!("'spira_lc_ro'@'%' IDENTIFIED BY '{RO}'")));
    }

    #[test]
    fn a_secret_that_could_leave_its_sql_literal_is_refused() {
        for bad in ["a'b", "a\\b", "a\"b", "a\nb", ""] {
            assert!(substitute_grants("@SPIRA_LC_PASSWORD@ @SPIRA_LC_RO_PASSWORD@", bad, RO).is_err(), "{bad:?}");
            assert!(substitute_grants("@SPIRA_LC_PASSWORD@ @SPIRA_LC_RO_PASSWORD@", RW, bad).is_err(), "{bad:?}");
        }
        assert!(substitute_grants("@SPIRA_LC_PASSWORD@ @SPIRA_LC_RO_PASSWORD@", RW, RW).is_err(), "one secret for both users");
        assert!(substitute_grants("no placeholders", RW, RO).is_err());
    }

    #[test]
    fn redact_removes_every_secret_and_ignores_an_empty_one() {
        let s = redact(&format!("near 'IDENTIFIED BY '{RW}'' and {RO} and root"), &[RW, RO, ""]);
        assert!(!s.contains(RW) && !s.contains(RO), "{s}");
        assert!(s.contains("root") && s.contains("<redacted>"));
    }

    #[test]
    fn credentials_are_trimmed_like_spira_lc_trims_them() {
        let t = testkit::TempDir::new("lc-cred");
        let p = t.path().join("c");
        std::fs::write(&p, format!("{RW}\n")).unwrap();
        assert_eq!(read_credential(&p).unwrap(), RW);
        std::fs::write(&p, "\n").unwrap();
        assert!(read_credential(&p).is_err());
    }

    /// The load-bearing property: across every spira-lc call the phase makes, no secret is
    /// ever in argv or in an environment value, the substituted grants file is 0600 inside
    /// a 0700 directory while the call runs, and nothing is left behind afterwards.
    #[test]
    fn secrets_never_reach_argv_or_env_and_the_temp_files_are_gone_afterwards() {
        let t = testkit::TempDir::new("lc-apply");
        let dir = lifecycle_dir(&t);
        let tmp = t.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let mut calls: Vec<Vec<String>> = Vec::new();
        let lines = apply(&dir, &tmp, &admin("adminPw9"), RW, RO, |args, env| {
            for a in args {
                for s in [RW, RO, "adminPw9"] {
                    assert!(!a.contains(s), "secret in argv: {args:?}");
                }
            }
            for (k, v) in env {
                for s in [RW, RO, "adminPw9"] {
                    assert!(!v.contains(s), "secret in env {k}");
                }
            }
            let pw = env.iter().find(|(k, _)| k == "SPIRA_LC_PASSWORD_FILE").unwrap().1.clone();
            assert_eq!(std::fs::read_to_string(&pw).unwrap(), "adminPw9");
            let pw_dir = Path::new(&pw).parent().unwrap();
            assert_eq!(std::fs::metadata(pw_dir).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(&pw).unwrap().permissions().mode() & 0o777, 0o600);
            if args[1].ends_with("grants.sql") && args[1].starts_with(tmp.to_str().unwrap()) {
                assert_eq!(std::fs::metadata(&args[1]).unwrap().permissions().mode() & 0o777, 0o600);
                let g = std::fs::read_to_string(&args[1]).unwrap();
                assert!(g.contains(RW) && g.contains(RO));
            }
            calls.push(args.to_vec());
            // A server error that quotes the statement it failed on — must come back redacted.
            (0, format!("note: near 'IDENTIFIED BY '{RW}''"))
        })
        .unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0], ["admin-apply-ddl".to_string(), dir.join("schema.sql").to_string_lossy().to_string()]);
        assert_eq!(calls[1], ["admin-migrate".to_string(), dir.join("migrations").to_string_lossy().to_string()]);
        assert_eq!(calls[2][0], "admin-apply-ddl");
        assert!(calls[2][1].starts_with(tmp.to_str().unwrap()), "grants come from the private dir, never lifecycle/grants.sql itself");
        assert!(!lines.iter().any(|l| l.contains(RW)), "{lines:?}");
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0, "the private dir is removed");
        assert!(!std::fs::read_to_string(dir.join("grants.sql")).unwrap().contains(RW), "the shipped template is never rewritten");
    }

    #[test]
    fn a_failing_step_stops_the_phase_names_it_and_still_cleans_up() {
        let t = testkit::TempDir::new("lc-fail");
        let dir = lifecycle_dir(&t);
        let tmp = t.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let mut n = 0;
        let e = apply(&dir, &tmp, &admin(""), RW, RO, |args, _| {
            n += 1;
            if args[0] == "admin-migrate" {
                (2, format!("cannot tell: Access denied for {RO}"))
            } else {
                (0, String::new())
            }
        })
        .unwrap_err();
        assert_eq!(n, 2, "grants never run after a failed migration");
        assert!(e.contains("migrations") && e.contains("exit 2"), "{e}");
        assert!(!e.contains(RO), "{e}");
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
    }

    #[test]
    fn a_missing_lifecycle_file_fails_before_any_call() {
        let t = testkit::TempDir::new("lc-missing");
        let dir = lifecycle_dir(&t);
        std::fs::remove_file(dir.join("grants.sql")).unwrap();
        let e = apply(&dir, t.path(), &admin(""), RW, RO, |_, _| panic!("no call may be made")).unwrap_err();
        assert!(e.contains("grants.sql"), "{e}");
    }

    #[test]
    fn the_admin_env_carries_a_path_and_never_forwards_to_a_service() {
        let e = admin_env(&Admin { user: "root".into(), password: "pw".into(), host: Some("10.0.0.1".into()), port: 3310 }, Path::new("/p/admin"));
        let get = |k: &str| e.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
        assert_eq!(get("SPIRA_LC_USER").as_deref(), Some("root"));
        assert_eq!(get("SPIRA_LC_PASSWORD_FILE").as_deref(), Some("/p/admin"));
        assert_eq!(get("SPIRA_LC_PASSWORD").as_deref(), Some(""));
        assert_eq!(get("SPIRA_LC_PORT").as_deref(), Some("3310"));
        assert_eq!(get("SPIRA_LC_HOST").as_deref(), Some("10.0.0.1"));
        assert!(!e.iter().any(|(_, v)| v == "pw"));
    }
}
