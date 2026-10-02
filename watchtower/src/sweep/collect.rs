use super::Cfg;
use crate::{disk_mem, env::Env, failed_units, gate_wait, incident, landstate, lapsed, log::fmt_compact, seams};
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
    pub nv_worst: i64,
    pub nv_worst_key: Option<String>,

    pub unsent_inflight_disp: String,

    pub yield_win_label: String,
    pub yield_note: String,

    pub tmp_pct: String,
    pub disk: disk_mem::DiskMem,
    pub aeons_live_disp: String,
    pub failed_units: Option<Vec<String>>,
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
    pub idle_while_ready: Vec<(String, u32, String)>, // (fayth, ready, last_idle_reason)
    pub release_status: Option<String>,
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
            nv_worst: 0,
            nv_worst_key: None,
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
            aeons_live_disp: "1".to_string(),
            failed_units: Some(Vec::new()),
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
        }
    }
}

fn read_first_line(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.lines().next().map(|s| s.to_string()))
}

pub fn collect(now: i64, cfg: &Cfg) -> SweepData {
    let run = &cfg.spira_run;

    // HALT / DRAIN -------------------------------------------------------------------
    let halt_stamp = run.join("world.halted");
    let halt = if halt_stamp.is_file() {
        let text = std::fs::read_to_string(&halt_stamp).unwrap_or_default();
        let mut lines = text.lines();
        let since = lines.next().unwrap_or("").to_string();
        let why = text
            .lines()
            .nth(1)
            .and_then(|l| l.strip_prefix("why: "))
            .map(|s| s.to_string());
        Some(Halt { since, why })
    } else {
        None
    };

    let drain_stamp = run.join("world.draining");
    let drain = if drain_stamp.is_file() {
        let since = read_first_line(&drain_stamp).unwrap_or_default();
        let mins = std::fs::metadata(&drain_stamp)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mt| mt.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (now - d.as_secs() as i64) / 60);
        Drain::Draining { since, mins }
    } else {
        Drain::NotDraining
    };

    // FAILED UNITS ---------------------------------------------------------------------
    let failed_units = failed_units::gather(&cfg.systemctl);
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
    let landstate_dir = cfg.landstate_dir();
    let last_landed = landstate::last_landed(&landstate_dir);
    let since_land_disp = last_landed
        .as_ref()
        .map(|(_, at)| ((now - at) / 60).to_string())
        .unwrap_or_else(|| "?".into());
    let last_land_id = last_landed.map(|(id, _)| id);

    // GATE WAIT ------------------------------------------------------------------------
    let gate_log_text = std::fs::read_to_string(cfg.gate_log()).unwrap_or_default();
    let gw = gate_wait::compute(&gate_log_text, now, cfg.gate_window_s);
    let gate_win_label = gate_wait::window_label(cfg.gate_window_s);
    let gate_silence = gate_wait::silence(&gate_log_text, &landstate::read_dir(&landstate_dir), now, cfg.gate_silence_window_s);

    // YIELD ------------------------------------------------------------------------------
    let yield_sh = cfg.yield_sh.clone().or_else(|| incident::which("yield.sh"));
    if let Some(ysh) = &yield_sh {
        if let Ok(out) = Command::new("bash")
            .arg(ysh)
            .arg("report")
            .env("SPIRA_RUN", run)
            .env("SPIRA_YIELD_WINDOW", cfg.yield_window_s.to_string())
            .output()
        {
            env.merge_lines(&String::from_utf8_lossy(&out.stdout), "YIELD_");
        }
    }
    let yield_win_label = if cfg.yield_window_s >= 86400 {
        format!("last {}d", cfg.yield_window_s / 86400)
    } else if cfg.yield_window_s >= 3600 {
        format!("last {}h", cfg.yield_window_s / 3600)
    } else {
        format!("last {}m", cfg.yield_window_s / 60)
    };
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
    let mut nv_worst = 0i64;
    let mut nv_worst_key: Option<String> = None;
    let nv_dir = run.join("noverdict");
    if let Ok(rd) = std::fs::read_dir(&nv_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            if p.extension().map(|x| x == "asked").unwrap_or(false) {
                continue;
            }
            if let Ok(t) = std::fs::read_to_string(&p) {
                if let Ok(n) = t.trim().parse::<i64>() {
                    if n > nv_worst {
                        nv_worst = n;
                        nv_worst_key = p.file_name().and_then(|s| s.to_str()).map(|s| s.to_string());
                    }
                }
            }
        }
    }

    // AEONS ALIVE / IDLE-WHILE-READY -------------------------------------------------------
    let ledger_path = run.join("aeon-ledger.log");
    let probe = seams::pipeline_probe(
        &cfg.lib_sh_dir,
        if ledger_path.is_file() { ledger_path.to_str() } else { None },
    );
    let aeons_live_disp = match &probe {
        Some(p) => p.fayth_counts.iter().map(|(_, n)| *n as i64).sum::<i64>().to_string(),
        None => "?".to_string(),
    };
    let idle_while_ready = idle_while_ready_hits(&ledger_path, &probe, cfg.idle_while_ready_n);

    // DISK / MEM / TMP ---------------------------------------------------------------------
    let tmp_pct = tmp_pct();
    let disk_cfg = disk_mem::Cfg {
        disk_warn_pct: cfg.disk_warn_pct,
        mem_warn_mb: cfg.mem_warn_mb,
        df_bin: "df".to_string(),
    };
    let disk_pct = disk_mem::disk_root_pct(&disk_cfg.df_bin);
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

    // LAPSED AEONS -----------------------------------------------------------------------------
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
    let release_status = Command::new("release")
        .arg("status")
        .output()
        .ok()
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
        nv_worst,
        nv_worst_key,
        unsent_inflight_disp,
        yield_win_label,
        yield_note,
        tmp_pct,
        disk,
        aeons_live_disp,
        failed_units,
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

fn tmp_pct() -> String {
    let out = Command::new("df").arg("/tmp").output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.lines()
                .nth(1)
                .and_then(|l| l.split_whitespace().nth(4))
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".to_string())
        }
        _ => "?".to_string(),
    }
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
/// class's keys — the T1 seam `test-watchtower.sh` used to reach by sourcing the bash
/// file directly (`. ./watchtower.sh; collect_czar_block`), not reachable that way once
/// the sweep is a compiled binary.
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
        Command::new("bash").arg(sh).arg("status").output()
    } else {
        Command::new("testenv").args(["suites", "status"]).output()
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
    match Command::new("bash").arg(&sh).arg("check").output() {
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

/// A fayth with ready work whose last N summons ALL came back "idle" (sp-o4trx).
fn idle_while_ready_hits(
    ledger_path: &std::path::Path,
    probe: &Option<seams::PipelineProbe>,
    n: usize,
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
    for (fayth, ready) in &p.ready_by_fayth {
        if *ready == 0 {
            continue;
        }
        let matching: Vec<&str> = lines
            .iter()
            .filter(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                f.len() >= 3 && f[1] == "awake" && f[2] == fayth.as_str()
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
        if reasons.iter().all(|r| r == "idle") {
            out.push((fayth.clone(), *ready, reasons.last().cloned().unwrap_or_default()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UC-26/sp-m0qeh's T1 seam, ported: a class with no `SP_CZAR_*` keys set renders `?`
    /// throughout, a fired class carries who handled it, and an unrelated class is
    /// untouched by another class's keys (`test-watchtower.sh`'s "collect_czar_block() is
    /// a T1 seam" section, which sourced the bash file directly — not reachable that way
    /// once the sweep is a compiled binary, so the property moves here).
    #[test]
    fn czar_block_renders_unset_classes_as_unknown_throughout() {
        let env = Env::new();
        let block = czar_block(&env);
        let row: Vec<&str> = block.lines().find(|l| l.contains("deadlock")).unwrap().split_whitespace().collect();
        assert_eq!(row, vec!["deadlock", "?", "?", "?"]);
    }

    #[test]
    fn czar_block_a_fired_class_carries_who_handled_it_and_leaves_others_untouched() {
        let mut env = Env::new();
        env.set("SP_CZAR_DEADLOCK_FIRED", "2026-09-20T10:00Z");
        env.set("SP_CZAR_DEADLOCK_BY", "aeon-fake");
        env.set("SP_CZAR_DEADLOCK_OUTCOME", "pending");
        let block = czar_block(&env);

        let deadlock: Vec<&str> = block.lines().find(|l| l.contains("deadlock")).unwrap().split_whitespace().collect();
        assert_eq!(deadlock, vec!["deadlock", "2026-09-20T10:00Z", "aeon-fake", "pending"]);

        let stalled: Vec<&str> = block.lines().find(|l| l.contains("loop-stalled")).unwrap().split_whitespace().collect();
        assert_eq!(stalled, vec!["loop-stalled", "?", "?", "?"]);
    }
}
