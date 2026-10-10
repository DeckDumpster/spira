//! `manifest`, `units`, `keys`, `exec`, `status`, `drain`/`peek`, `tailers`, `restart`,
//! `prune`, `health-ids`, `health-view` — every subcommand except `tail` (its own module,
//! `tail.rs`, for the streaming/locking machinery) and `notify` (`notify.rs`, for the
//! two-half escalation logic).

use crate::context::Context;
use crate::cursor;
use crate::filter::Filter;
use crate::fs_ops;
use crate::health::{self, Health};
use crate::manifest::{Kind, Row};
use crate::ops::Ops;
use crate::paths;
use crate::rows::{self, RowsError};
use std::path::PathBuf;

/// Loads the manifest and prints the same refusal text the bash did on failure. Every
/// command that needs the manifest goes through this so the refusal wording cannot drift
/// between subcommands.
pub fn load_rows_or_report(ctx: &Context) -> Result<Vec<Row>, i32> {
    let watchers = PathBuf::from(&ctx.watchers);
    let overlay = PathBuf::from(&ctx.watchers_overlay);
    match rows::load(&watchers, &overlay, &ctx.resolver()) {
        Ok(rows) => {
            if rows.is_empty() {
                eprintln!("watchd: no watchers are defined in {} or {}", ctx.watchers, ctx.watchers_overlay);
            }
            Ok(rows)
        }
        Err(RowsError::Missing(msg)) => {
            eprintln!("watchd: {msg}");
            Err(1)
        }
        Err(RowsError::Malformed(faults)) => {
            for f in &faults {
                eprintln!("watchd: {f}");
            }
            eprintln!("watchd: the watcher manifest is malformed — refusing to answer for any of it");
            Err(1)
        }
    }
}

pub fn cmd_manifest(rows: &[Row]) {
    for r in rows {
        println!("{}|{}|{}|{}", r.name, r.kind, r.target, r.health);
    }
}

/// The TEMPLATE form (`spira-watch@<name>.service`), never the installed, instance-qualified
/// form (`paths::watch_unit_name`) — `systemd/units.sh` is what turns this into the installed
/// name (`inst_watch_name`), same as the bash's own `cmd_units` did (a literal `@%s.service`,
/// not a call through `watch_unit_name`). Scar: calling `watch_unit_name` here doubled the
/// instance suffix on install, since units.sh re-qualifies whatever this prints.
pub fn cmd_units(rows: &[Row]) -> String {
    let mut out = String::new();
    for r in rows {
        if r.kind == Kind::Daemon {
            out.push_str(&format!("spira-watch@{}.service\n", r.name));
        }
    }
    out
}

pub fn cmd_keys() {
    for k in crate::manifest::KEYS {
        println!("{k}");
    }
}

/// `exec <name>` — becomes the watcher; this is what `ExecStart` calls. Never returns on
/// success: it execs the target program in place of this process, the same way the bash's
/// `exec "${argv[@]}"` did, so systemd supervises the real watcher rather than a wrapper.
pub fn cmd_exec(rows: &[Row], name: &str, run: &str) -> i32 {
    let Some(row) = rows.iter().find(|r| r.name == name) else {
        eprintln!("watchd: no watcher named '{name}' in the manifest");
        return 2;
    };
    match row.kind {
        Kind::Off => {
            eprintln!("watchd: '{name}' is optional and {} is not set in spira.conf — there is nothing to run", row.target);
            return 2;
        }
        Kind::Daemon => {}
        _ => {
            eprintln!("watchd: '{name}' is a {} row — {} is written by something else and there is nothing here to run", row.kind, row.target);
            return 2;
        }
    }
    let _ = std::fs::create_dir_all(paths::watchd_dir(run));
    let mut argv = row.target.split_whitespace();
    let Some(prog) = argv.next() else {
        eprintln!("watchd: '{name}' has no program to run");
        return 2;
    };
    let args: Vec<&str> = argv.collect();
    if which(prog).is_none() {
        eprintln!("watchd: {name}: {prog} is not on PATH — starting it anyway so the failure is systemd's to report");
    }
    use std::os::unix::process::CommandExt;
    // batch-job: child is spawned or exec-replaced, not awaited under a deadline
    let err = std::process::Command::new(prog).args(&args).exec();
    eprintln!("watchd: could not exec {prog}: {err}");
    1
}

fn which(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') {
        return Some(PathBuf::from(prog)).filter(|p| p.exists());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(prog)).find(|p| p.is_file())
}

fn logpath(run: &str, row: &Row) -> Option<PathBuf> {
    paths::logfile(run, &row.name, row.kind, &row.target)
}

fn mtime_epoch(path: &std::path::Path) -> Option<i64> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.modified().ok()?;
    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

pub fn cmd_status(rows: &[Row], ops: &dyn Ops, ctx: &Context, world_halted: bool) -> String {
    let now = ctx.now;
    let mut units = Vec::new();
    let mut unit_kind_idx = Vec::new(); // index into rows for each unit, in order
    for (i, r) in rows.iter().enumerate() {
        match r.kind {
            Kind::Daemon => {
                units.push(paths::watch_unit_name(&r.name, &ctx.instance));
                unit_kind_idx.push(i);
            }
            Kind::Extern => {
                units.push(ops.spira_unit(&r.target, "service", &ctx.instance));
                unit_kind_idx.push(i);
            }
            _ => {}
        }
    }
    let states = ops.is_active(&units);
    let mut state_by_row: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    for (pos, idx) in unit_kind_idx.iter().enumerate() {
        state_by_row.insert(*idx, states.get(pos).cloned().unwrap_or_else(|| "?".to_string()));
    }

    let mut out = String::new();
    out.push_str(&format!("{:<14} {:<10} {:<8} {:>7} {:>10} {:>8}  {}\n", "NAME", "UNIT", "HEALTH", "UNREAD", "LAST-EVENT", "RESTARTS", "LOG"));

    let mut degraded: Vec<(String, String)> = Vec::new();
    let mut not_installed: Vec<(String, String)> = Vec::new();

    for (i, r) in rows.iter().enumerate() {
        if r.kind == Kind::Off {
            out.push_str(&format!("{:<14} {:<10} {:<8} {:>7} {:>10} {:>8}  {}\n", r.name, "off", "-", "-", "-", "-", "-"));
            not_installed.push((r.name.clone(), format!("{} is not set in {}", r.target, ctx.conf_file)));
            continue;
        }
        let lf = logpath(&ctx.run, r);
        let total = lf.as_deref().map(fs_ops::total_lines).unwrap_or(0);
        let pos = fs_ops::read_pos(&paths::cursorfile(&ctx.run, &r.name), total);

        let (state, restarts) = if r.kind == Kind::Daemon || r.kind == Kind::Extern {
            let state = state_by_row.get(&i).cloned().unwrap_or_else(|| "?".to_string());
            let restarts = fs_ops::read_u64_file(&paths::restartfile(&ctx.run, &r.name)).unwrap_or(0).to_string();
            (state, restarts)
        } else {
            ("external".to_string(), "-".to_string())
        };

        let age = lf
            .as_deref()
            .and_then(mtime_epoch)
            .map(|mt| cursor::format_age(now - mt))
            .unwrap_or_else(|| "-".to_string());

        let (hstate, hwhy): (&str, String) = if (r.kind == Kind::Daemon || r.kind == Kind::Extern) && state != "active" {
            if world_halted {
                ("HALTED", String::new())
            } else {
                ("DEGRADED", format!("unit is {state} — no writer"))
            }
        } else {
            match health::probe(&r.health, ctx.health_timeout()) {
                Health::NotAsserted => ("-", String::new()),
                Health::Ok => ("OK", String::new()),
                Health::Degraded(why) => ("DEGRADED", why),
            }
        };
        if hstate == "DEGRADED" {
            degraded.push((r.name.clone(), hwhy.clone()));
        }

        out.push_str(&format!(
            "{:<14} {:<10} {:<8} {:>7} {:>10} {:>8}  {}\n",
            r.name,
            state,
            hstate,
            total.saturating_sub(pos),
            age,
            restarts,
            lf.map(|p| p.display().to_string()).unwrap_or_else(|| "-".to_string())
        ));
    }

    if !degraded.is_empty() {
        out.push_str("\nDEGRADED\n");
        for (n, why) in &degraded {
            out.push_str(&format!("  {n}: {why}\n"));
        }
    }
    if !not_installed.is_empty() {
        out.push_str("\nNOT INSTALLED\n");
        for (n, why) in &not_installed {
            out.push_str(&format!("  {n}: {why}\n"));
        }
    }
    out
}

pub struct DrainArgs<'a> {
    pub name: Option<&'a str>,
    pub all: bool,
    pub peek: bool,
    pub limit: u64,
}

/// `drain`/`peek` — one function for both, because they are one piece of arithmetic and two
/// policies: `drain` marks what it shows as read, `peek` (and the `--limit` cap, which only
/// `peek` may use) marks nothing.
pub fn cmd_drain(rows: &[Row], ctx: &Context, filter: Option<&Filter>, args: &DrainArgs) -> Result<String, i32> {
    let mut out = String::new();
    let mut found = false;
    for r in rows {
        if let Some(want) = args.name {
            if r.name != want {
                continue;
            }
        }
        found = true;
        if r.kind == Kind::Off {
            if args.name.is_some() {
                eprintln!("watchd: '{}' is optional and {} is not set in {} — it has never run", r.name, r.target, ctx.conf_file);
            }
            continue;
        }
        let Some(lf) = logpath(&ctx.run, r) else { continue };
        let total = fs_ops::total_lines(&lf);
        let cf = paths::cursorfile(&ctx.run, &r.name);
        let pos = fs_ops::read_pos(&cf, total);
        let new = total.saturating_sub(pos);
        if new == 0 {
            continue;
        }
        let chunk = fs_ops::read_range(&lf, pos, total);

        let shown: Vec<&String> = if args.all {
            out.push_str(&format!("=== {} ({new} new) ===\n", r.name));
            chunk.iter().collect()
        } else {
            let f = filter.expect("filter required unless --all");
            let shown: Vec<&String> = chunk.iter().filter(|l| f.matches(l)).collect();
            out.push_str(&format!("=== {} ({} actionable of {new} new) ===\n", r.name, shown.len()));
            shown
        };
        let k = shown.len() as u64;
        if args.limit != 0 && k > args.limit {
            let start = (k - args.limit) as usize;
            for l in &shown[start..] {
                out.push_str(l);
                out.push('\n');
            }
            out.push_str(&format!("    ... {} earlier actionable line(s) withheld; `watchd tail {}` has all of them\n", k - args.limit, r.name));
        } else {
            for l in &shown {
                out.push_str(l);
                out.push('\n');
            }
        }

        if !args.peek {
            let _ = fs_ops::write_pos(&cf, total);
        }
    }
    if let Some(want) = args.name {
        if !found {
            eprintln!("watchd: no watcher named '{want}' in the manifest");
            return Err(2);
        }
    }
    Ok(out)
}

pub fn cmd_tailers(rows: &[Row], run: &str) -> String {
    let mut out = String::new();
    for r in rows {
        if r.kind == Kind::Off {
            continue;
        }
        let lock = paths::tail_lockfile(run, &r.name);
        if !lock.exists() {
            continue;
        }
        // A lock file that exists but is not held answers nothing (mirrors
        // `_wd_tail_holder` returning 1) — `tailers` only reports a LIVE hold.
        if let Ok(Ok(f)) = crate::lock::try_lock(&lock) {
            drop(f);
            continue;
        }
        let pid = crate::lock::read_holder_pid(&lock);
        let since = if pid != "?" {
            std::fs::metadata(format!("/proc/{pid}"))
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        out.push_str(&format!("{}|{}|{}\n", r.name, pid, since));
    }
    out
}

pub fn cmd_restart(rows: &[Row], ops: &dyn Ops, ctx: &Context, only: Option<&str>) -> Result<String, i32> {
    let mut units = Vec::new();
    let mut names = Vec::new();
    let mut found = false;
    for r in rows {
        if let Some(only) = only {
            if r.name != only {
                continue;
            }
        }
        found = true;
        match r.kind {
            Kind::Off => {
                if only.is_some() {
                    eprintln!("watchd: '{}' is optional and {} is not set in {} — there is no unit to restart", r.name, r.target, ctx.conf_file);
                    return Err(2);
                }
            }
            Kind::Extern => {
                if only.is_some() {
                    eprintln!("watchd: {} is an extern row — use: systemctl --user restart {}", r.name, ops.spira_unit(&r.target, "service", &ctx.instance));
                    return Err(2);
                }
            }
            Kind::Log => {
                if only.is_some() {
                    eprintln!("watchd: {} is a log row — {} is written by something else, so there is no unit to restart", r.name, r.target);
                    return Err(2);
                }
            }
            Kind::Daemon => {
                units.push(paths::watch_unit_name(&r.name, &ctx.instance));
                names.push(r.name.clone());
            }
        }
    }
    if let Some(only) = only {
        if !found {
            eprintln!("watchd: no watcher named '{only}' in the manifest");
            return Err(2);
        }
    }
    if units.is_empty() {
        return Ok(String::new());
    }
    ops.restart(&units).map_err(|e| {
        eprintln!("watchd: {e}");
        1
    })?;
    for n in &names {
        let rf = paths::restartfile(&ctx.run, n);
        let current = fs_ops::read_u64_file(&rf).unwrap_or(0);
        let _ = fs_ops::write_u64_file(&rf, current + 1);
    }
    Ok(format!("restarted: {}\n", units.join(" ")))
}

pub fn cmd_health_ids(ctx: &Context, file: &str) -> i32 {
    let mut prefix = String::new();
    if !ctx.bd.is_empty() && !ctx.db.is_empty() {
        if let Ok(out) = spira_config::bounded::bounded(&ctx.bd).args(["-C", &ctx.db, "config", "get", "issue_prefix"]).output() {
            if out.status.success() {
                prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
        }
    }
    if prefix.is_empty() {
        prefix = ctx.id_prefix.clone();
    }
    if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_alphanumeric()) {
        eprintln!("watchd: database prefix is '{prefix}' — set SPIRA_BD and SPIRA_DB, or SPIRA_ID_PREFIX in {}", ctx.conf_file);
        return 2;
    }
    let path = std::path::Path::new(file);
    if !path.is_file() {
        eprintln!("{file} does not exist — this watcher has never written its state");
        return 1;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("{file} does not exist — this watcher has never written its state");
        return 1;
    };
    let re = regex::Regex::new(&format!(r"(^|[^A-Za-z0-9_-]){}-[A-Za-z0-9]", regex::escape(&prefix))).unwrap();
    if re.is_match(&text) {
        0
    } else {
        eprintln!("{file} names no {prefix}- id at all — it is tracking some other database");
        1
    }
}

pub fn cmd_health_view(prog: &str, sess: &str) -> i32 {
    if !std::path::Path::new(prog).is_file() || std::fs::metadata(prog).map(|m| m.permissions().mode() & 0o111 == 0).unwrap_or(true) {
        eprintln!("{prog} is not executable — nothing here can say what should be visible");
        return 2;
    }
    if which("tmux").is_none() {
        eprintln!("no multiplexer on PATH — what is visible cannot be read");
        return 2;
    }
    let out = spira_config::bounded::bounded(prog).arg("status").output();
    let (stdout, rc) = match &out {
        Ok(o) => (String::from_utf8_lossy(&o.stdout).into_owned(), o.status.code().unwrap_or(-1)),
        Err(_) => (String::new(), -1),
    };
    let want = stdout.lines().find_map(|l| l.strip_prefix("want:")).map(|s| s.trim().to_string());
    let Some(want) = want.filter(|w| !w.is_empty()) else {
        eprintln!("{prog} status printed no 'want:' line (exit {rc}) — it cannot say which view it is steering to");
        return 1;
    };
    if want.chars().any(|c| c.is_whitespace()) {
        eprintln!("{prog} status said 'want: {want}', which is not a session name");
        return 1;
    }
    let showing = tmux_active_window(sess);
    let Some(showing) = showing else {
        eprintln!("there is no '{sess}' session — the surface this steers is not there");
        return 1;
    };
    let wanted = tmux_window0(&want);
    let Some(wanted) = wanted else {
        eprintln!("want: {want}, but there is no '{want}' session to show");
        return 1;
    };
    if showing != wanted {
        eprintln!("want: {want} ({wanted}) but '{sess}' is showing {showing} — the state is right and nothing is enacting it");
        return 1;
    }
    0
}

fn tmux_active_window(session: &str) -> Option<String> {
    let out = spira_config::bounded::bounded("tmux").args(["list-windows", "-t", &format!("={session}"), "-F", "#{window_id}", "-f", "#{window_active}"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn tmux_window0(session: &str) -> Option<String> {
    let out = spira_config::bounded::bounded("tmux").args(["list-windows", "-t", &format!("={session}"), "-F", "#{window_id}", "-f", "#{==:#{window_index},0}"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

use std::os::unix::fs::PermissionsExt;

/// `prune` — removes lock/cursor/pending files and retired `daemon` units for watchers no
/// longer named by the manifest. The log itself is never pruned: it is evidence, not state.
///
/// `unit_dir` is the systemd user unit directory to scan and write to — an explicit
/// parameter, never read from `$HOME` inside this function, so a test can never reach the
/// real one by accident (scar 2026-09-30, see `Ops::disable_now`).
pub fn cmd_prune(rows: &[Row], ops: &dyn Ops, ctx: &Context, unit_dir: &std::path::Path) -> String {
    let dir = ctx.watchd_dir();
    let mut out = String::new();
    if !dir.is_dir() {
        eprintln!("watchd: {} does not exist — nothing to prune", dir.display());
        return out;
    }
    let known: std::collections::HashSet<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    let known_daemons: std::collections::HashSet<&str> = rows.iter().filter(|r| r.kind == Kind::Daemon).map(|r| r.name.as_str()).collect();

    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let owner = ["tail.lock", "cursor", "pending"].iter().find_map(|suf| name.strip_suffix(&format!(".{suf}")));
            let Some(owner) = owner else { continue };
            if known.contains(owner) {
                continue;
            }
            let _ = std::fs::remove_file(e.path());
            out.push_str(&format!("pruned: {}\n", e.path().display()));
            removed += 1;
        }
    }

    for unit in retired_watch_units(ops, unit_dir, &known_daemons, &ctx.instance) {
        ops.disable_now(&unit);
        let _ = std::fs::remove_file(unit_dir.join(&unit));
        out.push_str(&format!("pruned unit: {unit}\n"));
        removed += 1;
    }

    if removed == 0 {
        out.push_str("watchd: prune: nothing to remove\n");
    }
    out
}

/// Every `spira-watch-<name>-<instance>.service` unit systemd or `unit_dir` knows about
/// whose `<name>` is not among the manifest's own `daemon` rows. `unit_dir` is scanned
/// directly (a plain, explicitly-passed directory) rather than through `Ops`, because it is
/// filesystem state, not a systemd query.
fn retired_watch_units(ops: &dyn Ops, unit_dir: &std::path::Path, known_daemons: &std::collections::HashSet<&str>, instance: &str) -> Vec<String> {
    let mut names: std::collections::BTreeSet<String> = ops.list_watch_units(instance).into_iter().collect();
    let suffix = format!("-{instance}.service");
    if let Ok(entries) = std::fs::read_dir(unit_dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("spira-watch-") && n.ends_with(&suffix) {
                names.insert(n);
            }
        }
    }
    names
        .into_iter()
        .filter(|u| {
            u.strip_prefix("spira-watch-")
                .and_then(|rest| rest.strip_suffix(&suffix))
                .map(|wname| !known_daemons.contains(wname))
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Kind;
    use crate::ops::fake::Fake;
    use testkit::TempDir;

    fn ctx(run: &str) -> Context {
        Context {
            run: run.to_string(),
            watchers: String::new(),
            watchers_overlay: String::new(),
            conf_file: "spira.conf".into(),
            actionable: crate::filter::DEFAULT.to_string(),
            health_timeout: "10".into(),
            notify_age: "1800".into(),
            id_prefix: String::new(),
            bd: String::new(),
            db: String::new(),
            placeholders: Default::default(),
            systemctl: "systemctl".into(),
            instance: "prod".into(),
            now: 1_790_726_400,
        }
    }

    fn row(name: &str, kind: Kind, target: &str, health: &str) -> Row {
        Row { name: name.into(), kind, target: target.into(), health: health.into() }
    }

    /// Scar: `cmd_units` once printed the INSTALLED, instance-qualified unit name
    /// (`paths::watch_unit_name`) instead of the TEMPLATE form — `systemd/units.sh` applies
    /// its own instance qualification to whatever this prints, so the installed name came
    /// out with the instance suffix (and the "spira-watch-" prefix) doubled. Caught only by
    /// running a real install inside a testenv container; this is the regression test that
    /// should have caught it first.
    #[test]
    fn units_prints_the_template_form_never_the_instance_qualified_one() {
        let rows = vec![
            row("pool", Kind::Daemon, "pool.sh", ""),
            row("mail-deliver", Kind::Extern, "mail-deliver", ""),
            row("view", Kind::Off, "@SPIRA_VIEW@", ""),
        ];
        let out = cmd_units(&rows);
        assert_eq!(out, "spira-watch@pool.service\n", "only daemon rows, and never instance-qualified: {out}");
    }

    #[test]
    fn status_marks_off_rows_with_dashes_and_lists_them_not_installed() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("view", Kind::Off, "@SPIRA_VIEW@", "")];
        let ops = Fake::default();
        let out = cmd_status(&rows, &ops, &c, false);
        assert!(out.contains("view"), "{out}");
        assert!(out.contains("NOT INSTALLED"), "{out}");
        assert!(out.contains("@SPIRA_VIEW@ is not set"), "{out}");
    }

    #[test]
    fn status_reports_degraded_when_the_unit_is_down_and_the_world_is_not_halted() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let mut ops = Fake::default();
        ops.active.insert(paths::watch_unit_name("pool", "prod"), "inactive".into());
        let out = cmd_status(&rows, &ops, &c, false);
        assert!(out.contains("DEGRADED\n  pool: unit is inactive"), "{out}");
    }

    #[test]
    fn status_reports_halted_instead_of_degraded_when_the_world_is_halted() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let mut ops = Fake::default();
        ops.active.insert(paths::watch_unit_name("pool", "prod"), "inactive".into());
        let out = cmd_status(&rows, &ops, &c, true);
        assert!(!out.contains("DEGRADED"), "{out}");
        let row_line = out.lines().find(|l| l.starts_with("pool")).unwrap();
        assert!(row_line.contains("HALTED"), "{row_line}");
    }

    #[test]
    fn drain_reports_and_consumes_the_unread_range_and_advances_the_cursor() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let lf = paths::logfile(&c.run, "pool", Kind::Daemon, "pool.sh").unwrap();
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(&lf, "2026-09-30T00:00:00Z pool: FAIL suite x\n2026-09-30T00:00:01Z pool: progress\n").unwrap();
        let f = Filter::compile(crate::filter::DEFAULT).unwrap();
        let out = cmd_drain(&rows, &c, Some(&f), &DrainArgs { name: None, all: false, peek: false, limit: 0 }).unwrap();
        assert!(out.contains("1 actionable of 2 new"), "{out}");
        assert!(out.contains("FAIL suite x"));
        assert!(!out.contains("progress"));
        // a second drain with nothing new shows nothing for this watcher
        let out2 = cmd_drain(&rows, &c, Some(&f), &DrainArgs { name: None, all: false, peek: false, limit: 0 }).unwrap();
        assert!(!out2.contains("pool"), "{out2}");
    }

    #[test]
    fn peek_does_not_move_the_cursor() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", "")];
        let lf = paths::logfile(&c.run, "pool", Kind::Daemon, "pool.sh").unwrap();
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(&lf, "2026-09-30T00:00:00Z pool: FAIL suite x\n").unwrap();
        let f = Filter::compile(crate::filter::DEFAULT).unwrap();
        cmd_drain(&rows, &c, Some(&f), &DrainArgs { name: None, all: false, peek: true, limit: 0 }).unwrap();
        let out2 = cmd_drain(&rows, &c, Some(&f), &DrainArgs { name: None, all: false, peek: false, limit: 0 }).unwrap();
        assert!(out2.contains("FAIL suite x"), "peek must not have advanced the cursor: {out2}");
    }

    #[test]
    fn restart_bumps_the_meter_only_for_daemon_rows_that_were_actually_restarted() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("pool", Kind::Daemon, "pool.sh", ""), row("mail-deliver", Kind::Extern, "mail-deliver", "")];
        let ops = Fake::default();
        let out = cmd_restart(&rows, &ops, &c, None).unwrap();
        assert!(out.contains("restarted:"));
        assert_eq!(ops.restarted.borrow().len(), 1);
        assert_eq!(fs_ops::read_u64_file(&paths::restartfile(&c.run, "pool")), Some(1));
    }

    #[test]
    fn restarting_one_extern_row_by_name_is_refused_with_the_systemctl_hint() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        let rows = vec![row("mail-deliver", Kind::Extern, "mail-deliver", "")];
        let ops = Fake::default();
        let r = cmd_restart(&rows, &ops, &c, Some("mail-deliver"));
        assert_eq!(r, Err(2));
    }

    #[test]
    fn prune_removes_mechanism_files_for_a_watcher_no_longer_in_the_manifest_but_not_its_log() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        let dir = c.watchd_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for suf in ["tail.lock", "cursor", "pending"] {
            std::fs::write(dir.join(format!("gone.{suf}")), "x").unwrap();
        }
        std::fs::write(dir.join("gone.log"), "history\n").unwrap();
        std::fs::write(dir.join("kept.cursor"), "1").unwrap();
        let rows = vec![row("kept", Kind::Daemon, "kept.sh", "")];
        let units = TempDir::new("watchd-cmd-units");
        let ops = Fake {
            watch_units: vec!["spira-watch-gone-prod.service".to_string(), "spira-watch-kept-prod.service".to_string()],
            ..Fake::default()
        };
        let out = cmd_prune(&rows, &ops, &c, &units);
        assert!(out.contains("pruned:"), "{out}");
        assert!(out.contains("pruned unit: spira-watch-gone-prod.service"), "{out}");
        assert!(!out.contains("kept-prod"), "a unit whose watcher is still in the manifest is never touched: {out}");
        assert!(!dir.join("gone.cursor").exists());
        assert!(!dir.join("gone.tail.lock").exists());
        assert!(!dir.join("gone.pending").exists());
        assert!(dir.join("gone.log").exists(), "the log is evidence and must survive prune");
        assert!(dir.join("kept.cursor").exists());
        assert_eq!(*ops.disabled.borrow(), vec!["spira-watch-gone-prod.service".to_string()]);
    }

    /// The whole point of this test: prune must never reach a real systemd user manager or
    /// the real `$HOME/.config/systemd/user`, even when `unit_dir` holds actual unit files —
    /// everything about units goes through `Fake`, and the directory it scans is the
    /// TempDir given to it, never `$HOME` (scar 2026-09-30).
    #[test]
    fn prune_touches_only_the_injected_unit_dir_never_a_real_one() {
        let d = TempDir::new("watchd-cmd");
        let c = ctx(d.path().to_str().unwrap());
        std::fs::create_dir_all(c.watchd_dir()).unwrap();
        let units = TempDir::new("watchd-cmd-units");
        std::fs::write(units.join("spira-watch-gone-prod.service"), "[Unit]\n").unwrap();
        std::fs::write(units.join("spira-watch-kept-prod.service"), "[Unit]\n").unwrap();
        let rows = vec![row("kept", Kind::Daemon, "kept.sh", "")];
        let ops = Fake::default();
        cmd_prune(&rows, &ops, &c, &units);
        assert!(!units.join("spira-watch-gone-prod.service").exists());
        assert!(units.join("spira-watch-kept-prod.service").exists());
        assert_eq!(*ops.disabled.borrow(), vec!["spira-watch-gone-prod.service".to_string()]);
    }

    #[test]
    fn health_view_refuses_when_the_program_is_not_executable() {
        let d = TempDir::new("watchd-health-view");
        let p = d.join("not-a-program");
        std::fs::write(&p, "not executable").unwrap();
        assert_eq!(cmd_health_view(p.to_str().unwrap(), "sess"), 2);
    }

    #[test]
    fn health_view_refuses_when_the_program_does_not_exist() {
        assert_eq!(cmd_health_view("/no/such/program", "sess"), 2);
    }

    // health-ids: ported from test-watchd-health-ids.sh (retired, UC-operator-channel-34;
    // see docs/test-plan/operator-channel.toml's dated exception citing sp-pype5).
    mod health_ids {
        use super::*;

        const DB_PREFIX: &str = "sptest";
        const CONF_PREFIX: &str = "notthedb";

        fn stub_bd(dir: &std::path::Path, prints: &str) -> String {
            let p = dir.join("bd-stub");
            testkit::write_exe(&p, &format!("#!/usr/bin/env bash\nprintf '%s\\n' \"{prints}\"\n"));
            p.to_str().unwrap().to_string()
        }

        fn ctx_for(bd: &str, db: &str, id_prefix: &str) -> Context {
            let mut c = ctx("/tmp/unused");
            bd.clone_into(&mut c.bd);
            db.clone_into(&mut c.db);
            id_prefix.clone_into(&mut c.id_prefix);
            c
        }

        #[test]
        fn a_state_file_with_no_database_prefix_ids_fails_naming_the_prefix() {
            let d = TempDir::new("watchd-health-ids");
            let bd = stub_bd(&d, DB_PREFIX);
            let state = d.join("state-none.json");
            std::fs::write(&state, r#"{"seen":["other-abc1","other-xyz2"]}"#).unwrap();
            let c = ctx_for(&bd, "/tmp/db", CONF_PREFIX);
            assert_eq!(cmd_health_ids(&c, state.to_str().unwrap()), 1);
        }

        #[test]
        fn config_drift_the_database_prefix_wins_over_a_differing_spira_id_prefix() {
            let d = TempDir::new("watchd-health-ids");
            let bd = stub_bd(&d, DB_PREFIX);
            let state = d.join("state-good.json");
            std::fs::write(&state, format!(r#"{{"seen":["{DB_PREFIX}-abc1","{DB_PREFIX}-xyz2"]}}"#)).unwrap();
            let c = ctx_for(&bd, "/tmp/db", CONF_PREFIX);
            assert_eq!(cmd_health_ids(&c, state.to_str().unwrap()), 0);
        }

        #[test]
        fn a_missing_state_file_is_degraded_not_refused() {
            let d = TempDir::new("watchd-health-ids");
            let bd = stub_bd(&d, DB_PREFIX);
            let c = ctx_for(&bd, "/tmp/db", CONF_PREFIX);
            assert_eq!(cmd_health_ids(&c, d.join("no-such-file.json").to_str().unwrap()), 1);
        }

        #[test]
        fn falls_back_to_spira_id_prefix_when_no_database_is_configured() {
            let d = TempDir::new("watchd-health-ids");
            let state = d.join("state-good.json");
            std::fs::write(&state, format!(r#"{{"seen":["{DB_PREFIX}-abc1"]}}"#)).unwrap();
            let c = ctx_for("", "", DB_PREFIX);
            assert_eq!(cmd_health_ids(&c, state.to_str().unwrap()), 0);
        }

        #[test]
        fn the_fallback_prefix_can_also_be_wrong() {
            let d = TempDir::new("watchd-health-ids");
            let state = d.join("state-good.json");
            std::fs::write(&state, format!(r#"{{"seen":["{DB_PREFIX}-abc1"]}}"#)).unwrap();
            let c = ctx_for("", "", CONF_PREFIX);
            assert_eq!(cmd_health_ids(&c, state.to_str().unwrap()), 1);
        }

        #[test]
        fn an_unusable_prefix_exits_2_distinct_from_degraded_or_ok() {
            let d = TempDir::new("watchd-health-ids");
            let state = d.join("state-good.json");
            std::fs::write(&state, format!(r#"{{"seen":["{DB_PREFIX}-abc1"]}}"#)).unwrap();
            for bad in ["", "bad prefix", "sp-test"] {
                let c = ctx_for("", "", bad);
                assert_eq!(cmd_health_ids(&c, state.to_str().unwrap()), 2, "prefix {bad:?}");
            }
        }
    }
}
