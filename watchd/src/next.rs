//! `watchd next <row>` — block until the row has unread lines that survive the inbox drop and
//! dedup rules, print them, advance watchd's own cursor past everything examined, exit 0.
//! Input is the row's log and cursor only; the dedup memory is `<name>.dedup` beside them.

use crate::context::Context;
use crate::fs_ops;
use crate::lock;
use crate::manifest::{Kind, Row};
use crate::paths;
use std::time::{Duration, Instant};

pub struct NextArgs<'a> {
    pub name: &'a str,
    pub dedup_window: u64,
    pub timeout: Option<Duration>,
    pub poll: Duration,
}

pub const EXIT_TIMEOUT: i32 = 124;

pub fn dedupfile(run: &str, name: &str) -> std::path::PathBuf {
    paths::watchd_dir(run).join(format!("{name}.dedup"))
}

pub fn cmd_next(rows: &[Row], ctx: &Context, args: &NextArgs) -> Result<String, i32> {
    let Some(r) = rows.iter().find(|r| r.name == args.name) else {
        eprintln!("watchd: no watcher named '{}' in the manifest", args.name);
        return Err(2);
    };
    if r.kind == Kind::Off {
        eprintln!("watchd: '{}' is optional and {} is not set in {} — it has never run", r.name, r.target, ctx.conf_file);
        return Err(2);
    }
    let Some(lf) = paths::logfile(&ctx.run, &r.name, r.kind, &r.target) else { return Err(2) };

    let lock_path = paths::tail_lockfile(&ctx.run, &r.name);
    let _held = match lock::try_lock(&lock_path) {
        Ok(Ok(f)) => f,
        Ok(Err(())) => {
            eprintln!("watchd: another reader already holds '{}' — one reader per watcher", r.name);
            return Err(3);
        }
        Err(e) => {
            eprintln!("watchd: cannot take the reader lock {}: {e}", lock_path.display());
            return Err(1);
        }
    };

    let cf = paths::cursorfile(&ctx.run, &r.name);
    if !cf.exists() {
        let _ = fs_ops::write_pos(&cf, fs_ops::total_lines(&lf));
    }
    let df = dedupfile(&ctx.run, &r.name);
    let start = Instant::now();
    loop {
        let total = fs_ops::total_lines(&lf);
        let pos = fs_ops::read_pos(&cf, total);
        if total > pos {
            let chunk = fs_ops::read_range(&lf, pos, total);
            let wall = wall_secs();
            let text = std::fs::read_to_string(&df).unwrap_or_default();
            let mut dedup = inbox_rules::Dedup::parse(&text, wall, args.dedup_window);
            let mut out = String::new();
            for line in &chunk {
                if let Some(body) = dedup.pass(line, wall, args.dedup_window) {
                    out.push_str(body);
                    out.push('\n');
                }
            }
            let _ = std::fs::write(&df, dedup.render());
            if fs_ops::write_pos(&cf, total).is_err() {
                eprintln!("watchd: cannot advance the cursor {}", cf.display());
                return Err(1);
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
        if args.timeout.is_some_and(|t| start.elapsed() >= t) {
            return Err(EXIT_TIMEOUT);
        }
        std::thread::sleep(args.poll);
    }
}

fn wall_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use testkit::TempDir;

    fn ctx(run: &str) -> Context {
        Context {
            run: run.into(),
            watchers: String::new(),
            watchers_overlay: String::new(),
            conf_file: "spira.conf".into(),
            actionable: "x".into(),
            health_timeout: "1".into(),
            notify_age: "1800".into(),
            id_prefix: "sp".into(),
            bd: String::new(),
            db: String::new(),
            placeholders: Default::default(),
            systemctl: "systemctl".into(),
            instance: "prod".into(),
            now: 0,
        }
    }

    fn unique_dir(tag: &str) -> TempDir {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        TempDir::new(&format!("{tag}-{nanos}"))
    }

    fn append(p: &std::path::Path, text: &str) {
        std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap().write_all(text.as_bytes()).unwrap();
    }

    fn run_next(d: &TempDir, log: &std::path::Path, secs: u64) -> Result<String, i32> {
        let rows = vec![Row { name: "inbox".into(), kind: Kind::Log, target: log.display().to_string(), health: String::new() }];
        let args = NextArgs { name: "inbox", dedup_window: 600, timeout: Some(Duration::from_secs(secs)), poll: Duration::from_millis(20) };
        cmd_next(&rows, &ctx(&d.join("run").display().to_string()), &args)
    }

    #[test]
    fn echoes_and_repeats_are_dropped_a_new_line_is_delivered_and_unread_falls_to_zero() {
        let d = unique_dir("watchd-next");
        let log = d.join("inbox.log");
        append(&log, "");
        assert_eq!(run_next(&d, &log, 0), Err(EXIT_TIMEOUT));
        append(&log, "2026-10-08T00:00:01Z ROUND RESULT green\n2026-10-08T00:00:02Z a thing needs you\n2026-10-08T00:00:03Z a thing needs you\n");
        assert_eq!(run_next(&d, &log, 5).unwrap(), "a thing needs you\n");
        let cf = paths::cursorfile(&d.join("run").display().to_string(), "inbox");
        assert_eq!(fs_ops::read_pos(&cf, fs_ops::total_lines(&log)), 3);
        append(&log, "2026-10-08T00:00:09Z a thing needs you\n");
        assert_eq!(run_next(&d, &log, 0), Err(EXIT_TIMEOUT), "a repeat within the window is dropped across invocations");
        append(&log, "2026-10-08T00:00:10Z something else\n");
        assert_eq!(run_next(&d, &log, 5).unwrap(), "something else\n", "a line written while no waiter ran is delivered by the next call");
        assert_eq!(fs_ops::read_pos(&cf, fs_ops::total_lines(&log)), fs_ops::total_lines(&log));
    }

    #[test]
    fn a_second_reader_is_refused_and_history_is_not_replayed_on_first_use() {
        let d = unique_dir("watchd-next-lock");
        let log = d.join("inbox.log");
        append(&log, "old history line\n");
        let run = d.join("run").display().to_string();
        let held = lock::try_lock(&paths::tail_lockfile(&run, "inbox")).unwrap().unwrap();
        assert_eq!(run_next(&d, &log, 0), Err(3));
        drop(held);
        assert_eq!(run_next(&d, &log, 0), Err(EXIT_TIMEOUT));
    }
}
