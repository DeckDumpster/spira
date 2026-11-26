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
    pub ancestors: Vec<u32>,
}

pub const EXIT_TIMEOUT: i32 = 124;

// A sibling fork inherits the lock's descriptor until it execs, so a lock just released can read as held for a moment.
const LOCK_GRACE: Duration = Duration::from_millis(500);

pub fn dedupfile(run: &str, name: &str) -> std::path::PathBuf {
    paths::watchd_dir(run).join(format!("{name}.dedup"))
}

pub fn redeliverfile(run: &str, name: &str) -> std::path::PathBuf {
    paths::watchd_dir(run).join(format!("{name}.redeliver"))
}

/// Hands the last delivery back to the next `next`, unless a reader is live or the record names another session.
pub fn cmd_undeliver(run: &str, name: &str, session: Option<u32>) -> i32 {
    let lock_path = paths::tail_lockfile(run, name);
    let _held = match lock::wait_for_lock(&lock_path, LOCK_GRACE) {
        Ok(Some(f)) => f,
        Ok(None) => return 0,
        Err(e) => {
            eprintln!("watchd: cannot take the reader lock {}: {e}", lock_path.display());
            return 1;
        }
    };
    let path = paths::deliveredfile(run, name);
    let rec = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(e) => {
            eprintln!("watchd: cannot read the delivery record {}: {e}", path.display());
            return 1;
        }
    };
    let Some(text) = record_text(&rec) else {
        eprintln!("watchd: unreadable delivery record {}", path.display());
        return 1;
    };
    if let Some(pid) = session {
        if !record_ancestors(&rec).contains(&pid) {
            return 0;
        }
    }
    let rf = redeliverfile(run, name);
    let tmp = rf.with_extension("redeliver.tmp");
    if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &rf)).is_err() {
        eprintln!("watchd: cannot write {}", rf.display());
        return 1;
    }
    0
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn record_text(rec: &str) -> Option<String> {
    let rest = rec.split("\"text\": \"").nth(1)?;
    let mut out = String::new();
    let mut it = rest.chars();
    loop {
        match it.next()? {
            '"' => return Some(out),
            '\\' => match it.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = it.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
}

fn record_ancestors(rec: &str) -> Vec<u32> {
    let Some(list) = rec.split("\"ancestors\": [").nth(1).and_then(|r| r.split(']').next()) else { return Vec::new() };
    list.split(',').filter_map(|n| n.trim().parse().ok()).collect()
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
    let _held = match lock::wait_for_lock(&lock_path, LOCK_GRACE) {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("watchd: another reader already holds '{}' — one reader per watcher", r.name);
            return Err(3);
        }
        Err(e) => {
            eprintln!("watchd: cannot take the reader lock {}: {e}", lock_path.display());
            return Err(1);
        }
    };

    let rf = redeliverfile(&ctx.run, &r.name);
    if let Ok(text) = std::fs::read_to_string(&rf) {
        let _ = std::fs::remove_file(&rf);
        write_delivered(&ctx.run, &r.name, &text, &args.ancestors);
        return Ok(format!("[redelivered] {text}"));
    }

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
                write_delivered(&ctx.run, &r.name, &out, &args.ancestors);
                return Ok(out);
            }
        }
        if args.timeout.is_some_and(|t| start.elapsed() >= t) {
            return Err(EXIT_TIMEOUT);
        }
        std::thread::sleep(args.poll);
    }
}

fn write_delivered(run: &str, name: &str, out: &str, ancestors: &[u32]) {
    let exited_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let chain = ancestors.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let body = format!("{{\"exited_ms\": {exited_ms}, \"lines\": {}, \"ancestors\": [{chain}], \"text\": \"{}\"}}\n", out.lines().count(), json_escape(out));
    let path = paths::deliveredfile(run, name);
    let tmp = path.with_extension("delivered.tmp");
    if std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        eprintln!("watchd: cannot record the delivery {}", path.display());
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
            placeholders: Default::default(),
            systemctl: "systemctl".into(),
            systemctl_override: None,
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
        let args = NextArgs { name: "inbox", dedup_window: 600, timeout: Some(Duration::from_secs(secs)), poll: Duration::from_millis(20), ancestors: vec![4242, 77] };
        cmd_next(&rows, &ctx(&d.join("run").display().to_string()), &args)
    }

    #[test_id(t16wyti)]
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

    #[test_id(taxic92)]
    #[test]
    fn the_inboxs_unread_count_follows_the_cursor_this_reader_writes() {
        let d = unique_dir("watchd-next-unread");
        let log = d.join("inbox.log");
        append(&log, "");
        assert_eq!(run_next(&d, &log, 0), Err(EXIT_TIMEOUT));
        append(&log, "2026-10-08T00:00:02Z one\n");
        run_next(&d, &log, 5).unwrap();
        append(&log, "2026-10-08T00:00:03Z two\n");
        let cursor = paths::cursorfile(&d.join("run").display().to_string(), "inbox");
        assert_eq!(inbox_rules::unread(&log, &cursor).unwrap(), (1, Some("2026-10-08T00:00:03Z two".to_string())));
        assert_eq!(inbox_rules::concierge_cursor(&d.join("run").display().to_string()), paths::cursorfile(&d.join("run").display().to_string(), "concierge-inbox"));
    }

    #[test_id(tzzcplw)]
    #[test]
    fn a_delivery_leaves_a_record_of_its_exit_and_a_timeout_leaves_none() {
        let d = unique_dir("watchd-next-rec");
        let log = d.join("inbox.log");
        append(&log, "");
        let rec = paths::deliveredfile(&d.join("run").display().to_string(), "inbox");
        assert_eq!(run_next(&d, &log, 0), Err(EXIT_TIMEOUT));
        assert!(!rec.exists(), "a timeout is not a delivery");
        append(&log, "2026-10-08T00:00:02Z one\n2026-10-08T00:00:03Z two\n");
        let ms = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        let before = ms();
        assert_eq!(run_next(&d, &log, 5).unwrap(), "one\ntwo\n");
        let after = ms();
        let text = std::fs::read_to_string(&rec).unwrap();
        let num = |key: &str| -> u128 { text.split(key).nth(1).unwrap().trim_start_matches([':', ' ']).split([',', '}']).next().unwrap().trim().parse().unwrap() };
        assert!((before..=after).contains(&num("\"exited_ms\"")));
        assert_eq!(num("\"lines\""), 2);
        assert!(text.contains("\"ancestors\": [4242,77]"), "{text}");
    }

    fn undeliver(d: &TempDir, session: Option<u32>) -> i32 {
        cmd_undeliver(&d.join("run").display().to_string(), "inbox", session)
    }

    fn delivered_two(d: &TempDir) -> std::path::PathBuf {
        let log = d.join("inbox.log");
        append(&log, "");
        assert_eq!(run_next(d, &log, 0), Err(EXIT_TIMEOUT));
        append(&log, "2026-10-08T00:00:02Z one \"q\"\n2026-10-08T00:00:03Z two\\\n");
        assert_eq!(run_next(d, &log, 5).unwrap(), "one \"q\"\ntwo\\\n");
        log
    }

    #[test_id(tuqwvhw)]
    #[test]
    fn an_undelivered_delivery_is_printed_again_marked_and_only_once() {
        let d = unique_dir("watchd-undeliver");
        let log = delivered_two(&d);
        assert_eq!(undeliver(&d, None), 0);
        assert_eq!(run_next(&d, &log, 5).unwrap(), "[redelivered] one \"q\"\ntwo\\\n");
        assert_eq!(run_next(&d, &log, 0), Err(EXIT_TIMEOUT), "a redelivery is not delivered twice");
    }

    #[test_id(tm704wb)]
    #[test]
    fn undeliver_while_a_reader_holds_the_lock_changes_nothing() {
        let d = unique_dir("watchd-undeliver-live");
        let log = delivered_two(&d);
        let run = d.join("run").display().to_string();
        let held = lock::try_lock(&paths::tail_lockfile(&run, "inbox")).unwrap().unwrap();
        assert_eq!(undeliver(&d, None), 0);
        assert!(!redeliverfile(&run, "inbox").exists());
        drop(held);
        assert_eq!(run_next(&d, &log, 0), Err(EXIT_TIMEOUT));
    }

    #[test_id(tlhj8gb)]
    #[test]
    fn undeliver_for_a_session_not_in_the_ancestors_changes_nothing() {
        let d = unique_dir("watchd-undeliver-other");
        let log = delivered_two(&d);
        assert_eq!(undeliver(&d, Some(999)), 0);
        assert_eq!(run_next(&d, &log, 0), Err(EXIT_TIMEOUT));
        assert_eq!(undeliver(&d, Some(4242)), 0);
        assert!(run_next(&d, &log, 5).unwrap().starts_with("[redelivered] "));
    }

    #[test_id(tpg0d8z)]
    #[test]
    fn a_missing_record_is_nothing_to_undo_and_an_unreadable_one_fails() {
        let d = unique_dir("watchd-undeliver-rec");
        assert_eq!(undeliver(&d, None), 0);
        let run = d.join("run").display().to_string();
        std::fs::create_dir_all(paths::watchd_dir(&run)).unwrap();
        std::fs::write(paths::deliveredfile(&run, "inbox"), "not json").unwrap();
        assert_eq!(undeliver(&d, None), 1);
    }

    #[test_id(t3ml780)]
    #[test]
    fn a_redelivery_comes_before_a_line_that_arrived_after_it() {
        let d = unique_dir("watchd-undeliver-order");
        let log = delivered_two(&d);
        assert_eq!(undeliver(&d, None), 0);
        append(&log, "2026-10-08T00:00:09Z fresh\n");
        assert_eq!(run_next(&d, &log, 5).unwrap(), "[redelivered] one \"q\"\ntwo\\\n");
        assert_eq!(run_next(&d, &log, 5).unwrap(), "fresh\n");
    }

    #[test_id(tprsriv)]
    #[test]
    fn a_redelivery_that_fails_in_turn_is_redelivered_with_one_marker() {
        let d = unique_dir("watchd-undeliver-twice");
        let log = delivered_two(&d);
        assert_eq!(undeliver(&d, None), 0);
        run_next(&d, &log, 5).unwrap();
        assert_eq!(undeliver(&d, None), 0);
        assert_eq!(run_next(&d, &log, 5).unwrap(), "[redelivered] one \"q\"\ntwo\\\n");
    }
}
