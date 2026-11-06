//! The world acts on the scheduler, not on timers. Every loop unit is a task inside
//! `spira-runtime serve`, so `world stop`/`drain` is `scheduler halt <reason>` (every task's
//! effects held, observation continues) and `world start`/`resume` is `scheduler resume`, both
//! on the runtime's control socket (`SPIRA_RUNTIME_SOCKET`, S14). `world status` reads
//! `scheduler status`: the halt state and each task's last fired, next due and lag.
//!
//! What a stop never touches is the keep list: the scheduler itself, the store's socket, the
//! database and the watchdog that restarts the scheduler when it wedges (S15).

use std::path::Path;

/// Units a world stop never stops, by base name (no instance suffix, no `.service`/`.socket`/
/// `.timer`): the scheduler, the store's socket activation, the database and the watchdog.
pub const KEEP: &[&str] = &["spira-runtime", "lc-serve", "dolt-beads", "spira-watchdog"];

/// The unit names the keep list resolves to on an instance, the order `status` prints them.
pub fn keep_units(sfx: &str) -> Vec<String> {
    vec![
        format!("spira-runtime{sfx}.service"),
        "lc-serve.socket".to_string(),
        "dolt-beads.service".to_string(),
        format!("spira-watchdog{sfx}.timer"),
    ]
}

/// Whether `unit` is on the keep list, with or without the instance suffix `sfx` (`-prod`).
pub fn is_kept(unit: &str, sfx: &str) -> bool {
    let base = [".service", ".socket", ".timer"].iter().find_map(|s| unit.strip_suffix(s)).unwrap_or(unit);
    let base = if sfx.is_empty() { base } else { base.strip_suffix(sfx).unwrap_or(base) };
    KEEP.contains(&base)
}

/// One request on the control socket: the argv, answered by one line.
pub trait Control {
    fn call(&self, argv: &[&str]) -> Result<String, String>;
}

/// The runtime's control socket at a path.
pub struct Socket<'a>(pub &'a Path);

impl Control for Socket<'_> {
    fn call(&self, argv: &[&str]) -> Result<String, String> {
        runtime::socket::lc_call(self.0, argv).map_err(|e| format!("the scheduler is not answering on {}: {e}", self.0.display()))
    }
}

/// `scheduler halt <reason>`: the reply names the journaled halt. An empty reason is sent as
/// `unstated`, the same word the halt stamp carries.
pub fn halt(c: &dyn Control, reason: &str) -> Result<String, String> {
    let reason = if reason.trim().is_empty() { "unstated" } else { reason.trim() };
    c.call(&["scheduler", "halt", reason])
}

/// `scheduler resume`.
pub fn resume(c: &dyn Control) -> Result<String, String> {
    c.call(&["scheduler", "resume"])
}

/// One task's row of a `scheduler status` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub name: String,
    pub mode: String,
    pub last_fired_at: Option<u64>,
    pub next_due_at: Option<u64>,
    pub lag_ms: Option<u64>,
    pub last_outcome: Option<String>,
    pub in_flight: bool,
}

/// The parts of a `scheduler status` document the world reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub at: u64,
    pub halted: bool,
    pub held: u64,
    pub tasks: Vec<TaskRow>,
}

/// Parse a `scheduler status` document; a document without `at` or `halted` is refused, named.
pub fn parse_status(doc: &str) -> Result<Status, String> {
    let v: serde_json::Value = serde_json::from_str(doc).map_err(|e| format!("scheduler status is not JSON: {e}"))?;
    let at = v.get("at").and_then(serde_json::Value::as_u64).ok_or("scheduler status carries no `at`")?;
    let halted = v.get("halted").and_then(serde_json::Value::as_bool).ok_or("scheduler status carries no `halted`")?;
    let held = v.get("held").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let u = |t: &serde_json::Value, k: &str| t.get(k).and_then(serde_json::Value::as_u64);
    let s = |t: &serde_json::Value, k: &str| t.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
    let tasks = v
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .map(|ts| {
            ts.iter()
                .map(|t| TaskRow {
                    name: s(t, "name").unwrap_or_default(),
                    mode: s(t, "mode").unwrap_or_default(),
                    last_fired_at: u(t, "last_fired_at"),
                    next_due_at: u(t, "next_due_at"),
                    lag_ms: u(t, "lag_ms"),
                    last_outcome: s(t, "last_outcome"),
                    in_flight: t.get("in_flight").and_then(serde_json::Value::as_bool).unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Status { at, halted, held, tasks })
}

/// A span of milliseconds the way a status line reads it: `45s`, `12m`, `3h`, `2d`.
pub fn span(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=119 => format!("{s}s"),
        120..=7199 => format!("{}m", s / 60),
        7200..=172_799 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// The headline: halted or running, and how many effects the halt holds.
pub fn headline(s: &Status) -> String {
    if s.halted {
        format!("spira: scheduler HALTED — {} effect(s) held, observation continues", s.held)
    } else {
        format!("spira: scheduler RUNNING — {} task(s)", s.tasks.len())
    }
}

/// One status line per task, against the document's own `at`: last fired, next due, lag and
/// last outcome; `never` for a task that has not fired.
pub fn task_line(t: &TaskRow, at: u64) -> String {
    let fired = t.last_fired_at.map(|f| format!("{} ago", span(at.saturating_sub(f)))).unwrap_or_else(|| "never".into());
    let due = match t.next_due_at {
        Some(d) if d > at => format!("in {}", span(d - at)),
        Some(d) => format!("{} overdue", span(at - d)),
        None => "—".into(),
    };
    let lag = t.lag_ms.map(span).unwrap_or_else(|| "—".into());
    let outcome = t.last_outcome.as_deref().unwrap_or("—");
    let flight = if t.in_flight { " (in flight)" } else { "" };
    format!("  {:<30} {:<8} fired {fired:<10} due {due:<14} lag {lag:<6} {outcome}{flight}", t.name, t.mode)
}

/// What the world and the scheduler disagree about, if anything: a work halt stamp the
/// scheduler does not hold, or a scheduler halt no stamp explains.
pub fn mismatch(stamp_present: bool, s: &Status) -> Option<String> {
    match (stamp_present, s.halted) {
        (true, false) => Some("spira: DEGRADED — the world is halted (stamp present) but the scheduler is RUNNING; rerun: world stop".into()),
        (false, true) => Some("spira: DEGRADED — the scheduler is HALTED with no world halt stamp; lift with: world start".into()),
        _ => None,
    }
}

/// Exit codes of `world scheduler-status`, for a shell caller that must not parse prose.
pub const RC_RUNNING: i32 = 0;
pub const RC_UNANSWERED: i32 = 1;
pub const RC_HALTED: i32 = 2;

/// `world scheduler-status`: the headline and one line per task, and the exit code that says
/// running ([`RC_RUNNING`]), halted ([`RC_HALTED`]) or not answering ([`RC_UNANSWERED`]).
pub fn report(c: &dyn Control) -> (i32, Vec<String>) {
    match c.call(&["scheduler", "status"]).and_then(|d| parse_status(&d)) {
        Ok(s) => {
            let mut lines = vec![headline(&s)];
            lines.extend(s.tasks.iter().map(|t| task_line(t, s.at)));
            (if s.halted { RC_HALTED } else { RC_RUNNING }, lines)
        }
        Err(e) => (RC_UNANSWERED, vec![format!("spira: scheduler UNKNOWN — {e}")]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        calls: RefCell<Vec<Vec<String>>>,
        reply: Result<String, String>,
    }

    impl Fake {
        fn ok(reply: &str) -> Fake {
            Fake { calls: RefCell::new(Vec::new()), reply: Ok(reply.into()) }
        }
    }

    impl Control for Fake {
        fn call(&self, argv: &[&str]) -> Result<String, String> {
            self.calls.borrow_mut().push(argv.iter().map(|s| s.to_string()).collect());
            self.reply.clone()
        }
    }

    #[test]
    fn halt_sends_the_reason_as_one_argument() {
        let f = Fake::ok("scheduler.halted x");
        assert_eq!(halt(&f, "planted reason"), Ok("scheduler.halted x".into()));
        assert_eq!(*f.calls.borrow(), vec![vec!["scheduler".to_string(), "halt".into(), "planted reason".into()]]);
    }

    #[test]
    fn an_empty_reason_halts_as_unstated() {
        let f = Fake::ok("ok");
        halt(&f, "  ").unwrap();
        assert_eq!(f.calls.borrow()[0][2], "unstated");
    }

    #[test]
    fn resume_sends_scheduler_resume() {
        let f = Fake::ok("ok");
        resume(&f).unwrap();
        assert_eq!(*f.calls.borrow(), vec![vec!["scheduler".to_string(), "resume".into()]]);
    }

    #[test]
    fn a_refused_call_is_the_error() {
        let f = Fake { calls: RefCell::new(Vec::new()), reply: Err("down".into()) };
        assert_eq!(halt(&f, "x"), Err("down".into()));
    }

    #[test]
    fn the_keep_list_holds_the_scheduler_the_store_socket_dolt_and_the_watchdog() {
        for u in ["spira-runtime-prod.service", "spira-runtime.service", "lc-serve.socket", "lc-serve.service", "dolt-beads.service", "spira-watchdog-prod.timer", "spira-watchdog-prod.service"] {
            assert!(is_kept(u, "-prod"), "{u}");
        }
        for u in ["spira-sentinel-prod.timer", "spira-loom-prod.service", "spira-runtime-watch-prod.service", "spira-mail-deliver-prod.service"] {
            assert!(!is_kept(u, "-prod"), "{u}");
        }
        assert_eq!(keep_units("-prod"), vec!["spira-runtime-prod.service", "lc-serve.socket", "dolt-beads.service", "spira-watchdog-prod.timer"]);
    }

    const DOC: &str = r#"{"schema":1,"at":1000000,"halted":true,"held":3,"tasks":[
        {"name":"sentinel-call","mode":"act","state":null,"last_fired_at":940000,"last_due_at":940000,"lag_ms":1200,"next_due_at":1060000,"last_outcome":"succeeded","in_flight":false},
        {"name":"watch.tower","mode":"observe","state":null,"last_fired_at":null,"last_due_at":null,"lag_ms":null,"next_due_at":900000,"last_outcome":null,"in_flight":true}
    ],"classes":[]}"#;

    #[test]
    fn status_parses_the_halt_and_each_task() {
        let s = parse_status(DOC).unwrap();
        assert!(s.halted);
        assert_eq!((s.at, s.held, s.tasks.len()), (1_000_000, 3, 2));
        assert_eq!(s.tasks[0].name, "sentinel-call");
        assert_eq!(s.tasks[0].last_fired_at, Some(940_000));
        assert_eq!(s.tasks[1].last_fired_at, None);
        assert!(s.tasks[1].in_flight);
    }

    #[test]
    fn a_document_without_halted_is_refused_named() {
        assert!(parse_status(r#"{"at":1}"#).unwrap_err().contains("halted"));
        assert!(parse_status("not json").unwrap_err().contains("not JSON"));
    }

    #[test]
    fn task_lines_read_last_fired_next_due_and_lag_against_the_documents_time() {
        let s = parse_status(DOC).unwrap();
        let a = task_line(&s.tasks[0], s.at);
        assert!(a.contains("sentinel-call") && a.contains("fired 60s ago") && a.contains("due in 60s") && a.contains("lag 1s") && a.contains("succeeded"), "{a}");
        let b = task_line(&s.tasks[1], s.at);
        assert!(b.contains("fired never") && b.contains("100s overdue") && b.contains("(in flight)"), "{b}");
    }

    #[test]
    fn the_headline_says_halted_and_what_it_holds() {
        let s = parse_status(DOC).unwrap();
        assert!(headline(&s).contains("HALTED") && headline(&s).contains("3 effect(s) held"));
        let r = Status { halted: false, ..s };
        assert!(headline(&r).contains("RUNNING") && headline(&r).contains("2 task(s)"));
    }

    #[test]
    fn a_stamp_and_a_scheduler_that_disagree_are_named() {
        let s = parse_status(DOC).unwrap();
        assert_eq!(mismatch(true, &s), None);
        assert!(mismatch(false, &s).unwrap().contains("no world halt stamp"));
        let r = Status { halted: false, ..s };
        assert!(mismatch(true, &r).unwrap().contains("scheduler is RUNNING"));
        assert_eq!(mismatch(false, &r), None);
    }

    #[test]
    fn report_says_running_halted_or_unanswered_by_exit_code() {
        let (rc, lines) = report(&Fake::ok(DOC));
        assert_eq!(rc, RC_HALTED);
        assert!(lines[0].contains("HALTED"), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("sentinel-call")), "{lines:?}");
        let running = DOC.replace(r#""halted":true"#, r#""halted":false"#);
        assert_eq!(report(&Fake::ok(&running)).0, RC_RUNNING);
        let down = Fake { calls: RefCell::new(Vec::new()), reply: Err("not answering".into()) };
        let (rc, lines) = report(&down);
        assert_eq!(rc, RC_UNANSWERED);
        assert!(lines[0].contains("UNKNOWN") && lines[0].contains("not answering"), "{lines:?}");
        assert_eq!(report(&Fake::ok("garbage")).0, RC_UNANSWERED);
    }

    #[test]
    fn the_socket_speaks_the_control_wire() {
        let dir = testkit::TempDir::new("world-sched");
        let path = dir.path().join("ctl.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            runtime::socket::answer_one(stream, &|argv: &[String]| Ok(format!("got {}", argv.join(" ")))).unwrap();
        });
        assert_eq!(halt(&Socket(&path), "why"), Ok("got scheduler halt why".into()));
        t.join().unwrap();
    }
}
