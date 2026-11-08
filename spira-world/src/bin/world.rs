//! world.sh — stop and start Spira as a whole. Replaces spira/world.sh (bash).
//!
//! THE LOOP IS THE SCHEDULER. Every loop unit is a task inside `spira-runtime serve`, so
//! `stop` and `drain` are `scheduler halt <reason>` (every task's effects held, observation
//! continues) and `start` and `resume` are `scheduler resume`, on the control socket
//! `SPIRA_RUNTIME_SOCKET`; `status` reads `scheduler status`. The `world.halted` stamps are
//! still written by the same action for every reader not yet converted. The keep list
//! (`spira_world::scheduler::KEEP`: the scheduler, lc-serve's socket, dolt, the watchdog) is
//! never stopped. `resume` never lifts a stopped world; `start` does.
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
//!   world.sh scheduler-status [--json]   (exit 0 running, 1 not answering, 2 halted)
//!   world.sh guard        (every work unit's ExecStartPre: refuses while the work plane is halted)
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

use spira_world::scheduler::{self, Control};
use spira_world::sysctl::{self, Plane, PLANES};
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

/// `SPIRA_PROD`'s declared value, resolved once here and handed to [`spira_prod_or_home`] —
/// never read from the environment inside that pure function.
fn resolved_prod(home: &std::path::Path) -> PathBuf {
    let prod = spira_config::process::cfg("SPIRA_PROD").unwrap_or_else(|e| die(&e));
    spira_prod_or_home(home, &prod)
}

fn record_stop(kind: tsd::stop::Kind, reason: tsd::stop::Reason, actor: &str, repo: Option<&str>, bead: Option<&str>, detail: &str) {
    if let Err(e) = spira_world::stoprec::record(&run_or_die(), now_epoch(), kind, reason, actor, repo, bead, detail) {
        eprintln!("spira: WARNING — this {} is not in the loop-stop family, so goodput cannot charge it: {e}", kind.as_str());
    }
}

fn cause_or_die(args: &[String], default: tsd::stop::Reason) -> tsd::stop::Reason {
    spira_world::stoprec::cause(args, default).unwrap_or_else(|e| die(&e))
}

fn now_iso() -> String {
    spira_world::clock::utc_iso(spira_world::clock::now())
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

/// `SPIRA_RUNTIME_SOCKET`'s declared value: the door `scheduler status|halt|resume` answers on.
fn runtime_socket_or_die() -> PathBuf {
    spira_config::process::cfg("SPIRA_RUNTIME_SOCKET").map(PathBuf::from).unwrap_or_else(|e| die(&e))
}

/// `scheduler halt <why>` on the runtime's control socket, said in one line either way.
fn halt_scheduler(why: &str) -> bool {
    let sock = runtime_socket_or_die();
    match scheduler::halt(&scheduler::Socket(&sock), why) {
        Ok(reply) => {
            println!("  scheduler halted ({reply}) — effects held, observation continues");
            true
        }
        Err(e) => {
            eprintln!("  WARNING: scheduler halt not journaled — {e}");
            false
        }
    }
}

/// `scheduler resume` on the runtime's control socket, said in one line either way.
fn resume_scheduler() -> bool {
    let sock = runtime_socket_or_die();
    match scheduler::resume(&scheduler::Socket(&sock)) {
        Ok(reply) => {
            println!("  scheduler resumed ({reply})");
            true
        }
        Err(e) => {
            eprintln!("  WARNING: scheduler resume not journaled — {e}");
            false
        }
    }
}

fn work_services(planes: &BTreeSet<Plane>) -> Vec<String> {
    let sfx = sfx_or_die();
    // A oneshot is `activating` for as long as its ExecStart runs; `--state=active` alone
    // skips exactly those. The keep list is never stopped, whatever plane it declares.
    sysctl::run_lines(&["list-units", "spira-*.service", "--state=active,activating,reloading", "--no-legend"])
        .into_iter()
        .filter_map(|l| sysctl::first_field(&l).map(str::to_string))
        .filter(|u| !spira_world::proc::is_excluded_work_service(u, &sfx) && !scheduler::is_kept(u, &sfx))
        .filter(|u| planes.contains(&sysctl::plane_of(u)))
        .collect()
}

/// Installed services that declare the work plane and are not oneshots: the ones no timer
/// starts. A unit declaring no plane is left alone here; start never guesses one into life.
fn work_daemons() -> Vec<String> {
    let sfx = sfx_or_die();
    sysctl::run_lines(&["list-unit-files", "spira-*.service", "--no-legend"])
        .into_iter()
        .filter_map(|l| sysctl::first_field(&l).map(str::to_string))
        .filter(|u| !u.contains('@') && !spira_world::proc::is_excluded_work_service(u, &sfx))
        .filter(|u| {
            let text = sysctl::run(&["cat", u]);
            sysctl::plane_from_unit_text(&text) == Some(Plane::Work) && !sysctl::is_oneshot(&text)
        })
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
    spira_world::clock::utc_hms(spira_world::clock::now() + secs_from_now as i64)
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

const USAGE: &str = "usage: world.sh {stop [--why \"...\"] [--cause operator|incident|deploy|reboot|drain|landing] [--work|--observability|--maintenance|--all|--hard] [--round-drain] [--round-drain-timeout SECS] [--grace SECS] | drain [--until WHAT] [--owner O] [--owner-pid PID | --owner-unit UNIT] [--reboot] [--timeout SECS | --deadline SECS] | drain-check | resume | start [--work|--observability|--maintenance|--all] | pause --repo R [--lanes claims,rounds,publish] [--bead B] [--cause C] --reason WHY --until COND [--owner O] | pause --reserve BEADS --reason WHY [--owner O] | unpause --repo R [--lanes L] | unpause --reserve BEADS | status | scheduler-status [--json] | guard | keep-units | freeze-store | thaw-store}";

fn cmd_stop(args: &[String]) -> i32 {
    let cause = cause_or_die(args, tsd::stop::Reason::Operator);
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
            "--cause" => i += 2,
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
                std::thread::sleep,
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
    // Stamp before stopping anything: a dwarf exiting under the slay calls its successor
    // from ExecStopPost, and only a stamp already in place gates that. The stamp stays for
    // every reader not yet converted to `scheduler status`.
    let run = run_or_die();
    let _ = std::fs::create_dir_all(&run);
    for plane in &planes {
        let _ = std::fs::write(plane_stamp(*plane), format!("{}\nwhy: {}\n", now_iso(), if why.is_empty() { "unstated" } else { &why }));
    }
    if halts_work {
        record_stop(tsd::stop::Kind::Halt, cause, spira_world::stoprec::ACTOR, None, None, &why);
    }
    // The loop is the scheduler's tasks: a work halt is one `scheduler halt`, which holds
    // every task's effects and keeps observing. The scheduler has no per-plane halt.
    let halt_failed = halts_work && !halt_scheduler(if why.is_empty() { "world stop" } else { &why });

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
            let ok = spira_config::release_env::tool("slay").map_err(|e| eprintln!("    {e}")).is_ok_and(|slay| {
                // FORK-EDGE: sibling binary slay (bin/slay.rs holds its logic, not the library).
                Command::new(slay).args(["--bead", &bead, "--keep-work", "--why", &why_text]).output().is_ok_and(|o| o.status.success())
            });
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
    if halt_failed {
        eprintln!("spira: stop INCOMPLETE — the scheduler did not take the halt (see the warning above); the stamp is written, rerun: world.sh stop");
        return 1;
    }
    let left: Vec<&str> = PLANES.iter().filter(|p| !planes.contains(p)).map(|p| p.name()).collect();
    println!("spira: STOPPED ({}). Dolt and the cockpit are untouched.{} Restart with: world.sh start", plane_names(&planes), if left.is_empty() { String::new() } else { format!(" Still running: {}.", left.join(", ")) });
    0
}

fn cmd_start(args: &[String]) -> i32 {
    let planes = selected_planes(args, &[Plane::Work, Plane::Observability]);
    println!("spira: starting {}", plane_names(&planes));

    // Lift before resuming: a reader not yet converted to `scheduler status` still gates on
    // the stamp, so the scheduler's first act after the resume must already find it gone.
    for plane in &planes {
        let _ = std::fs::remove_file(plane_stamp(*plane));
    }
    if planes.contains(&Plane::Work) {
        record_stop(tsd::stop::Kind::Release, tsd::stop::Reason::Operator, spira_world::stoprec::ACTOR, None, None, "world start");
    }
    let mut degraded = Vec::new();
    if planes.contains(&Plane::Work) && !resume_scheduler() {
        degraded.push(format!("the scheduler did not take the resume; check: systemctl --user status spira-runtime{}.service", sfx_or_die()));
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

    // A work-plane daemon has no timer to bring it back, and booting while halted leaves it
    // failed on its start guard, possibly past its start limit; so start lifts that too.
    if planes.contains(&Plane::Work) {
        for u in work_daemons() {
            if sysctl::run(&["is-enabled", &u]) == "disabled" || sysctl::run(&["is-active", &u]) == "active" {
                continue;
            }
            let _ = sysctl::run_ok(&["reset-failed", &u]);
            if sysctl::run_ok(&["start", &u]) {
                println!("  started {u}");
            }
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
    if !degraded.is_empty() {
        eprintln!("spira: RUNNING (DEGRADED: {})", degraded.join("; "));
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
            "{}\ncalls gated; the scheduler observes and holds its effects. Lift with: {exe} resume\n{}\n{}",
            spira_world::clock::local_stamp(spira_world::clock::now()),
            spira_world::DRAIN_HOLDS_UNTIL_RESUME,
            drain.render()
        ),
    );
    if reboot {
        let marker = spira_world::reboot_marker().unwrap_or_else(|e| die(&e));
        let _ = std::fs::write(&marker, format!("{}\nplanned reboot; lift with: cockpit.sh resume\n", now_iso()));
    }
    let drain_reason = if reboot { tsd::stop::Reason::Reboot } else { tsd::stop::Reason::Drain };
    record_stop(tsd::stop::Kind::Halt, drain_reason, &drain.owner, None, None, &drain.until);
    println!("spira: draining — no new dwarves; live dwarves finish, the scheduler observes but holds its effects");
    let drain_why = format!("world drain: {}", drain.until);
    if !halt_scheduler(&drain_why) {
        eprintln!("spira: the drain stamp is written but the scheduler did not take the halt; rerun: {exe} drain");
    }

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
                    let ok = spira_config::release_env::tool("slay").map_err(|e| eprintln!("    {e}")).is_ok_and(|slay| {
                        // FORK-EDGE: sibling binary slay (bin/slay.rs holds its logic, not the library).
                        Command::new(slay).args(["--bead", &bead, "--keep-work", "--reopen", "--why", "drain deadline reached"]).output().is_ok_and(|o| o.status.success())
                    });
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
        if waited.is_multiple_of(60) && waited > 0 {
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
    spira_world::mail_out::send_configured(&spira_world::mail_out::Letter { mailbox: "concierge", subject, kind: "alert", body })
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
    let was_draining = drain_stamp_or_die().is_file();
    if was_draining {
        let _ = std::fs::remove_file(drain_stamp_or_die());
    }
    // A stopped world is lifted by `start`, never by `resume`: the scheduler stays halted
    // while the work plane's stamp is present.
    if halt_stamp_or_die().is_file() {
        println!("spira: {} — the world is stopped; the scheduler stays halted until: world.sh start", if was_draining { "drain lifted" } else { "was not draining" });
        return 0;
    }
    if was_draining {
        record_stop(tsd::stop::Kind::Release, tsd::stop::Reason::Operator, spira_world::stoprec::ACTOR, None, None, "world resume");
    }
    let resumed = resume_scheduler();
    if was_draining {
        println!("spira: calls resumed");
    } else {
        println!("spira: was not draining — nothing to resume");
    }
    if resumed {
        0
    } else {
        1
    }
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
            let cause = cause_or_die(args, tsd::stop::Reason::Operator);
            match spira_config::scoped_stop::pause_repo_typed(&run, repo, &lanes, owner, reason, until, now, cause, flag(args, "--bead")) {
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
    let halted = halt_stamp_or_die().is_file();
    if halted {
        let text = std::fs::read_to_string(halt_stamp_or_die()).unwrap_or_default();
        let mut lines = text.lines();
        println!("spira: HALTED since {}", lines.next().unwrap_or(""));
        if let Some(l2) = lines.next() {
            println!("{l2}");
        }
    } else {
        println!("spira: not halted by world.sh");
    }
    for plane in PLANES {
        let state = if plane_stamp(plane).is_file() { "STOPPED" } else { "RUNNING" };
        println!("spira: plane {}: {state}", plane.name());
    }

    // The loop is the scheduler's tasks: their halt state and per-task last fired, next due
    // and lag come from `scheduler status`, not from a timer list.
    let sock = runtime_socket_or_die();
    match scheduler::Socket(&sock).call(&["scheduler", "status"]).and_then(|d| scheduler::parse_status(&d)) {
        Ok(s) => {
            println!("{}", scheduler::headline(&s));
            if let Some(m) = scheduler::mismatch(halted, &s) {
                println!("{m}");
            }
            for t in &s.tasks {
                println!("{}", scheduler::task_line(t, s.at));
            }
        }
        Err(e) => {
            println!("spira: scheduler UNKNOWN — {e}");
        }
    }
    let sfx = sfx_or_die();
    for u in scheduler::keep_units(&sfx) {
        let state = sysctl::run(&["is-active", &u]);
        let result = scheduler::timer_service(&u).map(|svc| sysctl::run(&["show", &svc, "-p", "Result", "--value"])).unwrap_or_default();
        println!("{}", scheduler::keep_row(&u, &state, &result));
    }
    for line in sysctl::run_lines(&["list-units", "spira-*.service", "--state=active", "--no-legend"]) {
        let Some(svc) = sysctl::first_field(&line) else { continue };
        if scheduler::is_kept(svc, &sfx) || svc.starts_with("spira-cockpit") || svc.starts_with("spira-loom") || svc.starts_with("spira-watch@") {
            continue;
        }
        println!("  {:<30} {}", svc, sysctl::run(&["is-active", svc]));
    }

    match spira_world::locate_home() {
        Some(home) => {
            let prod = resolved_prod(&home);
            let worker_paths = spira_world::live_worker_paths(&home, &prod);
            let worker_refs: Vec<&str> = worker_paths.iter().map(String::as_str).collect();
            let wcount = spira_world::proc::live_workers(std::path::Path::new("/proc"), &worker_refs).len();
            println!("  {:<26} {}", "live workers (/proc)", wcount);
        }
        None => println!("  {:<26} ? (neither SPIRA_HOME nor SPIRA_RELEASE names a harness home)", "live workers (/proc)"),
    }

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

/// Every work-plane unit's `ExecStartPre=`: exit nonzero, naming the halt, while the work
/// plane's stamp is present. A stamp path that cannot be resolved refuses too.
fn cmd_guard() -> i32 {
    let stamp = halt_stamp_or_die();
    match spira_world::guard::halt_refusal(&stamp, std::fs::read_to_string(&stamp).ok().as_deref()) {
        Some(refusal) => {
            eprintln!("{refusal}");
            1
        }
        None => 0,
    }
}

type StoreVerb = fn(&spira_world::freeze::Store, &mut dyn spira_world::freeze::Units) -> Result<Vec<String>, String>;

fn cmd_store(name: &str, rest: &[String], verb: StoreVerb) -> i32 {
    if !rest.is_empty() {
        eprintln!("usage: world.sh {name}   (takes no arguments; refusing {:?})", rest[0]);
        return 2;
    }
    if let Some(e) = sysctl::bus_unreachable() {
        eprintln!("refused: {e}");
        return 1;
    }
    let dir = spira_world::freeze::runtime_unit_dir(spira_config::resolve::current_uid());
    let store = spira_world::freeze::Store::new(dir, &instance_or_die());
    match verb(&store, &mut spira_world::freeze::Systemd) {
        Ok(lines) => {
            for l in lines {
                println!("{l}");
            }
            0
        }
        Err(e) => {
            eprintln!("refused: {e}");
            1
        }
    }
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
        "guard" => cmd_guard(),
        "freeze-store" => cmd_store("freeze-store", rest, spira_world::freeze::freeze),
        "thaw-store" => cmd_store("thaw-store", rest, spira_world::freeze::thaw),
        // `scheduler status` for a shell caller: the headline and task lines, and an exit code
        // that says running (0), not answering (1) or halted (2).
        "scheduler-status" => {
            let sock = runtime_socket_or_die();
            if rest.iter().any(|a| a == "--json") {
                let (rc, doc) = scheduler::raw(&scheduler::Socket(&sock));
                if rc == scheduler::RC_UNANSWERED {
                    eprintln!("{doc}");
                } else {
                    println!("{doc}");
                }
                rc
            } else {
                let (rc, lines) = scheduler::report(&scheduler::Socket(&sock));
                for l in lines {
                    println!("{l}");
                }
                rc
            }
        }
        // The keep list, one unit per line: what a world stop never stops.
        "keep-units" => {
            for t in scheduler::keep_units(&sfx_or_die()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static ACTED: AtomicBool = AtomicBool::new(false);

    fn acts(_: &spira_world::freeze::Store, _: &mut dyn spira_world::freeze::Units) -> Result<Vec<String>, String> {
        ACTED.store(true, Ordering::SeqCst);
        Ok(vec![])
    }

    #[test]
    fn store_verbs_refuse_help_and_unknown_arguments_without_acting() {
        for name in ["freeze-store", "thaw-store"] {
            for arg in ["--help", "-h", "--bogus"] {
                assert_eq!(cmd_store(name, &[arg.to_string()], acts), 2, "{name} {arg}");
            }
        }
        assert!(!ACTED.load(Ordering::SeqCst), "a store verb acted on an argument");
    }
}
