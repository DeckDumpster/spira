//! `queue_keys` — lifecycle funnel depth/age, the active batch, the next-up CERTIFIED rows
//! and the quarantine count. Ported from `spira/cockpit.sh` (DESIGN.md "Design"); the
//! per-repository sort order (`queue_sort_rows`) and the certified listing
//! (`queue_certified_list`) stay `lib.sh`'s own — this bead does not own or re-derive them
//! (wave 4, not this one).

use super::lc::LcRow;
use super::{push, Cfg, Kv};
use crate::io;
use crate::quoting::epoch_to_age;
use serde_json::Value;
use std::collections::HashSet;

// The one definition of "a bead a dwarf can take" lives in `spira-claim`: the express-lane
// count below asks its `ready-count` rather than keeping a bd ready query of its own.

/// The DONE → LANDED funnel keys over the CERTIFIED, SUBMITTED and REWORK rows; `None` (a
/// store that could not answer) renders `?` everywhere, never a zero.
fn funnel_keys(funnel: Option<(Vec<LcRow>, Vec<LcRow>, Vec<LcRow>)>, now: i64) -> Kv {
    let mut out = Kv::new();
    if funnel.is_none() {
        for k in [
            "SP_QUEUE_DEPTH", "SP_QUEUE_EJECTED", "SP_QUEUE_RED", "SP_FUNNEL_CERTIFY_N",
            "SP_FUNNEL_CERTIFY_AGE", "SP_FUNNEL_RED_N", "SP_FUNNEL_RED_AGE",
            "SP_FUNNEL_RED_TIMEOUT", "SP_FUNNEL_RED_REBASE", "SP_FUNNEL_RED_GATE",
            "SP_FUNNEL_RED_CONFLICT", "SP_FUNNEL_CERT_AGE",
        ] {
            push(&mut out, k, "?");
        }
    } else {
        let mut depth = 0;
        let mut ejected = 0;
        let mut red = 0;
        let mut certify_n = 0;
        let mut certify_ep: Option<i64> = None;
        let mut red_to = 0;
        let mut red_rb = 0;
        let mut red_gt = 0;
        let mut red_cf = 0;
        let mut red_ep: Option<i64> = None;
        let mut cert_ep: Option<i64> = None;
        let (certified, submitted, rework) = funnel.unwrap_or_default();
        let older = |cur: &mut Option<i64>, ep: Option<i64>| {
            if let Some(ep) = ep {
                if cur.map(|c| ep < c).unwrap_or(true) {
                    *cur = Some(ep);
                }
            }
        };
        for r in &certified {
            depth += 1;
            older(&mut cert_ep, r.updated_at);
        }
        for r in &submitted {
            certify_n += 1;
            older(&mut certify_ep, r.updated_at);
        }
        for r in &rework {
            if matches!(r.reason.as_str(), "batch-ejected" | "batch-ejected-red" | "base-withdrawn") {
                ejected += 1;
                continue;
            }
            red += 1;
            older(&mut red_ep, r.updated_at);
            match r.reason.as_str() {
                "timeout" => red_to += 1,
                r if r.starts_with("no-rebase") => red_rb += 1,
                "gate" | "suites-failed" | "syntax" | "policy-violation" => red_gt += 1,
                r if r == "confine" || r.starts_with("conflicts-with-base") => red_cf += 1,
                _ => {}
            }
        }
        push(&mut out, "SP_QUEUE_DEPTH", depth.to_string());
        push(&mut out, "SP_QUEUE_EJECTED", ejected.to_string());
        push(&mut out, "SP_QUEUE_RED", red.to_string());
        push(&mut out, "SP_FUNNEL_CERTIFY_N", certify_n.to_string());
        push(&mut out, "SP_FUNNEL_CERTIFY_AGE", epoch_to_age(certify_ep, now));
        push(&mut out, "SP_FUNNEL_RED_N", red.to_string());
        push(&mut out, "SP_FUNNEL_RED_AGE", epoch_to_age(red_ep, now));
        push(&mut out, "SP_FUNNEL_RED_TIMEOUT", red_to.to_string());
        push(&mut out, "SP_FUNNEL_RED_REBASE", red_rb.to_string());
        push(&mut out, "SP_FUNNEL_RED_GATE", red_gt.to_string());
        push(&mut out, "SP_FUNNEL_RED_CONFLICT", red_cf.to_string());
        push(&mut out, "SP_FUNNEL_CERT_AGE", epoch_to_age(cert_ep, now));
    }
    out
}

fn express_count(code: i32, out: &str) -> usize {
    if code == 0 { out.trim().parse().unwrap_or(0) } else { 0 }
}

pub fn queue_keys(cfg: &Cfg) -> Kv {
    let mut out = Kv::new();
    let now = io::now();

    let funnel = match (super::lc::list(Some("CERTIFIED")), super::lc::list(Some("SUBMITTED")), super::lc::list(Some("REWORK"))) {
        (Some(c), Some(s), Some(r)) => Some((c, s, r)),
        _ => None,
    };
    out.extend(funnel_keys(funnel, now));

    // spira-claim's count, the one ready set (sp-7g5q6) — not a bd ready query of our own.
    let argv: Vec<String> = ["ready-count", "", "--express"].iter().map(|s| s.to_string()).collect();
    let o = spira_claim::cli::dispatch_in(&argv, &mut std::io::empty(), &|k| std::env::var(k).ok());
    let enr = express_count(o.code, &o.out);
    push(&mut out, "SP_EXPRESS_N", enr.to_string());

    // --- Active batch ---
    let reg = io::repo_registry();
    let forge: Vec<String> = reg.all().into_iter().filter(|r| reg.land(r) == "queue").collect();
    let active = active_batch(&forge, &super::round::batch_show, now);
    let (batch_pr, batch_age, batch_member_ids) = match &active {
        Some(Some(b)) => (b.pr, b.age.clone(), b.members.clone()),
        _ => (0, "0".to_string(), Vec::new()),
    };
    push(&mut out, "SP_QUEUE_BATCH_PR", batch_pr.to_string());
    push(&mut out, "SP_QUEUE_BATCH_AGE", batch_age);
    push(&mut out, "SP_QUEUE_BATCH_N", if active.is_none() { "?".to_string() } else { batch_member_ids.len().to_string() });

    if !batch_member_ids.is_empty() {
        let refs: Vec<&str> = batch_member_ids.iter().map(String::as_str).collect();
        let mut args = vec!["show"];
        args.extend(refs.iter().copied());
        let rows = io::json_rows(io::contentjson(&args)).unwrap_or_default();
        for (i, id) in batch_member_ids.iter().enumerate() {
            let row = rows.iter().find(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()));
            let (pri, title) = row.map(row_pri_title).unwrap_or(("?".to_string(), "-".to_string()));
            push(&mut out, &format!("SP_QUEUE_BATCH{i}"), format!("P{pri} {id} {title}"));
        }
    }

    // --- Next items: CERTIFIED entries not in the open batch, sorted by the batcher order ---
    let home = io::home_dir();
    let mut next_n = 0usize;
    let mut next_total = 0usize;
    let next_max: i64 = cfg.queue_batch_max;
    let batch_set: HashSet<&str> = batch_member_ids.iter().map(String::as_str).collect();
    {
        let reg = io::repo_registry();
        for rname in reg.all() {
            let Some(rp) = reg.root(&rname) else { continue };
            if reg.land(&rname) != "queue" {
                continue;
            }
            // spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
            // spira_landref lib.sh seam.
            let Some(rbase) = spira_config::repos::landref(&reg, &rname) else { continue };
            let rbase_sha = io::git(std::path::Path::new(&rp), &["rev-parse", &rbase]).map(|s| s.trim().to_string());
            let Some(rbase_sha) = rbase_sha else { continue };
            let Some(cert) = io::lib_call(&home, "queue_certified_list", &[&rp]) else { continue };
            if cert.trim().is_empty() {
                continue;
            }
            let cert_lines: Vec<&str> = cert.lines().filter(|l| !l.trim().is_empty()).collect();
            let filtered: Vec<&str> = cert_lines
                .iter()
                .filter(|l| {
                    let cid = l.split_whitespace().next().unwrap_or("");
                    !batch_set.contains(cid)
                })
                .copied()
                .collect();
            if filtered.is_empty() {
                continue;
            }
            next_total += filtered.len();
            let ids: Vec<&str> = filtered.iter().map(|l| l.split_whitespace().next().unwrap_or("")).collect();
            let mut show_args = vec!["show"];
            show_args.extend(ids.iter().copied());
            let pj_raw = io::contentjson(&show_args).unwrap_or_else(|| "[]".to_string());
            let stdin_rows = filtered.join("\n") + "\n";
            let sorted = io::lib_call_with_stdin(
                &home,
                "queue_sort_rows",
                &[&rp, &rbase_sha],
                Some(&stdin_rows),
            );
            let Some(sorted) = sorted else { continue };
            let rows = io::json_rows(Some(pj_raw)).unwrap_or_default();
            for srow in sorted.lines() {
                if next_n >= 20 {
                    break;
                }
                let fields: Vec<&str> = srow.split_whitespace().collect();
                let Some(nid) = fields.get(4) else { continue };
                let row = rows.iter().find(|r| r.get("id").and_then(Value::as_str) == Some(*nid));
                let (pri, title) = row.map(row_pri_title).unwrap_or(("?".to_string(), "-".to_string()));
                push(&mut out, &format!("SP_QUEUE_NEXT{next_n}"), format!("P{pri} {nid} {title}"));
                next_n += 1;
            }
        }
    }
    push(&mut out, "SP_QUEUE_NEXT_N", next_total.to_string());
    push(&mut out, "SP_QUEUE_NEXT_MAX", next_max.to_string());

    // --- Quarantine count ---
    let mut quarantine_n = 0;
    {
        let reg = io::repo_registry();
        for rname in reg.all() {
            let Some(rp) = reg.root(&rname) else { continue };
            let suite_state_file = spira_config::resolve::suite_state_file().unwrap_or_default();
            let sf = std::path::Path::new(&rp).join(&suite_state_file);
            if let Ok(content) = std::fs::read_to_string(&sf) {
                quarantine_n += content.lines().filter(|l| l.contains(" | quarantined |")).count();
            }
        }
    }
    push(&mut out, "SP_QUEUE_QUARANTINE_N", quarantine_n.to_string());

    out
}

/// The open forge batch the pane shows: the first forge repo, in name order, whose record
/// `queue batch show` (`show`) gives with a PR.
pub struct ActiveBatch {
    pub pr: i64,
    pub age: String,
    pub members: Vec<String>,
}

/// `Some(None)`: no forge repo has a batch with a PR open. `None`: one the queue could not answer
/// for, and no other's batch to show — cannot tell, never "no batch".
pub fn active_batch(repos: &[String], show: &dyn Fn(&str) -> Option<String>, now: i64) -> Option<Option<ActiveBatch>> {
    let mut repos = repos.to_vec();
    repos.sort();
    let mut unanswered = false;
    for repo in &repos {
        let Some(content) = show(repo) else {
            unanswered = true;
            continue;
        };
        let pr = content.lines().find_map(|l| l.strip_prefix("pr="));
        let opened = content.lines().find_map(|l| l.strip_prefix("opened="));
        let mems = content.lines().find_map(|l| l.strip_prefix("members="));
        let (Some(pr), Some(opened), Some(mems)) = (pr, opened, mems) else { continue };
        if pr.is_empty() {
            continue;
        }
        let age_secs = now - opened.parse().unwrap_or(now);
        let age = if age_secs < 90 {
            format!("{age_secs}s")
        } else if age_secs < 5400 {
            format!("{}m", age_secs / 60)
        } else if age_secs < 172800 {
            format!("{}h", age_secs / 3600)
        } else {
            format!("{}d", age_secs / 86400)
        };
        let members = mems.split_whitespace().map(|m| m.split(':').next().unwrap_or(m).to_string()).collect();
        return Some(Some(ActiveBatch { pr: pr.parse().unwrap_or(0), age, members }));
    }
    (!unanswered).then_some(None)
}

fn row_pri_title(row: &Value) -> (String, String) {
    let pri = row.get("priority").map(|v| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "?".to_string(),
    }).unwrap_or_else(|| "?".to_string());
    let mut title = crate::quoting::sanitize(row.get("title").and_then(Value::as_str).unwrap_or(""));
    title.truncate(60);
    (pri, title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_express_count_is_zero_on_a_refusal_or_a_non_number() {
        assert_eq!(express_count(0, "4\n"), 4);
        assert_eq!((express_count(1, "4"), express_count(0, "x")), (0, 0));
    }

    fn row(state: &str, reason: &str, updated_at: i64) -> LcRow {
        LcRow { id: "sp-x".into(), state: state.into(), reason: reason.into(), updated_at: Some(updated_at) }
    }

    fn funnel_get(kv: &Kv, k: &str) -> String {
        kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("no {k}"))
    }

    #[test]
    fn funnel_counts_stages_red_breakdown_and_oldest_ages() {
        let now = 100_000;
        let old = now - 7200;
        let kv = funnel_keys(
            Some((
                vec![row("CERTIFIED", "", old)],
                vec![row("SUBMITTED", "", old), row("SUBMITTED", "", now), row("SUBMITTED", "", now)],
                vec![
                    row("REWORK", "timeout", old),
                    row("REWORK", "no-rebase", now),
                    row("REWORK", "suites-failed", now),
                    row("REWORK", "policy-violation", now),
                    row("REWORK", "confine", now),
                    row("REWORK", "batch-ejected", now),
                ],
            )),
            now,
        );
        for (k, v) in [
            ("SP_QUEUE_DEPTH", "1"), ("SP_FUNNEL_CERTIFY_N", "3"), ("SP_FUNNEL_RED_N", "5"),
            ("SP_QUEUE_RED", "5"), ("SP_QUEUE_EJECTED", "1"), ("SP_FUNNEL_RED_TIMEOUT", "1"),
            ("SP_FUNNEL_RED_REBASE", "1"), ("SP_FUNNEL_RED_GATE", "2"), ("SP_FUNNEL_RED_CONFLICT", "1"),
            ("SP_FUNNEL_CERTIFY_AGE", "2h"), ("SP_FUNNEL_RED_AGE", "2h"), ("SP_FUNNEL_CERT_AGE", "2h"),
        ] {
            assert_eq!(funnel_get(&kv, k), v, "{k}");
        }
    }

    #[test]
    fn funnel_from_an_unanswering_store_is_question_marks_not_zero() {
        let kv = funnel_keys(None, 100_000);
        assert_eq!(kv.len(), 12);
        assert!(kv.iter().all(|(_, v)| v == "?"));
    }
    #[test]
    fn the_active_batch_is_the_store_record_the_queue_shows() {
        let now = 10_000;
        let show = |repo: &str| match repo {
            "deck" => Some(format!("batch_id=deck-1\nforge.phase=armed\npr=42\nmembers=sp-a:ta sp-b:tb\nopened={}\n", now - 180)),
            "alpha" => Some("batch_id=alpha-1\nforge.phase=opening\nmembers=sp-x:tx\nopened=1\n".to_string()),
            _ => Some(String::new()),
        };
        let b = active_batch(&["deck".into(), "alpha".into(), "zed".into()], &show, now).unwrap().expect("deck's batch is seen");
        assert_eq!((b.pr, b.age.as_str()), (42, "3m"));
        assert_eq!(b.members, ["sp-a", "sp-b"]);
        assert!(active_batch(&["zed".into()], &show, now).unwrap().is_none(), "no record: no batch");
        assert!(active_batch(&["zed".into(), "lost".into()], &|r| (r == "zed").then(String::new), now).is_none(), "cannot tell is not no batch");
    }

}
