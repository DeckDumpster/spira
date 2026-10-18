//! `queue churn` — what failures cost the queue and how the mender is doing against its
//! intent. Pure: the lifecycle's rows in, a report out; the caller supplies the diff size.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

pub const FAILURE_TO_OUTCOME_P95: u64 = 300;
pub const TRIAGE_P95: u64 = 90;
pub const BATCHER_WAIT_P95: u64 = 300;
pub const MEND_RATE_PCT: u64 = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eject {
    pub batch: String,
    pub bead: String,
    pub at: i64,
    pub tip: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mending {
    pub batch: String,
    pub pass: u32,
    pub suite: String,
    pub state: String,
    pub failed_at: i64,
    pub picked_at: Option<i64>,
    pub triage_ended_at: Option<i64>,
    pub outcome_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mend {
    pub batch: String,
    pub pass: u32,
    pub suite: String,
    pub bead: String,
    pub from_tip: String,
    pub tip: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub ejects: Vec<Eject>,
    pub landed_at: BTreeMap<String, Vec<i64>>,
    pub landed_tip: BTreeMap<String, String>,
    pub mendings: Vec<Mending>,
    pub mends: Vec<Mend>,
    pub pass_ends: Vec<(String, u32, i64)>,
}

fn text(v: &Value, k: &str) -> Option<String> {
    match v.get(k)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int(v: &Value, k: &str) -> Option<i64> {
    text(v, k)?.parse().ok()
}

fn rows<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// `evidence` is the event's own JSON (a string cell, or already an object).
fn evidence(row: &Value) -> Value {
    match row.get("evidence") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

/// Parses `spira-lc churn-facts`. Rows missing a field the measure needs are skipped, never guessed.
pub fn parse_facts(json: &str) -> Result<Facts, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("churn-facts: {e}"))?;
    let tips: BTreeMap<(String, String), String> = rows(&v, "members")
        .iter()
        .filter_map(|r| Some(((text(r, "batch_id")?, text(r, "bead_id")?), text(r, "tip")?)))
        .collect();
    let mut f = Facts::default();
    for r in rows(&v, "ejects") {
        let (Some(batch), Some(at)) = (text(r, "batch_id"), int(r, "at")) else {
            continue;
        };
        let Some(bead) = evidence(r)
            .pointer("/Eject/bead_id")
            .and_then(Value::as_str)
            .map(String::from)
        else {
            continue;
        };
        let tip = tips
            .get(&(batch.clone(), bead.clone()))
            .cloned()
            .unwrap_or_default();
        f.ejects.push(Eject {
            batch,
            bead,
            at,
            tip,
        });
    }
    for r in rows(&v, "landed_at") {
        if let (Some(b), Some(at)) = (text(r, "bead"), int(r, "at")) {
            f.landed_at.entry(b).or_default().push(at);
        }
    }
    for r in rows(&v, "landed_tip") {
        if let (Some(b), Some(t)) = (text(r, "bead_id"), text(r, "tip")) {
            f.landed_tip.insert(b, t);
        }
    }
    for r in rows(&v, "mendings") {
        let (Some(batch), Some(pass), Some(suite), Some(state), Some(failed_at)) = (
            text(r, "batch_id"),
            int(r, "pass"),
            text(r, "suite"),
            text(r, "state"),
            int(r, "failed_at"),
        ) else {
            continue;
        };
        f.mendings.push(Mending {
            batch,
            pass: pass as u32,
            suite,
            state,
            failed_at,
            picked_at: int(r, "picked_at"),
            triage_ended_at: int(r, "triage_ended_at"),
            outcome_at: int(r, "outcome_at"),
        });
    }
    for r in rows(&v, "mends") {
        if let (Some(batch), Some(pass), Some(suite), Some(bead), Some(from_tip), Some(tip)) = (
            text(r, "batch_id"),
            int(r, "pass"),
            text(r, "suite"),
            text(r, "bead_id"),
            text(r, "from_tip"),
            text(r, "tip"),
        ) {
            f.mends.push(Mend {
                batch,
                pass: pass as u32,
                suite,
                bead,
                from_tip,
                tip,
            });
        }
    }
    for r in rows(&v, "pass_ends") {
        let (Some(batch), Some(at)) = (text(r, "batch_id"), int(r, "at")) else {
            continue;
        };
        let ev = evidence(r);
        let n = ev
            .pointer("/PassRed/n")
            .or_else(|| ev.pointer("/PassPreempted/n"))
            .and_then(Value::as_i64);
        if let Some(n) = n {
            f.pass_ends.push((batch, n as u32, at));
        }
    }
    Ok(f)
}

/// A member sent back and the tip it landed at, when it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Return {
    pub bead: String,
    pub at: i64,
    pub from_tip: String,
    pub landed_at: Option<i64>,
    pub landed_tip: Option<String>,
}

pub fn returns(f: &Facts, from: i64, to: i64) -> Vec<Return> {
    let mut out: Vec<Return> = f
        .ejects
        .iter()
        .filter(|e| e.at >= from && e.at < to)
        .map(|e| Return {
            bead: e.bead.clone(),
            at: e.at,
            from_tip: e.tip.clone(),
            landed_at: f
                .landed_at
                .get(&e.bead)
                .and_then(|l| l.iter().copied().filter(|t| *t >= e.at).min()),
            landed_tip: f.landed_tip.get(&e.bead).cloned(),
        })
        .collect();
    out.sort_by(|a, b| (a.at, &a.bead).cmp(&(b.at, &b.bead)));
    out
}

/// Changed lines (added + deleted) between two tips; None when the diff cannot be read.
pub type Lines<'a> = &'a dyn Fn(&str, &str) -> Option<u64>;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Wasted {
    pub returns: u64,
    pub beads_returned: u64,
    pub landed: u64,
    pub le4: u64,
    pub le20: u64,
    pub identical: u64,
    pub unmeasured: u64,
}

/// Per bead, the last return before it landed, against the tip it landed at.
pub fn wasted(rs: &[Return], lines: Lines) -> Wasted {
    let mut w = Wasted {
        returns: rs.len() as u64,
        ..Wasted::default()
    };
    let mut last: BTreeMap<&str, &Return> = BTreeMap::new();
    for r in rs {
        last.insert(&r.bead, r);
    }
    w.beads_returned = last.len() as u64;
    for r in last.values() {
        let (Some(_), Some(tip)) = (r.landed_at, &r.landed_tip) else {
            continue;
        };
        w.landed += 1;
        match lines(&r.from_tip, tip) {
            None => w.unmeasured += 1,
            Some(n) => {
                w.le4 += (n <= 4) as u64;
                w.le20 += (n <= 20) as u64;
                w.identical += (n == 0) as u64;
            }
        }
    }
    w
}

/// Nearest-rank percentile; None for an empty sample.
pub fn percentile(mut xs: Vec<u64>, pct: u64) -> Option<u64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_unstable();
    let rank = (pct * xs.len() as u64).div_ceil(100).max(1) as usize;
    Some(xs[rank - 1])
}

fn gap(from: i64, to: i64) -> u64 {
    (to - from).max(0) as u64
}

#[derive(Debug, PartialEq, Eq)]
pub struct FailureRow {
    pub batch: String,
    pub pass: u32,
    pub suite: String,
    pub outcome: String,
    pub attribution: String,
    pub to_outcome_s: Option<u64>,
    pub triage_s: Option<u64>,
    pub lines: Option<u64>,
    pub red_to_landed_s: Option<u64>,
}

fn is_terminal(state: &str) -> bool {
    !matches!(state, "WAITING" | "MENDING")
}

/// The members a failure is attributed to: the beads a mender fixed for it, or for a return the
/// round's ejects after the failure and before the next pass ended.
fn members_of(f: &Facts, m: &Mending) -> Vec<(String, Option<(String, String)>)> {
    let mended: Vec<_> = f
        .mends
        .iter()
        .filter(|x| x.batch == m.batch && x.pass == m.pass && x.suite == m.suite)
        .map(|x| (x.bead.clone(), Some((x.from_tip.clone(), x.tip.clone()))))
        .collect();
    if !mended.is_empty() || m.state != "RETURNED" {
        return mended;
    }
    let end = f
        .pass_ends
        .iter()
        .filter(|(b, p, _)| *b == m.batch && *p > m.pass)
        .map(|(_, _, at)| *at)
        .min()
        .unwrap_or(i64::MAX);
    f.ejects
        .iter()
        .filter(|e| e.batch == m.batch && e.at >= m.failed_at && e.at <= end)
        .map(|e| (e.bead.clone(), None))
        .collect()
}

pub fn failures(f: &Facts, from: i64, to: i64, lines: Lines) -> Vec<FailureRow> {
    let mut out = Vec::new();
    for m in f
        .mendings
        .iter()
        .filter(|m| m.failed_at >= from && m.failed_at < to)
    {
        let members = members_of(f, m);
        let attribution = match (m.state.as_str(), members.is_empty()) {
            ("ENVIRONMENT", _) => "environment".to_string(),
            ("FLAKY", _) => "flaky".to_string(),
            ("UNSURE", _) => "unsure".to_string(),
            (_, true) => "member".to_string(),
            (_, false) => members
                .iter()
                .map(|(b, _)| b.as_str())
                .collect::<Vec<_>>()
                .join(","),
        };
        let line_count = members
            .iter()
            .map(|(_, t)| t.as_ref().map(|(a, b)| lines(a, b)))
            .try_fold(0u64, |acc, n| n.flatten().map(|n| acc + n));
        let landed = members
            .iter()
            .map(|(b, _)| {
                f.landed_at
                    .get(b)
                    .and_then(|l| l.iter().copied().filter(|t| *t >= m.failed_at).min())
            })
            .collect::<Option<Vec<_>>>()
            .and_then(|v| v.into_iter().max());
        out.push(FailureRow {
            batch: m.batch.clone(),
            pass: m.pass,
            suite: m.suite.clone(),
            outcome: m.state.to_lowercase(),
            attribution,
            to_outcome_s: m
                .outcome_at
                .filter(|_| is_terminal(&m.state))
                .map(|o| gap(m.failed_at, o)),
            triage_s: m.picked_at.zip(m.triage_ended_at).map(|(a, b)| gap(a, b)),
            lines: if members.is_empty() { None } else { line_count },
            red_to_landed_s: if members.is_empty() {
                None
            } else {
                landed.map(|l| gap(m.failed_at, l))
            },
        });
    }
    out.sort_by(|a, b| (&a.batch, a.pass, &a.suite).cmp(&(&b.batch, b.pass, &b.suite)));
    out
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Measures {
    pub failures: u64,
    pub open: u64,
    pub p95_to_outcome: Option<u64>,
    pub p95_triage: Option<u64>,
    pub p95_batcher_wait: Option<u64>,
    pub mended: u64,
    pub attributed: u64,
    pub wrong_ejects: u64,
}

/// `wrong_ejects` are returns whose member later landed byte-identical: nothing was wrong with it.
pub fn measures(f: &Facts, rows: &[FailureRow], from: i64, to: i64, wrong_ejects: u64) -> Measures {
    let in_window: Vec<&Mending> = f
        .mendings
        .iter()
        .filter(|m| m.failed_at >= from && m.failed_at < to)
        .collect();
    let mut m = Measures {
        failures: in_window.len() as u64,
        wrong_ejects,
        ..Measures::default()
    };
    m.open = in_window.iter().filter(|x| !is_terminal(&x.state)).count() as u64;
    m.p95_to_outcome = percentile(rows.iter().filter_map(|r| r.to_outcome_s).collect(), 95);
    m.p95_triage = percentile(rows.iter().filter_map(|r| r.triage_s).collect(), 95);
    m.mended = in_window.iter().filter(|x| x.state == "MENDED").count() as u64;
    m.attributed = m.mended + in_window.iter().filter(|x| x.state == "RETURNED").count() as u64;
    let mut waits = Vec::new();
    let passes: BTreeSet<(&str, u32)> = in_window
        .iter()
        .map(|x| (x.batch.as_str(), x.pass))
        .collect();
    for (batch, pass) in passes {
        let outs: Vec<i64> = in_window
            .iter()
            .filter(|x| x.batch == batch && x.pass == pass)
            .filter_map(|x| x.outcome_at)
            .collect();
        let end = f
            .pass_ends
            .iter()
            .find(|(b, p, _)| b == batch && *p == pass)
            .map(|(_, _, at)| *at);
        if let (Some(end), Some(last)) = (end, outs.iter().max()) {
            waits.push(gap(end, *last));
        }
    }
    m.p95_batcher_wait = percentile(waits, 95);
    m
}

fn secs(v: Option<u64>) -> String {
    v.map_or("?".into(), |s| format!("{s}s"))
}

fn verdict(v: Option<u64>, limit: u64) -> &'static str {
    match v {
        None => "NO DATA",
        Some(s) if s < limit => "MET",
        Some(_) => "MISSED",
    }
}

pub fn render(w: &Wasted, rows: &[FailureRow], m: &Measures) -> String {
    let mut o = String::new();
    o.push_str("sent back and landed\n");
    o.push_str(&format!("  returns:               {}\n", w.returns));
    o.push_str(&format!("  beads sent back:       {}\n", w.beads_returned));
    o.push_str(&format!("  later landed:          {}\n", w.landed));
    o.push_str(&format!("  landed with <= 4 lines:  {}\n", w.le4));
    o.push_str(&format!("  landed with <= 20 lines: {}\n", w.le20));
    o.push_str(&format!("  landed byte-identical: {}\n", w.identical));
    if w.unmeasured > 0 {
        o.push_str(&format!("  diff unreadable:       {}\n", w.unmeasured));
    }
    o.push_str("\nfailures\n");
    o.push_str("  batch pass suite outcome attribution to_outcome triage lines red_to_landed\n");
    for r in rows {
        let n = |v: Option<u64>, unit: &str| v.map_or("?".to_string(), |x| format!("{x}{unit}"));
        o.push_str(&format!(
            "  {} {} {} {} {} {} {} {} {}\n",
            r.batch,
            r.pass,
            r.suite,
            r.outcome,
            r.attribution,
            n(r.to_outcome_s, "s"),
            n(r.triage_s, "s"),
            n(r.lines, ""),
            n(r.red_to_landed_s, "s")
        ));
    }
    o.push_str("\nintent\n");
    o.push_str(&format!(
        "  failures:                    {} ({} still open)\n",
        m.failures, m.open
    ));
    o.push_str(&format!(
        "  p95 failure-to-outcome:      {} (target < {FAILURE_TO_OUTCOME_P95}s) {}\n",
        secs(m.p95_to_outcome),
        verdict(m.p95_to_outcome, FAILURE_TO_OUTCOME_P95)
    ));
    o.push_str(&format!(
        "  p95 triage:                  {} (target < {TRIAGE_P95}s) {}\n",
        secs(m.p95_triage),
        verdict(m.p95_triage, TRIAGE_P95)
    ));
    o.push_str(&format!(
        "  p95 batcher wait:            {} (target < {BATCHER_WAIT_P95}s) {}\n",
        secs(m.p95_batcher_wait),
        verdict(m.p95_batcher_wait, BATCHER_WAIT_P95)
    ));
    let rate = (m.attributed > 0).then(|| m.mended * 100 / m.attributed);
    let rate_s = rate.map_or("?".to_string(), |r| {
        format!("{r}% ({}/{})", m.mended, m.attributed)
    });
    let rate_v = match rate {
        None => "NO DATA",
        Some(r) if r >= MEND_RATE_PCT => "MET",
        Some(_) => "MISSED",
    };
    o.push_str(&format!(
        "  mend rate:                   {rate_s} (target >= {MEND_RATE_PCT}%) {rate_v}\n"
    ));
    o.push_str(&format!(
        "  wrong ejects:                {} (target 0) {}\n",
        m.wrong_ejects,
        if m.wrong_ejects == 0 { "MET" } else { "MISSED" }
    ));
    o
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// `<n>s|m|h|d` before `now`, `YYYY-MM-DD`, or `YYYY-MM-DDTHH:MM:SSZ`.
pub fn parse_when(s: &str, now: i64) -> Option<i64> {
    let (head, unit) = s.split_at(s.len().checked_sub(1)?);
    if let (Ok(n), Some(mult)) = (
        head.parse::<i64>(),
        [("s", 1), ("m", 60), ("h", 3600), ("d", 86400)]
            .iter()
            .find(|(u, _)| *u == unit)
            .map(|(_, k)| *k),
    ) {
        return Some(now - n * mult);
    }
    let (date, time) = s.split_once('T').unwrap_or((s, "00:00:00Z"));
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    if d.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }
    let mut t = time
        .strip_suffix('Z')?
        .split(':')
        .map(|p| p.parse::<i64>().ok());
    let (h, mi, se) = (t.next()??, t.next()??, t.next()??);
    Some(days_from_civil(y, mo, da) * 86400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        assert_eq!(percentile(vec![], 95), None);
        assert_eq!(percentile(vec![7], 95), Some(7));
        assert_eq!(percentile((1..=20).collect(), 95), Some(19));
        assert_eq!(percentile((1..=21).collect(), 95), Some(20));
    }

    #[test]
    fn when_parses_durations_dates_and_timestamps() {
        assert_eq!(parse_when("2d", 1_000_000), Some(1_000_000 - 172_800));
        assert_eq!(parse_when("1970-01-02", 0), Some(86_400));
        assert_eq!(parse_when("2026-09-26", 0), Some(1_790_380_800));
        assert_eq!(
            parse_when("2026-09-26T01:02:03Z", 0),
            Some(1_790_380_800 + 3723)
        );
        assert_eq!(parse_when("2026-13-01", 0), None);
        assert_eq!(parse_when("soon", 0), None);
        assert_eq!(parse_when("", 0), None);
    }

    fn landed_return(bead: &str, at: i64, from: &str, to: &str) -> (Eject, i64, String, String) {
        (
            Eject {
                batch: "b".into(),
                bead: bead.into(),
                at: 100,
                tip: from.into(),
            },
            at,
            bead.into(),
            to.into(),
        )
    }

    /// 68 beads sent back and landed: 37 within 4 lines, 21 more within 20, 10 above; two have a second return.
    #[test]
    fn the_design_baseline_is_reproduced_from_its_shape() {
        let mut f = Facts::default();
        let sizes: Vec<u64> = std::iter::repeat_n(2, 37)
            .chain(std::iter::repeat_n(15, 21))
            .chain(std::iter::repeat_n(90, 10))
            .collect();
        for (i, _) in sizes.iter().enumerate() {
            let (e, at, bead, to) = landed_return(
                &format!("sp-{i}"),
                500,
                &format!("from{i}"),
                &format!("to{i}"),
            );
            f.ejects.push(e);
            f.landed_at.entry(bead.clone()).or_default().push(at);
            f.landed_tip.insert(bead, to);
        }
        for i in 0..30 {
            f.ejects.push(Eject {
                batch: "b".into(),
                bead: format!("sp-{i}"),
                at: 90,
                tip: "old".into(),
            });
        }
        let rs = returns(&f, 0, 1000);
        assert_eq!(rs.len(), 98);
        let lines = |a: &str, _: &str| {
            a.strip_prefix("from")
                .and_then(|i| i.parse::<usize>().ok())
                .map(|i| sizes[i])
        };
        let w = wasted(&rs, &lines);
        assert_eq!(
            (w.returns, w.beads_returned, w.landed, w.le4, w.le20),
            (98, 68, 68, 37, 58)
        );
    }

    #[test]
    fn an_unlanded_or_unreadable_return_is_not_counted_as_small() {
        let f = Facts {
            ejects: vec![
                Eject {
                    batch: "b".into(),
                    bead: "a".into(),
                    at: 10,
                    tip: "t1".into(),
                },
                Eject {
                    batch: "b".into(),
                    bead: "c".into(),
                    at: 10,
                    tip: "t2".into(),
                },
            ],
            landed_at: BTreeMap::from([("a".into(), vec![50])]),
            landed_tip: BTreeMap::from([("a".into(), "t9".into())]),
            ..Facts::default()
        };
        let w = wasted(&returns(&f, 0, 100), &|_, _| None);
        assert_eq!(
            (w.beads_returned, w.landed, w.le4, w.unmeasured),
            (2, 1, 0, 1)
        );
    }

    #[test]
    fn a_landing_before_the_return_is_not_the_return_landing() {
        let f = Facts {
            ejects: vec![Eject {
                batch: "b".into(),
                bead: "a".into(),
                at: 100,
                tip: "t".into(),
            }],
            landed_at: BTreeMap::from([("a".into(), vec![50])]),
            landed_tip: BTreeMap::from([("a".into(), "t2".into())]),
            ..Facts::default()
        };
        assert_eq!(returns(&f, 0, 1000)[0].landed_at, None);
    }

    fn mending(
        suite: &str,
        state: &str,
        failed: i64,
        picked: i64,
        triage: i64,
        out: Option<i64>,
    ) -> Mending {
        Mending {
            batch: "b".into(),
            pass: 1,
            suite: suite.into(),
            state: state.into(),
            failed_at: failed,
            picked_at: Some(picked),
            triage_ended_at: Some(triage),
            outcome_at: out,
        }
    }

    #[test]
    fn measures_cover_each_of_the_intents_numbers() {
        let f = Facts {
            mendings: vec![
                mending("a.sh", "MENDED", 1000, 1002, 1040, Some(1100)),
                mending("b.sh", "RETURNED", 1010, 1012, 1100, Some(1400)),
                mending("c.sh", "ENVIRONMENT", 1020, 1021, 1030, Some(1050)),
                mending("d.sh", "MENDING", 1030, 1031, 1031, None),
            ],
            mends: vec![Mend {
                batch: "b".into(),
                pass: 1,
                suite: "a.sh".into(),
                bead: "sp-a".into(),
                from_tip: "x".into(),
                tip: "y".into(),
            }],
            ejects: vec![Eject {
                batch: "b".into(),
                bead: "sp-b".into(),
                at: 1390,
                tip: "z".into(),
            }],
            landed_at: BTreeMap::from([("sp-a".into(), vec![2000]), ("sp-b".into(), vec![9000])]),
            pass_ends: vec![("b".into(), 1, 1200)],
            ..Facts::default()
        };
        let rows = failures(&f, 0, 10_000, &|_, _| Some(3));
        let by = |s: &str| rows.iter().find(|r| r.suite == s).unwrap();
        assert_eq!(
            (
                by("a.sh").attribution.as_str(),
                by("a.sh").lines,
                by("a.sh").red_to_landed_s
            ),
            ("sp-a", Some(3), Some(1000))
        );
        assert_eq!(
            (
                by("b.sh").attribution.as_str(),
                by("b.sh").to_outcome_s,
                by("b.sh").red_to_landed_s
            ),
            ("sp-b", Some(390), Some(7990))
        );
        assert_eq!(
            (by("c.sh").attribution.as_str(), by("c.sh").lines),
            ("environment", None)
        );
        assert_eq!(by("d.sh").to_outcome_s, None);
        let m = measures(&f, &rows, 0, 10_000, 0);
        assert_eq!((m.failures, m.open, m.mended, m.attributed), (4, 1, 1, 2));
        assert_eq!(m.p95_to_outcome, Some(390));
        assert_eq!(m.p95_triage, Some(88));
        assert_eq!(m.p95_batcher_wait, Some(200));
        let out = render(&wasted(&[], &|_, _| None), &rows, &m);
        assert!(out.contains("p95 failure-to-outcome:      390s (target < 300s) MISSED"));
        assert!(out.contains("p95 triage:                  88s (target < 90s) MET"));
        assert!(out.contains("mend rate:                   50% (1/2) (target >= 80%) MISSED"));
        assert!(out.contains("wrong ejects:                0 (target 0) MET"));
    }

    #[test]
    fn no_rows_render_no_data_never_met() {
        let m = measures(&Facts::default(), &[], 0, 1, 0);
        let out = render(&Wasted::default(), &[], &m);
        assert!(out.contains("p95 failure-to-outcome:      ? (target < 300s) NO DATA"));
        assert!(out.contains("mend rate:                   ? (target >= 80%) NO DATA"));
    }

    #[test]
    fn facts_parse_from_the_lifecycles_rows() {
        let json = r#"{
          "ejects":[{"batch_id":"b1","evidence":"{\"Eject\":{\"bead_id\":\"sp-a\",\"reason\":\"red\"}}","at":"100"}],
          "members":[{"batch_id":"b1","bead_id":"sp-a","tip":"t1"}],
          "landed_at":[{"bead":"sp-a","at":"300"}],
          "landed_tip":[{"bead_id":"sp-a","tip":"t2"}],
          "mendings":[{"batch_id":"b1","pass":"1","suite":"s.sh","state":"MENDED","failed_at":"90","picked_at":"91","triage_ended_at":"120","outcome_at":"150"}],
          "mends":[{"batch_id":"b1","pass":"1","suite":"s.sh","bead_id":"sp-a","from_tip":"t1","tip":"t3"}],
          "pass_ends":[{"batch_id":"b1","evidence":"{\"PassRed\":{\"n\":1,\"red_suites\":[],\"suites_s\":1,\"build_s\":1}}","at":"160"}]
        }"#;
        let f = parse_facts(json).unwrap();
        assert_eq!(
            f.ejects,
            vec![Eject {
                batch: "b1".into(),
                bead: "sp-a".into(),
                at: 100,
                tip: "t1".into()
            }]
        );
        assert_eq!(f.landed_at["sp-a"], vec![300]);
        assert_eq!(f.landed_tip["sp-a"], "t2");
        assert_eq!(f.mendings[0].outcome_at, Some(150));
        assert_eq!(f.mends[0].tip, "t3");
        assert_eq!(f.pass_ends, vec![("b1".to_string(), 1, 160)]);
        assert!(parse_facts("not json").is_err());
    }
}
