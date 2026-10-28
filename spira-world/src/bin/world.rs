//! world.sh — stop and start Spira as a whole. Replaces spira/world.sh (bash).
//!
//!   world.sh stop [--why "..."] [--work] [--observability] [--maintenance] [--all]
//!                 [--hard] [--round-drain] [--round-drain-timeout SECS]
//!   world.sh start [--work] [--observability] [--maintenance] [--all]
//!   world.sh drain [--until WHAT] [--owner O] [--owner-pid PID | --owner-unit UNIT]
//!                  [--timeout SECS | --for SECS | --deadline SECS]
//!   world.sh drain-check
//!   world.sh resume
//!   world.sh pause --repo R [--lanes claims,rounds,publish] --reason WHY --until CONDITION [--owner O]
//!   world.sh pause --reserve BEAD[,BEAD...] --reason WHY [--until CONDITION] [--owner O]
//!   world.sh unpause --repo R [--lanes ...] | --reserve BEAD[,BEAD...]
//!   world.sh status
//!
//! SCOPED PAUSES are recorded state (`spira_config::scoped_stop`), never halts: one repo's
//! claims, rounds or publish lane, or named beads no dwarf may claim. Running work is never
//! touched, and every reader treats a stop as deliberate.
//!
//! PLANES. Every unit declares `Plane=work|observability|maintenance` in its own
//! `[X-Spira]` section; world reads that, never a list of its own. A plane flag selects
//! exactly those planes. With none, `stop` halts `work` and `start` starts `work` and
//! `observability` — detectors stay on through a halt, and read the `world.halted` stamp
//! (the work plane's) as idle. `maintenance` has its own switch. `--hard` is `work` +
//! `observability`. A unit with no declaration is `work`.
//!
//! WHAT IT DOES NOT TOUCH, deliberately: dolt-beads*.service (the databases — a halt must
//! never risk the data, and a stopped server makes every diagnostic fail) and cockpit*/
//! concierge (halting the loop must not blind the operator reading it). Nothing here ever
//! stops Dolt.
//!
//! A ROUND ON THE ROUND VM IS NAMED, NEVER SILENTLY INTERRUPTED (sp-2bkpn). `status` and
//! `stop` both read `round-vm status`; `stop` prints what it finds before doing anything
//! else, and `--round-drain` waits (up to `--round-drain-timeout`, default 1800s) for it to
//! clear first — the same shape `drain` already gives live dwarves. See `spira_world::round`
//! for exactly what this can and cannot see.
//!
//! DWARVES ARE STOPPED THROUGH slay, never killed directly — a bare kill leaves the bead
//! held until its lease expires and charges the attempt anyway, which is how a halt for an
//! unrelated reason walks a bead toward poison. Spawned by its real binary name, `slay`,
//! not the `slay.sh` compat alias — this is harness-to-harness, not an operator typing a
//! remembered name, and it must not depend on a release having built the compat symlink
//! table correctly (sp-6onps-compat: production's bin/ had neither for a cycle). `slay.sh`
//! is invoked as an external process, exactly as bash `world.sh` always called it — the
//! two tools share no Rust code at that boundary, only the binary's name on PATH.

use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;
use std::process::Command;

use spira_world::sysctl::{self, Plane, StartAction, PLANES};
use spira_world::spira_prod_or_home;

/// sp-ivfu3: `spira.run`/`spira.instance` are resolved in-process through `spira_config`
/// (`spira_world::spira_run`/`instance`/`instance_suffix`/`halt_stamp`/`drain_stamp`), which
/// REFUSE with a named error rather than fall back to a literal when they cannot resolve.
/// This prints that refusal and exits — world.sh has no sensible "guess and keep going"
/// here, the same reason `supervise`/`broker` already exit rather than default a missing
/// `SPIRA_RUN`.
fn die(msg: &str) -> ! {
    eprintln!("spira: FATAL world: {msg}");
    spira_otel::oneshot::exit(1);
}

fn run_or_die() -> PathBuf {
    spira_world::spira_run().unwrap_or_else(|e| die(&e))
}

fn instance_or_die() -> String {
    spira_world::instance().unwrap_or_else(|e| die(&e))
}

fn sfx_or_die() -> String {
    spira_world::instance_suffix().unwrap_or_else(|e| die(&e))
}

fn halt_stamp_or_die() -> PathBuf {
    spira_world::halt_stamp().unwrap_or_else(|e| die(&e))
}

fn drain_stamp_or_die() -> PathBuf {
    spira_world::drain_stamp().unwrap_or_else(|e| die(&e))
}

/// `SPIRA_CTRL`'s declared value — the operational control plane file's path. The declared
/// default is itself `$SPIRA_RUN/control` (`spira/conf.d/SPIRA_CTRL`), so this is never a
/// second, Rust-side fallback onto `run_or_die().join("control")` (per Ryan 2026-10-05: one
/// source of config).
fn ctrl_path_or_die() -> PathBuf {
    spira_config::process::cfg("SPIRA_CTRL").map(PathBuf::from).unwrap_or_else(|e| die(&e))
}

/// `SPIRA_PROD`'s declared value, resolved once here and handed to [`spira_prod_or_home`] —
/// never read from the environment inside that pure function.
fn resolved_prod(home: &std::path::Path) -> PathBuf {
    let prod = spira_config::process::cfg("SPIRA_PROD").unwrap_or_else(|e| die(&e));
    spira_prod_or_home(home, &prod)
}

fn now_iso() -> String {
    spira_config::bounded::bounded("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The planes a `stop`/`start` acts on: exactly the ones named by flag, or `default` when
/// none is. `--hard` names work and observability.
fn selected_planes(args: &[String], default: &[Plane]) -> BTreeSet<Plane> {
    let mut named = BTreeSet::new();
    for a in args {
        match a.as_str() {
            "--work" => {
                named.insert(Plane::Work);
            }
            "--observability" => {
                named.insert(Plane::Observability);
            }
            "--maintenance" => {
                named.insert(Plane::Maintenance);
            }
            "--hard" => {
                named.insert(Plane::Work);
                named.insert(Plane::Observability);
            }
            "--all" => named.extend(PLANES),
            _ => {}
        }
    }
    if named.is_empty() {
        default.iter().copied().collect()
    } else {
        named
    }
}

/// `world.halted` is the work plane's stamp; the others carry `world.halted.<plane>`, a
/// different file, so a detector checking `world.halted` reads only a work halt.
fn plane_stamp(plane: Plane) -> PathBuf {
    let base = halt_stamp_or_die();
    if plane == Plane::Work {
        return base;
    }
    let mut name = base.into_os_string();
    name.push(format!(".{}", plane.name()));
    PathBuf::from(name)
}

fn plane_names(planes: &BTreeSet<Plane>) -> String {
    planes.iter().map(|p| p.name()).collect::<Vec<_>>().join("+")
}

/// One `TIMER_PRIORITY` slot, resolved or not: `enumerate_timers`'s own per-base answer.
/// sp-ivfu3-2: a base that resolves to NEITHER unit must never be silently treated as the
/// plain name — `status`/`start`/`stop` all handle `Missing` explicitly, never acting on
/// a name systemd has never heard of.
enum TimerSlot {
    Resolved(String),
    Missing { base: &'static str, reason: String },
}

/// Every timer world.sh acts on: TIMER_PRIORITY first (instance-qualified if that form is
/// known to systemd AT ALL, else plain if THAT is known, else `Missing` — see
/// `sysctl::choose_timer`'s own doc for why this is existence, not enabled/active state),
/// then every other `spira-*.timer` systemd reports — loaded or not — deduplicated in the
/// order first seen.
fn enumerate_timers() -> Vec<TimerSlot> {
    if let Some(e) = sysctl::bus_unreachable() {
        die(&e);
    }
    let sfx = sfx_or_die();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut add_resolved = |t: String, out: &mut Vec<TimerSlot>| {
        if seen.insert(t.clone()) {
            out.push(TimerSlot::Resolved(t));
        }
    };
    for base in sysctl::TIMER_PRIORITY {
        match sysctl::resolve_timer(base, &sfx) {
            Ok(name) => add_resolved(name, &mut out),
            Err(reason) => out.push(TimerSlot::Missing { base, reason }),
        }
    }
    for line in sysctl::run_lines(&["list-unit-files", "spira-*.timer", "--no-legend"]) {
        if let Some(u) = sysctl::first_field(&line) {
            add_resolved(u.to_string(), &mut out);
        }
    }
    for line in sysctl::run_lines(&["list-units", "spira-*.timer", "--all", "--no-legend"]) {
        if let Some(u) = sysctl::first_field(&line) {
            add_resolved(u.to_string(), &mut out);
        }
    }
    out
}

/// Every `Missing` base in `timers`, formatted `"<base>: <reason>"` — the named refusal
/// `cmd_start`/`cmd_stop` print and abort on, never silently acting on a phantom unit.
fn missing_timers(timers: &[TimerSlot]) -> Vec<String> {
    timers
        .iter()
        .filter_map(|s| match s {
            TimerSlot::Missing { base, reason } => Some(format!("{base}: {reason}")),
            TimerSlot::Resolved(_) => None,
        })
        .collect()
}

fn work_services(planes: &BTreeSet<Plane>) -> Vec<String> {
    let sfx = sfx_or_die();
    sysctl::run_lines(&["list-units", "spira-*.service", "--state=active", "--no-legend"])
        .into_iter()
        .filter_map(|l| sysctl::first_field(&l).map(str::to_string))
        .filter(|u| !spira_world::proc::is_excluded_work_service(u, &sfx))
        .filter(|u| planes.contains(&sysctl::plane_of(u)))
        .collect()
}

fn dwarf_pidfiles() -> Vec<PathBuf> {
    let run = run_or_die();
    let Ok(rd) = std::fs::read_dir(&run) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("dwarf-") && n.ends_with(".pid"))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

fn dwarf_lease_running(pf: &std::path::Path) -> bool {
    hel::reap::dwarf_alive(pf)
}

fn live_dwarf_pidfiles() -> Vec<PathBuf> {
    dwarf_pidfiles().into_iter().filter(|p| dwarf_lease_running(p)).collect()
}

/// `dwarf-<rune>-<bead>.pid` -> `<bead>` (drop the `dwarf-` prefix, then everything up to
/// and including the first remaining `-`), matching world.sh's own
/// `bead="${bead#dwarf-}"; bead="${bead#*-}"`.
fn bead_of_pidfile(path: &std::path::Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let rest = stem.strip_prefix("dwarf-").unwrap_or(stem);
    match rest.split_once('-') {
        Some((_, bead)) => bead.to_string(),
        None => String::new(),
    }
}

fn mail_root_or_die() -> PathBuf {
    spira_config::process::cfg("SPIRA_MAIL").map(PathBuf::from).unwrap_or_else(|e| die(&e))
}

fn live_beads() -> Vec<String> {
    let mut v = Vec::new();
    for pf in dwarf_pidfiles() {
        let bead = bead_of_pidfile(&pf);
        if !bead.is_empty() && dwarf_lease_running(&pf) {
            v.push(bead);
        }
    }
    v
}

fn deadline_utc(secs_from_now: u64) -> String {
    spira_config::bounded::bounded("date")
        .args(["-u", "-d", &format!("+{secs_from_now} seconds"), "+%H:%M:%SZ"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn notify_dwarves(notifier: &mut spira_world::notice::Notifier, what: &str, elapsed: u64, limit: u64, live: &[String]) {
    let mail = mail_root_or_die();
    notifier.tick(elapsed, limit, live, |bead, stage, remaining| {
        let body = spira_world::notice::message(stage, what, remaining, &deadline_utc(remaining));
        if spira_world::notice::send(&mail, bead, &body) {
            println!("  noticed {bead}: {remaining}s remain");
        }
    });
}

const USAGE: &str = "usage: world.sh {stop [--why \"...\"] [--work|--observability|--maintenance|--all|--hard] [--round-drain] [--round-drain-timeout SECS] [--grace SECS] | drain [--until WHAT] [--owner O] [--owner-pid PID | --owner-unit UNIT] [--reboot] [--timeout SECS | --deadline SECS] | drain-check | resume | start [--work|--observability|--maintenance|--all] | pause --repo R [--lanes claims,rounds,publish] --reason WHY --until COND [--owner O] | pause --reserve BEADS --reason WHY [--owner O] | unpause --repo R [--lanes L] | unpause --reserve BEADS | status}";

fn cmd_stop(args: &[String]) -> i32 {
    let mut why = String::new();
    let planes = selected_planes(args, &[Plane::Work]);
    let halts_work = planes.contains(&Plane::Work);
    let mut round_drain = false;
    let mut grace: u64 = 0;
    // batch-job: the drain waits for in-flight round work to finish
    let mut round_drain_timeout = std::time::Duration::from_secs(1800);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--why" => {
                why = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--grace" => {
                grace = args.get(i + 1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                i += 2;
            }
            "--round-drain" => {
                round_drain = true;
                i += 1;
            }
            "--round-drain-timeout" => {
                if let Some(n) = args.get(i + 1).and_then(|v| v.parse::<u64>().ok()) {
                    round_drain_timeout = std::time::Duration::from_secs(n);
                }
                i += 2;
            }
            "--work" | "--observability" | "--maintenance" | "--all" | "--hard" => i += 1,
            other => {
                eprintln!("spira: stop: unrecognised argument {other:?} — a reason is given as --why \"...\", never positionally; nothing was stopped");
                eprintln!("{USAGE}");
                return 64;
            }
        }
    }

    // sp-2bkpn: a halt used to interrupt an in-flight Concierge round on the round VM
    // (batcher-cut, round-vm run) without ever knowing one existed. Name it before doing
    // anything else, and — with --round-drain — wait for it to clear first, the same shape
    // `drain` already gives live dwarves.
    if let Some(desc) = spira_world::round::read().filter(|_| halts_work).and_then(|s| spira_world::round::in_flight_description(&s)) {
        eprintln!("spira: {desc}");
        if round_drain {
            eprintln!("spira: --round-drain given — waiting up to {}s for it to clear before halting", round_drain_timeout.as_secs());
            let cleared = spira_world::round::wait_for_clear(
                round_drain_timeout,
                // batch-job: poll interval of the round drain wait
                std::time::Duration::from_secs(10),
                |d| std::thread::sleep(d),
                std::time::Instant::now,
                spira_world::round::read,
            );
            if !cleared {
                eprintln!("spira: round-vm still provisioning after {}s — halting anyway (it was not blocking, only named); rerun with a longer --round-drain-timeout to wait further", round_drain_timeout.as_secs());
            } else {
                eprintln!("spira: round-vm clear — proceeding with the halt");
            }
        } else {
            eprintln!("spira: proceeding without --round-drain — this halt may interrupt it. Use --round-drain to wait first.");
        }
    }

    println!("spira: halting {}", plane_names(&planes));
    let timers = enumerate_timers();
    let missing = missing_timers(&timers);
    if !missing.is_empty() {
        eprintln!("spira: REFUSING to stop — {} essential timer(s) resolve to no systemd unit at all:", missing.len());
        for m in &missing {
            eprintln!("  {m}");
        }
        return 1;
    }
    // Stamp before stopping anything: a dwarf exiting under the slay calls its successor
    // from ExecStopPost, and only a stamp already in place gates that.
    let run = run_or_die();
    let _ = std::fs::create_dir_all(&run);
    for plane in &planes {
        let _ = std::fs::write(plane_stamp(*plane), format!("{}\nwhy: {}\n", now_iso(), if why.is_empty() { "unstated" } else { &why }));
    }

    if halts_work && grace > 0 {
        let mut notifier = spira_world::notice::Notifier::default();
        let mut elapsed = 0;
        loop {
            let live = live_beads();
            if live.is_empty() {
                break;
            }
            notify_dwarves(&mut notifier, "stopping", elapsed, grace, &live);
            if elapsed >= grace {
                println!("spira: grace of {grace}s elapsed — slaying what remains");
                break;
            }
            let step = (grace - elapsed).min(10);
            std::thread::sleep(std::time::Duration::from_secs(step));
            elapsed += step;
        }
    }

    for t in &timers {
        let TimerSlot::Resolved(t) = t else { continue };
        if !planes.contains(&sysctl::plane_of(t)) {
            continue;
        }
        if sysctl::run_ok(&["stop", t]) {
            println!("  stopped {t}");
        }
    }
    if planes.contains(&Plane::Observability) {
        let mut units = BTreeSet::new();
        for l in sysctl::run_lines(&["list-units", "spira-watch@*", "--no-legend"]) {
            if let Some(u) = sysctl::first_field(&l) {
                units.insert(u.to_string());
            }
        }
        let sfx = sfx_or_die();
        for l in sysctl::run_lines(&["list-units", &format!("spira-watch-*{sfx}.service"), "--state=active", "--no-legend"]) {
            if let Some(u) = sysctl::first_field(&l) {
                units.insert(u.to_string());
            }
        }
        for u in units {
            if sysctl::run_ok(&["stop", &u]) {
                println!("  stopped {u}");
            }
        }
    }

    let mut svc_failed = false;
    for svc in work_services(&planes) {
        if sysctl::run_ok(&["stop", &svc]) {
            println!("  stopped {svc}");
        } else {
            eprintln!("  WARNING: could not stop {svc} — still running");
            svc_failed = true;
        }
    }

    if halts_work {
        let mut n = 0u32;
        for pf in dwarf_pidfiles() {
            let pid = std::fs::read_to_string(&pf).unwrap_or_default().trim().to_string();
            if !dwarf_lease_running(&pf) {
                let _ = std::fs::remove_file(&pf);
                continue;
            }
            let bead = bead_of_pidfile(&pf);
            if bead.is_empty() {
                continue;
            }
            println!("  slaying {bead} (pid {pid})");
            let why_text = if why.is_empty() { "the world was stopped".to_string() } else { why.clone() };
            let ok = Command::new(spira_config::release_env::tool("slay"))
                .args(["--bead", &bead, "--keep-work", "--why", &why_text])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                println!("    slay.sh could not stop {bead} — left running, say so rather than pretend");
            }
            n += 1;
        }

        let mut stray = 0u32;
        for pf in live_dwarf_pidfiles() {
            let bead = bead_of_pidfile(&pf);
            println!("  WARNING: {bead} — its identity lease is still running; slay did not stop it; run: slay.sh --bead {bead}");
            stray += 1;
        }
        let units = sysctl::live_dwarf_units();
        if stray == 0 && !units.is_empty() {
            for u in &units {
                println!("  WARNING: {u} is live under systemd — not stopped; inspect: systemctl --user status {u}");
            }
            stray = units.len() as u32;
        }
        if n == 0 && stray == 0 {
            println!("  no live dwarves");
        }
    }

    if svc_failed {
        eprintln!("spira: stop INCOMPLETE — work service(s) could not be stopped (see warnings above)");
        return 1;
    }
    let left: Vec<&str> = PLANES.iter().filter(|p| !planes.contains(p)).map(|p| p.name()).collect();
    println!("spira: STOPPED ({}). Dolt and the cockpit are untouched.{} Restart with: world.sh start", plane_names(&planes), if left.is_empty() { String::new() } else { format!(" Still running: {}.", left.join(", ")) });
    0
}

fn cmd_start(args: &[String]) -> i32 {
    let planes = selected_planes(args, &[Plane::Work, Plane::Observability]);
    println!("spira: starting {}", plane_names(&planes));
    let ctrl_path = ctrl_path_or_die();
    let ctrl_data = spira_ctrl::read(&ctrl_path).unwrap_or_default();
    let suspended = spira_ctrl::load_suspended(&ctrl_data);
    let instance = instance_or_die();

    let timers = enumerate_timers();
    let missing = missing_timers(&timers);
    if !missing.is_empty() {
        eprintln!("spira: REFUSING to start — {} essential timer(s) resolve to no systemd unit at all:", missing.len());
        for m in &missing {
            eprintln!("  {m}");
        }
        return 1;
    }
    let mut degraded = Vec::new();
    for t in &timers {
        let TimerSlot::Resolved(t) = t else { continue };
        if !planes.contains(&sysctl::plane_of(t)) {
            continue;
        }
        let is_enabled_disabled = sysctl::run(&["is-enabled", t]) == "disabled";
        let subj = sysctl::subject_of(t, &instance);
        let action = sysctl::start_action(is_enabled_disabled, suspended.get(&subj).map(String::as_str));
        match action {
            StartAction::SkipSuspended(reason) => {
                if reason.is_empty() {
                    println!("  skipped {t} (suspended)");
                } else {
                    println!("  skipped {t} (suspended: {reason})");
                }
            }
            StartAction::SkipDisabled => {
                println!("  skipped {t} (disabled)");
                if sysctl::is_essential_timer(t, &instance, sysctl::TIMER_PRIORITY) {
                    degraded.push(t.clone());
                }
            }
            StartAction::Start => {
                if sysctl::run_ok(&["start", t]) {
                    println!("  started {t}");
                }
            }
        }
    }

    let sfx = sfx_or_die();
    let mut watchers = BTreeSet::new();
    if planes.contains(&Plane::Observability) {
        for l in sysctl::run_lines(&["list-unit-files", "spira-watch@*", "--no-legend"]) {
            if let Some(u) = sysctl::first_field(&l) {
                watchers.insert(u.to_string());
            }
        }
        for l in sysctl::run_lines(&["list-unit-files", &format!("spira-watch-*{sfx}.service"), "--no-legend"]) {
            if let Some(u) = sysctl::first_field(&l) {
                watchers.insert(u.to_string());
            }
        }
    }
    for u in watchers {
        if sysctl::run(&["show", &u, "-p", "Type", "--value"]) == "oneshot" {
            continue;
        }
        if sysctl::run(&["is-active", &u]) == "active" {
            continue;
        }
        if sysctl::run_ok(&["start", &u]) {
            println!("  started {u}");
        }
    }

    let mut mail_units = BTreeSet::new();
    for l in sysctl::run_lines(&["list-unit-files", &format!("spira-mail-deliver{sfx}.service"), "--no-legend"]) {
        if let Some(u) = sysctl::first_field(&l) {
            mail_units.insert(u.to_string());
        }
    }
    for l in sysctl::run_lines(&["list-unit-files", "spira-mail-deliver.service", "--no-legend"]) {
        if let Some(u) = sysctl::first_field(&l) {
            mail_units.insert(u.to_string());
        }
    }
    for u in mail_units {
        if !planes.contains(&sysctl::plane_of(&u)) {
            continue;
        }
        let base = u.strip_suffix(".service").unwrap_or(&u);
        let subj = sysctl::subject_of(&format!("{base}.timer"), &instance);
        if let Some(reason) = suspended.get(&subj) {
            if reason.is_empty() {
                println!("  skipped {u} (suspended)");
            } else {
                println!("  skipped {u} (suspended: {reason})");
            }
            continue;
        }
        if sysctl::run(&["is-enabled", &u]) == "disabled" {
            continue;
        }
        if sysctl::run(&["is-active", &u]) == "active" {
            continue;
        }
        if sysctl::run_ok(&["start", &u]) {
            println!("  started {u}");
        }
    }

    if planes.contains(&Plane::Work) {
        let (lifted, failed) = spira_world::checkpoint::lift_all();
        for b in &lifted {
            println!("  resumable {b} (checkpoint lifted)");
        }
        for b in &failed {
            eprintln!("  WARNING: checkpoint hold on {b} not lifted — `spira-lc unhold {b} wait` (it expires on its own)");
        }
    }
    for plane in &planes {
        let _ = std::fs::remove_file(plane_stamp(*plane));
    }
    if !degraded.is_empty() {
        eprintln!(
            "spira: RUNNING (DEGRADED: {} essential timer(s) disabled with no recorded suspension: {})",
            degraded.len(),
            degraded.join(" ")
        );
        return 1;
    }
    println!("spira: RUNNING");
    0
}

fn cmd_drain(args: &[String]) -> i32 {
    let mut dtimeout: u64 = env::var("SPIRA_DRAIN_TTL").ok().and_then(|v| v.parse().ok()).unwrap_or(1800);
    let mut dfor: u64 = dtimeout;
    let mut dslay = false;
    let mut reboot = false;
    let mut for_given = false;
    let mut until = String::new();
    let mut owner = "concierge".to_string();
    let mut owner_pid: Option<u32> = None;
    let mut owner_unit: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reboot" => {
                reboot = true;
                i += 1;
            }
            "--until" => {
                until = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--owner" => {
                owner = args.get(i + 1).cloned().unwrap_or(owner);
                i += 2;
            }
            "--owner-pid" => {
                owner_pid = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "--owner-unit" => {
                owner_unit = args.get(i + 1).cloned().filter(|v| !v.is_empty());
                i += 2;
            }
            "--timeout" => {
                dtimeout = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1800);
                i += 2;
            }
            "--for" => {
                dfor = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1800);
                for_given = true;
                i += 2;
            }
            "--deadline" => {
                dtimeout = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1800);
                dslay = true;
                i += 2;
            }
            _ => break,
        }
    }

    let run = run_or_die();
    let _ = std::fs::create_dir_all(&run);
    let exe = env::args().next().unwrap_or_else(|| "world.sh".to_string());
    let exe_name = exe.clone();
    let drain = spira_world::drain::Drain {
        owner,
        until: if until.is_empty() { format!("{exe_name} resume") } else { until },
        owner_pid,
        owner_unit,
        cap_secs: Some(if dslay || !for_given { dtimeout } else { dfor }),
        since_epoch: Some(now_epoch()),
    };
    let _ = std::fs::remove_file(spira_world::drain::paged_marker(&drain_stamp_or_die()));
    let _ = std::fs::write(
        drain_stamp_or_die(),
        format!(
            "{}\ncalls gated in call_rune; loop and landing still running. Lift with: {exe} resume\n{}\n{}",
            Command::new("date").arg("+%Y-%m-%d %H:%M:%S %Z").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default(),
            spira_world::DRAIN_HOLDS_UNTIL_RESUME,
            drain.render()
        ),
    );
    if reboot {
        let marker = spira_world::reboot_marker().unwrap_or_else(|e| die(&e));
        let _ = std::fs::write(&marker, format!("{}\nplanned reboot; lift with: cockpit.sh resume\n", now_iso()));
    }
    println!("spira: draining — no new dwarves; loop, landing and reaping continue");

    let limit = if dslay { dtimeout } else { dfor };
    let mut notifier = spira_world::notice::Notifier::default();
    let mut waited: u64 = 0;
    let captured = live_beads();
    if !captured.is_empty() {
        println!("spira: waiting for {} dwarf(s): {}", captured.len(), captured.join(" "));
    }
    let mut ended: BTreeSet<String> = BTreeSet::new();
    loop {
        let now_live = live_beads();
        for b in &captured {
            if !now_live.contains(b) && ended.insert(b.clone()) {
                println!("  {b} ended ({waited}s)");
            }
        }
        let n = if captured.is_empty() {
            let procs = live_dwarf_pidfiles().len() as u64;
            procs.max(sysctl::live_dwarf_units().len() as u64)
        } else {
            captured.iter().filter(|b| now_live.contains(b)).count() as u64
        };
        if n == 0 {
            break;
        }
        let pending: Vec<String> = captured.iter().filter(|b| now_live.contains(b)).cloned().collect();
        notify_dwarves(&mut notifier, "draining", waited, limit, &pending);
        if waited >= dtimeout {
            if dslay {
                eprintln!("spira: drain deadline reached — slaying {n} dwarf(s)");
                for pf in dwarf_pidfiles() {
                    let pid = std::fs::read_to_string(&pf).unwrap_or_default().trim().to_string();
                    if !dwarf_lease_running(&pf) {
                        let _ = std::fs::remove_file(&pf);
                        continue;
                    }
                    let bead = bead_of_pidfile(&pf);
                    if bead.is_empty() {
                        continue;
                    }
                    println!("  slaying {bead} (pid {pid})");
                    let ok = Command::new(spira_config::release_env::tool("slay"))
                        .args(["--bead", &bead, "--keep-work", "--reopen", "--why", "drain deadline reached"])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false);
                    if !ok {
                        eprintln!("    slay.sh could not stop {bead} — left running");
                    }
                }
                println!("spira: DRAINED — dwarves slain at deadline. Calls remain gated.");
                println!("spira: resume with: {exe} resume");
                return 0;
            }
            eprintln!("spira: NOT DRAINED — {n} dwarf(s) still live after {dtimeout}s: {}", pending.join(" "));
            eprintln!("spira: calls REMAIN GATED. Lift with: {exe} resume");
            drain_check_and_page();
            return 1;
        }
        if waited % 60 == 0 && waited > 0 {
            println!("  {n} dwarf(s) still working ({waited}s elapsed)");
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
        waited += 10;
    }

    println!("spira: DRAINED — no dwarf running, calls gated");
    println!("spira: resume with: {exe} resume");
    0
}

fn owner_alive(d: &spira_world::drain::Drain) -> Option<bool> {
    if let Some(pid) = d.owner_pid {
        return Some(std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
    d.owner_unit.as_deref().map(|u| sysctl::run(&["is-active", u]) == "active")
}

fn page_concierge(subject: &str, body: &str) -> bool {
    use std::io::Write;
    let Ok(mut child) = spira_config::bounded::bounded("mail")
        .args(["send", "concierge", "--from", "world <world@spira>", "--subject", subject, "--kind", "alert"])
        .env("SPIRA_MAIL_LINT_CONSIDERED", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn drain_check_and_page() -> spira_world::drain::Outcome {
    let stragglers = live_beads();
    spira_world::drain::check(&drain_stamp_or_die(), now_epoch(), owner_alive, &stragglers, page_concierge)
}

fn cmd_drain_check() -> i32 {
    use spira_world::drain::Outcome;
    match drain_check_and_page() {
        Outcome::PageFailed => {
            eprintln!("spira: drain-check could not page the concierge — will retry");
            1
        }
        Outcome::PagedCap => {
            println!("spira: drain past its cap — paged; the drain stays in force");
            0
        }
        Outcome::PagedOrphan => {
            println!("spira: drain owner is gone — reported as orphaned; the drain stays in force");
            0
        }
        Outcome::Quiet | Outcome::NotDraining => 0,
    }
}

fn cmd_resume() -> i32 {
    let _ = std::fs::remove_file(spira_world::reboot_marker().unwrap_or_else(|e| die(&e)));
    let _ = std::fs::remove_file(spira_world::drain::paged_marker(&drain_stamp_or_die()));
    if drain_stamp_or_die().is_file() {
        let _ = std::fs::remove_file(drain_stamp_or_die());
        println!("spira: calls resumed");
    } else {
        println!("spira: was not draining — nothing to resume");
    }
    0
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn ids_of(csv: &str) -> Vec<String> {
    csv.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn cmd_pause(args: &[String]) -> i32 {
    let run = run_or_die();
    let owner = flag(args, "--owner").unwrap_or("concierge");
    let reason = flag(args, "--reason").unwrap_or("");
    let until = flag(args, "--until").unwrap_or("");
    let now = now_epoch();
    match (flag(args, "--repo"), flag(args, "--reserve")) {
        (Some(repo), None) => {
            let lanes = match spira_config::scoped_stop::parse_lanes(flag(args, "--lanes").unwrap_or("claims,rounds,publish")) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("spira: world pause: {e}");
                    return 64;
                }
            };
            match spira_config::scoped_stop::pause_repo(&run, repo, &lanes, owner, reason, until, now) {
                Ok(s) => {
                    println!("spira: paused {} lane(s) of {repo}: now stopped [{}]; lift with: world.sh unpause --repo {repo}", lanes.join(","), s.lanes.join(","));
                    0
                }
                Err(e) => {
                    eprintln!("spira: world pause: {e}");
                    64
                }
            }
        }
        (None, Some(csv)) => {
            let ids = ids_of(csv);
            if ids.is_empty() {
                eprintln!("spira: world pause: --reserve names no bead");
                return 64;
            }
            for id in &ids {
                if let Err(e) = spira_config::scoped_stop::reserve(&run, id, owner, reason, until, now) {
                    eprintln!("spira: world pause: {e}");
                    return 64;
                }
                println!("spira: reserved {id}: no dwarf will claim it; lift with: world.sh unpause --reserve {id}");
            }
            0
        }
        _ => {
            eprintln!("{USAGE}");
            64
        }
    }
}

fn cmd_unpause(args: &[String]) -> i32 {
    let run = run_or_die();
    match (flag(args, "--repo"), flag(args, "--reserve")) {
        (Some(repo), None) => {
            let lanes = match flag(args, "--lanes") {
                Some(l) => match spira_config::scoped_stop::parse_lanes(l) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("spira: world unpause: {e}");
                        return 64;
                    }
                },
                None => Vec::new(),
            };
            match spira_config::scoped_stop::unpause_repo(&run, repo, &lanes) {
                Ok(None) => println!("spira: {repo} has no scoped stop — nothing to lift"),
                Ok(Some(left)) if left.is_empty() => println!("spira: {repo}: scoped stop lifted"),
                Ok(Some(left)) => println!("spira: {repo}: still stopped [{}]", left.join(",")),
                Err(e) => {
                    eprintln!("spira: world unpause: {e}");
                    return 1;
                }
            }
            0
        }
        (None, Some(csv)) => {
            for id in ids_of(csv) {
                match spira_config::scoped_stop::unreserve(&run, &id) {
                    Ok(true) => println!("spira: {id}: reservation lifted"),
                    Ok(false) => println!("spira: {id} was not reserved"),
                    Err(e) => {
                        eprintln!("spira: world unpause: {e}");
                        return 1;
                    }
                }
            }
            0
        }
        _ => {
            eprintln!("{USAGE}");
            64
        }
    }
}

fn print_scoped_stops() {
    let run = run_or_die();
    let review = spira_config::process::cfg_parse::<u64>("SPIRA_SCOPED_STOP_REVIEW_SECS").unwrap_or_else(|e| die(&e));
    let now = now_epoch();
    for s in spira_config::scoped_stop::repo_stops(&run) {
        println!("spira: {}", spira_config::scoped_stop::describe_stop(&s, now, review));
    }
    for r in spira_config::scoped_stop::reservations(&run) {
        println!("spira: {}", spira_config::scoped_stop::describe_reservation(&r, now, review));
    }
}

fn cmd_status() -> i32 {
    print_scoped_stops();
    if drain_stamp_or_die().is_file() {
        let text = std::fs::read_to_string(drain_stamp_or_die()).unwrap_or_default();
        let mut lines = text.lines();
        println!("spira: DRAINING since {}", lines.next().unwrap_or(""));
        if let Some(l2) = lines.next() {
            println!("{l2}");
        }
        let d = spira_world::drain::Drain::parse(&text);
        let alive = match owner_alive(&d) {
            Some(true) => " (alive)",
            Some(false) => " (DEAD — orphaned drain; still in force)",
            None => "",
        };
        println!("  {}{alive}", d.describe(now_epoch()));
    }
    if spira_world::reboot_marker().unwrap_or_else(|e| die(&e)).is_file() {
        println!("spira: REBOOT-DRAIN — drained for a planned reboot; lift with: cockpit.sh resume");
    }
    let instance = instance_or_die();
    if halt_stamp_or_die().is_file() {
        let text = std::fs::read_to_string(halt_stamp_or_die()).unwrap_or_default();
        let mut lines = text.lines();
        println!("spira: HALTED since {}", lines.next().unwrap_or(""));
        if let Some(l2) = lines.next() {
            println!("{l2}");
        }
    } else {
        println!("spira: not halted by world.sh");
        let ctrl_path = ctrl_path_or_die();
        let ctrl_data = spira_ctrl::read(&ctrl_path).unwrap_or_default();
        let suspended = spira_ctrl::load_suspended(&ctrl_data);
        let mut degraded = Vec::new();
        for slot in enumerate_timers() {
            let TimerSlot::Resolved(t) = slot else { continue };
            if !sysctl::is_essential_timer(&t, &instance, sysctl::TIMER_PRIORITY) {
                continue;
            }
            let subj = sysctl::subject_of(&t, &instance);
            if suspended.contains_key(&subj) {
                continue;
            }
            if sysctl::run(&["is-enabled", &t]) == "disabled" {
                degraded.push(t);
            }
        }
        if !degraded.is_empty() {
            println!(
                "spira: DEGRADED — {} essential timer(s) disabled with no recorded suspension: {}",
                degraded.len(),
                degraded.join(" ")
            );
        }
    }

    let slots = enumerate_timers();
    for slot in &slots {
        if let TimerSlot::Missing { base, reason } = slot {
            println!("  {:<26} MISSING ({reason})", format!("{base}.timer"));
        }
    }
    let by_plane: Vec<(Plane, String)> = slots
        .iter()
        .filter_map(|s| match s {
            TimerSlot::Resolved(t) => Some((sysctl::plane_of(t), t.clone())),
            TimerSlot::Missing { .. } => None,
        })
        .collect();
    for plane in PLANES {
        let stamp = plane_stamp(plane);
        let state = if stamp.is_file() { "STOPPED" } else { "RUNNING" };
        println!("spira: plane {}: {state}", plane.name());
        for (_, t) in by_plane.iter().filter(|(p, _)| *p == plane) {
            let unit_state = sysctl::run(&["is-active", t]);
            let svc = format!("{}.service", t.strip_suffix(".timer").unwrap_or(t));
            let svc_result = sysctl::run(&["show", &svc, "-p", "Result", "--value"]);
            print!("{}", sysctl::status_timer_row(t, &unit_state, &svc_result));
        }
    }
    println!("  {:<26} {}", "dolt-beads.service", sysctl::run(&["is-active", "dolt-beads.service"]));

    let landing = sysctl::run(&["is-active", "spira-landing.service"]);
    println!("  {:<26} {}", "spira-landing.service", if landing.is_empty() { "inactive".to_string() } else { landing });
    for line in sysctl::run_lines(&["list-units", "spira-*.service", "--state=active", "--no-legend"]) {
        let Some(svc) = sysctl::first_field(&line) else { continue };
        if svc == "spira-landing.service" || svc.starts_with("spira-cockpit") || svc.starts_with("spira-loom") || svc.starts_with("spira-watch@") {
            continue;
        }
        println!("  {:<26} {}", svc, sysctl::run(&["is-active", svc]));
    }

    let home = spira_world::locate_home(&env::current_exe().unwrap_or_default()).unwrap_or_default();
    let prod = resolved_prod(&home);
    let worker_paths = spira_world::live_worker_paths(&home, &prod);
    let worker_refs: Vec<&str> = worker_paths.iter().map(String::as_str).collect();
    let wcount = spira_world::proc::live_workers(std::path::Path::new("/proc"), &worker_refs).len();
    println!("  {:<26} {}", "live workers (/proc)", wcount);

    let a = live_dwarf_pidfiles().len();
    let a = a.max(sysctl::live_dwarf_units().len());
    println!("  live dwarves: {a}");

    // sp-2bkpn: name an in-flight round so a halt is never the first anyone hears of it.
    match spira_world::round::read() {
        Some(s) => match spira_world::round::in_flight_description(&s) {
            Some(desc) => println!("  {desc}"),
            None => println!("  round-vm: no round in flight"),
        },
        None => println!("  round-vm: not on PATH or could not be read — unknown"),
    }
    0
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| "status".to_string());
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    spira_otel::oneshot::begin(spira_config::process::metrics_endpoint().as_deref(), "spira-world", &cmd);
    let rc = match cmd.as_str() {
        "stop" => cmd_stop(rest),
        "start" => cmd_start(rest),
        "drain" => cmd_drain(rest),
        "resume" => cmd_resume(),
        "drain-check" => cmd_drain_check(),
        "pause" => cmd_pause(rest),
        "unpause" => cmd_unpause(rest),
        "status" => cmd_status(),
        // `TIMER_PRIORITY`, one base name per line — the seam watchtower.sh's
        // disabled-timer-check reads instead of `WORLD_LIB=1 . world.sh`, which stopped
        // being possible the moment world.sh became a binary (sp-6onps).
        "timer-priority" => {
            for t in sysctl::TIMER_PRIORITY {
                println!("{t}");
            }
            0
        }
        _ => {
            eprintln!("{USAGE}");
            64
        }
    };
    if rc == 0 && matches!(cmd.as_str(), "stop" | "start" | "drain" | "resume") {
        spira_otel::oneshot::event("spira_world_state_changes", 1, &[("to", &cmd)]);
    }
    spira_otel::oneshot::exit(rc);
}
