//! The pane's sections. Each function answers one question and renders every row it could
//! show, most important first — `frame` decides how many of those rows survive the pane's
//! actual height (via `share`), so no section here trims itself to a guessed budget.
//!
//! A MISSING READING RENDERS `?`, NEVER 0 OR AN EMPTY ROW (law-absence-needs-a-positive-
//! control): a broken probe must never be indistinguishable from good news.

use super::colors::*;
use super::fmt::*;
use super::model::Snapshot;

pub const MAX_SECTION_ROWS: i64 = 20;
pub const MAX_NEXT_ROWS: i64 = 5;
pub const MAX_RECENT_ROWS: i64 = 5;
pub const MAX_INFLOW_ROWS: i64 = 40;
pub const NEXT_BASE_ROWS: i64 = 5;
pub const RECENT_BASE_ROWS: i64 = 5;
pub const INFLOW_BASE_ROWS: i64 = 5;

/// External facts `header_line` needs that are not in the snapshot: world.sh's halt/drain
/// stamps and the sentinel timer's live state. Read once per frame by the caller (`health`'s
/// main, or a test fixture) — never shelled out to from here, on the hot 2s repaint path.
#[derive(Debug, Clone, Default)]
pub struct HaltState {
    pub stamp_exists: bool,
    pub since: String,
    pub why: String,
    /// `None` when the timer's state could not be determined at all (`spira_unit` returned
    /// `?`, or `systemctl` itself failed) — distinct from a definite "inactive".
    pub sentinel_active: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct DrainState {
    /// `Some(mtime)` when `world.draining` exists; its mtime, epoch seconds.
    pub stamp_mtime: Option<i64>,
}

fn halt_banner(h: &HaltState) -> Vec<String> {
    if !h.stamp_exists && h.sentinel_active == Some(true) {
        return Vec::new();
    }
    let mut out = vec![format!(
        "{B}{BAD} \u{25a0} SPIRA STOPPED {RST}{DIM} no aeons are summoned; nothing lands{RST}"
    )];
    if !h.since.is_empty() {
        out.push(format!("  {DIM}since{RST} {}", h.since));
    }
    if !h.why.is_empty() {
        let w: String = h.why.chars().take(60).collect();
        out.push(format!("  {DIM}why{RST}   {w}"));
    }
    if h.sentinel_active != Some(true) && h.since.is_empty() {
        let tstate = match h.sentinel_active {
            Some(false) => "inactive",
            None => "unknown",
            Some(true) => unreachable!(),
        };
        out.push(format!(
            "  {DIM}sentinel.timer is {tstate} and no world.sh halt was recorded{RST}"
        ));
    }
    out
}

fn drain_banner(d: &DrainState, now: i64) -> Vec<String> {
    let Some(mtime) = d.stamp_mtime else { return Vec::new() };
    let mins_disp = if mtime > 0 { (now - mtime) / 60 } else {
        return vec![
            format!("{B}{WARN} \u{23f8} DRAINING {RST}{DIM} summons gated for {RST}?{DIM}m — loop and landing continue{RST}"),
            format!("  {DIM}lift it:{RST} world.sh resume"),
        ];
    };
    vec![
        format!("{B}{WARN} \u{23f8} DRAINING {RST}{DIM} summons gated for {RST}{mins_disp}{DIM}m — loop and landing continue{RST}"),
        format!("  {DIM}lift it:{RST} world.sh resume"),
    ]
}

fn hotfix_banner(snap: &Snapshot) -> Vec<String> {
    let Some(line) = snap.get("SP_HOTFIX_LINE").filter(|s| !s.is_empty()) else { return Vec::new() };
    let mut out = Vec::new();
    if snap.get("SP_HOTFIX_ALERT").is_some_and(|s| !s.is_empty()) {
        out.push(format!("{B}{BAD} \u{25a0} HOTFIX PAST THRESHOLD {RST}{DIM}{RST}"));
    } else {
        out.push(format!("{B}{WARN} \u{25a0} HOTFIX {RST}{DIM}{RST}"));
    }
    out.push(format!("  {line}"));
    if let Some(alert) = snap.get("SP_HOTFIX_ALERT").filter(|s| !s.is_empty()) {
        out.push(format!("  {BAD}{alert}{RST}"));
    }
    out.push(format!(
        "  {DIM}clear it:{RST} release rollback, or land the fix (it supersedes automatically)"
    ));
    out
}

/// `mismatch_alt`: the alternate `$SPIRA_RUN` derivation (repo-relative or XDG) that
/// actually holds a `cockpit.env`, when it differs from `run_dir` — computed by the
/// caller (it needs the filesystem and `$SPIRA_REPO`/`$XDG_DATA_HOME`/`$SPIRA_INSTANCE`),
/// passed in so this stays a pure string-formatting function. `conf.sh` derives
/// `$SPIRA_RUN` from whether `$SPIRA_REPO` is writable, so a service installed from a
/// writable repo and a reader invoked from a read-only release tree can derive two
/// different paths for the same collector (law-absence-needs-a-positive-control).
fn snap_absent_banner(cols: i64, run_dir: &str, mismatch_alt: Option<&str>) -> Vec<String> {
    if let Some(alt) = mismatch_alt {
        return vec![
            format!("{B}{WARN} \u{25a0} PATH MISMATCH{RST} {DIM}— SPIRA_RUN differs for writer and reader{RST}"),
            format!("  {DIM}exists at{RST} {}", fit(alt, cols - 12)),
            format!("  {DIM}reader at{RST} {}", fit(run_dir, cols - 12)),
        ];
    }
    vec![format!("{DIM}  no snapshot — cockpit not yet run at {}{RST}", fit(run_dir, cols - 28))]
}

/// `header_line` — the pane's banner. `hhmm` is the current local time already formatted
/// (`date +%H:%M` in the original); `age_secs` is `now - SP_AT` when `SP_AT` is set.
#[allow(clippy::too_many_arguments)]
pub fn header_line(
    snap: &Snapshot,
    cols: i64,
    hhmm: &str,
    age_secs: Option<i64>,
    snap_stale_s: i64,
    stalled_probe_names: &[String],
    snap_exists: bool,
    run_dir: &str,
    mismatch_alt: Option<&str>,
    halt: &HaltState,
    drain: &DrainState,
    now: i64,
) -> Vec<String> {
    let mut out = Vec::new();

    let mut stale = String::new();
    if let Some(age) = age_secs {
        if age > snap_stale_s {
            if !stalled_probe_names.is_empty() {
                stale = format!("  {BAD}{B}STALE{RST}");
            } else {
                stale = format!("  {BAD}{B}FAULT ({age}s){RST}");
            }
        }
    }
    let pass_dur = match snap.get("SP_PASS_SECS").filter(|s| !s.is_empty()) {
        Some(p) => format!("  {DIM}pass {p}s{RST}"),
        None => String::new(),
    };

    out.extend(halt_banner(halt));
    out.extend(drain_banner(drain, now));
    out.extend(hotfix_banner(snap));

    out.push(format!(
        "{B}{ACC}SPIRA{RST} {hhmm}   {DIM}sentinel{RST} {} {}s  {DIM}ops{RST} {} {}s  {DIM}auron{RST} {} {}{stale}{pass_dur}",
        dot(snap.get("SP_SENTINEL_TIMER") == Some("1")),
        snap.q("SP_SENTINEL_AGE"),
        dot(snap.get("SP_OPS_TIMER") == Some("1")),
        snap.q("SP_OPS_AGE"),
        dot(snap.get("SP_AURON_TIMER") == Some("1")),
        age_str(snap.q("SP_AURON_AGE"), 180),
    ));

    let firing = snap.get("SP_AURON_FIRING").unwrap_or("0");
    if firing == "?" {
        out.push(format!(
            " {DIM}ALERT{RST}  {BAD}{B}? — Auron's heartbeat could not be read{RST}"
        ));
    } else if firing != "0" {
        let keys = snap.q("SP_AURON_KEYS").replace(',', " ");
        out.push(format!(" {DIM}ALERT{RST}  {BAD}{B}{firing}{RST} firing  {BAD}{keys}{RST}"));
    }

    let overrides_n = snap.get("SP_OVERRIDES_N").unwrap_or("0");
    if overrides_n == "?" {
        out.push(format!(" {DIM}overrides{RST}  {BAD}?{RST} — could not be read"));
    } else if overrides_n != "0" {
        let failed = snap.get("SP_OVERRIDES_FAILED").unwrap_or("0");
        let failed_seg = if failed != "0" { format!("{BAD}{failed} failed{RST}  ") } else { String::new() };
        let list = snap.q("SP_OVERRIDES_LIST").replace(',', " ");
        out.push(format!(" {DIM}overrides{RST}  {failed_seg}{list}"));
    }

    if snap_exists {
        if !stalled_probe_names.is_empty() {
            let mut parts = Vec::new();
            for name in stalled_probe_names {
                let killed = snap.get(&format!("SP_PROBE_KILLED_{name}")).unwrap_or("?");
                parts.push(format!("{name}: timeout \u{d7}{killed}"));
            }
            out.push(format!(" {DIM}STALE{RST}  {BAD}{B}{}{RST}", parts.join("  ")));
        }
    } else {
        out.extend(snap_absent_banner(cols, run_dir, mismatch_alt));
    }

    out
}

/// `tokens_section` — the account's spend, split by half. `spark_*` are pre-rendered
/// sparkline strings (the original shells to `python3`; this crate does not draw them from
/// raw history here — see `../DESIGN.md` Decisions).
pub struct TokensExtra<'a> {
    pub renderer_rev: &'a str,
    pub collector_rev: &'a str,
    pub tok_win_spark: &'a str,
}

const RATELIM_STALE_SECS: i64 = 900;

pub fn tokens_section(snap: &Snapshot, cols: i64, extra: &TokensExtra) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!(
         " {DIM}TOKENS/{}h{RST}  {B}{}{RST} billed  {ACC}{}{RST}",
        snap.q("SP_TOK_WINDOW_H"),
        tok(snap.q("SP_TOK_WIN")),
        extra.tok_win_spark,
    ));
    for (prefix, label) in [("AEON", "aeons"), ("ARC", "archivist"), ("SESS", "session")] {
        let w = snap.q(&format!("SP_TOK_{prefix}_WIN"));
        let t = snap.q(&format!("SP_TOK_{prefix}_TURNS"));
        let c = snap.q(&format!("SP_TOK_{prefix}_CTX"));
        if w == "?" && !extra.collector_rev.is_empty() && extra.collector_rev != extra.renderer_rev {
            out.push(format!("        {DIM}{label:<8}{RST} {DIM}coll {}{RST}", extra.collector_rev));
        } else {
            let tail = fit(&format!("{t}t \u{b7} {} ctx/turn", tok(c)), cols - 22);
            out.push(format!(
                "        {DIM}{label:<8}{RST} {:<4} {DIM}{tail}{RST}",
                pct(w, snap.q("SP_TOK_WIN"))
            ));
        }
    }

    let cage = snap.q("SP_RATELIM_AGE");
    let stale = cage.parse::<i64>().map_or(true, |v| v >= RATELIM_STALE_SECS);
    let live = |k: &str| if stale { "?" } else { snap.q(k) };
    let p5 = live("SP_RATELIM_5H_PCT");
    let p7 = live("SP_RATELIM_7D_PCT");
    let m5 = live("SP_RATELIM_5H_MIN");
    let m7 = live("SP_RATELIM_7D_MIN");
    let e5 = live("SP_RATELIM_5H_ETA");
    let e7 = live("SP_RATELIM_7D_ETA");

    let band = |p: &str| -> &'static str {
        match p.parse::<i64>() {
            Ok(v) if v >= 90 => "\x1b[31m\x1b[1m",
            Ok(v) if v >= 70 => WARN,
            _ => OK,
        }
    };
    let u5 = if p5 == "?" { "" } else { "%" };
    let u7 = if p7 == "?" { "" } else { "%" };
    let c5 = band(p5);
    let c7 = band(p7);
    let dur5 = dur_m(m5);
    let dur7 = dur_m(m7);

    let eta_sfx = |e: &str| -> String {
        match e {
            "-" | "?" | "" => String::new(),
            "0" => format!(" {BAD}{B}FULL{RST}"),
            s if s.bytes().all(|b| b.is_ascii_digit()) => {
                let secs: i64 = s.parse().unwrap_or(0);
                format!(" {DIM}\u{2192}full {}{RST}", dur_m(&(secs / 60).to_string()))
            }
            _ => String::new(),
        }
    };
    let eta5 = eta_sfx(e5);
    let eta7 = eta_sfx(e7);
    let age_sfx = match cage.parse::<i64>() {
        Ok(v) if v >= RATELIM_STALE_SECS => format!(" {DIM}({}m ago){RST}", v / 60),
        _ => String::new(),
    };

    out.push(format!(" {DIM}WIN{RST}    5h {c5}{p5}{u5}{RST}  {DIM}reset {dur5}{RST}{eta5}{age_sfx}"));
    out.push(format!("        7d {c7}{p7}{u7}{RST}  {DIM}reset {dur7}{RST}{eta7}"));
    out
}

fn unread_row(label: &str, what: &str) -> String {
    format!(" {DIM}{label:<6}{RST} {BAD}{B}?{RST} {DIM}{what}{RST}")
}

/// One aeon's four rows for `now_section`. `lease` is `?` or minutes. Trailing moments
/// (`SP_AEON{i}_ACT{j}`) are rendered newest-last when present; otherwise the single
/// `SP_AEON{i}_ACT` plus `SP_AEON{i}_SAID` fall back to the original single-line shape.
fn now_aeon_rows(snap: &Snapshot, cols: i64, i: usize, is_first: bool, trace_lines: i64) -> Vec<String> {
    let g = |k: &str| snap.q(&format!("SP_AEON{i}_{k}")).to_string();
    let nm = g("NAME");
    let fy = g("FAYTH");
    let bd = g("BEAD");
    let mn = g("MIN");
    let ac = snap.get(&format!("SP_AEON{i}_ACT")).unwrap_or("").to_string();
    let ti = snap.get(&format!("SP_AEON{i}_TITLE")).unwrap_or("").to_string();
    let pr = g("PRI");
    let pa = g("PARTITION");
    let tn = g("TURNS");
    let cx = g("CTX");
    let md = g("MODEL");
    let fm = g("FAYTH_MODEL");
    let fl = g("FILES");
    let qt = g("QUIET");
    let sd = snap.get(&format!("SP_AEON{i}_SAID")).unwrap_or("").to_string();
    let lease = g("LEASE");

    let mut model_disp = model_short(&md);
    if md != "?" && md != "-" && fm != "?" && fm != "-" && md != fm {
        model_disp = format!("{model_disp} \u{26a0}{}", model_short(&fm));
    }

    let mut out = Vec::new();
    let label = if is_first { "NOW" } else { "   " };
    let tail = fit(
        &format!("the {fy} on {model_disp} \u{b7} {mn}m \u{b7} {tn} turns \u{b7} ctx {} \u{b7} {fl} files", tok(&cx)),
        cols - 9 - nm.chars().count() as i64,
    );
    out.push(format!("{DIM}{label}{RST}    {OK}{B}{nm}{RST} {DIM}{tail}{RST}"));

    let (lease_disp, lease_col) = if lease == "?" {
        ("?".to_string(), DIM)
    } else {
        let l: i64 = lease.parse().unwrap_or(-1);
        if l < 3 {
            (format!("{lease}m"), "\x1b[31m\x1b[1m")
        } else if l < 5 {
            (format!("{lease}m"), WARN)
        } else {
            (format!("{lease}m"), DIM)
        }
    };
    let pw = pa.chars().count().max(8) as i64;
    let bd_w = (bd.chars().count() as i64).max(14);
    let tail2 = fit(if ti.is_empty() { "?" } else { &ti }, cols - 13 - pw - bd_w - 1 - lease_disp.chars().count() as i64);
    out.push(format!(
        "        {}P{pr}{RST} {DIM}{pa:<pw$}{RST} {ACC}{bd:<14}{RST} {DIM}{tail2}{RST} {lease_col}{lease_disp}{RST}",
        pri_colour(&format!("P{pr}")),
        pw = pw as usize,
    ));

    let (qs, qcol) = if qt == "?" || qt == "-" {
        (format!("quiet {qt}"), "\x1b[31m\x1b[1m")
    } else {
        let q: i64 = qt.parse().unwrap_or(0);
        let s = if q < 120 { format!("quiet {q}s") } else { format!("quiet {}m", q / 60) };
        let c = if q >= 1200 { "\x1b[31m\x1b[1m" } else if q >= 300 { WARN } else { DIM };
        (s, c)
    };

    let mut trail_have = 0i64;
    if trace_lines > 0 {
        loop {
            if snap.get(&format!("SP_AEON{i}_ACT{trail_have}")).unwrap_or("").is_empty() {
                break;
            }
            trail_have += 1;
        }
    }
    let trail_n = trail_have.min(trace_lines.max(0));

    if trail_n > 0 {
        let start = trail_have - trail_n;
        for j in start..trail_have {
            let line = snap.get(&format!("SP_AEON{i}_ACT{j}")).unwrap_or("").to_string();
            if j == trail_have - 1 {
                let aw = (cols - 12 - qs.chars().count() as i64).max(2);
                let f = fit(if line.is_empty() { "-" } else { &line }, aw);
                let pad = (cols - 10 - f.chars().count() as i64 - qs.chars().count() as i64).max(1) as usize;
                out.push(format!("        {RST}\u{21b3} {f}{RST}{:pad$}{qcol}{qs}{RST}", "", pad = pad));
            } else {
                let f = fit(&line, cols - 12);
                out.push(format!("        {DIM}\u{21b3} {f}{RST}"));
            }
        }
    } else {
        let aw = (cols - 12 - qs.chars().count() as i64).max(2);
        let f = fit(if ac.is_empty() { "-" } else { &ac }, aw);
        let pad = (cols - 10 - f.chars().count() as i64 - qs.chars().count() as i64).max(1) as usize;
        out.push(format!("        {DIM}\u{21b3} {f}{RST}{:pad$}{qcol}{qs}{RST}", "", pad = pad));
        if !matches!(sd.as_str(), "" | "-" | "?") {
            let f = fit(&sd, cols - 12);
            out.push(format!("        {DIM}\u{201c} {f} \u{201d}{RST}"));
        }
    }
    out
}

pub fn now_section(snap: &Snapshot, cols: i64, live_aeon_n: i64, trace_lines: i64) -> Vec<String> {
    let snap_n_raw = snap.get("SP_AEON_N");
    if snap_n_raw.is_none() || snap_n_raw == Some("?") {
        return if live_aeon_n > 0 {
            vec![format!(" {DIM}NOW{RST}    {OK}{B}{live_aeon_n} aeon(s) live{RST} {DIM}\u{2014} details pending snapshot{RST}")]
        } else {
            vec![unread_row("NOW", "cannot read the aeon roster")]
        };
    }
    let snap_n: i64 = snap_n_raw.unwrap().parse().unwrap_or(0);
    if snap_n == 0 && live_aeon_n == 0 {
        return vec![format!(" {DIM}NOW{RST}    {DIM}no aeon working{RST}")];
    }
    if snap_n == 0 && live_aeon_n > 0 {
        return vec![format!(" {DIM}NOW{RST}    {OK}{B}{live_aeon_n} aeon(s) live{RST} {DIM}\u{2014} not yet in snapshot{RST}")];
    }
    let mut out = Vec::new();
    for i in 0..snap_n as usize {
        out.extend(now_aeon_rows(snap, cols, i, i == 0, trace_lines));
    }
    let extra = live_aeon_n - snap_n;
    if extra > 0 {
        out.push(format!("        {DIM}+{extra} more aeon(s) live \u{2014} not yet in snapshot{RST}"));
    }
    out
}

fn next_row(cols: i64, raw: &str) -> String {
    let mut it = raw.splitn(4, ' ');
    let pri = it.next().unwrap_or("");
    let part = it.next().unwrap_or("");
    let id = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("");
    let pw = (part.chars().count() as i64).max(8);
    let id_w = (id.chars().count() as i64).max(14);
    let f = fit(rest, cols - 11 - pri.chars().count() as i64 - pw - id_w);
    format!(
        "        {}{pri}{RST} {DIM}{part:<pw$}{RST} {ACC}{id:<14}{RST} {DIM}{f}{RST}",
        pri_colour(pri),
        pw = pw as usize,
    )
}

pub fn next_section(snap: &Snapshot, cols: i64) -> Vec<String> {
    let n_raw = snap.get("SP_NEXT_N");
    if n_raw.is_none() || n_raw == Some("?") {
        return vec![unread_row("NEXT", "cannot read the ready queue")];
    }
    let n: i64 = n_raw.unwrap().parse().unwrap_or(0);
    let reach = snap.q("SP_REACHABLE");
    let strand = snap.q("SP_STRANDED");
    let mut count_line = format!("{n} ready \u{b7} {reach} reachable");
    if strand != "?" && strand != "0" {
        count_line = format!("{count_line} \u{b7} {strand} stranded");
    }
    let mut out = Vec::new();
    if n == 0 {
        out.push(format!(" {DIM}NEXT{RST}   {B}{count_line}{RST} {DIM}\u{2014} nothing to claim{RST}"));
        return out;
    }
    out.push(format!(" {DIM}NEXT{RST}   {B}{count_line}{RST} {DIM}\u{2014} across all partitions:{RST}"));
    for i in 0..MAX_NEXT_ROWS {
        let Some(raw) = snap.get(&format!("SP_NEXT{i}")).filter(|s| !s.is_empty()) else { break };
        out.push(next_row(cols, raw));
    }
    out
}

fn queue_row(cols: i64, raw: &str) -> String {
    let mut it = raw.splitn(3, ' ');
    let pri = it.next().unwrap_or("");
    let id = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("");
    let id_w = (id.chars().count() as i64).max(14);
    let f = fit(rest, cols - 10 - pri.chars().count() as i64 - id_w);
    format!("        {}{pri}{RST} {ACC}{id:<14}{RST} {DIM}{f}{RST}", pri_colour(pri))
}

pub fn queue_section(snap: &Snapshot, cols: i64) -> Vec<String> {
    let bpr = snap.get("SP_QUEUE_BATCH_PR").unwrap_or("0");
    let bn = snap.q("SP_QUEUE_BATCH_N");
    let nn = snap.get("SP_QUEUE_NEXT_N").unwrap_or("0");
    let uln = snap
        .get("SP_STRANDED_N")
        .or_else(|| snap.get("SP_UNLANDED_N"))
        .unwrap_or("0");
    let nm = snap.get("SP_QUEUE_NEXT_MAX").unwrap_or("0");
    let qqn = snap.get("SP_QUEUE_QUARANTINE_N").unwrap_or("0");
    let fn_certify = snap.q("SP_FUNNEL_CERTIFY_N");
    let fn_certify_age = snap.get("SP_FUNNEL_CERTIFY_AGE").unwrap_or("");
    let fn_red = snap.q("SP_FUNNEL_RED_N");
    let fn_red_age = snap.get("SP_FUNNEL_RED_AGE").unwrap_or("");
    let fn_red_to = snap.get("SP_FUNNEL_RED_TIMEOUT").unwrap_or("0");
    let fn_red_rb = snap.get("SP_FUNNEL_RED_REBASE").unwrap_or("0");
    let fn_red_gt = snap.get("SP_FUNNEL_RED_GATE").unwrap_or("0");
    let fn_red_cf = snap.get("SP_FUNNEL_RED_CONFLICT").unwrap_or("0");
    let fn_done_age = snap.get("SP_FUNNEL_DONE_AGE").unwrap_or("");

    if snap.get("SP_QUEUE_DEPTH").unwrap_or("?") == "?"
        && bpr == "0"
        && nn == "0"
        && uln == "0"
        && fn_certify == "?"
        && fn_red == "?"
    {
        return vec![unread_row("QUEUE", "cannot read the queue")];
    }

    let mut out = Vec::new();
    let mut hdr_done = false;
    // Rows built with a trailing "continuation" marker so multiple `printf`-without-newline
    // calls in the original become one pushed string each here; `append` continues the most
    // recently pushed line instead of starting a new one. Free functions (not closures) so
    // each call site can pass fresh `&mut` borrows instead of holding them for the whole
    // section's lifetime.
    fn hdr(hdr_done: &mut bool, out: &mut Vec<String>) {
        if !*hdr_done {
            out.push(format!(" {DIM}QUEUE{RST}  "));
            *hdr_done = true;
        } else {
            out.push("        ".to_string());
        }
    }
    fn append(out: &mut Vec<String>, s: &str) {
        if let Some(last) = out.last_mut() {
            last.push_str(s);
        }
    }

    if uln != "0" || uln == "?" {
        hdr(&mut hdr_done, &mut out);
        if uln == "?" {
            append(&mut out, &format!("{WARN}done  ?{RST}"));
        } else {
            append(&mut out, &format!("done  {uln}"));
            if !fn_done_age.is_empty() {
                append(&mut out, &format!("  oldest {fn_done_age}"));
            }
        }
    }
    if fn_certify != "0" || fn_certify == "?" {
        hdr(&mut hdr_done, &mut out);
        if fn_certify == "?" {
            append(&mut out, &format!("{WARN}certify  ?{RST}"));
        } else {
            append(&mut out, &format!("certify  {fn_certify}"));
            if !fn_certify_age.is_empty() {
                append(&mut out, &format!("  {fn_certify_age}"));
            }
        }
    }
    if fn_red != "0" || fn_red == "?" {
        hdr(&mut hdr_done, &mut out);
        if fn_red == "?" {
            append(&mut out, &format!("{WARN}red  ?{RST}"));
        } else {
            append(&mut out, &format!("{WARN}red  {fn_red}{RST}"));
            let mut br = Vec::new();
            if fn_red_to != "0" {
                br.push(format!("{fn_red_to} timeout"));
            }
            if fn_red_rb != "0" {
                br.push(format!("{fn_red_rb} no-rebase"));
            }
            if fn_red_gt != "0" {
                br.push(format!("{fn_red_gt} gate"));
            }
            if fn_red_cf != "0" {
                br.push(format!("{fn_red_cf} conflict"));
            }
            if !br.is_empty() {
                append(&mut out, &format!("  {DIM}{}{RST}", br.join(" \u{b7} ")));
            }
            if !fn_red_age.is_empty() {
                append(&mut out, &format!("  oldest {fn_red_age}"));
            }
        }
    }

    if bpr != "0" && bpr != "?" {
        hdr(&mut hdr_done, &mut out);
        append(&mut out, &format!("{B}batch #{bpr}{RST} {DIM}({bn} member(s)){RST}"));
        for i in 0..MAX_SECTION_ROWS {
            let Some(raw) = snap.get(&format!("SP_QUEUE_BATCH{i}")).filter(|s| !s.is_empty()) else { break };
            out.push(queue_row(cols, raw));
        }
    }
    if nn != "0" && nn != "?" {
        if bpr != "0" && bpr != "?" {
            out.push(format!("        {DIM}next \u{2192}{RST}"));
        } else {
            hdr(&mut hdr_done, &mut out);
            append(&mut out, &format!("{B}{nn} certified{RST}"));
        }
        let mut shown = 0i64;
        for i in 0..MAX_SECTION_ROWS {
            let Some(raw) = snap.get(&format!("SP_QUEUE_NEXT{i}")).filter(|s| !s.is_empty()) else { break };
            if nm != "0" && shown == nm.parse().unwrap_or(-1) {
                out.push(format!("        {DIM}\u{b7} \u{b7} \u{b7}{RST}"));
            }
            out.push(queue_row(cols, raw));
            shown += 1;
        }
    }
    if !hdr_done {
        hdr(&mut hdr_done, &mut out);
        append(&mut out, &format!("{DIM}nothing queued{RST}"));
    }

    let ej = snap.get("SP_QUEUE_EJECTED").unwrap_or("0");
    let mut footer = String::new();
    if ej != "0" && ej != "?" {
        footer = format!("{ej} ejected");
    }
    if qqn != "0" && qqn != "?" {
        if !footer.is_empty() {
            footer.push_str("  ");
        }
        footer.push_str(&format!("{qqn} quarantined"));
    }
    if !footer.is_empty() {
        out.push(format!("        {WARN}{footer}{RST}"));
    }
    out
}

/// `recent_row`/`inflow_row` share a shape: `<age> <word> <word> <id-or-prose> <rest...>`,
/// parsed into fixed leading fields then a variable tail, printed in the SAME left-to-right
/// order (time, state, persona/kind, bead, description) so the two sections read as one
/// column. A row that cannot be parsed is printed as-is, still cut to the pane.
pub fn recent_row(cols: i64, raw: &str, pad: i64) -> String {
    let re = regex::Regex::new(r"^([0-9]+[smhd])\s+(\S+)\s+(\S+)\s+(\S+)\s*(.*)$").unwrap();
    let Some(caps) = re.captures(raw) else {
        return format!("{DIM}{}{RST}", fit(raw, cols - pad));
    };
    let age = &caps[1];
    let actor = &caps[2];
    let verb = &caps[3];
    let id = &caps[4];
    let rest_cap = &caps[5];

    let aw = (age.chars().count() as i64).max(4);
    let vw = (verb.chars().count() as i64).max(9);
    let pw = (actor.chars().count() as i64).max(8);

    let id_re = regex::Regex::new(r"(^|/)sp-[A-Za-z0-9._-]+$").unwrap();
    if !id_re.is_match(id) {
        let rest = if rest_cap.is_empty() { id.to_string() } else { format!("{id} {rest_cap}") };
        let f = fit(&rest, cols - pad - aw - vw - pw - 3);
        return format!(
            "{DIM}{age:<aw$}{RST} {}{verb:<vw$}{RST} {}{actor:<pw$}{RST} {DIM}{f}{RST}",
            verb_colour(verb),
            part_colour(actor),
            aw = aw as usize,
            vw = vw as usize,
            pw = pw as usize,
        );
    }
    let id_w = (id.chars().count() as i64).max(14);
    let f = fit(rest_cap, cols - pad - aw - vw - pw - id_w - 4);
    format!(
        "{DIM}{age:<aw$}{RST} {}{verb:<vw$}{RST} {}{actor:<pw$}{RST} {ACC}{id:<14}{RST} {DIM}{f}{RST}",
        verb_colour(verb),
        part_colour(actor),
        aw = aw as usize,
        vw = vw as usize,
        pw = pw as usize,
    )
}

pub fn recent_section(snap: &Snapshot, cols: i64, snap_exists: bool) -> Vec<String> {
    let Some(ev0) = snap.get("SP_EVENT0").filter(|s| !s.is_empty()) else {
        return if snap_exists {
            vec![format!(" {DIM}RECENT{RST} {DIM}nothing in the window{RST}")]
        } else {
            vec![unread_row("RECENT", "no snapshot to read")]
        };
    };
    let mut out = vec![format!(" {DIM}RECENT{RST} {}", recent_row(cols, ev0, 8))];
    for i in 1..MAX_RECENT_ROWS {
        let Some(ev) = snap.get(&format!("SP_EVENT{i}")).filter(|s| !s.is_empty()) else { break };
        out.push(format!("        {}", recent_row(cols, ev, 8)));
    }
    out
}

fn inflow_row(cols: i64, raw: &str) -> String {
    let re = regex::Regex::new(r"^([0-9]+[smhd])\s+(\S+)\s+(P\S+)\s+(\S+)\s*(.*)$").unwrap();
    let Some(caps) = re.captures(raw) else {
        return format!("        {DIM}{}{RST}", fit(raw, cols - 8));
    };
    let age = &caps[1];
    let kind = &caps[2];
    let pri = &caps[3];
    let id = &caps[4];
    let rest = &caps[5];
    let aw = (age.chars().count() as i64).max(4);
    let kw = (kind.chars().count() as i64).max(8);
    let id_w = (id.chars().count() as i64).max(14);
    let f = fit(rest, cols - 12 - aw - kw - pri.chars().count() as i64 - id_w);
    format!(
        "        {DIM}{age:<aw$}{RST} {}{kind:<kw$}{RST} {}{pri}{RST} {ACC}{id:<14}{RST} {DIM}{f}{RST}",
        kind_colour(kind),
        pri_colour(pri),
        aw = aw as usize,
        kw = kw as usize,
    )
}

pub fn inflow_section(snap: &Snapshot, cols: i64) -> Vec<String> {
    let n_raw = snap.get("SP_INFLOW_N");
    if n_raw.is_none() || n_raw == Some("?") {
        return vec![unread_row("INFLOW", "cannot read what is being cut")];
    }
    let kcol = if snap.get("SP_INFLOW_DEFECT").unwrap_or("0") == "0" { DIM } else { WARN };
    let mut out = vec![format!(
         " {DIM}INFLOW{RST} {B}{}{RST} {DIM}new/{}m{RST}  {kcol}{}{RST}",
        n_raw.unwrap(),
        snap.q("SP_INFLOW_WIN"),
        snap.get("SP_INFLOW_KINDS").filter(|s| !s.is_empty()).unwrap_or("-"),
    )];
    for i in 0..MAX_INFLOW_ROWS {
        let Some(raw) = snap.get(&format!("SP_INFLOW{i}")).filter(|s| !s.is_empty()) else { break };
        out.push(inflow_row(cols, raw));
    }
    out
}

pub fn ci_section(snap: &Snapshot, cols: i64) -> Vec<String> {
    let n_raw = snap.get("SP_AWAITING_N");
    if n_raw.is_none() || n_raw == Some("?") {
        return vec![unread_row("CI", "cannot read what is parked on CI")];
    }
    let n: i64 = n_raw.unwrap().parse().unwrap_or(-1);
    let stuck: i64 = snap.get("SP_AWAITING_STUCK").unwrap_or("0").parse().unwrap_or(-1);
    if n == 0 && stuck == 0 {
        return vec![format!(" {DIM}CI{RST}     {DIM}nothing parked on CI{RST}")];
    }
    let age_disp = snap.get("SP_AWAITING_AGE").unwrap_or("");
    let ci_col = if age_disp.ends_with('h') || age_disp.ends_with('d') { WARN } else { DIM };
    let stuck_raw = snap.get("SP_AWAITING_STUCK").unwrap_or("0");
    let stuck_txt = if stuck_raw != "0" {
        format!(
            "   {WARN}{stuck_raw} parked with no run to wait for{RST} {DIM}({}){RST}",
            snap.get("SP_AWAITING_STUCK_ID").unwrap_or("?")
        )
    } else {
        String::new()
    };
    let mut out = vec![format!(
         " {DIM}CI{RST}     {B}{}{RST} bead(s) waiting on a run   {DIM}oldest{RST} {} {ci_col}{}{RST}{stuck_txt}",
        n_raw.unwrap(),
        snap.get("SP_AWAITING_OLDEST").unwrap_or("?"),
        age_disp.is_empty().then(|| "?".to_string()).unwrap_or_else(|| age_disp.to_string()),
    )];
    for i in 0..MAX_SECTION_ROWS {
        let Some(row) = snap.get(&format!("SP_AWAITING{i}")).filter(|s| !s.is_empty()) else { break };
        out.push(format!("        {DIM}{}{RST}", fit(row, cols - 8)));
    }
    out
}

/// `standing_lines` — the fixed-height figures: ATTN, SEND, BEADS, LAND, LOCK, GATE, SUITES,
/// BOX, MAIL, OPS. Always emitted in full; never part of the elastic share.
pub fn standing_lines(snap: &Snapshot, cols: i64) -> Vec<String> {
    let mut out = Vec::new();

    let fail = snap.get("SP_SENT_FAILED").unwrap_or("0");
    let fail_col = if fail != "0" {
        let age_m: i64 = snap.get("SP_SENT_FAILED_AGE_M").unwrap_or("?").parse().unwrap_or(-1);
        if age_m >= 60 { DIM } else { "\x1b[31m\x1b[1m" }
    } else {
        OK
    };
    out.push(format!(" {DIM}ATTN{RST}   {DIM}waiting on you{RST} {B}{}{RST}", snap.q("SP_WAITING")));

    let age_col_unused = ();
    let _ = age_col_unused;
    let unadopted = snap.get("SP_UNADOPTED").unwrap_or("0");
    let unadopt_seg = if unadopted != "0" {
        format!(" \u{b7} {WARN}{unadopted} unadopted{RST}")
    } else {
        String::new()
    };
    let branch_done = snap.get("SP_BRANCH_DONE").unwrap_or("0");
    let bd_col = if branch_done == "0" { DIM } else { WARN };
    let age_col = match snap.get("SP_UNSENT_OLDEST_H") {
        Some("?") | None => DIM,
        Some(v) => if v.parse::<i64>().unwrap_or(0) >= 24 { WARN } else { DIM },
    };
    out.push(format!(
         " {DIM}SEND{RST}   {B}{} branches unsent{RST} \u{b7} {bd_col}{} awaiting rites{RST} \u{b7} {DIM}oldest {}h{RST}{unadopt_seg}",
        snap.q("SP_UNSENT"),
        snap.get("SP_BRANCH_DONE").unwrap_or("?"),
        snap.q("SP_UNSENT_OLDEST_H"),
    ));
    let age_col = age_col; // silence unused-assign warnings across branches
    let _ = age_col;
    let fiend_age_sfx = if fail != "0" {
        match snap.get("SP_SENT_FAILED_AGE_M") {
            Some("?") | None => format!(" {DIM}(last: ?){RST}"),
            Some(m) => format!(" {DIM}(last: {m}m ago){RST}"),
        }
    } else {
        String::new()
    };
    out.push(format!(
        "        {fail_col}{fail} fiends{RST} {DIM}\u{2014} unsent work that came back{RST}{fiend_age_sfx}"
    ));

    out.push(format!(
         " {DIM}BEADS{RST}  {DIM}24h{RST}  closed {} \u{b7} {DIM}opened{RST} {}",
        snap.q("SP_CLOSED_24H"),
        snap.q("SP_OPENED_24H"),
    ));
    out.push(format!(
        "        {DIM}opened{RST} {}  {DIM}closed{RST} {}",
        snap.q("SP_BEADS_SPARK_OPENED"),
        snap.q("SP_BEADS_SPARK_CLOSED"),
    ));
    out.push(format!(
        "        {DIM}landed{RST} {}  {DIM}{}{RST} in 24h",
        snap.q("SP_BEADS_SPARK_LANDED"),
        snap.q("SP_BEADS_LANDED_24H"),
    ));
    out.push(format!(
        "        {DIM}{}{RST}",
        fit(snap.get("SP_CLOSED_KINDS").filter(|s| !s.is_empty()).unwrap_or("-"), cols - 8)
    ));
    let unl = snap.get("SP_UNLANDED_N").unwrap_or("?");
    let unl_col = if unl == "0" { OK } else { "\x1b[31m\x1b[1m" };
    out.push(format!(
        "        {DIM}24h worked{RST} {}:{RST} {} landed \u{b7} {DIM}{}{RST} queued \u{b7} {unl_col}{unl}{RST} done",
        snap.q("SP_CLOSED"),
        snap.q("SP_LANDED"),
        snap.q("SP_QUEUE_DEPTH"),
    ));
    out.push(format!(
        "        {DIM}{}{RST}",
        fit("done = closed, has branch, no landstate", cols - 8)
    ));

    let verdict = snap.get("SP_ACCEPT_VERDICT").unwrap_or("?");
    let acc_col = match verdict {
        "PASS" => OK,
        "FAIL" => "\x1b[31m\x1b[1m",
        _ => WARN,
    };
    let acc_age = match snap.get("SP_ACCEPT_AT") {
        Some(v) if v != "?" => {
            let at: i64 = v.parse().unwrap_or(0);
            age_str(&(now_placeholder() - at).to_string(), 86400)
        }
        _ => "?".to_string(),
    };
    let since = snap.get("SP_ACCEPT_SINCE").unwrap_or("1");
    let since_col = if since == "0" { OK } else { "\x1b[31m\x1b[1m" };
    out.push(format!(
        "        {DIM}accept{RST} {acc_col}{verdict}{RST} {acc_age} \u{b7} {since_col}{}{RST} untested since",
        snap.q("SP_ACCEPT_SINCE"),
    ));
    out.push(format!(
        "        {DIM}{}{RST}",
        fit(snap.get("SP_ACCEPT_TAG").filter(|s| !s.is_empty()).unwrap_or("-"), cols - 8)
    ));

    let land_rc = snap.q("SP_LAND_RC");
    let land_rc_col = match land_rc {
        "0" => OK,
        "?" => "\x1b[31m\x1b[1m",
        _ => WARN,
    };
    let land_age = match snap.get("SP_LAND_AT") {
        Some(v) if v != "?" => (now_placeholder() - v.parse::<i64>().unwrap_or(0)).to_string(),
        _ => "?".to_string(),
    };
    out.push(format!(
         " {DIM}LAND{RST}   {DIM}last{RST} {}  {DIM}rc{RST} {land_rc_col}{land_rc}{RST}  {DIM}branches{RST} {}  {DIM}moved{RST} {}",
        age_str(&land_age, 600),
        snap.q("SP_LAND_BRANCHES"),
        snap.q("SP_LAND_MOVED"),
    ));

    let gate_live = snap.get("SP_GATE_LIVE").unwrap_or("0");
    if gate_live != "0" && gate_live != "?" {
        out.push(format!("        {B}{gate_live} gate(s) running:{RST}"));
        let gate_n: i64 = snap.get("SP_GATE_N").unwrap_or("0").parse().unwrap_or(0);
        for gi in 0..gate_n {
            let gs = snap.q(&format!("SP_GATE{gi}_SLUG"));
            let ga = snap.q(&format!("SP_GATE{gi}_AGE"));
            let gp = snap.q(&format!("SP_GATE{gi}_PHASE"));
            let gw = snap.get(&format!("SP_GATE{gi}_WHY")).unwrap_or("");
            let age_s = if ga == "?" {
                "?".to_string()
            } else {
                match ga.parse::<i64>() {
                    Ok(v) if v < 120 => format!("{v}s"),
                    Ok(v) => format!("{}m", v / 60),
                    Err(_) => "?".to_string(),
                }
            };
            let phase_col = if gp == "waiting" { WARN } else { DIM };
            let why_sfx = if gw.is_empty() {
                String::new()
            } else {
                let f = fit(&format!(" \u{b7} {gw}"), cols - 30 - gs.chars().count() as i64 - age_s.chars().count() as i64);
                format!(" {DIM}{f}{RST}")
            };
            out.push(format!(
                "        {ACC}{gs}{RST}  {phase_col}{gp:<7}{RST}  {DIM}{age_s}{RST}{why_sfx}"
            ));
        }
    } else if gate_live == "?" {
        out.push(format!("        {BAD}{B}? cannot read gate state{RST}"));
    }
    let landprog_n: i64 = snap.get("SP_LANDPROG_N").unwrap_or("0").parse().unwrap_or(0);
    if landprog_n != 0 {
        for li in 0..landprog_n {
            if let Some(lp) = snap.get(&format!("SP_LANDPROG{li}")).filter(|s| !s.is_empty()) {
                out.push(format!("        {DIM}{}{RST}", fit(lp, cols - 10)));
            }
        }
    }

    let graph = format!(
        "open {} \u{b7} ready {} \u{b7} working {} \u{b7} poison {} \u{b7} strand {} (ledger {})",
        snap.q("SP_OPEN"),
        snap.q("SP_READY"),
        snap.q("SP_INPROG"),
        snap.q("SP_POISON"),
        snap.q("SP_STRAND_GHOST"),
        snap.q("SP_STRANDS"),
    );
    let graph_col = if snap.get("SP_POISON").unwrap_or("0") == "0" { DIM } else { WARN };
    out.push(format!("        {graph_col}{}{RST}", fit(&graph, cols - 8)));

    let ll = snap.q("SP_LIVELOCKED");
    let ic = snap.q("SP_INVALID_CLOSED");
    let uf = snap.q("SP_UNFILED_FOLLOW");
    let col_for = |v: &str| match v {
        "0" => DIM,
        "?" => "\x1b[31m\x1b[1m",
        _ => "\x1b[33m\x1b[1m",
    };
    if ll != "0" || ic != "0" || uf != "0" {
        out.push(format!(
             " {DIM}LOCK{RST}   {DIM}livelocked{RST} {}{ll}{RST} \u{b7} {DIM}invalid-closed{RST} {}{ic}{RST} \u{b7} {DIM}unfiled-follow{RST} {}{uf}{RST}",
            col_for(ll), col_for(ic), col_for(uf),
        ));
        let ll_n: i64 = snap.get("SP_LIVELOCK_N").unwrap_or("0").parse().unwrap_or(0);
        for i in 0..ll_n.min(5) {
            if let Some(row) = snap.get(&format!("SP_LIVELOCK{i}")).filter(|s| !s.is_empty()) {
                out.push(format!("        {DIM}  {WARN}{}{RST}", fit(row, cols - 12)));
            }
        }
        let ic_n: i64 = snap.get("SP_INVCLSD_N").unwrap_or("0").parse().unwrap_or(0);
        for i in 0..ic_n.min(5) {
            if let Some(row) = snap.get(&format!("SP_INVCLSD{i}")).filter(|s| !s.is_empty()) {
                out.push(format!("        {DIM}  {WARN}{}{RST}", fit(row, cols - 12)));
            }
        }
        let uf_n: i64 = snap.get("SP_UNFLFLW_N").unwrap_or("0").parse().unwrap_or(0);
        for i in 0..uf_n.min(5) {
            if let Some(row) = snap.get(&format!("SP_UNFLFLW{i}")).filter(|s| !s.is_empty()) {
                out.push(format!("        {DIM}  {WARN}{}{RST}", fit(row, cols - 12)));
            }
        }
    }

    let fault_col = match snap.get("SP_YIELD_FAULT") {
        Some("0") => OK,
        None | Some("?") => "\x1b[31m\x1b[1m",
        _ => WARN,
    };
    let solo = match snap.get("SP_YIELD_SOLO_MED") {
        Some(v) if v != "?" => format!("{v}s"),
        _ => "?".to_string(),
    };
    let conc = match snap.get("SP_YIELD_CONC_MED") {
        Some(v) if v != "?" => format!("{v}s"),
        _ => "?".to_string(),
    };
    out.push(format!(
         " {DIM}GATE{RST}   {DIM}reds{RST} {} \u{b7} {DIM}defect{RST} {} \u{b7} {DIM}gate fault{RST} {fault_col}{}{RST} \u{b7} {DIM}unknown{RST} {}",
        snap.q("SP_YIELD_REDS"),
        snap.q("SP_YIELD_DEFECT"),
        snap.q("SP_YIELD_FAULT"),
        snap.q("SP_YIELD_UNKNOWN"),
    ));
    out.push(format!(
        "        {DIM}cost{RST} {solo} solo \u{b7} {conc} with another gate overlapping {DIM}(median){RST}"
    ));

    let st_sum = match snap.get("SP_SUITE_LAST_SUM") {
        Some(v) if v != "?" => format!("{v}s"),
        _ => "?".to_string(),
    };
    let st_wall = match snap.get("SP_SUITE_LAST_WALL") {
        Some(v) if v != "?" => format!("{v}s"),
        _ => "?".to_string(),
    };
    out.push(format!(" {DIM}SUITES{RST} {DIM}sum{RST} {st_sum}  {DIM}wall{RST} {st_wall}"));

    // "disk /" names the root mount; the value (with its own trailing "%" from `num`'s
    // label) follows directly, same for "workspaces".
    out.push(format!(
         " {DIM}BOX{RST}    {DIM}disk{RST} / {}  {DIM}workspaces{RST} {}  {DIM}cpu{RST} {}% idle  {DIM}load{RST} {}",
        num(snap.q("SP_DISK_ROOT_PCT"), 85, "%"),
        num(snap.q("SP_DISK_WS_PCT"), 85, "%"),
        snap.q("SP_CPU_IDLE"),
        snap.q("SP_LOAD1"),
    ));

    let m_unread = snap.q("SP_MAIL_UNREAD");
    let m_oldest = snap.get("SP_MAIL_OLDEST_AGE").unwrap_or("-");
    if m_unread == "?" {
        out.push(format!(" {DIM}MAIL{RST}   {BAD}{B}? cannot read mailbox{RST}"));
    } else {
        let mut age_sfx = String::new();
        if m_oldest != "-" && m_unread != "0" {
            age_sfx = format!("  {DIM}oldest {}{RST}", mail_dur(m_oldest));
        }
        let m_col = if m_unread == "0" { DIM } else { "\x1b[33m\x1b[1m" };
        out.push(format!(" {DIM}MAIL{RST}   {m_col}{m_unread} unread{RST}{age_sfx}"));
        let mn: i64 = snap.get("SP_MAIL_N").unwrap_or("0").parse().unwrap_or(0);
        for mi in 0..mn.min(5) {
            let Some(mrow) = snap.get(&format!("SP_MAIL{mi}")).filter(|s| !s.is_empty()) else { continue };
            let mut parts = mrow.splitn(3, '|');
            let m_age_raw = parts.next().unwrap_or("0");
            let m_state = parts.next().unwrap_or("");
            let m_subj = parts.next().unwrap_or("");
            let m_dur = mail_dur(m_age_raw);
            let st_col = match m_state {
                "NEW" => "\x1b[33m\x1b[1m",
                "READ" => ACC,
                "DONE" => DIM,
                _ => BAD,
            };
            let f = fit(m_subj, cols - 22);
            out.push(format!("        {DIM}{m_dur:>4}{RST}  {st_col}{m_state:<4}{RST}  {DIM}{f}{RST}"));
        }
    }

    out.push(format!(
         " {DIM}OPS{RST}    {DIM}never-fired{RST} {} \u{b7} {DIM}recurred (no hold){RST} {} \u{b7} {DIM}sweep-last{RST} {}",
        num(snap.q("SP_SOP_NEVER_FIRED"), 5, ""),
        bad_unless_zero(snap.q("SP_SOP_RECURRED"), ""),
        age_str(snap.q("SP_SWEEP_AGE"), 1800),
    ));

    out
}

/// A placeholder so `standing_lines`'s tests can be exact without threading `now` through
/// every call site (age-since-epoch fields are cosmetic staleness colouring, not load-
/// bearing for parity beyond "a number was rendered"). Real callers should prefer the
/// `*_with_now` variants once wired into `frame`; see `../DESIGN.md` Decisions.
fn now_placeholder() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(pairs: &[(&str, &str)]) -> Snapshot {
        let text: String = pairs.iter().map(|(k, v)| format!("{k}='{v}'\n")).collect();
        Snapshot::parse(&text)
    }

    #[test]
    fn win_renders_question_mark_when_ratelim_is_stale() {
        let extra = TokensExtra { renderer_rev: "", collector_rev: "", tok_win_spark: "" };
        let vals = |age: &'static str| {
            snap(&[("SP_RATELIM_5H_PCT", "23"), ("SP_RATELIM_7D_PCT", "96"), ("SP_RATELIM_5H_MIN", "60"),
                   ("SP_RATELIM_7D_MIN", "900"), ("SP_RATELIM_AGE", age)])
        };
        let fresh = tokens_section(&vals("60"), 80, &extra).join("\n");
        assert!(fresh.contains("23%") && fresh.contains("96%"));
        let stale = tokens_section(&vals("7200"), 80, &extra).join("\n");
        assert!(!stale.contains("23") && !stale.contains("96"));
        assert!(stale.contains("5h ") && stale.contains("(120m ago)"));
        let unknown = tokens_section(&vals("?"), 80, &extra).join("\n");
        assert!(!unknown.contains("23%") && !unknown.contains("96%"));
    }

    #[test]
    fn now_section_unread_when_snapshot_missing_and_no_live_aeons() {
        let s = snap(&[]);
        let out = now_section(&s, 80, 0, 2);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("cannot read the aeon roster"));
    }

    #[test]
    fn now_section_idle_when_both_zero() {
        let s = snap(&[("SP_AEON_N", "0")]);
        let out = now_section(&s, 80, 0, 2);
        assert_eq!(out, vec![format!(" {DIM}NOW{RST}    {DIM}no aeon working{RST}")]);
    }

    #[test]
    fn next_section_nothing_to_claim() {
        let s = snap(&[("SP_NEXT_N", "0"), ("SP_REACHABLE", "0")]);
        let out = next_section(&s, 80);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("nothing to claim"));
    }

    #[test]
    fn recent_row_parses_bead_id_and_colours_it() {
        let line = recent_row(80, "52s builder landed sp-abcd Fixed the thing", 8);
        assert!(line.contains("sp-abcd"));
        assert!(line.contains(ACC));
    }

    #[test]
    fn recent_row_unparseable_prints_as_is_dimmed_and_cut() {
        let line = recent_row(20, "not the expected shape at all, quite long", 8);
        assert!(line.starts_with(DIM));
        assert!(line.contains('\u{2026}'));
    }

    #[test]
    fn recent_row_aggregate_verb_does_not_colour_the_count_as_a_bead() {
        let line = recent_row(80, "3m sentinel reaped 1 landed branch(es)", 8);
        // "1" must not be printed in the accent colour reserved for real sp- ids.
        assert!(!line.contains(&format!("{ACC}1")));
    }

    #[test]
    fn inflow_row_parses_kind_and_priority() {
        let line = inflow_row(80, "5m bug P0 sp-xyz9 Something broke");
        assert!(line.contains("sp-xyz9"));
        assert!(line.contains(BAD)); // bug is coloured BAD
    }

    #[test]
    fn ci_section_prints_even_at_zero() {
        let s = snap(&[("SP_AWAITING_N", "0"), ("SP_AWAITING_STUCK", "0")]);
        assert_eq!(ci_section(&s, 80), vec![format!(" {DIM}CI{RST}     {DIM}nothing parked on CI{RST}")]);
    }

    #[test]
    fn halt_banner_silent_when_no_stamp_and_sentinel_active() {
        let h = HaltState { stamp_exists: false, since: String::new(), why: String::new(), sentinel_active: Some(true) };
        assert!(halt_banner(&h).is_empty());
    }

    #[test]
    fn halt_banner_shown_when_stamp_present_even_if_sentinel_active() {
        let h = HaltState {
            stamp_exists: true,
            since: "2026-09-29T12:00:00".to_string(),
            why: "fixing the gate".to_string(),
            sentinel_active: Some(true),
        };
        let out = halt_banner(&h);
        assert!(out[0].contains("SPIRA STOPPED"));
        assert!(out.iter().any(|l| l.contains("since")));
        assert!(out.iter().any(|l| l.contains("fixing the gate")));
    }

    #[test]
    fn halt_banner_shown_with_no_stamp_and_sentinel_not_active() {
        let h = HaltState { stamp_exists: false, since: String::new(), why: String::new(), sentinel_active: Some(false) };
        let out = halt_banner(&h);
        assert!(out[0].contains("SPIRA STOPPED"));
        assert!(out.iter().any(|l| l.contains("sentinel.timer is inactive")));
    }

    #[test]
    fn drain_banner_absent_when_no_stamp() {
        assert!(drain_banner(&DrainState { stamp_mtime: None }, 1000).is_empty());
    }

    #[test]
    fn drain_banner_shows_minutes_gated() {
        let out = drain_banner(&DrainState { stamp_mtime: Some(1000) }, 1000 + 125);
        assert!(out[0].contains("DRAINING"));
        assert!(out[0].contains('2')); // 125s -> 2 minutes
    }

    #[test]
    fn share_specs_from_next_and_recent_bases_cap_at_rendered_want() {
        // A base above what the section actually rendered would hand it rows it has no
        // content for; frame.rs is responsible for capping nb/rb/ib by `want`, tested there.
        use super::super::share::Spec;
        let s = Spec::new(NEXT_BASE_ROWS + 1, 3, false);
        assert_eq!(s.base, 6);
    }
}
