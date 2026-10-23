//! unit-run watch [--family F] | ensure-accounting [--dir DIR]
//!
//! watch: learn unit runs from the user manager's D-Bus signals, pushing one record per run to
//! the metrics store (family `unit-run-dbus`), and pushing its counts and duration there too.

use std::path::PathBuf;
use std::time::Duration;
use unit_run::watch;
use unit_run::{accounting_dropin_path, ensure_io_accounting, io_controller_available, own_controllers_file, written_ids, Clock, Proc, Triggers};

struct Wall;
impl Clock for Wall {
    fn now_epoch(&self) -> u64 {
        spira_config::vtime::now_epoch()
    }
}

struct Spawn;
impl Proc for Spawn {
    fn run(&self, argv: &[&str]) -> Result<String, String> {
        let out = spira_config::bounded::bounded(argv[0]).args(&argv[1..]).output().map_err(|e| format!("{}: {e}", argv[0]))?;
        if !out.status.success() {
            return Err(format!("{}: {}", argv[0], String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

fn push_metrics(sink: &spira_otel::metrics::MetricsSink, r: &unit_run::Record) {
    use spira_otel::metrics::Point;
    let unit = watch::metric_unit(&r.unit);
    let mut points = vec![Point::Count { name: "spira_unit_runs", value: 1 }];
    if let Some(s) = r.started_us {
        points.push(Point::Seconds { name: "spira_unit_run_seconds", value: r.ended_us.saturating_sub(s) as f64 / 1e6 });
    }
    sink.push(&points, &[("unit", &unit), ("result", &r.result)]);
}

/// How far back a restarted listener looks for the invocations it already stored and the run it
/// last saw end. Longer than any catch-up overlap, short enough to export in one read.
const RESUME_SECS: u64 = 2 * 3600;
const RETRY: Duration = Duration::from_secs(10);
const BACKLOG_MAX: usize = 20_000;

/// The family's recent rows from the store. The listener does not start without them: a resume
/// read that failed would store every overlapping run twice.
fn resume_rows(endpoint: Option<&str>, family: &str, now: u64) -> Vec<serde_json::Value> {
    loop {
        let read = tsd::store::Reader::new(endpoint).and_then(|r| r.rows_or_empty(family, now.saturating_sub(RESUME_SECS)));
        match read {
            Ok(rows) => return rows,
            Err(e) => {
                eprintln!("unit-run watch: resume read refused: {e}; retrying in {}s", RETRY.as_secs());
                std::thread::sleep(RETRY);
            }
        }
    }
}

fn watch_cmd(family: &str) -> Result<(), String> {
    let instance = spira_config::process::cfg("SPIRA_INSTANCE")?;
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().filter(|e| !e.trim().is_empty());
    let resume = resume_rows(endpoint.as_deref(), family, Wall.now_epoch());
    let mut seen = written_ids(&resume);
    let metrics = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-unit-run", Duration::from_secs(2));
    let store = tsd::store::Pusher::new(endpoint.clone());
    let mut backlog = unit_run::Backlog::new(BACKLOG_MAX);
    let mut triggers = Triggers::new(&Spawn);
    let mut conn = unit_run::dbus::Conn::connect(&unit_run::dbus::bus_path()?)?;
    watch::arm(&mut conn)?;
    let now_us = || spira_config::vtime::now_epoch().saturating_mul(1_000_000);
    let started_us = now_us();
    let mut tracker = watch::Tracker::new(watch::last_ended_us(&resume).unwrap_or(started_us));
    let mut record = |r: unit_run::Record| -> Result<(), String> {
        if !seen.insert(r.invocation_id.clone()) {
            return Ok(());
        }
        let trig = triggers.of(&r.unit);
        let fields = unit_run::record_fields(&r, trig.as_deref(), &instance);
        let (dropped, stored) = backlog.offer(&store, family, tsd::iso_utc(Wall.now_epoch()), fields);
        if dropped {
            eprintln!("unit-run watch: backlog full ({BACKLOG_MAX}) while the store refuses rows; dropped the oldest");
        }
        if let Err(e) = stored {
            eprintln!("unit-run watch: {} row(s) held: {e}", backlog.len());
        }
        push_metrics(&metrics, &r);
        Ok(())
    };
    for r in watch::catch_up(&mut conn, &mut tracker, now_us)? {
        record(r)?;
    }
    eprintln!("unit-run watch: armed on the user manager; storing {family} rows in the metrics store");
    loop {
        for r in watch::step(&mut conn, &mut tracker, now_us, Duration::from_secs(60))? {
            record(r)?;
        }
    }
}

fn ensure_accounting(dir: Option<String>) -> Result<(), String> {
    let home = match dir {
        Some(d) => PathBuf::from(d),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or("neither XDG_CONFIG_HOME nor HOME is set")?,
    };
    if ensure_io_accounting(&home)? {
        eprintln!("unit-run: wrote {} — re-executing the user manager to apply it", accounting_dropin_path(&home).display());
        Spawn.run(&["systemctl", "--user", "daemon-reexec"])?;
    }
    if !own_controllers_file().is_some_and(|f| io_controller_available(&f)) {
        eprintln!("unit-run: the io cgroup controller is not delegated to the user manager, so no record can carry io bytes; set Delegate=io in a user@.service drop-in (root) and restart the user manager");
    }
    Ok(())
}


fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("watch") => watch_cmd(args.iter().position(|a| a == "--family").and_then(|i| args.get(i + 1)).map_or(unit_run::FAMILY, String::as_str)),
        Some("ensure-accounting") => ensure_accounting(args.iter().position(|a| a == "--dir").and_then(|i| args.get(i + 1)).cloned()),
        _ => {
            eprintln!("usage: unit-run <watch [--family F]|ensure-accounting [--dir DIR]>");
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("unit-run: {e}");
        std::process::exit(1);
    }
}
