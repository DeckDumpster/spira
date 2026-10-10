//! The lifecycle-store phase (sp-xfqnr): build the `spira_lifecycle` database a fresh
//! same-user install never got — `lifecycle/schema.sql`, then `lifecycle/migrations/*.sql`,
//! then `lifecycle/grants.sql` with the two same-user credentials phase 1.5 wrote — so an
//! aeon can claim and submit, and `spira-lc history` answers, the moment install finishes.
//! Two callers run it: `spira-install` (re-exported as `install::lifecycle_store`) against
//! the operator's Dolt, and `release stage up` (sp-880u4) against the stage's own private
//! sql-server, so a stage's store is built exactly the way a real one is.
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
        // admin-migrate applies a pending migration only as the admin these name (sp-p1z81);
        // the password stays in the file, out of every environment.
        ("SPIRA_LC_ADMIN_USER".to_string(), admin.user.clone()),
        ("SPIRA_LC_ADMIN_PASSWORD_FILE".to_string(), password_file.to_string_lossy().to_string()),
        ("SPIRA_LC_ADMIN_PASSWORD".to_string(), String::new()),
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

/// A fresh random credential: 32 bytes of the OS generator, base64 without padding — the
/// alphabet cannot break out of a single-quoted SQL literal.
pub fn random_secret() -> Result<String, String> {
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf)).map_err(|e| format!("cannot read /dev/urandom: {e}"))?;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    Ok(buf.chunks(3).flat_map(|c| {
        let n = c.iter().enumerate().fold(0u32, |a, (i, b)| a | (*b as u32) << (16 - 8 * i));
        (0..=c.len()).map(move |i| CHARS[(n >> (18 - 6 * i) & 63) as usize] as char)
    }).collect())
}

/// Close passwordless root: leave `root` with the password in `cred_path` and return the
/// admin to use. A password already in force (the file's) is kept. Otherwise the file is
/// written 0600 FIRST — a crash between the file and the `ALTER USER` leaves a password
/// the next run still finds — then root is moved off its empty password. A server where
/// root has neither the file's password nor an empty one is refused, never guessed at.
pub fn ensure_admin(
    cred_path: &Path,
    tmp_parent: &Path,
    host: Option<String>,
    port: u16,
    mut run: impl FnMut(&[String], &[(String, String)]) -> (i32, String),
) -> Result<(Admin, String), String> {
    let dir = PrivateDir::new(tmp_parent, "adm")?;
    let probe = dir.write("probe.sql", "SELECT 1;\n")?;
    let probe_args = vec!["admin-apply-ddl".to_string(), probe.to_string_lossy().to_string()];
    let try_as = |pw: &str, run: &mut dyn FnMut(&[String], &[(String, String)]) -> (i32, String), file: &str, sql: &[String]| -> Result<bool, String> {
        let admin = Admin { user: "root".into(), password: pw.to_string(), host: host.clone(), port };
        let pw_file = dir.write(file, pw)?;
        Ok(run(sql, &admin_env(&admin, &pw_file)).0 == 0)
    };
    let existing = match std::fs::read_to_string(cred_path) {
        Ok(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read the admin credential {}: {e}", cred_path.display())),
    };
    let make = |pw: String| Admin { user: "root".into(), password: pw, host: host.clone(), port };
    if let Some(pw) = &existing {
        if try_as(pw, &mut run, "pw-existing", &probe_args)? {
            return Ok((make(pw.clone()), format!("admin credential {} already in force", cred_path.display())));
        }
    }
    let pw = match existing {
        Some(p) => p,
        None => {
            let p = random_secret()?;
            if let Some(parent) = cred_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(cred_path).map_err(|e| format!("cannot create {}: {e}", cred_path.display()))?;
            f.write_all(p.as_bytes()).map_err(|e| format!("cannot write {}: {e}", cred_path.display()))?;
            p
        }
    };
    let alter = dir.write("alter.sql", &format!("ALTER USER 'root'@'localhost' IDENTIFIED BY '{pw}';\n"))?;
    let alter_args = vec!["admin-apply-ddl".to_string(), alter.to_string_lossy().to_string()];
    if !try_as("", &mut run, "pw-empty", &alter_args)? {
        return Err(format!("root accepts neither the password in {} nor an empty one — set it by hand (ALTER USER 'root'@'localhost' IDENTIFIED BY <that file's content>)", cred_path.display()));
    }
    if !try_as(&pw, &mut run, "pw-new", &probe_args)? {
        return Err(format!("root was given the password in {} but does not accept it", cred_path.display()));
    }
    Ok((make(pw), format!("root's password set; credential written to {} (0600)", cred_path.display())))
}

pub const BEADS_USER: &str = "beads";

/// SQL giving `BEADS_USER` the password and every privilege on the beads database `db` and
/// nothing else. `db` is quoted into a backtick identifier, so only a plain name is accepted.
pub fn beads_user_sql(db: &str, password: &str) -> Result<String, String> {
    if db.is_empty() || !db.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("the beads database name {db:?} is not a plain identifier — refusing to place it in a GRANT"));
    }
    if password.is_empty() || password.chars().any(|c| c == '\'' || c == '\\' || c == '"' || c.is_control()) {
        return Err("the beads user's password is empty or carries a quote, backslash or control character".into());
    }
    let u = BEADS_USER;
    Ok(format!("CREATE USER IF NOT EXISTS '{u}'@'%' IDENTIFIED BY '{password}';\nALTER USER '{u}'@'%' IDENTIFIED BY '{password}';\nGRANT ALL ON `{db}`.* TO '{u}'@'%';\n"))
}

/// The password already in `file`'s `[host:port]` section, if any.
pub fn read_beads_credential(file: &Path, host: &str, port: u16) -> Option<String> {
    let header = format!("[{host}:{port}]");
    let mut in_section = false;
    for line in std::fs::read_to_string(file).ok()?.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_section = t == header;
        } else if in_section {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == "password" && !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// Point a beads database's `metadata.json` at `BEADS_USER` (`dolt_server_user`), keeping every other key.
pub fn set_beads_user(metadata: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(metadata).map_err(|e| format!("cannot read {}: {e}", metadata.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", metadata.display()))?;
    let obj = v.as_object_mut().ok_or_else(|| format!("{} is not a JSON object", metadata.display()))?;
    if obj.get("dolt_server_user").and_then(|u| u.as_str()) == Some(BEADS_USER) {
        return Ok(format!("{} already names {BEADS_USER}", metadata.display()));
    }
    obj.insert("dolt_server_user".into(), serde_json::Value::String(BEADS_USER.into()));
    let tmp = metadata.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).map_err(|e| e.to_string())? + "\n").map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, metadata).map_err(|e| format!("cannot replace {}: {e}", metadata.display()))?;
    Ok(format!("{} now names {BEADS_USER}", metadata.display()))
}

/// Give `bd` its own Dolt user instead of root: created as `admin` with grants on `db` alone,
/// its password kept in the beads credentials file (`ensure_beads_credential`), and the
/// database's `metadata.json` pointed at it. A password already in the file's section is
/// reused unless it is the admin's own; re-running converges.
pub fn ensure_beads_user(
    file: &Path,
    metadata: &Path,
    db: &str,
    admin: &Admin,
    tmp_parent: &Path,
    mut run: impl FnMut(&[String], &[(String, String)]) -> (i32, String),
) -> Result<Vec<String>, String> {
    let host = admin.host.clone().unwrap_or_else(|| "127.0.0.1".into());
    let password = match read_beads_credential(file, &host, admin.port) {
        Some(p) if p != admin.password => p,
        _ => random_secret()?,
    };
    let sql = beads_user_sql(db, &password)?;
    let dir = PrivateDir::new(tmp_parent, "bdu")?;
    let pw_file = dir.write("admin", &admin.password)?;
    let sql_file = dir.write("beads-user.sql", &sql)?;
    let args = vec!["admin-apply-ddl".to_string(), sql_file.to_string_lossy().to_string()];
    let (rc, out) = run(&args, &admin_env(admin, &pw_file));
    if rc != 0 {
        let out = redact(out.trim_end(), &[&password, &admin.password]);
        return Err(format!("creating the {BEADS_USER} user as {} failed (spira-lc exit {rc}){}", admin.user, if out.is_empty() { String::new() } else { format!(": {out}") }));
    }
    let mut lines = vec![format!("{BEADS_USER} user granted on {db} only")];
    lines.push(ensure_beads_credential(file, &host, admin.port, &password)?);
    lines.push(set_beads_user(metadata)?);
    Ok(lines)
}

/// The beads credentials file (`$BEADS_CREDENTIALS_FILE`, else `~/.config/beads/credentials` —
/// bd's own password lookup after `BEADS_DOLT_PASSWORD`) carries a password, 0600, in the
/// `[host:port]` section bd resolves. Without it, closing passwordless root locked every bd
/// client out ("Access denied for user"). Any other section is kept; this one is replaced.
pub fn ensure_beads_credential(file: &Path, host: &str, port: u16, password: &str) -> Result<String, String> {
    let header = format!("[{host}:{port}]");
    let old = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let mut out = String::new();
    let mut skipping = false;
    for line in old.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            skipping = t == header;
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(&format!("{header}\npassword = {password}\n"));
    if out == old {
        return Ok(format!("bd credential for {host}:{port} already in {}", file.display()));
    }
    if let Some(parent) = file.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let tmp = file.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    f.write_all(out.as_bytes()).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("cannot replace {}: {e}", file.display()))?;
    Ok(format!("bd credential for {host}:{port} written to {} (0600)", file.display()))
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

    #[test]
    fn the_beads_credential_section_is_written_0600_and_replaced_not_duplicated() {
        use std::os::unix::fs::PermissionsExt;
        let d = testkit::TempDir::new("lc-beads-cred");
        let f = d.path().join("beads/credentials");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "[10.0.0.1:3307]\npassword = other\n[127.0.0.1:3307]\npassword = old\n").unwrap();
        ensure_beads_credential(&f, "127.0.0.1", 3307, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "[10.0.0.1:3307]\npassword = other\n[127.0.0.1:3307]\npassword = new\n");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        let msg = ensure_beads_credential(&f, "127.0.0.1", 3307, "new").unwrap();
        assert!(msg.contains("already"), "{msg}");
    }


    #[test]
    fn beads_user_sql_grants_the_one_database_and_refuses_a_hostile_name() {
        let sql = beads_user_sql("beads_db", "pw").unwrap();
        assert!(sql.contains("GRANT ALL ON `beads_db`.* TO 'beads'@'%'"), "{sql}");
        assert!(!sql.contains("*.*"), "{sql}");
        assert!(beads_user_sql("a`; DROP", "pw").is_err());
        assert!(beads_user_sql("db", "p'w").is_err());
    }

    #[test]
    fn ensure_beads_user_connects_bd_as_beads_not_root() {
        let d = testkit::TempDir::new("lc-beads-user");
        let cred = d.path().join("beads/credentials");
        let meta = d.path().join("metadata.json");
        std::fs::write(&meta, "{\"dolt_database\":\"spira\",\"dolt_mode\":\"server\"}").unwrap();
        let admin = Admin { user: "root".into(), password: "rootpw".into(), host: None, port: 3307 };
        let mut sql_seen = String::new();
        let lines = ensure_beads_user(&cred, &meta, "spira", &admin, d.path(), |args, _| {
            sql_seen = std::fs::read_to_string(&args[1]).unwrap();
            (0, String::new())
        }).unwrap();
        assert!(sql_seen.contains("GRANT ALL ON `spira`.*"), "{sql_seen}");
        let pw = read_beads_credential(&cred, "127.0.0.1", 3307).unwrap();
        assert_ne!(pw, "rootpw");
        assert!(sql_seen.contains(&pw));
        assert!(!lines.join("\n").contains(&pw));
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&meta).unwrap()).unwrap();
        assert_eq!(m["dolt_server_user"], "beads");
        assert_eq!(m["dolt_database"], "spira");
        ensure_beads_user(&cred, &meta, "spira", &admin, d.path(), |_, _| (0, String::new())).unwrap();
        assert_eq!(read_beads_credential(&cred, "127.0.0.1", 3307).unwrap(), pw);
        let failed = ensure_beads_user(&cred, &meta, "spira", &admin, d.path(), |_, _| (1, format!("bad {pw}")));
        assert!(!failed.unwrap_err().contains(&pw));
    }

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

    /// A fake server whose root holds `root_pw` and which obeys an ALTER USER sql file.
    fn fake_root(root_pw: &std::cell::RefCell<String>) -> impl FnMut(&[String], &[(String, String)]) -> (i32, String) + '_ {
        move |args, env| {
            let pw = std::fs::read_to_string(&env.iter().find(|(k, _)| k == "SPIRA_LC_PASSWORD_FILE").unwrap().1).unwrap();
            if pw != *root_pw.borrow() {
                return (2, "Access denied".into());
            }
            let sql = std::fs::read_to_string(&args[1]).unwrap();
            if let Some(rest) = sql.split("IDENTIFIED BY '").nth(1) {
                *root_pw.borrow_mut() = rest.split('\'').next().unwrap().to_string();
            }
            (0, String::new())
        }
    }

    #[test]
    fn passwordless_root_gets_a_0600_credential_and_the_password_is_in_force() {
        let t = testkit::TempDir::new("lc-ensure-admin");
        let tmp = t.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let cred = t.path().join("cfg/spira-lc-admin.credential");
        let root = std::cell::RefCell::new(String::new());
        let (admin, _) = ensure_admin(&cred, &tmp, None, 3307, fake_root(&root)).unwrap();
        let written = std::fs::read_to_string(&cred).unwrap();
        assert_eq!(std::fs::metadata(&cred).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(written.len() >= 40 && !written.contains('\''), "{written}");
        assert_eq!(*root.borrow(), written, "root is no longer passwordless");
        assert_eq!(admin.password, written);
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0, "no temp files left");
        let (_, msg) = ensure_admin(&cred, &tmp, None, 3307, fake_root(&root)).unwrap();
        assert!(msg.contains("already in force"), "{msg}");
        assert_eq!(std::fs::read_to_string(&cred).unwrap(), written, "a rerun never rotates it");
    }

    #[test]
    fn a_crash_between_the_file_and_the_alter_is_finished_by_the_next_run() {
        let t = testkit::TempDir::new("lc-ensure-crash");
        let tmp = t.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let cred = t.path().join("admin.credential");
        std::fs::write(&cred, "leftoverPw\n").unwrap();
        let root = std::cell::RefCell::new(String::new());
        ensure_admin(&cred, &tmp, None, 3307, fake_root(&root)).unwrap();
        assert_eq!(*root.borrow(), "leftoverPw");
    }

    #[test]
    fn a_root_that_accepts_neither_the_file_nor_empty_is_refused() {
        let t = testkit::TempDir::new("lc-ensure-refuse");
        let tmp = t.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let cred = t.path().join("admin.credential");
        let root = std::cell::RefCell::new("someone-elses".to_string());
        let e = ensure_admin(&cred, &tmp, None, 3307, fake_root(&root)).unwrap_err();
        assert!(e.contains("neither"), "{e}");
        assert_eq!(*root.borrow(), "someone-elses");
    }

    #[test]
    fn random_secrets_are_distinct_and_safe_inside_a_sql_literal() {
        let (a, b) = (random_secret().unwrap(), random_secret().unwrap());
        assert_ne!(a, b);
        assert!(a.len() >= 40 && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{a}");
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
        // admin-migrate applies a pending migration as the admin these name (sp-p1z81).
        assert_eq!(get("SPIRA_LC_ADMIN_USER").as_deref(), Some("root"));
        assert_eq!(get("SPIRA_LC_ADMIN_PASSWORD_FILE").as_deref(), Some("/p/admin"));
        assert_eq!(get("SPIRA_LC_ADMIN_PASSWORD").as_deref(), Some(""));
        assert_eq!(get("SPIRA_LC_PORT").as_deref(), Some("3310"));
        assert_eq!(get("SPIRA_LC_HOST").as_deref(), Some("10.0.0.1"));
        assert!(!e.iter().any(|(_, v)| v == "pw"));
    }
}
