//! `queue_keys` — lifecycle funnel depth/age, the active batch, the next-up CERTIFIED rows
//! and the tests the registry holds out of runs. Ported from `spira/cockpit.sh` (DESIGN.md "Design").

use super::lc::LcRow;
use super::{push, Cfg, Kv};
use crate::io;
use crate::quoting::epoch_to_age;
use queue::ports::{Git, Lc};
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
    let next_max: i64 = cfg.queue_batch_max;
    let batch_set: HashSet<&str> = batch_member_ids.iter().map(String::as_str).collect();
    let roots: Vec<std::path::PathBuf> = {
        let reg = io::repo_registry();
        reg.all().into_iter().filter(|r| reg.land(r) == "queue").filter_map(|r| reg.root(&r)).map(Into::into).collect()
    };
    let (next_total, next) = match spira_store::lc_call::LcClient::configured() {
        Ok(client) => {
            let lc = queue::real::RealLc { bin: None, client: Some(client) };
            let branches = |root: &std::path::Path| queue::real::RealGit.branches(root, "refs/heads/spira/");
            next_items(&lc, &roots, &branches, &batch_set, &|ids| {
                let mut args = vec!["show"];
                args.extend(ids.iter().map(String::as_str));
                io::contentjson(&args)
            })
        }
        Err(_) => (0, Vec::new()),
    };
    for (i, (nid, (pri, title))) in next.iter().enumerate() {
        push(&mut out, &format!("SP_QUEUE_NEXT{i}"), format!("P{pri} {nid} {title}"));
    }
    push(&mut out, "SP_QUEUE_NEXT_N", next_total.to_string());
    push(&mut out, "SP_QUEUE_NEXT_MAX", next_max.to_string());

    out.extend(testreg_keys(&|args| spira_config::git_refs::held_registry("list", || io::run_tool("test-registry", args, None).ok_or_else(|| "test-registry list failed".to_string())).ok(), now));

    out
}

const NEXT_ROW_CAP: usize = 20;

type Shown = (String, (String, String));

/// The certified beads not in the open batch, per repo in the batcher's order, read from one
/// `polled_rows("CERTIFIED")`: how many there are in all, and the first [`NEXT_ROW_CAP`] with
/// their priority and title. A store that cannot answer leaves none. The express set is read
/// only once a row is left to rank.
fn next_items(
    lc: &dyn Lc,
    roots: &[std::path::PathBuf],
    branches: &dyn Fn(&std::path::Path) -> Vec<(String, String)>,
    batch: &HashSet<&str>,
    show: &dyn Fn(&[String]) -> Option<String>,
) -> (usize, Vec<Shown>) {
    let Ok(rows) = lc.polled_rows("CERTIFIED") else { return (0, Vec::new()) };
    let mut express: Option<HashSet<String>> = None;
    let mut total = 0;
    let mut drawn: Vec<Shown> = Vec::new();
    for root in roots {
        let certified: Vec<(String, String, i64)> = queue::ops::helpers::rows_with_branch(&rows, &branches(root))
            .into_iter()
            .filter(|(id, _, _)| !batch.contains(id.as_str()))
            .map(|(id, tip, since)| (id, tip, since as i64))
            .collect();
        if certified.is_empty() {
            continue;
        }
        total += certified.len();
        let ids: Vec<String> = certified.iter().map(|(id, _, _)| id.clone()).collect();
        let prio_json = show(&ids).unwrap_or_else(|| "[]".to_string());
        let express = express.get_or_insert_with(|| lc.express_ids().unwrap_or_default().into_iter().collect());
        let (ranked, _) = queue::ops::helpers::sort_rows(&certified, &prio_json, express);
        let shown = io::json_rows(Some(prio_json)).unwrap_or_default();
        for r in ranked.iter().take(NEXT_ROW_CAP.saturating_sub(drawn.len())) {
            let row = shown.iter().find(|v| v.get("id").and_then(Value::as_str) == Some(r.id.as_str()));
            drawn.push((r.id.clone(), row.map(row_pri_title).unwrap_or(("?".to_string(), "-".to_string()))));
        }
    }
    (total, drawn)
}

const TESTREG_ROWS: usize = 10;

/// The tests the registry holds out of runs and has not expired, soonest-expiring first;
/// a call that fails or prints what is not the registry's list is `SP_TESTREG=unread`.
pub fn testreg_keys(call: &dyn Fn(&[&str]) -> Option<String>, now: i64) -> Kv {
    let mut out = Kv::new();
    let rows = call(&["list", "--json"]) // BDQ_READ_CONSIDERED="the test registry binary's own list, no bead store"
        .and_then(|raw| serde_json::from_str::<Value>(io::json_only(&raw)).ok())
        .and_then(|v| v.get("rows").and_then(Value::as_array).cloned());
    let Some(rows) = rows else {
        push(&mut out, "SP_TESTREG", "unread");
        return out;
    };
    let mut held: Vec<&Value> = rows.iter().filter(|r| r.get("until").and_then(Value::as_i64).is_some_and(|until| until > now)).collect();
    held.sort_by_key(|r| r.get("until").and_then(Value::as_i64).unwrap_or(i64::MAX));
    push(&mut out, "SP_TESTREG_N", held.len().to_string());
    for (i, r) in held.iter().take(TESTREG_ROWS).enumerate() {
        let text = |k: &str| crate::quoting::sanitize(r.get(k).and_then(Value::as_str).unwrap_or(""));
        push(&mut out, &format!("SP_TESTREG{i}_STATE"), text("state"));
        push(&mut out, &format!("SP_TESTREG{i}_TEST"), text("test"));
        push(&mut out, &format!("SP_TESTREG{i}_BEAD"), text("bead"));
        push(&mut out, &format!("SP_TESTREG{i}_UNTIL"), r.get("until").and_then(Value::as_i64).unwrap_or(0).to_string());
    }
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

    #[test_id(t13zk2r)]
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

    // covers: UC-cockpit-observability-11
    #[test_id(txt7dna)]
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

    #[test_id(t4pp5kj)]
    #[test]
    fn funnel_from_an_unanswering_store_is_question_marks_not_zero() {
        let kv = funnel_keys(None, 100_000);
        assert_eq!(kv.len(), 12);
        assert!(kv.iter().all(|(_, v)| v == "?"));
    }
    struct Scripted {
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl spira_store::lc_call::Transport for Scripted {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.calls.lock().unwrap().push(argv.join(" "));
            let out = match argv.join(" ").as_str() {
                "list --state CERTIFIED" => r#"[{"bead_id":"sp-b1","tip":"t1","since":100},{"bead_id":"sp-a1","tip":"t2","since":300},{"bead_id":"sp-a2","tip":"t3","since":200},{"bead_id":"sp-z9","tip":"t9","since":50}]"#,
                "list --express --live" => r#"[{"bead_id":"sp-a1"}]"#,
                other => panic!("unscripted call {other}"),
            };
            Ok(spira_store::lc_call::Reply { code: 0, out: out.into(), err: String::new(), read_ended: false })
        }
    }

    // covers: UC-cockpit-observability-11
    #[test_id(tidos72)]
    #[test]
    fn next_items_read_certified_once_leave_out_the_open_batch_and_rank_by_the_batcher_order() {
        let t = std::sync::Arc::new(Scripted { calls: Default::default() });
        let lc = queue::real::RealLc { bin: None, client: Some(spira_store::lc_call::LcClient::over(t.clone(), true)) };
        let roots = [std::path::PathBuf::from("alpha"), std::path::PathBuf::from("beta")];
        let branches = |root: &std::path::Path| match root.to_str().unwrap() {
            "alpha" => vec![("spira/sp-a1".to_string(), "x".to_string()), ("spira/sp-a2".to_string(), "x".to_string()), ("spira/sp-b1".to_string(), "x".to_string())],
            _ => vec![("spira/sp-z9".to_string(), "x".to_string())],
        };
        let batch: HashSet<&str> = ["sp-b1"].into();
        let show = |ids: &[String]| Some(ids.iter().map(|i| format!(r#"{{"id":"{i}","priority":2,"title":"t {i}"}}"#)).collect::<Vec<_>>().join(",")).map(|b| format!("[{b}]"));
        let (total, next) = next_items(&lc, &roots, &branches, &batch, &show);
        assert_eq!(total, 3);
        let ids: Vec<&str> = next.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["sp-a1", "sp-a2", "sp-z9"], "express first, then age, repo by repo");
        assert_eq!(next[1].1, ("2".to_string(), "t sp-a2".to_string()));
        let calls = t.calls.lock().unwrap();
        assert_eq!(calls.iter().filter(|c| *c == "list --state CERTIFIED").count(), 1, "{calls:?}");
        assert_eq!(calls.iter().filter(|c| c.contains("--express")).count(), 1, "{calls:?}");
    }

    #[test_id(tyil94g)]
    #[test]
    fn the_express_set_is_not_read_when_every_certified_bead_is_in_the_open_batch() {
        let t = std::sync::Arc::new(Scripted { calls: Default::default() });
        let lc = queue::real::RealLc { bin: None, client: Some(spira_store::lc_call::LcClient::over(t.clone(), true)) };
        let batch: HashSet<&str> = ["sp-b1"].into();
        let (total, next) = next_items(&lc, &[std::path::PathBuf::from("alpha")], &|_| vec![("spira/sp-b1".into(), String::new())], &batch, &|_| None);
        assert_eq!((total, next.len()), (0, 0));
        assert_eq!(*t.calls.lock().unwrap(), ["list --state CERTIFIED"]);
    }

    #[test_id(tql94gh)]
    #[test]
    fn next_items_draw_at_most_the_row_cap_and_count_every_certified_bead() {
        let rows: Vec<String> = (0..32).map(|i| format!(r#"{{"bead_id":"sp-c{i}","tip":"t","since":{i}}}"#)).collect();
        struct Fixed(String);
        impl spira_store::lc_call::Transport for Fixed {
            fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
                let out = if argv.iter().any(|a| a == "--express") { "[]".to_string() } else { self.0.clone() };
                Ok(spira_store::lc_call::Reply { code: 0, out, err: String::new(), read_ended: false })
            }
        }
        let lc = queue::real::RealLc { bin: None, client: Some(spira_store::lc_call::LcClient::over(std::sync::Arc::new(Fixed(format!("[{}]", rows.join(",")))), true)) };
        let branches = |_: &std::path::Path| (0..32).map(|i| (format!("spira/sp-c{i}"), String::new())).collect();
        let (total, next) = next_items(&lc, &[std::path::PathBuf::from("r")], &branches, &HashSet::new(), &|_| None);
        assert_eq!((total, next.len()), (32, NEXT_ROW_CAP));
    }

    #[test_id(tjv8qnh)]
    #[test]
    fn next_items_of_an_unreadable_store_are_none() {
        struct Down;
        impl spira_store::lc_call::Transport for Down {
            fn exchange(&self, _: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
                Err("gone".into())
            }
        }
        let lc = queue::real::RealLc { bin: None, client: Some(spira_store::lc_call::LcClient::over(std::sync::Arc::new(Down), true)) };
        let (total, next) = next_items(&lc, &[std::path::PathBuf::from("r")], &|_| vec![("spira/sp-a".into(), String::new())], &HashSet::new(), &|_| None);
        assert_eq!((total, next.len()), (0, 0));
    }

    #[test_id(tcia3g7)]
    #[test]
    fn the_registry_rows_still_held_are_emitted_soonest_first_and_a_failed_call_is_unread() {
        let list = r#"{"ok":true,"at":100,"rows":[
            {"test":"unit:old","state":"quarantined","bead":"sp-o","until":50,"expired":true},
            {"test":"unit:loom factory::tests::b","state":"quarantined","bead":"sp-y2","until":900,"expired":false},
            {"test":"unit:loom factory::tests::a","state":"disabled","bead":"sp-x1","until":300,"expired":false}]}"#;
        let seen = std::cell::RefCell::new(Vec::new());
        let kv = testreg_keys(&|a| { seen.borrow_mut().push(a.join(" ")); Some(list.to_string()) }, 100);
        assert_eq!(seen.borrow()[0], "list --json");
        let get = |k: &str| funnel_get(&kv, k);
        assert_eq!(get("SP_TESTREG_N"), "2");
        assert_eq!(
            [get("SP_TESTREG0_STATE"), get("SP_TESTREG0_TEST"), get("SP_TESTREG0_BEAD"), get("SP_TESTREG0_UNTIL")],
            ["disabled", "unit:loom factory::tests::a", "sp-x1", "300"]
        );
        assert_eq!(get("SP_TESTREG1_BEAD"), "sp-y2");
        assert!(!kv.iter().any(|(k, _)| k == "SP_TESTREG2_TEST" || k == "SP_QUEUE_QUARANTINE_N"));
        let none = testreg_keys(&|_| None, 100);
        assert_eq!(none, [("SP_TESTREG".to_string(), "unread".to_string())]);
        assert_eq!(funnel_get(&testreg_keys(&|_| Some("not json".into()), 100), "SP_TESTREG"), "unread");
    }

    #[test_id(tq62fed)]
    #[test]
    fn a_held_registry_list_shows_a_row_expired_once_now_reaches_its_until_with_no_further_call() {
        let list = r#"{"rows":[{"test":"unit:a","state":"disabled","bead":"sp-a","until":300},{"test":"unit:b","state":"disabled","bead":"sp-b","until":900}]}"#;
        let calls = std::cell::Cell::new(0);
        let call = |_: &[&str]| { calls.set(calls.get() + 1); Some(list.to_string()) };
        let n = |now| funnel_get(&testreg_keys(&call, now), "SP_TESTREG_N");
        assert_eq!((n(299), n(300), n(900)), ("2".to_string(), "1".to_string(), "0".to_string()));
    }

    // covers: UC-cockpit-observability-11
    #[test_id(t9jmykf)]
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
