use super::Cfg;
use crate::{cpu_throttle, disk_mem, env::Env, failed_units, gate_wait, incident, lapsed, lc, log::fmt_compact, seams};
use std::process::Command;

pub struct Halt {
    pub since: String,
    pub why: Option<String>,
}

#[derive(Clone)]
pub enum Drain {
    NotDraining,
    Draining { since: String, mins: Option<i64> },
}

pub struct ThrottleState {
    pub since: Option<String>,
    pub depth: Option<String>,
    pub drain: Option<String>,
    pub override_off: bool,
}

pub struct SweepData {
    pub now: i64,
    pub env: Env,
    pub halt: Option<Halt>,
    pub drain: Drain,
    pub throttle: ThrottleState,

    pub since_land_disp: String,
    pub last_land_id: Option<String>,
    pub gate_wait: gate_wait::GateWait,
    pub gate_win_label: String,
    pub gate_silence: Option<gate_wait::Silence>,
    pub gate_slow: Option<gate_wait::Slow>,
    pub gate_build_p90: Option<i64>,
    pub nv_worst: i64,
    pub nv_worst_key: Option<String>,
    pub nv_win_label: String,

    pub unsent_inflight_disp: String,

    pub yield_win_label: String,
    pub yield_note: String,

    pub tmp_pct: String,
    pub disk: disk_mem::DiskMem,
    pub dwarves_live_disp: String,
    pub cpu_throttle_breach: bool,
    pub cpu_throttle_disp: String,
    pub failed_units: Option<Vec<String>>,
    pub failed_invocations: std::collections::BTreeMap<String, String>,
    pub failed_unit_names: String,
    pub failed_units_warn_mins: i64,

    pub czar_block: String,
    pub lapsed: lapsed::Lapsed,
    pub lapsed_now: String,

    pub suites_block: String,
    pub guard_block: String,

    pub snap_age_disp: String,
    pub snap_age_disp_raw: String,
    pub snap_age: Option<i64>,

    pub lapsed_count_disp: String,
    pub ask_label: String,

    // Carried through for escalate.rs, computed once here to avoid a second gather pass.
    pub idle_while_ready: Vec<(String, u32, String)>, // (rune, ready, last_idle_reason)
    pub release_status: Option<String>,
    pub slow_probes: Vec<String>,
}

#[cfg(test)]
impl SweepData {
    /// A fully green fixture — every threshold below its escalation line, no lapses, a
    /// fresh snapshot. `render::tests` and `escalate::tests` mutate one field at a time off
    /// this baseline rather than repeating the whole struct literal per case.
    pub fn fixture_nominal(now: i64) -> SweepData {
        SweepData {
            now,
            env: Env::new(),
            halt: None,
            drain: Drain::NotDraining,
            throttle: ThrottleState {
                since: None,
                depth: None,
                drain: None,
                override_off: false,
            },
            since_land_disp: "5".to_string(),
            last_land_id: Some("sp-abc".to_string()),
            gate_wait: gate_wait::GateWait {
                oldest_wait: None,
                oldest_branch: String::new(),
            },
            gate_win_label: "last 6h".to_string(),
            gate_silence: None,
            gate_slow: None,
            gate_build_p90: None,
            nv_worst: 0,
            nv_worst_key: None,
            nv_win_label: win_label(172800),
            unsent_inflight_disp: "0".to_string(),
            yield_win_label: "last 1d".to_string(),
            yield_note: String::new(),
            tmp_pct: "1%".to_string(),
            disk: disk_mem::DiskMem {
                disk_breach: false,
                disk_disp: "10%".to_string(),
                mem_breach: false,
                mem_disp: "8000MB".to_string(),
            },
            dwarves_live_disp: "1".to_string(),
            cpu_throttle_breach: false,
            cpu_throttle_disp: "baseline".to_string(),
            failed_units: Some(Vec::new()),
            failed_invocations: Default::default(),
            failed_unit_names: String::new(),
            failed_units_warn_mins: 15,
            czar_block: "  class\n".to_string(),
            lapsed: lapsed::Lapsed {
                count: lapsed::Count::Known(0),
                section: "  none since last sweep".to_string(),
            },
            lapsed_now: "20260930T000000Z".to_string(),
            suites_block: "  (no suites)".to_string(),
            guard_block: "  (clean)".to_string(),
            snap_age_disp: "5s".to_string(),
            snap_age_disp_raw: "5".to_string(),
            snap_age: Some(5),
            lapsed_count_disp: "0".to_string(),
            ask_label: "needs-operator".to_string(), // literal-ok: test fixture
            idle_while_ready: Vec::new(),
            release_status: None,
            slow_probes: Vec::new(),
        }
    }
}

/// Largest counter under `dir` written within `window_s`; `window_s <= 0` reads all time.
/// Counters are cleared only by a PASS on their own branch, so a dead branch's would pin it.
fn worst_noverdict(dir: &std::path::Path, now: i64, window_s: i64) -> (i64, Option<String>) {
    let mut worst = 0i64;
    let mut key: Option<String> = None;
    let Ok(rd) = std::fs::read_dir(dir) else { return (worst, key) };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() || p.extension().map(|x| x == "asked").unwrap_or(false) {
            continue;
        }
        if window_s > 0 && mtime_secs(&p).map(|m| now.saturating_sub(m) > window_s).unwrap_or(true) {
            continue;
        }
        let Ok(t) = std::fs::read_to_string(&p) else { continue };
        let Ok(n) = t.trim().parse::<i64>() else { continue };
        if n > worst {
            worst = n;
            key = p.file_name().and_then(|s| s.to_str()).map(str::to_string);
        }
    }
    (worst, key)
}

fn mtime_secs(p: &std::path::Path) -> Option<i64> {
    let md = std::fs::metadata(p).ok()?;
    let d = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_secs() as i64)
}

fn win_label(w: i64) -> String {
    if w <= 0 {
        "all time".to_string()
    } else if w >= 86400 {
        format!("last {}d", w / 86400)
    } else if w >= 3600 {
        format!("last {}h", w / 3600)
    } else {
        format!("last {}m", w / 60)
    }
}

fn read_first_line(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.lines().next().map(|s| s.to_string()))
}

fn read_halt(run: &std::path::Path) -> Option<Halt> {
    let stamp = run.join("world.halted");
    if !stamp.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&stamp).unwrap_or_default();
    let since = text.lines().next().unwrap_or("").to_string();
    let why = text.lines().nth(1).and_then(|l| l.strip_prefix("why: ")).map(|s| s.to_string());
    Some(Halt { since, why })
}

fn read_drain(run: &std::path::Path, now: i64) -> Drain {
    let stamp = run.join("world.draining");
    if !stamp.is_file() {
        return Drain::NotDraining;
    }
    let since = read_first_line(&stamp).unwrap_or_default();
    let mins = std::fs::metadata(&stamp)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|mt| mt.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (now - d.as_secs() as i64) / 60);
    Drain::Draining { since, mins }
}

/// The claimable count the claim view gives, or `?` when it is missing or stale.
fn claim_ready(run: &std::path::Path, now: i64) -> String {
    let claim = cockpit_ops::lcview::read_fresh_claim(run, now);
    claim.map(|c| cockpit_ops::lcview::claim_keys(&c)).and_then(|k| k.into_iter().find(|(k, _)| k == "SP_READY")).map_or_else(|| "?".to_string(), |(_, v)| v)
}

pub fn collect(now: i64, cfg: &Cfg) -> SweepData {
    let run = &cfg.spira_run;

    // HALT / DRAIN -------------------------------------------------------------------
    let halt = read_halt(run);
    let drain = read_drain(run, now);

    // FAILED UNITS ---------------------------------------------------------------------
    let failed_runs = failed_units::gather_runs(&*cfg.units);
    let failed_units = failed_runs.as_ref().map(|r| r.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>());
    let failed_invocations: std::collections::BTreeMap<String, String> = failed_runs.unwrap_or_default().into_iter().collect();
    let cpu_cfg = cpu_throttle::Cfg {
        units: cfg.cpu_throttle_units.clone(),
        warn_pct: cfg.cpu_throttle_warn_pct,
        min_periods: cfg.cpu_throttle_min_periods,
        cgroup_root: cfg.cgroup_root.clone(),
        systemctl: cfg.systemctl.clone(),
    };
    let (cpu_throttle_breach, cpu_throttle_disp) = cpu_throttle::summarize(
        &cpu_throttle::gather(&cpu_cfg, &cfg.spira_run.join("cpu-throttle.state")),
        cfg.cpu_throttle_warn_pct,
    );
    let failed_unit_names = failed_units.as_deref().unwrap_or(&[]).join(" ");

    // COCKPIT.ENV SNAPSHOT ---------------------------------------------------------------
    let mut env = Env::new();
    let envf_a = run.join("cockpit/cockpit.env");
    let envf_b = run.join("cockpit.env");
    let envf = if envf_a.is_file() { Some(envf_a) } else if envf_b.is_file() { Some(envf_b) } else { None };
    let mut snap_age: Option<i64> = None;
    if let Some(p) = &envf {
        if let Ok(text) = std::fs::read_to_string(p) {
            env.merge_lines(&text, "SP_");
            if let Some(at) = env.raw("SP_AT").and_then(|v| v.parse::<i64>().ok()) {
                snap_age = Some(now - at);
            }
        }
    }
    env.set("SP_READY", claim_ready(run, now));
    let snap_age_disp_raw = snap_age.map(|s| s.to_string()).unwrap_or_else(|| "?".into());
    let snap_age_disp = match snap_age {
        Some(s) if s >= cfg.snap_stale_s => format!("FAULT ({s}s, stale above {}s)", cfg.snap_stale_s),
        Some(s) => format!("{s}s"),
        None => "?s".to_string(),
    };

    let unsent_inflight_disp = match (
        env.raw("SP_UNSENT").and_then(|v| v.parse::<i64>().ok()),
        env.raw("SP_CLOSED_STRANDED").and_then(|v| v.parse::<i64>().ok()),
    ) {
        (Some(u), Some(c)) => (u - c).to_string(),
        _ => "?".to_string(),
    };

    // LANDING FIELD ------------------------------------------------------------------
    let (since_land_disp, last_land_id) = landing_field(&cfg.lc, now);

    // GATE WAIT ------------------------------------------------------------------------
    let gate_log_text = std::fs::read_to_string(cfg.gate_log()).unwrap_or_default();
    let gw = gate_wait::compute(&gate_log_text, now, cfg.gate_window_s);
    let gate_win_label = gate_wait::window_label(cfg.gate_window_s);
    let gate_slow = gate_wait::slow(&gate_log_text, now, 86400, cfg.gate_deadline_s, 20);
    let gate_build_p90 = gate_wait::build_p90(&gate_log_text, now, 86400);
    let gate_silence = gate_wait::silence(&gate_log_text, &lc::beads_in("SUBMITTED").unwrap_or_default(), now, cfg.gate_silence_window_s);

    // YIELD ------------------------------------------------------------------------------
    let yield_sh = cfg.yield_sh.clone().or_else(|| incident::which("yield.sh"));
    if let Some(ysh) = &yield_sh {
        if let Ok(out) = crate::deadline::output(
            "yield",
            // FORK-EDGE: yield.sh is 536 lines of shell
            Command::new("bash")
                .arg(ysh)
                .arg("report")
                .env("SPIRA_RUN", run)
                .env("SPIRA_YIELD_WINDOW", cfg.yield_window_s.to_string()),
        ) {
            env.merge_lines(&String::from_utf8_lossy(&out.stdout), "YIELD_");
        }
    }
    let yield_win_label = win_label(cfg.yield_window_s);
    let yield_note = match env.raw("YIELD_RECORDER") {
        Some("silent") => format!(
            "   <- the gate meter saw {} red(s) and none reached the record: THE RECORDER IS NOT RUNNING",
            env.g("YIELD_LOG_REDS")
        ),
        Some("absent") => "   <- nothing recorded here yet, and the meter has logged no reds either".to_string(),
        Some("?") | None => "   <- no positive control: the gate meter could not be read".to_string(),
        _ => String::new(),
    };

    // NO-VERDICT STREAKS -----------------------------------------------------------------
    let (nv_worst, nv_worst_key) = worst_noverdict(&run.join("noverdict"), now, cfg.noverdict_window_s);
    let nv_win_label = win_label(cfg.noverdict_window_s);

    // DWARVES ALIVE / IDLE-WHILE-READY -------------------------------------------------------
    let ledger_path = run.join("dwarf-ledger.log");
    let probe = seams::pipeline_probe(
        &cfg.lib_sh_dir,
        if ledger_path.is_file() { ledger_path.to_str() } else { None },
    );
    let dwarves_live_disp = match &probe {
        Some(p) => p.rune_counts.iter().map(|(_, n)| *n as i64).sum::<i64>().to_string(),
        None => "?".to_string(),
    };
    let active_since: Vec<i64> = ["WORKING", "SUBMITTED"]
        .iter()
        .flat_map(|s| lc::beads_in(s).unwrap_or_default())
        .filter_map(|b| b.since)
        .collect();
    let idle_while_ready = idle_while_ready_hits(&ledger_path, &probe, cfg.idle_while_ready_n, &active_since);

    // DISK / MEM / TMP ---------------------------------------------------------------------
    let tmp_pct = tmp_pct();
    let disk_cfg = disk_mem::Cfg {
        disk_warn_pct: cfg.disk_warn_pct,
        mem_warn_mb: cfg.mem_warn_mb,
    };
    let disk_pct = disk_mem::disk_root_pct();
    let mem_mb = disk_mem::mem_available_mb(&cfg.meminfo_path);
    let disk = disk_mem::render(disk_pct, mem_mb, &disk_cfg);

    // THROTTLE STAMP -----------------------------------------------------------------------
    let throttle_stamp_path = cfg.throttle_stamp();
    let (t_since, t_depth, t_drain) = if throttle_stamp_path.is_file() {
        let line = read_first_line(&throttle_stamp_path).unwrap_or_default();
        (
            extract_kv(&line, "since="),
            extract_kv(&line, "depth="),
            extract_kv(&line, "since_land=").map(|s| format!("{s}m")),
        )
    } else {
        (None, None, None)
    };
    let throttle = ThrottleState {
        since: t_since,
        depth: t_depth,
        drain: t_drain,
        override_off: cfg.queue_throttle_override == "off",
    };

    // CZAR TABLE -----------------------------------------------------------------------------
    let czar_block = czar_block(&env);

    // LAPSED DWARVES -----------------------------------------------------------------------------
    let prev_marker = std::fs::read_to_string(cfg.lapsed_marker()).unwrap_or_default().trim().to_string();
    let lapsed_now = fmt_compact(now);
    let lapsed = lapsed::read(&cfg.lapsed_dir(), &prev_marker);
    let lapsed_count_disp = match &lapsed.count {
        lapsed::Count::Known(n) => n.to_string(),
        lapsed::Count::Unknown => "?".to_string(),
    };

    // MENU / GUARD -----------------------------------------------------------------------------
    let suites_block = suites_block(cfg);
    let guard_block = guard_block(cfg);

    // RELEASE STATUS (for the hotfix escalation, computed here so escalate.rs need not
    // spawn `release` a second time).
    let release_status = spira_config::release_env::tool("release")
        .ok()
        .and_then(|release| crate::deadline::output("release status", Command::new(release).arg("status")).ok())
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());

    SweepData {
        now,
        env,
        halt,
        drain,
        throttle,
        since_land_disp,
        last_land_id,
        gate_wait: gw,
        gate_win_label,
        gate_silence,
        gate_slow,
        gate_build_p90,
        nv_worst,
        nv_worst_key,
        nv_win_label,
        unsent_inflight_disp,
        yield_win_label,
        yield_note,
        tmp_pct,
        disk,
        dwarves_live_disp,
        cpu_throttle_breach,
        cpu_throttle_disp,
        failed_units,
        failed_invocations,
        failed_unit_names,
        failed_units_warn_mins: cfg.failed_units_warn_mins,
        czar_block,
        lapsed,
        lapsed_now,
        suites_block,
        guard_block,
        snap_age_disp,
        snap_age_disp_raw,
        snap_age,
        lapsed_count_disp,
        ask_label: cfg.ask_label.clone(),
        idle_while_ready,
        release_status,
        slow_probes: crate::deadline::slow_probes(),
    }
}

/// Minutes since the newest `LANDED` record and the bead it names; `?` and no bead when the
/// store cannot say or holds no readable landing — never `0`, which would read as "just landed".
fn landing_field(lc: &spira_store::lc_call::LcClient, now: i64) -> (String, Option<String>) {
    match lc::last_landed_on(lc) {
        Some((id, at)) => (((now - at) / 60).to_string(), Some(id)),
        None => ("?".to_string(), None),
    }
}

fn extract_kv(line: &str, key: &str) -> Option<String> {
    let idx = line.find(key)?;
    let rest = &line[idx + key.len()..];
    let token: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// `/tmp`'s Use% as `df /tmp` printed it (`83%`), `?` when it cannot be read.
fn tmp_pct() -> String {
    crate::fsstat::usage(system_paths::TMP).map_or_else(|| "?".to_string(), |u| format!("{}%", u.used_pct))
}

fn czar_row(env: &Env, label: &str, tag: &str) -> String {
    format!(
        "  {:<22} {:<22} {:<12} {}\n",
        label,
        env.g(&format!("SP_CZAR_{tag}_FIRED")),
        env.g(&format!("SP_CZAR_{tag}_BY")),
        env.g(&format!("SP_CZAR_{tag}_OUTCOME"))
    )
}

/// Per-class display of the four queue-check trigger classes, read from the `SP_CZAR_*`
/// keys `cockpit.sh`'s `czar_triggers_keys` writes. `pub(super)` so `collect::tests` can
/// assert each class renders `?` throughout when unset and is untouched by another
/// class's keys.
pub(super) fn czar_block(env: &Env) -> String {
    let mut s = format!("  {:<22} {:<22} {:<12} {}\n", "class", "last fired", "handled by", "outcome");
    s += &czar_row(env, "deadlock", "DEADLOCK");
    s += &czar_row(env, "attribution-failed", "ATTRIB");
    s += &czar_row(env, "sort-failed", "SORT");
    s.push_str(czar_row(env, "loop-stalled", "STALL").trim_end());
    s
}

fn suites_block(cfg: &Cfg) -> String {
    let out = if let Some(sh) = &cfg.suites_sh {
        // FORK-EDGE: SPIRA_SUITES_SH test seam; production runs testenv
        crate::deadline::output("suites status", Command::new("bash").arg(sh).arg("status"))
    } else {
        match spira_config::release_env::tool("testenv") {
            Ok(testenv) => crate::deadline::output("suites status", Command::new(testenv).args(["suites", "status"])),
            Err(e) => return format!("  (unreadable — {e})"),
        }
    };
    match out {
        Ok(o) if !o.stdout.is_empty() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => "  (unreadable — testenv suites status produced nothing)".to_string(),
    }
}

fn guard_block(cfg: &Cfg) -> String {
    let guard_sh = cfg.branch_guard_sh.clone().or_else(|| incident::which("branch-guard.sh"));
    let Some(sh) = guard_sh.filter(|p| incident::is_usable(p)) else {
        return "  (unavailable — branch-guard.sh is missing or unreadable)".to_string();
    };
    // FORK-EDGE: branch-guard.sh is 211 lines of shell
    match crate::deadline::output("branch guard", Command::new("bash").arg(&sh).arg("check")) {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).into_owned()
                + &String::from_utf8_lossy(&o.stderr);
            match o.status.code() {
                Some(0) => format!("  {}", text.trim_end()),
                Some(3) => "  (no registered repositories — nothing to audit)".to_string(),
                _ => text.lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n"),
            }
        }
        Err(_) => "  (unavailable — branch-guard.sh is missing or unreadable)".to_string(),
    }
}

/// A rune with ready work whose last N calls ALL came back "idle", no dwarf of it is
/// alive, and no bead entered WORKING/SUBMITTED since the first of those calls. Lifecycle
/// rows carry no persona, so any such transition in the window counts as the rune working.
fn idle_while_ready_hits(
    ledger_path: &std::path::Path,
    probe: &Option<seams::PipelineProbe>,
    n: usize,
    active_since: &[i64],
) -> Vec<(String, u32, String)> {
    let mut out = Vec::new();
    let Some(p) = probe else { return out };
    if !ledger_path.is_file() {
        return out;
    }
    let Ok(text) = std::fs::read_to_string(ledger_path) else {
        return out;
    };
    let lines: Vec<&str> = text.lines().collect();
    for (rune, ready) in &p.ready_by_rune {
        if *ready == 0 {
            continue;
        }
        let matching: Vec<&str> = lines
            .iter()
            .filter(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                f.len() >= 3 && f[1] == "awake" && f[2] == rune.as_str()
            })
            .cloned()
            .collect();
        if matching.len() < n {
            continue;
        }
        let last_n = &matching[matching.len() - n..];
        let reasons: Vec<String> = last_n
            .iter()
            .map(|l| l.split_whitespace().skip(3).collect::<Vec<_>>().join(" "))
            .collect();
        if p.rune_counts.iter().any(|(f, c)| f == rune && *c > 0) {
            continue;
        }
        let window_start = last_n[0].split_whitespace().next().and_then(crate::log::parse_iso_utc);
        if let Some(start) = window_start {
            if active_since.iter().any(|at| *at >= start) {
                continue;
            }
        }
        if reasons.iter().all(|r| r == "idle") {
            out.push((rune.clone(), *ready, reasons.last().cloned().unwrap_or_default()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A counter whose branch never passed is never cleared, so the directory keeps it forever.
    /// Inside the window it is reported; a day older it is not — and the positive control is the
    /// SAME value planted fresh, which must still be reported, so a zero here cannot be a reader
    /// that stopped working.
    #[test_id(tt6jghf)]
    #[test]
    fn a_stale_noverdict_counter_is_not_the_worst_streak_but_the_same_value_fresh_is() {
        let d = testkit::TempDir::new("wt-nv");
        let dir = d.path().join("noverdict");
        std::fs::create_dir_all(&dir).unwrap();
        let now = 1_900_000_000i64;
        let plant = |name: &str, n: i64, age: i64| {
            let f = dir.join(name);
            std::fs::write(&f, format!("{n}\n")).unwrap();
            let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs((now - age) as u64);
            std::fs::File::options().write(true).open(&f).unwrap().set_modified(at).unwrap();
        };

        plant("spira-sp-dead-conflict", 98, 46 * 86400);
        plant("spira-sp-live-deadline", 3, 600);
        assert_eq!(worst_noverdict(&dir, now, 172800), (3, Some("spira-sp-live-deadline".into())));

        // POSITIVE CONTROL: the same 98, written now, is still the worst streak.
        plant("spira-sp-fresh-conflict", 98, 600);
        assert_eq!(worst_noverdict(&dir, now, 172800), (98, Some("spira-sp-fresh-conflict".into())));

        // The window off keeps the all-time reading this bead was filed about.
        std::fs::remove_file(dir.join("spira-sp-fresh-conflict")).unwrap();
        assert_eq!(worst_noverdict(&dir, now, 0), (98, Some("spira-sp-dead-conflict".into())));
    }

    /// A class with no `SP_CZAR_*` keys set renders `?` throughout, a fired class carries who
    /// handled it, and an unrelated class is untouched by another class's keys.
    // covers: UC-gate-verdict-24
    #[test_id(ttr6afh)]
    #[test]
    fn the_sweep_carries_the_yield_columns_and_a_missing_record_reaches_ops_as_a_question_mark() {
        let mut d = SweepData::fixture_nominal(1_700_000_000);
        d.env = Env::new();
        for (k, v) in [("YIELD_REDS", "4"), ("YIELD_DEFECT", "1"), ("YIELD_FAULT", "2"), ("YIELD_UNKNOWN", "1"), ("YIELD_SOLO_MED", "60"), ("YIELD_CONC_MED", "95")] {
            d.env.set(k, v);
        }
        let fed = crate::sweep::render::render(&d);
        for want in ["gate reds, last 1d", "the branch really was wrong", "the gate's own fault", "never classified", "another gate overlapping"] {
            assert!(fed.contains(want), "{want} in:\n{fed}");
        }
        assert!(fed.lines().any(|l| l.starts_with("  gate reds, last 1d") && l.trim_end().ends_with('4')), "{fed}");
        d.env = Env::new();
        let none = crate::sweep::render::render(&d);
        assert!(none.lines().any(|l| l.starts_with("  gate reds, last 1d") && l.trim_end().ends_with('?')), "a missing record is ?, never a clean sheet:\n{none}");
        assert!(!none.contains("?s median"), "an unreadable cost carries no unit:\n{none}");
    }

    #[test_id(tvtw5mi)]
    #[test]
    fn czar_block_renders_unset_classes_as_unknown_throughout() {
        let env = Env::new();
        let block = czar_block(&env);
        let row: Vec<&str> = block.lines().find(|l| l.contains("deadlock")).unwrap().split_whitespace().collect();
        assert_eq!(row, vec!["deadlock", "?", "?", "?"]);
    }

    #[test_id(tsfy0o9)]
    #[test]
    fn czar_block_a_fired_class_carries_who_handled_it_and_leaves_others_untouched() {
        let mut env = Env::new();
        env.set("SP_CZAR_DEADLOCK_FIRED", "2026-09-20T10:00Z");
        env.set("SP_CZAR_DEADLOCK_BY", "dwarf-fake");
        env.set("SP_CZAR_DEADLOCK_OUTCOME", "pending");
        let block = czar_block(&env);

        let deadlock: Vec<&str> = block.lines().find(|l| l.contains("deadlock")).unwrap().split_whitespace().collect();
        assert_eq!(deadlock, vec!["deadlock", "2026-09-20T10:00Z", "dwarf-fake", "pending"]);

        let stalled: Vec<&str> = block.lines().find(|l| l.contains("loop-stalled")).unwrap().split_whitespace().collect();
        assert_eq!(stalled, vec!["loop-stalled", "?", "?", "?"]);
    }

    #[test_id(tvgxoa3)]
    #[test]
    fn the_ready_count_is_the_claim_views_and_a_stale_or_absent_view_is_unknown() {
        let d = testkit::TempDir::new("wt-claim");
        let row = |id: &str| cockpit_ops::lcview::Row { id: id.into(), state: "READY".into(), claim: Some(cockpit_ops::lcview::ClaimView { verdict: cockpit_ops::lcview::Kind::Claimable, blocker: None, runes: vec!["builder".into()] }), ..Default::default() };
        let mut snap = cockpit_ops::lcview::Snapshot { now: 5_000, rows: vec![row("sp-a"), row("sp-b"), row("sp-c")], ..Default::default() };
        cockpit_ops::lcview::apply_claim_views(&mut snap);
        std::fs::create_dir_all(d.path().join("lcview")).unwrap();
        std::fs::write(d.path().join("lcview/snapshot.json"), serde_json::to_string(&snap).unwrap()).unwrap();
        assert_eq!(claim_ready(d.path(), 5_001), "3");
        assert_eq!(claim_ready(d.path(), 5_000 + cockpit_ops::lcview::CLAIM_STALE_S + 1), "?");
        assert_eq!(claim_ready(&d.path().join("none"), 5_001), "?");
    }

    fn probe(ready: &[(&str, u32)]) -> Option<seams::PipelineProbe> {
        Some(seams::PipelineProbe {
            rune_counts: Vec::new(),
            ready_by_rune: ready.iter().map(|(f, n)| (f.to_string(), *n)).collect(),
        })
    }

    fn ledger(dir: &std::path::Path, lines: &[&str]) -> std::path::PathBuf {
        let p = dir.join("dwarf-ledger.log");
        std::fs::write(&p, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        p
    }

    const IDLE: &str = "2026-01-01T00:00:01Z awake builder idle";

    #[test_id(tt4qpz6)]
    #[test]
    fn a_halt_stamp_reads_its_timestamp_and_its_why_line() {
        let d = testkit::TempDir::new("wt-halt");
        std::fs::write(d.join("world.halted"), "2026-09-08T01:23:45Z\nwhy: deliberate halt\n").unwrap();
        let h = read_halt(&d).unwrap();
        assert_eq!(h.since, "2026-09-08T01:23:45Z");
        assert_eq!(h.why.as_deref(), Some("deliberate halt"));
    }

    #[test_id(ts7vpku)]
    #[test]
    fn a_halt_stamp_without_a_why_line_has_none_and_no_stamp_is_no_halt() {
        let d = testkit::TempDir::new("wt-halt-nowhy");
        assert!(read_halt(&d).is_none());
        std::fs::write(d.join("world.halted"), "2026-09-08T01:23:45Z\nsomething else\n").unwrap();
        let h = read_halt(&d).unwrap();
        assert_eq!(h.since, "2026-09-08T01:23:45Z");
        assert!(h.why.is_none());
    }

    fn drain_at(d: &std::path::Path, text: &str, age_s: i64, now: i64) -> Drain {
        let stamp = d.join("world.draining");
        std::fs::write(&stamp, text).unwrap();
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs((now - age_s) as u64);
        std::fs::File::options().write(true).open(&stamp).unwrap().set_modified(at).unwrap();
        read_drain(d, now)
    }

    #[test_id(ts21o18)]
    #[test]
    fn a_drain_stamp_reads_its_first_line_and_its_age_in_whole_minutes() {
        let d = testkit::TempDir::new("wt-drain");
        match drain_at(&d, "2026-09-08 20:02:00 UTC\ncalls gated.\n", 1230, 1_800_000_000) {
            Drain::Draining { since, mins } => {
                assert_eq!(since, "2026-09-08 20:02:00 UTC");
                assert_eq!(mins, Some(20));
            }
            Drain::NotDraining => panic!("a stamp is a drain"),
        }
    }

    #[test_id(t778z39)]
    #[test]
    fn no_drain_stamp_is_not_draining() {
        let d = testkit::TempDir::new("wt-nodrain");
        assert!(matches!(read_drain(&d, 1_800_000_000), Drain::NotDraining));
    }

    #[test_id(tq9hvjf)]
    #[test]
    fn a_malformed_drain_stamp_still_reads_as_draining_never_as_running() {
        let d = testkit::TempDir::new("wt-drain-bad");
        match drain_at(&d, "not-a-timestamp\ncalls gated.\n", 300, 1_800_000_000) {
            Drain::Draining { since, mins } => {
                assert_eq!(since, "not-a-timestamp");
                assert_eq!(mins, Some(5));
            }
            Drain::NotDraining => panic!("a malformed stamp must not read as running"),
        }
    }

    #[test_id(t85obbd)]
    #[test]
    fn five_idle_calls_with_ready_work_is_a_hit_naming_rune_count_and_reason() {
        let d = testkit::TempDir::new("wt-iwr-hit");
        let l = ledger(&d, &[IDLE; 5]);
        let hits = idle_while_ready_hits(&l, &probe(&[("builder", 3)]), 5, &[]);
        assert_eq!(hits, vec![("builder".to_string(), 3, "idle".to_string())]);
    }

    #[test_id(twp4i8p)]
    #[test]
    fn an_empty_ready_set_is_not_a_hit_however_idle_the_ledger() {
        let d = testkit::TempDir::new("wt-iwr-empty");
        let l = ledger(&d, &[IDLE; 5]);
        assert!(idle_while_ready_hits(&l, &probe(&[("builder", 0)]), 5, &[]).is_empty());
    }

    #[test_id(trr2lvo)]
    #[test]
    fn a_real_claim_among_the_last_n_calls_is_not_a_hit() {
        let d = testkit::TempDir::new("wt-iwr-mixed");
        let mut lines = vec!["2026-01-01T00:00:01Z awake builder sp-real1"];
        lines.extend([IDLE; 4]);
        let l = ledger(&d, &lines);
        assert!(idle_while_ready_hits(&l, &probe(&[("builder", 1)]), 5, &[]).is_empty());
    }

    #[test_id(tbrjut1)]
    #[test]
    fn too_few_calls_other_runes_and_a_missing_probe_or_ledger_are_no_hit() {
        let d = testkit::TempDir::new("wt-iwr-short");
        let mut lines = vec!["2026-01-01T00:00:01Z awake other idle"; 5];
        lines.extend([IDLE; 4]);
        let l = ledger(&d, &lines);
        assert!(idle_while_ready_hits(&l, &probe(&[("builder", 1)]), 5, &[]).is_empty());
        assert!(idle_while_ready_hits(&l, &None, 4, &[]).is_empty());
        assert!(idle_while_ready_hits(&d.join("absent.log"), &probe(&[("builder", 1)]), 4, &[]).is_empty());
        assert_eq!(idle_while_ready_hits(&l, &probe(&[("builder", 1)]), 4, &[]).len(), 1);
    }

    fn live_probe(rune: &str, ready: u32, live: u32) -> Option<seams::PipelineProbe> {
        Some(seams::PipelineProbe {
            rune_counts: vec![(rune.to_string(), live)],
            ready_by_rune: vec![(rune.to_string(), ready)],
        })
    }

    #[test_id(tsfxqoa)]
    #[test]
    fn a_working_or_submitted_row_inside_the_idle_window_is_not_a_hit() {
        let d = testkit::TempDir::new("wt-iwr-submitted");
        let l = ledger(&d, &[IDLE; 5]);
        let start = crate::log::parse_iso_utc("2026-01-01T00:00:01Z").unwrap();
        assert!(idle_while_ready_hits(&l, &probe(&[("builder", 1)]), 5, &[start + 60]).is_empty());
        assert_eq!(idle_while_ready_hits(&l, &probe(&[("builder", 1)]), 5, &[start - 60]).len(), 1);
    }

    #[test_id(th4egz5)]
    #[test]
    fn a_live_dwarf_of_the_rune_is_not_a_hit() {
        let d = testkit::TempDir::new("wt-iwr-live");
        let l = ledger(&d, &[IDLE; 5]);
        assert!(idle_while_ready_hits(&l, &live_probe("builder", 1, 1), 5, &[]).is_empty());
        assert_eq!(idle_while_ready_hits(&l, &live_probe("builder", 1, 0), 5, &[]).len(), 1);
    }

    use crate::lc::scripted::scripted_by;

    // covers: UC-ops-detection-remediation-26
    #[test_id(tr1agj4)]
    #[test]
    fn the_landing_field_is_the_newest_landed_audit_never_a_bead_that_has_not_landed() {
        let (t, lc) = scripted_by(&[("audits --newest --to-state LANDED", 0, r#"{"newest_at":9400,"lc_key":"sp-land"}"#)]);
        assert_eq!(landing_field(&lc, 10_000), ("10".to_string(), Some("sp-land".to_string())));
        assert_eq!(t.seen.lock().unwrap().len(), 1, "one question, not a listing of every landing");
    }

    // covers: UC-ops-detection-remediation-26
    #[test_id(t95knv4)]
    #[test]
    fn a_landing_that_cannot_be_read_is_a_question_mark_not_zero() {
        let none = |lc| landing_field(&lc, 10_000);
        assert_eq!(none(scripted_by(&[("audits", 0, r#"{"newest_at":null}"#)]).1), ("?".to_string(), None), "no landing recorded");
        assert_eq!(none(scripted_by(&[("audits", 0, r#"{"newest_at":"soon","lc_key":"sp-a"}"#)]).1), ("?".to_string(), None), "a record with no time is not credited");
        assert_eq!(none(scripted_by(&[("audits", 0, "not json")]).1), ("?".to_string(), None), "a malformed answer");
        assert_eq!(none(scripted_by(&[("audits", 2, "cannot tell")]).1), ("?".to_string(), None), "an unreachable store");
        assert_eq!(landing_field(&scripted_by(&[("audits", 0, r#"{"newest_at":10000,"lc_key":"sp-now"}"#)]).1, 10_000).0, "0", "0 only for a true zero");
    }

    // covers: UC-ops-detection-remediation-27
    #[test_id(tn08wxp)]
    #[test]
    fn the_sweep_names_the_suites_and_asks_only_for_their_status() {
        let d = testkit::TempDir::new("wt-collect-suites");
        let sh = d.join("suites.sh");
        let args = d.join("args");
        std::fs::write(&sh, format!("echo \"$*\" >> {}\necho '  suites in the tree   9'\n", args.display())).unwrap();
        let mut cfg = Cfg::fixture(d.path(), "inc".into());
        cfg.suites_sh = Some(sh.to_str().unwrap().into());
        assert!(suites_block(&cfg).contains("suites in the tree"));
        assert_eq!(std::fs::read_to_string(&args).unwrap().trim(), "status", "the sweep reads the figures; it runs no suite");
    }
}
