//! world.sh — stop and start Spira as a whole. Replaces spira/world.sh (bash).
//!
//!   world.sh stop [--why "..."] [--work] [--observability] [--maintenance] [--all]
//!                 [--hard] [--round-drain] [--round-drain-timeout SECS]
//!   world.sh start [--work] [--observability] [--maintenance] [--all]
//!   world.sh drain [--timeout SECS | --for SECS | --deadline SECS]
//!   world.sh resume
//!   world.sh status
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
//! clear first — the same shape `drain` already gives live aeons. See `spira_world::round`
//! for exactly what this can and cannot see.
//!
//! AEONS ARE STOPPED THROUGH slay, never killed directly — a bare kill leaves the bead
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
use std::time::{SystemTime, UNIX_EPOCH};

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
    std::process::exit(1);
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

fn now_iso() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn epoch_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
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

fn aeon_pidfiles() -> Vec<PathBuf> {
    let run = run_or_die();
    let Ok(rd) = std::fs::read_dir(&run) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("aeon-") && n.ends_with(".pid"))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

/// `aeon-<fayth>-<bead>.pid` -> `<bead>` (drop the `aeon-` prefix, then everything up to
/// and including the first remaining `-`), matching world.sh's own
/// `bead="${bead#aeon-}"; bead="${bead#*-}"`.
fn bead_of_pidfile(path: &std::path::Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let rest = stem.strip_prefix("aeon-").unwrap_or(stem);
    match rest.split_once('-') {
        Some((_, bead)) => bead.to_string(),
        None => String::new(),
    }
}

fn cmd_stop(args: &[String]) -> i32 {
    let mut why = String::new();
    let planes = selected_planes(args, &[Plane::Work]);
    let halts_work = planes.contains(&Plane::Work);
    let mut round_drain = false;
    let mut round_drain_timeout = std::time::Duration::from_secs(1800);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--why" => {
                why = args.get(i + 1).cloned().unwrap_or_default();
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
            _ => i += 1,
        }
    }

    // sp-2bkpn: a halt used to interrupt an in-flight Concierge round on the round VM
    // (batcher-cut, round-vm run) without ever knowing one existed. Name it before doing
    // anything else, and — with --round-drain — wait for it to clear first, the same shape
    // `drain` already gives live aeons.
    if let Some(desc) = spira_world::round::read().filter(|_| halts_work).and_then(|s| spira_world::round::in_flight_description(&s)) {
        eprintln!("spira: {desc}");
        if round_drain {
            eprintln!("spira: --round-drain given — waiting up to {}s for it to clear before halting", round_drain_timeout.as_secs());
            let cleared = spira_world::round::wait_for_clear(
                round_drain_timeout,
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
        for pf in aeon_pidfiles() {
            let pid = std::fs::read_to_string(&pf).unwrap_or_default().trim().to_string();
            if pid.is_empty() || !std::path::Path::new(&format!("/proc/{pid}")).is_dir() {
                let _ = std::fs::remove_file(&pf);
                continue;
            }
            let bead = bead_of_pidfile(&pf);
            if bead.is_empty() {
                continue;
            }
            println!("  slaying {bead} (pid {pid})");
            let why_text = if why.is_empty() { "the world was stopped".to_string() } else { why.clone() };
            let ok = Command::new("slay")
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
        let home = spira_world::locate_home(&env::current_exe().unwrap_or_default()).unwrap_or_default();
        let prod = spira_prod_or_home(&home);
        let aeon_paths: Vec<String> = vec![
            home.join("aeon.sh").to_string_lossy().into_owned(),
            prod.join("aeon.sh").to_string_lossy().into_owned(),
        ];
        let aeon_path_refs: Vec<&str> = aeon_paths.iter().map(String::as_str).collect();
        let live = spira_world::proc::live_aeons(std::path::Path::new("/proc"), &aeon_path_refs, |pid| {
            let out = sysctl::run(&["status", pid]);
            out.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_string()
        });
        for a in live {
            let mut bead_for_pid = String::new();
            for pf in aeon_pidfiles() {
                let pp = std::fs::read_to_string(&pf).unwrap_or_default().trim().to_string();
                if pp == a.pid {
                    bead_for_pid = bead_of_pidfile(&pf);
                    break;
                }
            }
            if !bead_for_pid.is_empty() {
                println!(
                    "  WARNING: {bead_for_pid} (pid {}) — named by a pidfile but slay did not stop it; run: slay.sh --bead {bead_for_pid}",
                    a.pid
                );
            } else {
                println!(
                    "  WARNING: pid {} ({}) — no pidfile names it; could not resolve to a bead — inspect /proc/{}/cmdline before killing",
                    a.pid,
                    if a.unit.is_empty() { "-" } else { &a.unit },
                    a.pid
                );
            }
            stray += 1;
        }
        if n == 0 && stray == 0 {
            println!("  no live aeons");
        }
    }

    let run = run_or_die();
    let _ = std::fs::create_dir_all(&run);
    for plane in &planes {
        let _ = std::fs::write(plane_stamp(*plane), format!("{}\nwhy: {}\n", now_iso(), if why.is_empty() { "unstated" } else { &why }));
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
    let ctrl_path = env::var_os("SPIRA_CTRL")
        .map(PathBuf::from)
        .unwrap_or_else(|| run_or_die().join("control"));
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
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" => {
                dtimeout = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1800);
                i += 2;
            }
            "--for" => {
                dfor = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1800);
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
    let _ = std::fs::write(
        drain_stamp_or_die(),
        format!(
            "{}\nsummons gated in summon_fayth; loop and landing still running. Lift with: {exe} resume\nexpires {}\n",
            Command::new("date").arg("+%Y-%m-%d %H:%M:%S %Z").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default(),
            epoch_secs() + dfor
        ),
    );
    println!("spira: draining — no new aeons; loop, landing and reaping continue");

    let home = spira_world::locate_home(&env::current_exe().unwrap_or_default()).unwrap_or_default();
    let prod = spira_prod_or_home(&home);
    let aeon_paths: Vec<String> = vec![
        home.join("aeon.sh").to_string_lossy().into_owned(),
        prod.join("aeon.sh").to_string_lossy().into_owned(),
    ];
    let aeon_path_refs: Vec<&str> = aeon_paths.iter().map(String::as_str).collect();

    let mut waited: u64 = 0;
    loop {
        let n = spira_world::proc::live_aeons(std::path::Path::new("/proc"), &aeon_path_refs, |_| String::new()).len() as u64;
        if n == 0 {
            break;
        }
        if waited >= dtimeout {
            if dslay {
                eprintln!("spira: drain deadline reached — slaying {n} aeon(s)");
                for pf in aeon_pidfiles() {
                    let pid = std::fs::read_to_string(&pf).unwrap_or_default().trim().to_string();
                    if pid.is_empty() || !std::path::Path::new(&format!("/proc/{pid}")).is_dir() {
                        let _ = std::fs::remove_file(&pf);
                        continue;
                    }
                    let bead = bead_of_pidfile(&pf);
                    if bead.is_empty() {
                        continue;
                    }
                    println!("  slaying {bead} (pid {pid})");
                    let ok = Command::new("slay")
                        .args(["--bead", &bead, "--keep-work", "--reopen", "--why", "drain deadline reached"])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false);
                    if !ok {
                        eprintln!("    slay.sh could not stop {bead} — left running");
                    }
                }
                println!("spira: DRAINED — aeons slain at deadline. Summons remain gated.");
                println!("spira: resume with: {exe} resume");
                return 0;
            }
            eprintln!("spira: NOT DRAINED — {n} aeon(s) still live after {dtimeout}s");
            eprintln!("spira: summons REMAIN GATED. Lift with: {exe} resume");
            return 1;
        }
        if waited % 60 == 0 && waited > 0 {
            println!("  {n} aeon(s) still working ({waited}s elapsed)");
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
        waited += 10;
    }

    println!("spira: DRAINED — no aeon running, summons gated");
    println!("spira: resume with: {exe} resume");
    0
}

fn cmd_resume() -> i32 {
    if drain_stamp_or_die().is_file() {
        let _ = std::fs::remove_file(drain_stamp_or_die());
        println!("spira: summons resumed");
    } else {
        println!("spira: was not draining — nothing to resume");
    }
    0
}

fn cmd_status() -> i32 {
    if drain_stamp_or_die().is_file() {
        let text = std::fs::read_to_string(drain_stamp_or_die()).unwrap_or_default();
        let mut lines = text.lines();
        println!("spira: DRAINING since {}", lines.next().unwrap_or(""));
        if let Some(l2) = lines.next() {
            println!("{l2}");
        }
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
        let ctrl_path = env::var_os("SPIRA_CTRL").map(PathBuf::from).unwrap_or_else(|| run_or_die().join("control"));
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
    println!("  {:<26} {}", "dolt-beads-test.service", sysctl::run(&["is-active", "dolt-beads-test.service"]));

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
    let prod = spira_prod_or_home(&home);
    let worker_paths = spira_world::live_worker_paths(&home);
    let worker_refs: Vec<&str> = worker_paths.iter().map(String::as_str).collect();
    let wcount = spira_world::proc::live_workers(std::path::Path::new("/proc"), &worker_refs).len();
    println!("  {:<26} {}", "live workers (/proc)", wcount);

    let aeon_paths: Vec<String> = vec![
        home.join("aeon.sh").to_string_lossy().into_owned(),
        prod.join("aeon.sh").to_string_lossy().into_owned(),
    ];
    let aeon_refs: Vec<&str> = aeon_paths.iter().map(String::as_str).collect();
    let a = spira_world::proc::live_aeons(std::path::Path::new("/proc"), &aeon_refs, |_| String::new()).len();
    println!("  live aeons: {a}");

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
    let rc = match cmd.as_str() {
        "stop" => cmd_stop(rest),
        "start" => cmd_start(rest),
        "drain" => cmd_drain(rest),
        "resume" => cmd_resume(),
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
            eprintln!("usage: world.sh {{stop [--why \"...\"] [--work|--observability|--maintenance|--all|--hard] [--round-drain] [--round-drain-timeout SECS] | drain [--timeout SECS | --deadline SECS] | resume | start [--work|--observability|--maintenance|--all] | status}}");
            64
        }
    };
    std::process::exit(rc);
}
