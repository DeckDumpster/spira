//! CHECK 8's inference tier, `reflect.sh` and `reflect-recur.sh` in Rust. Reached only when
//! every deterministic check passed and the work graph is still not moving: it shows the
//! judgement model the open beads and the blocked ones, asks why nothing is ready, and turns
//! the answer into at most two deliveries: an `ESCALATE:` line becomes a question to the
//! operator, and a `CHECK:` line that recurs `SPIRA_REFLECT_RECUR_HITS` times inside
//! `SPIRA_REFLECT_RECUR_DAYS` is filed once as a bead (at most `…_WEEKLY_CAP` a week). Its
//! output is never an action on the graph: the deny rules bind the model's tools.
//!
//! [`prompt`], [`answer`], [`recur_key`] and [`recur`] are pure; [`run`] is the leg the
//! scheduler's `sentinel-pass` starts as a unit (`sentinel --reflect <ids>`), since one model
//! call runs for minutes and no effect may block that long.

use std::path::Path;

/// The deny rules the model runs under: no writes, no store or mail verbs, no commits.
pub const DENY: &str = r#"{"permissions":{"deny":["Write","Edit","NotebookEdit","Bash(bd:*)","Bash(work:*)","Bash(lc:*)","Bash(mail:*)","Bash(spira-lc:*)","Bash(git commit:*)","Bash(git push:*)"]}}"#;
/// The one model call's bound (`timeout 600`).
pub const AGENT_TIMEOUT_SECS: u64 = 600;

/// The judgement prompt over the open beads' shows and the blocked list.
pub fn prompt(state: &str, blocked: &str, design: &str, operator: &str) -> String {
    let design = if design.is_empty() { String::new() } else { format!("Design: {design}\n") };
    format!(
        "You are the Spira sentinel's judgement tier. Every deterministic check has passed\n\
and the work graph is still not moving: beads remain open, none are ready, none are\n\
running, and nothing was reclaimed, poisoned, reopened or landed this pass.\n\n\
{design}\n## Open beads\n\n{state}\n\n## Blocked\n\n{blocked}\n\n## Your task\n\n\
Diagnose why nothing is ready, in at most 150 words. Then answer exactly one question:\n\
is this a gap a program could have detected? If yes, state the deterministic check that\n\
would have caught it, precisely enough to implement in the sentinel (sentinel/src) — that is the valuable\n\
output, because a stall diagnosed by inference twice is a check that was never written.\n\n\
If the fix needs a decision only {operator} can make, end with a line beginning 'ESCALATE:'\n\
followed by the decision stated as a question with a default. Otherwise end with a line\n\
beginning 'CHECK:' followed by the sentinel file or function it belongs in, a '|', then the condition it\n\
would detect. Do not take any action\nand do not modify any bead."
    )
}

/// A `bdq show` text with the hint lines the script's grep dropped.
pub fn clean_show(text: &str) -> String {
    text.lines().filter(|l| !(l.starts_with('💡') || l.starts_with("warning") || l.starts_with("  Fix") || l.starts_with("  Or"))).collect::<Vec<_>>().join("\n")
}

/// What the model's answer asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// The first `CHECK:` line's text, without the prefix.
    pub check: Option<String>,
    /// The first `ESCALATE:` line's question.
    pub escalate: Option<String>,
}

pub fn answer(out: &str) -> Answer {
    let first = |p: &str| out.lines().find_map(|l| l.strip_prefix(p)).map(|r| r.trim_start().to_string());
    Answer { check: first("CHECK:"), escalate: first("ESCALATE:") }
}

/// The escalation's default: the proposed check, or the generic one.
pub fn escalation_default(a: &Answer) -> String {
    a.check.clone().unwrap_or_else(|| "read the diagnosis and decide".to_string())
}

/// The escalation's body, as the script's heredoc wrote it.
pub fn escalation_body(question: &str, default: &str, out: &str) -> String {
    format!("## Question\n{question}\n\n## Default\n{default}\n\nthe Spira DAG is stalled with open work and nothing ready\n\n{out}\n")
}

/// `reflect-recur.sh`'s key: the text before the first `|`, lowercased, every run of other
/// characters one `-`, trimmed of `-`, at most 80 characters.
pub fn recur_key(text: &str) -> String {
    let head = text.split('|').next().unwrap_or("").to_lowercase();
    let mut k = String::new();
    for c in head.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            k.push(c);
        } else if !k.ends_with('-') {
            k.push('-');
        }
    }
    let k = k.trim_matches('-');
    k.chars().take(80).collect()
}

/// One ledger row: `<epoch>\t<key>\t<filed 0|1>\t<text>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub at: i64,
    pub key: String,
    pub filed: bool,
}

pub fn ledger_rows(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some(Row { at: f.next()?.parse().ok()?, key: f.next()?.to_string(), filed: f.next()? == "1" })
        })
        .collect()
}

/// The recur limits, as pinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recur {
    pub days: i64,
    pub hits: usize,
    pub weekly_cap: usize,
}

/// `(hits including this one, file now)`: file when the key has `hits` rows inside the window,
/// none of them filed, and fewer than the weekly cap were filed this week.
pub fn recur(rows: &[Row], key: &str, now: i64, r: Recur) -> (usize, bool) {
    let since = now - r.days * 86_400;
    let week = now - 7 * 86_400;
    let hits = rows.iter().filter(|x| x.key == key && x.at >= since).count() + 1;
    let filed_key = rows.iter().filter(|x| x.key == key && x.at >= since && x.filed).count();
    let filed_week = rows.iter().filter(|x| x.filed && x.at >= week).count();
    (hits, hits >= r.hits && filed_key == 0 && filed_week < r.weekly_cap)
}

/// The filed bead's body.
pub fn recur_body(hits: usize, days: i64, text: &str, key: &str) -> String {
    format!("## Build\n\nImplement the deterministic check the judgement tier (reflect.sh) proposed {hits} times in\n{days} days, so it is never asked about this stall again:\n\n{text}\n\nKey: {key}\n")
}

/// The configuration the leg reads (the keys `reflect.sh` read through lib.sh).
pub struct Cfg {
    pub agent: String,
    pub model: String,
    pub design: String,
    pub operator: String,
    pub ledger: std::path::PathBuf,
    pub recur: Recur,
}

impl Cfg {
    /// From the one config door; a key that does not resolve refuses the run naming it.
    pub fn resolve() -> Result<Cfg, String> {
        let get = spira_config::process::cfg;
        let num = |k: &str| get(k).and_then(|v| v.trim().parse::<i64>().map_err(|_| format!("{k} must be a whole number, got {v:?}")));
        Ok(Cfg {
            agent: get("SPIRA_AGENT").ok().filter(|a| !a.is_empty()).unwrap_or_else(|| "claude".into()),
            model: get("SPIRA_REFLECT_MODEL")?,
            design: get("SPIRA_DESIGN").unwrap_or_default(),
            operator: get("SPIRA_OPERATOR")?,
            ledger: get("SPIRA_REFLECT_LEDGER")?.into(),
            recur: Recur { days: num("SPIRA_REFLECT_RECUR_DAYS")?, hits: num("SPIRA_REFLECT_RECUR_HITS")?.max(1) as usize, weekly_cap: num("SPIRA_REFLECT_RECUR_WEEKLY_CAP")?.max(0) as usize },
        })
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The prompt's blocked list: every live lifecycle row carrying a hold, one line each
/// (`<id> <STATE> held: <holds>`), at most 40.
pub fn blocked_rows(rows: &[spira_config::lc_state::Row]) -> String {
    rows.iter().filter(|r| !r.terminal() && !r.holds.is_empty()).take(40).map(|r| format!("{} {} held: {}", r.bead_id, r.state, r.holds.join(","))).collect::<Vec<_>>().join("\n")
}

/// The leg: `reflect.sh "<ids>"` end to end. Prints the run's header and the model's answer
/// (the unit's output is the reflect log), files a recurring check, mails an escalation.
pub fn run(ids: &[String]) -> Result<String, String> {
    use runtime::ports::{Proc, ProcSpec, SysProc};
    if ids.is_empty() {
        return Ok("reflect: nothing to reflect on".into());
    }
    let cfg = Cfg::resolve()?;
    let lc = spira_config::lc_call::LcClient::configured()?;
    let show = |args: &[&str]| lc.call(args).ok().filter(|r| r.code == 0).map(|r| r.out).unwrap_or_default();
    let state: String = ids.iter().map(|id| format!("{}\n", clean_show(&show(&["content", "show", id])))).collect();
    // What holds work back is the lifecycle machine's account (its held rows), not bd's
    // status field, which is inert for a work bead (design §3.4).
    let blocked = blocked_rows(&spira_config::lc_state::list_live_on(&lc).unwrap_or_default());
    let text = prompt(&state, &blocked, &cfg.design, &cfg.operator);
    let args = ["-p", "--model", &cfg.model, "--settings", DENY, "--dangerously-skip-permissions"].iter().map(|s| s.to_string()).collect();
    let o = SysProc.run(&ProcSpec { prog: cfg.agent.clone(), args, stdin: text.into_bytes(), deadline: std::time::Duration::from_secs(AGENT_TIMEOUT_SECS) })?;
    let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let mut log = format!("\n===== {} =====\n{out}\n", tsd::now_iso());
    let a = answer(&out);
    if let Some(check) = &a.check {
        log.push_str(&recur_file(&cfg, check, now())?);
    }
    if let Some(q) = &a.escalate {
        let default = escalation_default(&a);
        let body = escalation_body(q, &default, &out);
        match crate::ask::send(&crate::ask::Mail { mailbox: "operator", from: "Reflect <reflect@spira>", subject: q, kind: "question", default: &default, body: &body, bead: "", own_dedup: false }) {
            Ok(()) => log.push_str("reflect: escalated to the cockpit\n"),
            Err(e) => log.push_str(&format!("reflect: the escalation was not accepted: {e}\n")),
        }
    }
    Ok(log)
}

/// `reflect-recur.sh "<CHECK line>"`: append the ledger row, filing the bead first when due.
fn recur_file(cfg: &Cfg, check: &str, now: i64) -> Result<String, String> {
    let key = recur_key(check);
    if key.is_empty() {
        return Ok(String::new());
    }
    if let Some(dir) = cfg.ledger.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let rows = ledger_rows(&std::fs::read_to_string(&cfg.ledger).unwrap_or_default());
    let (hits, due) = recur(&rows, &key, now, cfg.recur);
    let mut log = String::new();
    let mut filed = false;
    if due {
        let title = format!("Sentinel check proposed {hits}x by the judgement tier: {}", check.split('|').next().unwrap_or(check));
        filed = file_bead(&title, &recur_body(hits, cfg.recur.days, check, &key)).is_ok();
        if filed {
            log.push_str(&format!("reflect: recurring CHECK '{key}' filed as a bead\n"));
        }
    }
    append(&cfg.ledger, &format!("{now}\t{key}\t{}\t{check}\n", u8::from(filed)))?;
    Ok(log)
}

fn append(path: &Path, line: &str) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(line.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

fn file_bead(title: &str, body: &str) -> Result<(), String> {
    use runtime::ports::{Proc, ProcSpec, SysProc};
    let bead_sh = spira_config::resolve::locate_home_for_process()?.join("bead.sh");
    // FORK-EDGE: bead.sh is the operator tool for filing (rune, repo and lifecycle row in one); it stays.
    // batch-job: bead.sh shells into conf.sh, which outlasts a 5 s call deadline on a loaded host
    let o = SysProc.run(&ProcSpec { prog: bead_sh.display().to_string(), args: ["file", title, "--for", "builder", "--repo", "spira", "--body-file", "-"].iter().map(|s| s.to_string()).collect(), stdin: body.as_bytes().to_vec(), deadline: std::time::Duration::from_secs(60) })?;
    if o.code == Some(0) { Ok(()) } else { Err(format!("bead.sh file exited {:?}", o.code)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_reads_the_first_check_and_escalate_lines() {
        let a = answer("why\nCHECK: holds.rs | a hold with no ask\nESCALATE: ship it? default no\nCHECK: other");
        assert_eq!(a.check.as_deref(), Some("holds.rs | a hold with no ask"));
        assert_eq!(a.escalate.as_deref(), Some("ship it? default no"));
        assert_eq!(escalation_default(&Answer::default()), "read the diagnosis and decide");
    }

    #[test]
    fn the_recur_key_is_the_scripts() {
        assert_eq!(recur_key("Holds.rs stale_ask_holds | a hold whose ask is gone"), "holds-rs-stale-ask-holds");
        assert_eq!(recur_key("|x"), "");
        assert_eq!(recur_key(&"a".repeat(100)).len(), 80);
    }

    #[test]
    fn a_proposal_is_filed_once_it_recurs_and_never_twice() {
        let r = Recur { days: 7, hits: 3, weekly_cap: 2 };
        let row = |at, key: &str, filed| Row { at, key: key.into(), filed };
        assert_eq!(recur(&[row(100, "k", false)], "k", 200, r), (2, false), "a one-off files nothing");
        assert_eq!(recur(&[row(100, "k", false), row(150, "k", false)], "k", 200, r), (3, true));
        assert!(!recur(&[row(100, "k", true), row(150, "k", false)], "k", 200, r).1, "already filed in the window");
        assert!(!recur(&[row(100, "k", false), row(150, "k", false), row(160, "a", true), row(170, "b", true)], "k", 200, r).1, "the weekly cap");
        assert_eq!(ledger_rows("100\tk\t1\ttext\nbad\n"), vec![row(100, "k", true)]);
    }

    #[test]
    fn the_prompt_carries_the_state_and_the_operator() {
        let p = prompt("sp-a open", "[sp-b] blocked", "", "Ryan");
        assert!(p.contains("## Open beads\n\nsp-a open") && p.contains("[sp-b] blocked") && p.contains("only Ryan can make"));
        assert_eq!(clean_show("sp-a\n💡 hint\nwarning: x\n  Fix it\nbody"), "sp-a\nbody");
    }

    #[test]
    fn the_blocked_list_is_the_machines_held_rows() {
        use spira_config::lc_state::Row as Lc;
        let row = |id: &str, state: &str, holds: &[&str]| Lc { bead_id: id.into(), state: state.into(), holds: holds.iter().map(|h| h.to_string()).collect(), ..Lc::default() };
        let rows = [row("sp-a", "READY", &["ask", "blocker"]), row("sp-b", "READY", &[]), row("sp-c", "LANDED", &["ask"])];
        assert_eq!(blocked_rows(&rows), "sp-a READY held: ask,blocker");
    }
}
