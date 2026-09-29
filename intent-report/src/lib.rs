//! intent-report — pure core (DESIGN.md). Parses the producers' rows, windows them, and
//! renders the epic's Intent measures. No IO here; main.rs reads the files.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

pub const TARGET_RUST_ONLY_MEDIAN_SECS: f64 = 180.0;
pub const TARGET_NO_VERDICT_SHARE: f64 = 0.05;
pub const TARGET_ATTRIBUTION_SECS: f64 = 900.0;
pub const BRANCH_TYPES: [&str; 4] = ["rust-only", "bash-touching", "nothing-buildable", "unknown"];

/// `[from, to)` in epoch seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub from: u64,
    pub to: u64,
}

impl Window {
    pub fn contains(&self, t: u64) -> bool {
        t >= self.from && t < self.to
    }
}

/// `24h`, `90m`, `7d`, `30s` → seconds before `now`; otherwise an ISO timestamp.
pub fn parse_when(s: &str, now: u64) -> Option<u64> {
    let s = s.trim();
    if let Some(unit) = s.chars().last().filter(|c| "smhd".contains(*c)) {
        if let Ok(n) = s[..s.len() - 1].parse::<u64>() {
            let mult = match unit {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                _ => 86_400,
            };
            return Some(now.saturating_sub(n * mult));
        }
    }
    tsd::parse_ts(s)
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.trim().is_empty() => s.trim().parse().ok(),
        _ => None,
    }
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn rows(textual: &str) -> impl Iterator<Item = (u64, Value)> + '_ {
    textual.lines().filter_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        let t = tsd::parse_ts(v.get("ts")?.as_str()?)?;
        Some((t, v))
    })
}

// ---------------------------------------------------------------------------- the gate

#[derive(Clone, Debug, PartialEq)]
pub struct Trial {
    pub ts: u64,
    pub status: String,
    pub reason: String,
    pub waited: f64,
    pub ran: f64,
    pub gate_mode: String,
    pub branch_type: String,
    pub from_log: bool,
}

impl Trial {
    fn cached(&self) -> bool {
        self.reason == "cached"
    }
    fn wall(&self) -> f64 {
        self.waited + self.ran
    }
}

pub fn gate_rows(textual: &str) -> Vec<Trial> {
    rows(textual)
        .filter(|(_, v)| text(v, "family") == "gate-run")
        .map(|(ts, v)| Trial {
            ts,
            status: text(&v, "status").to_string(),
            reason: text(&v, "reason").to_string(),
            waited: num(v.get("waited_secs")).unwrap_or(0.0),
            ran: num(v.get("ran_secs")).unwrap_or(0.0),
            gate_mode: text(&v, "gate_mode").to_string(),
            branch_type: match text(&v, "branch_type") {
                "" => "unknown".to_string(),
                b => b.to_string(),
            },
            from_log: false,
        })
        .collect()
}

pub fn status_of_rc(rc: i64) -> &'static str {
    match rc {
        0 => "PASS",
        75 => "NO_VERDICT",
        76 => "BASE_FAIL",
        _ => "FAIL",
    }
}

/// One gate.log meter line (DESIGN.md "gate.log backfill"); None when it does not parse.
pub fn gate_log_line(l: &str) -> Option<Trial> {
    let mut w = l.split_whitespace();
    let ts = tsd::parse_ts(w.next()?)?;
    let _repo = w.next()?;
    let _branch = w.next()?;
    let secs =
        |f: &str, k: &str| -> Option<f64> { f.strip_prefix(k)?.strip_suffix('s')?.parse().ok() };
    let waited = secs(w.next()?, "waited=")?;
    let ran = secs(w.next()?, "ran=")?;
    let rc: i64 = w.next()?.strip_prefix("rc=")?.parse().ok()?;
    let mut reason = String::new();
    let mut compose = String::new();
    for f in w {
        if let Some(c) = f.strip_prefix("compose=") {
            compose = c.to_string();
        } else if reason.is_empty() && !f.starts_with("phases=") {
            reason = f.to_string();
        }
    }
    let branch_type = match compose.as_str() {
        "unit" => "rust-only",
        "fences" => "nothing-buildable",
        "suites(script)" => "bash-touching",
        _ => "unknown",
    };
    // Only unit mode composes anything but `suites(mode)`.
    let gate_mode = match compose.as_str() {
        "" => "",
        "suites(mode)" => "suites",
        _ => "unit",
    };
    Some(Trial {
        ts,
        status: status_of_rc(rc).to_string(),
        reason,
        waited,
        ran,
        gate_mode: gate_mode.to_string(),
        branch_type: branch_type.to_string(),
        from_log: true,
    })
}

/// The trials in `w`: every gate-run row, plus gate.log lines older than the first row.
pub fn trials(gate_run: &str, gate_log: Option<&str>, w: Window) -> Vec<Trial> {
    let mut out = gate_rows(gate_run);
    let first_row = out.iter().map(|t| t.ts).min().unwrap_or(u64::MAX);
    if let Some(log) = gate_log {
        out.extend(
            log.lines()
                .filter_map(gate_log_line)
                .filter(|t| t.ts < first_row),
        );
    }
    out.retain(|t| w.contains(t.ts));
    out.sort_by_key(|t| t.ts);
    out
}

// ------------------------------------------------------------------------- statistics

/// Interpolated quantile (DuckDB quantile_cont); None on no data.
pub fn quantile(v: &[f64], q: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pos = (s.len() - 1) as f64 * q;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    Some(s[lo] + (s[hi] - s[lo]) * (pos - lo as f64))
}

fn q(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.0}")).unwrap_or_else(|| "?".into())
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "?".into()
    } else {
        format!("{:.1}%", 100.0 * n as f64 / d as f64)
    }
}

fn verdict(ok: Option<bool>) -> &'static str {
    match ok {
        None => "NO DATA",
        Some(true) => "MET",
        Some(false) => "NOT MET",
    }
}

// ---------------------------------------------------------------------- the round, bd, landing

#[derive(Clone, Debug, PartialEq)]
pub struct Attribution {
    pub outcome: String,
    pub secs: Option<f64>,
}

pub fn attributions(textual: &str, w: Window) -> Vec<Attribution> {
    rows(textual)
        .filter(|(t, v)| w.contains(*t) && text(v, "family") == "round-attribution")
        .map(|(_, v)| Attribution {
            outcome: text(&v, "outcome").to_string(),
            secs: num(v.get("attribution_secs")),
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SuiteBd {
    pub rows: usize,
    pub metered: usize,
    pub wall_secs: f64,
    pub bd_ms: f64,
    pub bd_calls: f64,
}

/// Per suite over `suite-timing` rows in `w` (the `__batch__` row is not a suite).
pub fn bd_wait(textual: &str, w: Window) -> BTreeMap<String, SuiteBd> {
    let mut by: BTreeMap<String, SuiteBd> = BTreeMap::new();
    for (_, v) in
        rows(textual).filter(|(t, v)| w.contains(*t) && text(v, "family") == "suite-timing")
    {
        let suite = text(&v, "suite");
        if suite.is_empty() || suite == "__batch__" {
            continue;
        }
        let e = by.entry(suite.to_string()).or_default();
        let calls = num(v.get("bd_calls")).unwrap_or(0.0);
        e.rows += 1;
        e.metered += usize::from(calls > 0.0);
        e.bd_calls += calls;
        e.bd_ms += num(v.get("bd_ms")).unwrap_or(0.0);
        e.wall_secs += num(v.get("wall_secs")).unwrap_or(0.0);
    }
    by
}

/// Seconds from each bead's latest CERTIFIED to its first LANDED in `w`.
pub fn certified_to_landed(textual: &str, w: Window) -> Vec<f64> {
    let mut events: Vec<(u64, String, String)> = rows(textual)
        .filter(|(_, v)| text(v, "family") == "landing-event")
        .map(|(t, v)| {
            (
                t,
                text(&v, "bead").to_string(),
                text(&v, "state").to_string(),
            )
        })
        .filter(|(_, b, s)| !b.is_empty() && (s == "CERTIFIED" || s == "LANDED"))
        .collect();
    events.sort_by_key(|e| e.0);
    let mut certified: HashMap<String, u64> = HashMap::new();
    let mut landed: HashMap<String, ()> = HashMap::new();
    let mut out = Vec::new();
    for (t, bead, state) in events {
        if state == "CERTIFIED" {
            certified.insert(bead, t);
        } else if w.contains(t) && landed.insert(bead.clone(), ()).is_none() {
            if let Some(c) = certified.get(&bead) {
                out.push(t.saturating_sub(*c) as f64);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------- the report

pub struct Inputs<'a> {
    pub gate_run: &'a str,
    pub gate_log: Option<&'a str>,
    pub round_attribution: &'a str,
    pub suite_timing: &'a str,
    pub landing_event: &'a str,
}

pub fn render(inp: &Inputs, w: Window) -> String {
    let mut o = String::new();
    let trials = trials(inp.gate_run, inp.gate_log, w);
    let from_log = trials.iter().filter(|t| t.from_log).count();
    let _ = writeln!(
        o,
        "intent-report  {} .. {}",
        tsd::iso_utc(w.from),
        tsd::iso_utc(w.to)
    );
    let _ = writeln!(
        o,
        "gate trials: {} ({} from gate-run rows, {} backfilled from gate.log)\n",
        trials.len(),
        trials.len() - from_log,
        from_log
    );

    // 1. Certification wall by branch type and mode.
    let _ = writeln!(
        o,
        "1. Gate wall by branch type (non-cached trials; wall = waited + ran, seconds)"
    );
    let _ = writeln!(
        o,
        "   {:<18} {:<7} {:>6} {:>6} {:>9} {:>9} {:>9} {:>9}",
        "branch_type", "mode", "trials", "pass", "med wall", "p90 wall", "med pass", "no-verd"
    );
    let mut modes: Vec<String> = trials.iter().map(|t| t.gate_mode.clone()).collect();
    modes.sort();
    modes.dedup();
    for bt in BRANCH_TYPES {
        for m in &modes {
            let set: Vec<&Trial> = trials
                .iter()
                .filter(|t| !t.cached() && t.branch_type == bt && &t.gate_mode == m)
                .collect();
            if set.is_empty() {
                continue;
            }
            let walls: Vec<f64> = set.iter().map(|t| t.wall()).collect();
            let pass: Vec<f64> = set
                .iter()
                .filter(|t| t.status == "PASS")
                .map(|t| t.wall())
                .collect();
            let nv = set.iter().filter(|t| t.status == "NO_VERDICT").count();
            let _ = writeln!(
                o,
                "   {:<18} {:<7} {:>6} {:>6} {:>9} {:>9} {:>9} {:>9}",
                bt,
                if m.is_empty() { "?" } else { m },
                set.len(),
                pass.len(),
                q(quantile(&walls, 0.5)),
                q(quantile(&walls, 0.9)),
                q(quantile(&pass, 0.5)),
                pct(nv, set.len())
            );
        }
    }
    let cached = trials.iter().filter(|t| t.cached()).count();
    let _ = writeln!(o, "   cached verdicts (no work, excluded above): {cached}");
    let rust_pass: Vec<f64> = trials
        .iter()
        .filter(|t| !t.cached() && t.branch_type == "rust-only" && t.status == "PASS")
        .map(|t| t.wall())
        .collect();
    let med = quantile(&rust_pass, 0.5);
    let _ = writeln!(
        o,
        "   TARGET median certification, rust-only: {} s over {} passes vs <= {:.0} s -> {}\n",
        q(med),
        rust_pass.len(),
        TARGET_RUST_ONLY_MEDIAN_SECS,
        verdict(med.map(|m| m <= TARGET_RUST_ONLY_MEDIAN_SECS))
    );

    // 2. No-verdict share.
    let nv: Vec<&Trial> = trials.iter().filter(|t| t.status == "NO_VERDICT").collect();
    let share = (!trials.is_empty()).then(|| nv.len() as f64 / trials.len() as f64);
    let _ = writeln!(
        o,
        "2. No-verdict share: {} / {} = {} vs < {:.0}% -> {}",
        nv.len(),
        trials.len(),
        pct(nv.len(), trials.len()),
        TARGET_NO_VERDICT_SHARE * 100.0,
        verdict(share.map(|s| s < TARGET_NO_VERDICT_SHARE))
    );
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for t in &nv {
        *reasons.entry(t.reason.as_str()).or_default() += 1;
    }
    let mut reasons: Vec<(&str, usize)> = reasons.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (r, n) in reasons.iter().take(6) {
        let _ = writeln!(o, "   {:>5}  {}", n, if r.is_empty() { "-" } else { r });
    }
    let _ = writeln!(o);

    // 3. Attribution per member defect.
    let at = attributions(inp.round_attribution, w);
    let owned: Vec<f64> = at
        .iter()
        .filter(|a| a.outcome == "owner")
        .filter_map(|a| a.secs)
        .collect();
    let count = |o: &str| at.iter().filter(|a| a.outcome == o).count();
    let _ = writeln!(
        o,
        "3. Round attribution per member defect: {} owned reds; median {} s, p90 {} s, max {} s",
        owned.len(),
        q(quantile(&owned, 0.5)),
        q(quantile(&owned, 0.9)),
        q(quantile(&owned, 1.0))
    );
    let _ = writeln!(
        o,
        "   also: flaky {}, base {}, unattributed {}, unsettled {}",
        count("flaky"),
        count("base"),
        count("unattributed"),
        count("unsettled")
    );
    let worst = quantile(&owned, 1.0);
    let _ = writeln!(
        o,
        "   TARGET <= {:.0} s per member defect (max): {}\n",
        TARGET_ATTRIBUTION_SECS,
        verdict(worst.map(|m| m <= TARGET_ATTRIBUTION_SECS))
    );

    // 4. bd / Dolt wait per suite.
    let bd = bd_wait(inp.suite_timing, w);
    let rows: usize = bd.values().map(|s| s.rows).sum();
    let metered: usize = bd.values().map(|s| s.metered).sum();
    let bd_ms: f64 = bd.values().map(|s| s.bd_ms).sum();
    let wall: f64 = bd.values().map(|s| s.wall_secs).sum();
    let waiting = bd.values().filter(|s| s.bd_ms > 0.0).count();
    let _ = writeln!(
        o,
        "4. bd/Dolt wait per suite: {} suite runs of {} suites; metered {} ({})",
        rows,
        bd.len(),
        metered,
        pct(metered, rows)
    );
    if metered == 0 {
        let _ = writeln!(
            o,
            "   bd wait: ? (no metered suite run in the window — bd-meter not yet in the running artifact)"
        );
    } else {
        let _ = writeln!(
            o,
            "   bd wall {:.0} s of {:.0} s suite wall ({}); suites with any bd wait: {}",
            bd_ms / 1000.0,
            wall,
            if wall > 0.0 {
                format!("{:.1}%", 100.0 * bd_ms / 1000.0 / wall)
            } else {
                "?".into()
            },
            waiting
        );
        let mut top: Vec<(&String, &SuiteBd)> = bd.iter().filter(|(_, s)| s.bd_ms > 0.0).collect();
        top.sort_by(|a, b| {
            b.1.bd_ms
                .partial_cmp(&a.1.bd_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (name, s) in top.iter().take(5) {
            let _ = writeln!(
                o,
                "   {:>8.1} s bd  {:>6.0} calls  {:>7.0} s wall  {}",
                s.bd_ms / 1000.0,
                s.bd_calls,
                s.wall_secs,
                name
            );
        }
    }
    let _ = writeln!(o);

    // 5. Certified -> landed.
    let lat = certified_to_landed(inp.landing_event, w);
    let _ = writeln!(
        o,
        "5. Certified -> landed: {} landings; median {} s, p90 {} s",
        lat.len(),
        q(quantile(&lat, 0.5)),
        q(quantile(&lat, 0.9))
    );
    o
}

#[cfg(test)]
mod tests;
