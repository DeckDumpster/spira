//! `install` — the root installer (`install.sh`, in Rust). Nine phases: preflight, conflict
//! checks, config, build, database, units, hooks, cockpit, verify. Every tool this phase
//! sequence calls that has not itself moved to Rust yet (`doctor.sh`, `configure.sh`,
//! `build.sh`, `seed.sh`, `mail.sh`, `install-session-hook.sh`, `install-intake.sh`,
//! `exclude.sh`, `ready.sh`) is invoked by bare name on the launcher `PATH`, exactly as
//! `install.sh` did — none of them are this bead's scope.
//!
//! usage: install [<instance>] [--dry-run] [--ephemeral] [--laptop] [--skip-build]
//!                 [--no-session-hook] [--system-user]

use install::bootstrap::{self, nonempty_env};
use install::checks;
use install::install_units::{self, Ctx};
use install::systemctl::RealSystemctl;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

struct Opts {
    instance: Option<String>,
    dry: bool,
    ephemeral: bool,
    laptop: bool,
    skip_build: bool,
    no_session_hook: bool,
    system_user: bool,
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts { instance: None, dry: false, ephemeral: false, laptop: false, skip_build: false, no_session_hook: false, system_user: false };
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--dry-run" => o.dry = true,
            "--ephemeral" => o.ephemeral = true,
            "--laptop" => o.laptop = true,
            "--skip-build" => o.skip_build = true,
            "--no-session-hook" => o.no_session_hook = true,
            "--system-user" => o.system_user = true,
            s if s.starts_with("--") => return Err(format!("unknown flag: {s}")),
            s if o.instance.is_none() => o.instance = Some(s.to_string()),
            s => return Err(format!("extra argument: {s}")),
        }
    }
    Ok(o)
}

fn phase(n: &str) {
    println!("\n[{n}]");
}
fn info(s: &str) {
    println!("  {s}");
}
fn skip(s: &str) {
    println!("  already done: {s}");
}
fn would(s: &str) {
    println!("  would: {s}");
}

/// Run a bare-name tool, inheriting stdio, returning its exit code (127 on a spawn failure).
fn tool_status(name: &str, args: &[&str]) -> i32 {
    Command::new(name).args(args).status().map(|s| s.code().unwrap_or(1)).unwrap_or(127)
}

fn tool_output(name: &str, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String) {
    let mut c = Command::new(name);
    c.args(args);
    for (k, v) in extra_env {
        c.env(k, v);
    }
    match c.output() {
        Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (127, format!("cannot run {name}: {e}")),
    }
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(i) = &opts.instance {
        std::env::set_var("SPIRA_INSTANCE", i);
    }
    if opts.ephemeral && nonempty_env("SPIRA_INSTANCE").is_none() {
        std::env::set_var("SPIRA_INSTANCE", format!("eph-{}", std::process::id()));
    }
    if let Some(p) = nonempty_env("CONFIGURE_PROD") {
        std::env::set_var("SPIRA_PROD", p);
    }
    let instance = nonempty_env("SPIRA_INSTANCE").unwrap_or_else(|| "prod".into());

    // ---- phase 0: preflight -------------------------------------------------------------
    phase("phase 0: preflight");
    let (rc, out) = tool_output("doctor.sh", &[], &[("SPIRA_DOCTOR_INSTALLING", "1")]);
    for l in out.lines() {
        println!("  {l}");
    }
    if rc != 0 {
        eprintln!("\ninstall: preflight failed — see doctor output above");
        return ExitCode::from(1);
    }
    info("preflight passed");

    // ---- phase 0.5: conflicts -------------------------------------------------------------
    if nonempty_env("SPIRA_INSTALL_CONFLICT_CONSIDERED").is_none() {
        phase("phase 0.5: conflict checks");
        if let Err(c) = conflicts(&instance) {
            eprintln!("install: CONFLICT — {}", c.message);
            eprintln!("install:   remedy: {}", c.remedy);
            eprintln!("install:   override: SPIRA_INSTALL_CONFLICT_CONSIDERED=1");
            return ExitCode::from(5);
        }
        info("conflict checks clear");
    }

    if let Some(p) = nonempty_env("CONFIGURE_PROD") {
        let has_conf = Path::new(&p).join("conf.sh").is_file();
        if let Err(e) = install::guards::configure_prod_guard(&p, has_conf) {
            eprintln!("install: {e}");
            return ExitCode::from(1);
        }
    }

    // ---- phase 1: config --------------------------------------------------------------
    phase("phase 1: config");
    let conf_dest = nonempty_env("XDG_CONFIG_HOME").map(|x| format!("{x}/spira/spira.conf")).or_else(|| nonempty_env("HOME").map(|h| format!("{h}/.config/spira/spira.conf"))).unwrap_or_default();
    let mut changes = 0u32;
    if Path::new(&conf_dest).is_file() {
        skip(&format!("config exists at {conf_dest}"));
    } else if opts.dry {
        would("run: spira/configure.sh --out ...");
    } else {
        info("running configure.sh");
        if tool_status("configure.sh", &[]) != 0 {
            eprintln!("install: phase config failed — configure.sh exited non-zero");
            return ExitCode::from(2);
        }
        changes += 1;
    }

    // ---- phase 2: build -----------------------------------------------------------------
    phase("phase 2: build");
    if opts.skip_build || opts.ephemeral {
        skip("build skipped (--skip-build or --ephemeral)");
    } else if opts.dry {
        would("run: spira/build.sh");
    } else {
        info("running build.sh");
        if tool_status("build.sh", &[]) != 0 {
            eprintln!("install: phase build failed — build.sh exited non-zero");
            return ExitCode::from(2);
        }
    }

    // ---- phase 3: database --------------------------------------------------------------
    phase("phase 3: database");
    let db = nonempty_env("SPIRA_DB").unwrap_or_default();
    let dolt_data = nonempty_env("SPIRA_DOLT_DATA");
    {
        let own_remotes = git_remotes(&db.to_string());
        let ancestor_git = ancestor_git_dir(Path::new(&db).parent());
        if let Err(e) = install::guards::db_git_guard(&db, &own_remotes, ancestor_git.as_deref()) {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    }
    let mut seed_deferred = false;
    let mut db_fresh = false;
    let mut dolt_bg: Option<std::process::Child> = None;
    let mut dolt_port: u16 = 3307;
    if let Some(dd) = &dolt_data {
        if !Path::new(dd).is_dir() {
            if opts.dry {
                would(&format!("create Dolt data directory: {dd}"));
            } else {
                info(&format!("create Dolt data directory: {dd}"));
                let _ = std::fs::create_dir_all(dd);
                changes += 1;
            }
        } else {
            skip(&format!("Dolt data directory exists: {dd}"));
        }
        let yaml_dest = format!("{dd}/dolt-server.yaml");
        let yaml_tpl = format!("{}/dolt-server.yaml", bootstrap::templates_dir().display());
        if !Path::new(&yaml_dest).is_file() && Path::new(&yaml_tpl).is_file() {
            if opts.dry {
                would(&format!("write dolt-server.yaml to {yaml_dest}"));
            } else if let Ok(tpl) = std::fs::read_to_string(&yaml_tpl) {
                let _ = std::fs::write(&yaml_dest, tpl.replace("@SPIRA_DOLT_DATA@", dd));
                info(&format!("wrote dolt-server.yaml to {yaml_dest}"));
                changes += 1;
            }
        } else {
            skip(&format!("dolt-server.yaml already at {yaml_dest}"));
        }
        dolt_port = read_yaml_port(&yaml_dest).unwrap_or(3307);
    }

    if Path::new(&db).join(".beads").is_dir() {
        if let Ok(meta) = std::fs::read_to_string(Path::new(&db).join(".beads/metadata.json")) {
            if meta.contains("\"dolt_mode\"") && meta.contains("\"embedded\"") {
                eprintln!("install: phase database failed — existing store at {db} is embedded (one lock, all clients queue); set SPIRA_DOLT_DATA in spira.conf and re-run install");
                return ExitCode::from(2);
            }
        }
        skip(&format!("database exists at {db}"));
    } else {
        db_fresh = true;
        if opts.dry {
            if dolt_data.is_some() {
                would(&format!("start dolt server and run: (cd {db} && bd init --server --external)"));
            } else {
                would(&format!("run: (cd {db} && bd init)"));
            }
        } else {
            info(&format!("initialising database at {db}"));
            let _ = std::fs::create_dir_all(&db);
            if let Some(_dd) = &dolt_data {
                if which_prog("dolt").is_none() {
                    eprintln!("install: phase database failed — dolt is not on PATH — required to init the database");
                    return ExitCode::from(2);
                }
                if !tcp_up(dolt_port) {
                    info(&format!("starting dolt server on port {dolt_port} for database init"));
                    let yaml = format!("{}/dolt-server.yaml", dolt_data.clone().unwrap());
                    dolt_bg = Command::new("dolt").args(["sql-server", "--config", &yaml]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok();
                    if !wait_tcp(dolt_port, 30) {
                        if let Some(mut c) = dolt_bg.take() {
                            let _ = c.kill();
                        }
                        eprintln!("install: phase database failed — dolt server did not start on port {dolt_port} within 30s");
                        return ExitCode::from(2);
                    }
                }
                let ready_max: u64 = nonempty_env("SPIRA_INSTALL_DOLT_READY_WAIT").and_then(|v| v.parse().ok()).unwrap_or(30);
                if !wait_dolt_query(dolt_port, ready_max) {
                    if let Some(mut c) = dolt_bg.take() {
                        let _ = c.kill();
                    }
                    eprintln!("install: phase database failed — dolt server on port {dolt_port} did not answer queries within {ready_max}s");
                    return ExitCode::from(2);
                }
                let dbname = Path::new(&db).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let mut tries = 0;
                loop {
                    tries += 1;
                    let (rc, out) = bd_output(&db, &["init", "--non-interactive", "--prefix", "sp", "--skip-agents", "--skip-hooks", "--server", "--server-host", "127.0.0.1", "--server-port", &dolt_port.to_string(), "--database", &dbname, "--external", "-q"]);
                    if rc == 0 {
                        break;
                    }
                    if tries < 2 && out.contains("invalid connection") {
                        info("  bd init hit an invalid connection — removing the partial database and retrying");
                        let _ = std::fs::remove_dir_all(Path::new(&db).join(".beads"));
                        continue;
                    }
                    eprint!("{out}");
                    if let Some(mut c) = dolt_bg.take() {
                        let _ = c.kill();
                    }
                    eprintln!("install: phase database failed — bd init (server mode) failed");
                    return ExitCode::from(2);
                }
                let _ = Command::new("git").args(["-C", &db, "config", "beads.role", "maintainer"]).status();
            } else {
                if tool_status("bd", &["init"]) != 0 {
                    eprintln!("install: phase database failed — bd init failed");
                    return ExitCode::from(2);
                }
                let _ = Command::new("git").args(["-C", &db, "config", "beads.role", "maintainer"]).status();
            }
            changes += 1;
        }
    }

    if Path::new(&db).join(".beads").is_dir() || !opts.dry {
        let server_mode = dolt_data.is_some();
        let server_up = server_mode && tcp_up(dolt_port);
        if opts.dry {
            would("run: spira/seed.sh (skips statutes already in force)");
        } else if install::guards::seed_when(db_fresh, server_mode, server_up) == install::guards::SeedWhen::Defer {
            seed_deferred = true;
            info("seeding statutes after phase 4 — the database server is not running yet");
        } else {
            info("seeding statutes");
            let (rc, out) = tool_output("seed.sh", &[], &[]);
            print!("{out}");
            if let Some(mut c) = dolt_bg.take() {
                let _ = c.kill();
            }
            if dolt_data.is_some() {
                let close_wait: u64 = nonempty_env("SPIRA_INSTALL_DOLT_CLOSE_WAIT").and_then(|v| v.parse().ok()).unwrap_or(30);
                wait_tcp_down(dolt_port, close_wait);
            }
            if rc != 0 && db_fresh {
                eprintln!("install: phase database failed — seed.sh failed — statutes not seeded on fresh database");
                return ExitCode::from(2);
            }
        }
    }

    // ---- phase 4: units -------------------------------------------------------------------
    phase("phase 4: units");
    let prod = nonempty_env("SPIRA_PROD").unwrap_or_default();
    if nonempty_env("SPIRA_INSTALL_PROD_GIT_CONSIDERED").is_none() {
        let is_git = is_git_checkout(&prod);
        let releases = nonempty_env("SPIRA_RELEASES").unwrap_or_default();
        if let Err(e) = install::guards::prod_guard(&prod, is_git, false, &releases) {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    }
    if opts.dry {
        would("run: mail.sh ensure operator");
    } else if tool_status("mail.sh", &["ensure", "operator"]) != 0 {
        eprintln!("install: phase units failed — could not create the operator mailbox (mail.sh ensure operator)");
        return ExitCode::from(2);
    }

    let manifest = match bootstrap::manifest_from_env(&instance) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("install: {e}");
            return ExitCode::from(2);
        }
    };
    let host = bootstrap::host_from_env(&instance);
    let systemctl = RealSystemctl::from_env();

    if nonempty_env("SPIRA_INSTALL_FORCE").is_none() {
        if let Err(lines) = checks::preflight(&instance, &host, &systemctl) {
            for l in lines {
                eprintln!("install: {l}");
            }
            return ExitCode::from(2);
        }
    }

    if opts.dry {
        let dir = bootstrap::unit_dir();
        let templates_dir = bootstrap::templates_dir();
        let suspended = bootstrap::suspended_set();
        let no = |s: &str| suspended.contains(s);
        let ctx = Ctx { unit_dir: &dir, templates_dir: &templates_dir, host: &host, manifest: &manifest, systemctl: &systemctl, suspended: &no, world_halted: bootstrap::world_halted(), skip_migrate_watchers: false };
        match install_units::diff(&ctx) {
            Ok(lines) if lines.is_empty() => skip("all units match what would be rendered"),
            Ok(lines) => {
                for l in lines {
                    println!("  {l}");
                }
            }
            Err(e) => println!("  {e}"),
        }
    } else {
        info(&format!("installing units for instance '{instance}'"));
        let dir = bootstrap::unit_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Some(run) = nonempty_env("SPIRA_RUN") {
            let _ = std::fs::create_dir_all(&run);
            let _ = std::fs::create_dir_all(Path::new(&run).join("watchd"));
        }
        let templates_dir = bootstrap::templates_dir();
        let suspended = bootstrap::suspended_set();
        let no = |s: &str| suspended.contains(s);
        let ctx = Ctx { unit_dir: &dir, templates_dir: &templates_dir, host: &host, manifest: &manifest, systemctl: &systemctl, suspended: &no, world_halted: bootstrap::world_halted(), skip_migrate_watchers: false };
        let report = install_units::run(&ctx);
        for e in &report.errors {
            eprintln!("install: {e}");
        }
        if !report.errors.is_empty() {
            eprintln!("install: phase units failed");
            return ExitCode::from(2);
        }
        for u in &report.written {
            println!("installed {u}");
        }
        changes += 1;
        if let Some(dd) = &dolt_data {
            let bp = read_yaml_port(&format!("{dd}/dolt-server.yaml")).unwrap_or(3307);
            info(&format!("waiting for dolt-beads.service on port {bp}"));
            let pwt: u64 = nonempty_env("SPIRA_INSTALL_DOLT_WAIT").and_then(|v| v.parse().ok()).unwrap_or(60);
            if !wait_tcp(bp, pwt) {
                eprintln!("install: phase units failed — dolt-beads.service is active but not listening on {bp} after {pwt}s");
                return ExitCode::from(2);
            }
            info(&format!("dolt-beads.service listening on port {bp}"));
            let _ = Command::new("bd").args(["-C", &db, "doctor", "--fix", "--yes"]).env("BD_NON_INTERACTIVE", "1").status();
            let db_max: u64 = nonempty_env("SPIRA_INSTALL_DB_WAIT").and_then(|v| v.parse().ok()).unwrap_or(30);
            if !wait_bd_list(&db, db_max) {
                eprintln!("install: phase units failed — bd did not accept connections within {db_max}s after dolt-beads.service started");
                return ExitCode::from(2);
            }
            info("bd store accepting connections");
        }
    }

    if seed_deferred {
        info("seeding statutes (deferred from phase 3 — the database server is up now)");
        let (rc, out) = tool_output("seed.sh", &[], &[]);
        print!("{out}");
        if rc != 0 {
            eprintln!("install: phase units failed — seed.sh failed with the database server running");
            return ExitCode::from(2);
        }
    }

    // Linger.
    let linger_user = nonempty_env("USER").unwrap_or_else(whoami);
    let cur_linger = Command::new("loginctl").args(["show-user", &linger_user, "-p", "Linger"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if cur_linger == "Linger=yes" {
        skip(&format!("linger already enabled for {linger_user}"));
    } else if opts.dry {
        would(&format!("run: loginctl enable-linger {linger_user}"));
    } else {
        let _ = Command::new("loginctl").args(["enable-linger", &linger_user]).status();
        if let Some(run) = nonempty_env("SPIRA_RUN") {
            let _ = std::fs::create_dir_all(&run);
            let _ = std::fs::write(Path::new(&run).join("install-linger-enabled"), "");
        }
        info(&format!("enabled linger for {linger_user}"));
        changes += 1;
    }

    // ---- phase 5: hooks -------------------------------------------------------------------
    phase("phase 5: hooks");
    let repo = nonempty_env("SPIRA_REPO").unwrap_or_default();
    if Path::new(&repo).join(".git").is_dir() || Path::new(&repo).join(".git").is_file() {
        if opts.dry {
            would(&format!("run: spira/exclude.sh install {repo}"));
        } else {
            let (rc, out) = tool_output("exclude.sh", &["install", &repo], &[]);
            print!("{out}");
            if rc != 0 {
                eprintln!("install: phase hooks failed — exclude.sh install failed — core.hooksPath not set");
                return ExitCode::from(2);
            }
        }
    } else {
        skip(&format!("no git checkout at {repo} — no commit hooks to arm"));
    }

    if opts.ephemeral || opts.no_session_hook {
        skip("session hook skipped (--ephemeral or --no-session-hook)");
    } else if opts.dry {
        would("run: spira/install-session-hook.sh install");
    } else {
        info("installing session hook");
        if tool_status("install-session-hook.sh", &["install"]) != 0 {
            eprintln!("install: phase hooks failed — install-session-hook.sh failed");
            return ExitCode::from(2);
        }
        changes += 1;
    }

    if let Some(glob) = nonempty_env("SPIRA_ALERT_GLOB") {
        if opts.ephemeral {
            skip("alert intake skipped (--ephemeral)");
        } else if opts.dry {
            would(&format!("run: spira/install-intake.sh install (SPIRA_ALERT_GLOB={glob})"));
        } else {
            info(&format!("wiring alert intake for SPIRA_ALERT_GLOB={glob}"));
            let (_, out) = tool_output("install-intake.sh", &["install"], &[]);
            print!("{out}");
        }
    } else {
        skip("alert intake skipped (SPIRA_ALERT_GLOB not set or --ephemeral)");
    }

    // ---- phase 6: cockpit -------------------------------------------------------------------
    phase("phase 6: cockpit");
    let home = nonempty_env("SPIRA_HOME").unwrap_or_default();
    let cockpit_dir = Path::new(&home).parent().map(|p| p.join("cockpit")).unwrap_or_default();
    let bin_dir = nonempty_env("HOME").map(|h| PathBuf::from(h).join(".local/bin")).unwrap_or_default();
    for prog in ["cockpit", "cockpit-remote"] {
        let src = cockpit_dir.join(prog);
        let link = bin_dir.join(prog);
        if !src.is_file() {
            skip(&format!("{prog} not found at {} — skipping", src.display()));
            continue;
        }
        if std::fs::read_link(&link).map(|t| t == src).unwrap_or(false) {
            skip(&format!("{} -> {}", link.display(), src.display()));
            continue;
        }
        if opts.dry {
            would(&format!("link: {} -> {}", link.display(), src.display()));
        } else {
            let _ = std::fs::create_dir_all(&bin_dir);
            let _ = std::fs::remove_file(&link);
            if std::os::unix::fs::symlink(&src, &link).is_ok() {
                info(&format!("linked {} -> {}", link.display(), src.display()));
                changes += 1;
            }
        }
    }
    if which_prog("tmux").is_some() && Command::new("tmux").args(["list-panes", "-a"]).output().map(|o| o.status.success()).unwrap_or(false) {
        let panel = Command::new("tmux").args(["list-panes", "-a", "-F", "#{@cockpit}"]).output().map(|o| String::from_utf8_lossy(&o.stdout).lines().filter(|l| *l == "panel").count()).unwrap_or(0);
        if panel > 0 {
            skip("cockpit panes already present");
        } else if opts.dry {
            would("run: cockpit/layout.sh up");
        } else {
            info("building cockpit panes");
            let _ = Command::new(cockpit_dir.join("layout.sh")).arg("up").status();
            changes += 1;
        }
    } else {
        info("tmux server not reachable — build the cockpit when ready:");
        info(&format!("  {repo}/cockpit/layout.sh up"));
    }

    // ---- phase 6.5: spira-lc system user --------------------------------------------------
    phase("phase 6.5: spira-lc system user");
    if !opts.system_user {
        info("not requested (--system-user) — spira-lc runs in same-user fallback");
    } else if !is_root() {
        info("--system-user requires root — spira-lc runs in same-user fallback");
    } else if opts.dry {
        would("would create the spira-lc system user, group and install spira-lc.service/.socket");
    } else if let Err(e) = system_user_phase(&home, &repo) {
        eprintln!("install: phase spira-lc failed — {e}");
        return ExitCode::from(2);
    }

    // ---- phase 7: verify --------------------------------------------------------------------
    phase("phase 7: verify");
    if opts.dry {
        if changes > 0 {
            info(&format!("dry-run complete — {changes} change(s) would be made"));
        } else {
            info("dry-run complete — no changes needed");
        }
        info("running ready.sh in read-only mode to show current state");
        let (_, out) = tool_output("ready.sh", &[], &[]);
        for l in out.lines() {
            println!("  {l}");
        }
        return ExitCode::SUCCESS;
    }

    println!();
    let ready_rc = tool_status("ready.sh", &[]);
    if ready_rc == 0 {
        println!("\ninstall: done — Spira is ready.");
        ExitCode::SUCCESS
    } else {
        eprintln!("\ninstall: installation complete but ready.sh exited {ready_rc} — installed but NOT ready");
        eprintln!("install: see the output above for what is preventing the loop from receiving work");
        ExitCode::from(3)
    }
}

fn whoami() -> String {
    Command::new("id").arg("-un").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn which_prog(p: &str) -> Option<String> {
    bootstrap::which(p)
}

fn is_root() -> bool {
    Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0").unwrap_or(false)
}

fn is_git_checkout(mut path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    loop {
        if Path::new(path).join(".git").exists() {
            return true;
        }
        let Some(parent) = Path::new(path).parent() else { return false };
        if parent.as_os_str().is_empty() || parent == Path::new("/") {
            return false;
        }
        path = parent.to_str().unwrap_or("");
        if path.is_empty() {
            return false;
        }
    }
}

fn git_remotes(db: &str) -> Vec<String> {
    if !Path::new(db).join(".git").exists() {
        return Vec::new();
    }
    Command::new("git").args(["-C", db, "remote"]).output().map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect()).unwrap_or_default()
}

fn ancestor_git_dir(mut dir: Option<&Path>) -> Option<String> {
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d.to_string_lossy().to_string());
        }
        dir = d.parent();
    }
    None
}

fn read_yaml_port(path: &str) -> Option<u16> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("port:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn tcp_up(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn wait_tcp(port: u16, max_secs: u64) -> bool {
    let start = std::time::Instant::now();
    loop {
        if tcp_up(port) {
            return true;
        }
        if start.elapsed().as_secs() >= max_secs {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn wait_tcp_down(port: u16, max_secs: u64) {
    let start = std::time::Instant::now();
    while tcp_up(port) && start.elapsed().as_secs() < max_secs {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn wait_dolt_query(port: u16, max_secs: u64) -> bool {
    let start = std::time::Instant::now();
    loop {
        let ok = Command::new("dolt").args(["--host", "127.0.0.1", "--port", &port.to_string(), "--no-tls", "--user", "root", "--password", "", "sql", "-q", "select 1"]).stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            return true;
        }
        if start.elapsed().as_secs() >= max_secs {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn wait_bd_list(db: &str, max_secs: u64) -> bool {
    let start = std::time::Instant::now();
    loop {
        let ok = Command::new("bd").args(["-C", db, "list", "--json"]).env("BD_NON_INTERACTIVE", "1").output().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            return true;
        }
        if start.elapsed().as_secs() >= max_secs {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn bd_output(db: &str, args: &[&str]) -> (i32, String) {
    let mut c = Command::new("bd");
    c.arg("-C").arg(db).args(args).env("BD_NON_INTERACTIVE", "1");
    match c.output() {
        Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (127, format!("cannot run bd: {e}")),
    }
}

/// Conflict checks 1–5 (root `install.sh` phase 0.5), resolved with real `/proc`, `flock` and
/// a TCP probe, decided by [`install::guards`].
fn conflicts(instance: &str) -> Result<(), install::guards::Conflict> {
    use install::guards::*;
    let home = nonempty_env("SPIRA_HOME").unwrap_or_default();
    let unit_dir = bootstrap::unit_dir();
    let our_dir = Path::new(&home).canonicalize().map(|p| p.to_string_lossy().to_string()).unwrap_or(home.clone());

    // Conflict 1: a foreign harness copy.
    let sentinel_file = unit_dir.join(format!("spira-sentinel-{instance}.service"));
    let installed_exec_dir = std::fs::read_to_string(&sentinel_file).ok().and_then(|t| {
        t.lines().find_map(|l| l.strip_prefix("ExecStart=")).map(|v| v.split_whitespace().next().unwrap_or("").to_string()).and_then(|exe| Path::new(&exe).parent().map(|p| p.to_string_lossy().to_string())).and_then(|d| Path::new(&d).canonicalize().ok()).map(|p| p.to_string_lossy().to_string())
    });
    conflict_foreign(instance, installed_exec_dir.as_deref(), &our_dir)?;

    // Conflict 2: a live aeon under this installation.
    let cmdlines = proc_cmdlines();
    conflict_aeon(&home, cmdlines.iter().map(|(p, c)| (p.as_str(), c.as_str())))?;

    // Conflict 3: the landing gate's tree lock.
    let repo_base = nonempty_env("SPIRA_REPO").map(|r| Path::new(&r).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).unwrap_or_default();
    let run = nonempty_env("SPIRA_RUN").unwrap_or_default();
    let lock = format!("{run}/worktree/.gate.{repo_base}.lock");
    let held = Path::new(&lock).is_file() && !flock_free(&lock);
    conflict_lock(&lock, held)?;

    // Conflict 4: instance argument / run-dir collision.
    let conf_file = nonempty_env("SPIRA_CONF_FILE").unwrap_or_default();
    let conf_instance = std::fs::read_to_string(&conf_file).ok().and_then(|t| t.lines().find_map(|l| l.trim_start().strip_prefix("SPIRA_INSTANCE").map(str::to_string))).and_then(|l| l.split('=').nth(1).map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string()));
    conflict_instance_arg(instance, conf_instance.as_deref(), &conf_file)?;
    let our_unit = format!("spira-sentinel-{instance}.service");
    let mut others = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&unit_dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("spira-sentinel-") && n.ends_with(".service") && n != our_unit {
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    if let Some(rd_line) = t.lines().find_map(|l| l.strip_prefix("StandardOutput=append:").or_else(|| l.strip_prefix("StandardError=append:"))) {
                        if let Some(dir) = Path::new(rd_line).parent() {
                            let real = dir.canonicalize().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| dir.to_string_lossy().to_string());
                            others.push((n.clone(), real));
                        }
                    }
                }
            }
        }
    }
    let run_real = Path::new(&run).canonicalize().map(|p| p.to_string_lossy().to_string()).unwrap_or(run.clone());
    conflict_instance_run(&run_real, &our_unit, others.iter().map(|(a, b)| (a.as_str(), b.as_str())))?;

    // Conflict 5: a Dolt port collision.
    let dolt_data = nonempty_env("SPIRA_DOLT_DATA").unwrap_or_default();
    let port = read_yaml_port(&format!("{dolt_data}/dolt-server.yaml")).unwrap_or(3307);
    let listening = !dolt_data.is_empty() && tcp_up(port);
    let servers = dolt_servers();
    conflict_dolt(&dolt_data, port, listening, servers.iter().map(|(a, b)| (a.as_str(), b.as_str())))?;

    Ok(())
}

fn proc_cmdlines() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(cmd) = std::fs::read(e.path().join("cmdline")) {
            out.push((name, String::from_utf8_lossy(&cmd).to_string()));
        }
    }
    out
}

fn flock_free(path: &str) -> bool {
    Command::new("flock").arg("-n").arg(path).arg("true").status().map(|s| s.success()).unwrap_or(true)
}

fn dolt_servers() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (_, cmd) in proc_cmdlines() {
        let args: Vec<&str> = cmd.split('\0').filter(|s| !s.is_empty()).collect();
        if !args.iter().any(|a| a.contains("sql-server")) {
            continue;
        }
        let mut data_dir = None;
        if let Some(i) = args.iter().position(|a| *a == "--data-dir") {
            data_dir = args.get(i + 1).map(|s| s.to_string());
        } else if let Some(i) = args.iter().position(|a| *a == "--config") {
            if let Some(cfg) = args.get(i + 1) {
                if let Ok(text) = std::fs::read_to_string(cfg) {
                    for l in text.lines() {
                        if let Some(v) = l.trim_start().strip_prefix("data_dir:") {
                            data_dir = Some(v.trim().trim_matches('"').to_string());
                        }
                    }
                }
            }
        }
        if let Some(d) = data_dir {
            let real = Path::new(&d).canonicalize().map(|p| p.to_string_lossy().to_string()).unwrap_or(d);
            out.push((cmd.clone(), real));
        }
    }
    out
}

/// Phase 6.5 (design §3.6.4): the one root-requiring step — a Unix user of its own for the
/// spira_lifecycle credential. Refuses rather than half-installing.
fn system_user_phase(home: &str, repo: &str) -> Result<(), String> {
    let user = nonempty_env("SPIRA_LC_UNIX_USER").unwrap_or_else(|| "spira-lc".into());
    let group = nonempty_env("SPIRA_LC_UNIX_GROUP").unwrap_or_else(|| "spira".into());

    if Command::new("getent").arg("group").arg(&group).status().map(|s| !s.success()).unwrap_or(true) {
        info(&format!("create group {group}"));
        run_ok("groupadd", &["--system", &group])?;
    }
    if Command::new("id").arg("-u").arg(&user).status().map(|s| !s.success()).unwrap_or(true) {
        info(&format!("create user {user} (system, no login, no home)"));
        run_ok("useradd", &["--system", "--no-create-home", "--shell", "/usr/sbin/nologin", "--gid", &group, &user])?;
    }

    let cred_dir = "/etc/spira-lc";
    let cred_file = format!("{cred_dir}/credential");
    if !Path::new(&cred_file).is_file() {
        info(&format!("create {cred_dir}"));
        run_ok("install", &["-d", "-m", "0750", "-o", &user, "-g", &group, cred_dir])?;
        write_random_credential(&cred_file, 0o600, &user, &group)?;
    } else {
        skip(&format!("{cred_file} already exists"));
    }
    let ro_file = format!("{cred_dir}/credential-ro");
    if !Path::new(&ro_file).is_file() {
        write_random_credential(&ro_file, 0o644, &user, &group)?;
    } else {
        skip(&format!("{ro_file} already exists"));
    }

    let host_values = install::values::HostValues { home: home.to_string(), repo: repo.to_string(), run: nonempty_env("SPIRA_RUN").unwrap_or_default(), ..Default::default() };
    for unit in ["spira-lc.service", "spira-lc.socket"] {
        let text = install::values::render_file(&Path::new(home).join("systemd").join(unit), &host_values, None)?;
        let dest = format!("/etc/systemd/system/{unit}");
        std::fs::write(format!("{dest}.tmp"), text).map_err(|e| e.to_string())?;
        run_ok("install", &["-m", "0644", &format!("{dest}.tmp"), &dest])?;
        let _ = std::fs::remove_file(format!("{dest}.tmp"));
        info(&format!("install {dest}"));
    }
    run_ok("systemctl", &["daemon-reload"])?;
    info("reload systemd");
    run_ok("systemctl", &["enable", "--now", "spira-lc.socket"])?;
    info("enable spira-lc.socket (spira-lc.service itself is socket-activated, not enabled directly)");
    Ok(())
}

fn run_ok(prog: &str, args: &[&str]) -> Result<(), String> {
    Command::new(prog).args(args).status().map_err(|e| format!("cannot run {prog}: {e}")).and_then(|s| if s.success() { Ok(()) } else { Err(format!("{prog} {} failed ({s})", args.join(" "))) })
}

fn write_random_credential(path: &str, mode: u32, user: &str, group: &str) -> Result<(), String> {
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf)).map_err(|e| e.to_string())?;
    let encoded = base64_no_pad(&buf);
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(path).map_err(|e| e.to_string())?;
    f.write_all(encoded.as_bytes()).map_err(|e| e.to_string())?;
    run_ok("chown", &[&format!("{user}:{group}"), path])?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    Ok(())
}

fn base64_no_pad(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[(n >> 18 & 63) as usize] as char);
        out.push(CHARS[(n >> 12 & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[(n >> 6 & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(CHARS[(n & 63) as usize] as char);
        }
    }
    out
}
