//! `queue_keys` — landstate funnel depth/age, the active batch, the next-up CERTIFIED rows
//! and the quarantine count. Ported from `spira/cockpit.sh` (DESIGN.md "Design"); the
//! per-repository sort order (`queue_sort_rows`) and the certified listing
//! (`queue_certified_list`) stay `lib.sh`'s own — this bead does not own or re-derive them
//! (wave 4, not this one).

use super::{push, Kv};
use crate::io;
use crate::quoting::epoch_to_age;
use serde_json::Value;
use std::collections::HashSet;

/// The one definition of "a bead an aeon can take" (`lib.sh` `READY_ARGS`) — mirrored here
/// only for the express-lane count, which asked exactly this query in the bash.
const READY_ARGS: &[&str] = &["ready", "--limit", "0", "--exclude-type", "epic,event", "-u"];

struct Landstate {
    status: String,
    epoch: Option<i64>,
    reason: String,
}

fn read_landstate(path: &std::path::Path) -> Option<Landstate> {
    let content = std::fs::read_to_string(path).ok()?;
    let first_line = content.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let status = parts.next()?.to_string();
    let _tip = parts.next();
    let epoch = parts.next().and_then(|s| s.parse().ok());
    let reason = parts.next().unwrap_or("").to_string();
    Some(Landstate { status, epoch, reason })
}

pub fn queue_keys() -> Kv {
    let mut out = Kv::new();
    let run = io::run_dir();
    let qdir = std::env::var("SPIRA_QUEUE_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| run.join("queue"));
    let now = io::now();

    let landstate_dir = run.join("landstate");
    if !landstate_dir.is_dir() {
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
        if let Ok(entries) = std::fs::read_dir(&landstate_dir) {
            for e in entries.flatten() {
                let Some(ls) = read_landstate(&e.path()) else { continue };
                match ls.status.as_str() {
                    "CERTIFIED" => {
                        depth += 1;
                        if let Some(ep) = ls.epoch {
                            if cert_ep.map(|c| ep < c).unwrap_or(true) {
                                cert_ep = Some(ep);
                            }
                        }
                    }
                    "EJECTED" => ejected += 1,
                    "GATED" => {
                        certify_n += 1;
                        if let Some(ep) = ls.epoch {
                            if certify_ep.map(|c| ep < c).unwrap_or(true) {
                                certify_ep = Some(ep);
                            }
                        }
                    }
                    "RED" => {
                        red += 1;
                        if let Some(ep) = ls.epoch {
                            if red_ep.map(|c| ep < c).unwrap_or(true) {
                                red_ep = Some(ep);
                            }
                        }
                        match ls.reason.as_str() {
                            "timeout" => red_to += 1,
                            r if r.starts_with("no-rebase") => red_rb += 1,
                            "gate" => red_gt += 1,
                            r if r == "confine" || r.starts_with("conflicts-with-base") => red_cf += 1,
                            _ => {}
                        }
                    }
                    _ => {}
                }
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

    let express_label = std::env::var("SPIRA_EXPRESS_LABEL").unwrap_or_else(|_| "express".to_string());
    let mut args: Vec<&str> = READY_ARGS.to_vec();
    args.push("--label");
    args.push(&express_label);
    let enr = io::bd_rows(io::bdjson(&args)).map(|r| r.len()).unwrap_or(0);
    push(&mut out, "SP_EXPRESS_N", enr.to_string());

    // --- Active batch ---
    let mut batch_pr = 0i64;
    let mut batch_age = "0".to_string();
    let mut batch_member_ids: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&qdir) {
        let mut opens: Vec<_> = entries.flatten().map(|e| e.path().join("open")).filter(|p| p.is_file()).collect();
        opens.sort();
        for open_f in opens {
            let Ok(content) = std::fs::read_to_string(&open_f) else { continue };
            let pr = content.lines().find_map(|l| l.strip_prefix("pr="));
            let opened = content.lines().find_map(|l| l.strip_prefix("opened="));
            let mems = content.lines().find_map(|l| l.strip_prefix("members="));
            let (Some(pr), Some(opened), Some(mems)) = (pr, opened, mems) else { continue };
            if pr.is_empty() {
                continue;
            }
            batch_pr = pr.parse().unwrap_or(0);
            let opened_epoch: i64 = opened.parse().unwrap_or(now);
            let age_secs = now - opened_epoch;
            batch_age = if age_secs < 90 {
                format!("{age_secs}s")
            } else if age_secs < 5400 {
                format!("{}m", age_secs / 60)
            } else if age_secs < 172800 {
                format!("{}h", age_secs / 3600)
            } else {
                format!("{}d", age_secs / 86400)
            };
            for mem in mems.split_whitespace() {
                let id = mem.split(':').next().unwrap_or(mem);
                batch_member_ids.push(id.to_string());
            }
            break;
        }
    }
    push(&mut out, "SP_QUEUE_BATCH_PR", batch_pr.to_string());
    push(&mut out, "SP_QUEUE_BATCH_AGE", batch_age);
    push(&mut out, "SP_QUEUE_BATCH_N", batch_member_ids.len().to_string());

    if !batch_member_ids.is_empty() {
        let refs: Vec<&str> = batch_member_ids.iter().map(String::as_str).collect();
        let mut args = vec!["show"];
        args.extend(refs.iter().copied());
        let rows = io::bd_rows(io::bdjson(&args)).unwrap_or_default();
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
    let next_max: i64 = std::env::var("SPIRA_QUEUE_BATCH_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let batch_set: HashSet<&str> = batch_member_ids.iter().map(String::as_str).collect();
    if let Some(repos) = io::lib_call(&home, "spira_repos", &[]) {
        for rname in repos.split_whitespace() {
            let Some(rp) = io::lib_call(&home, "repo_root", &[rname]) else { continue };
            if io::lib_call(&home, "repo_land", &[rname]).as_deref() != Some("queue") {
                continue;
            }
            let Some(rbase) = io::lib_call(&home, "spira_landref", &[rname]) else { continue };
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
            let pj_raw = io::bdjson(&show_args).unwrap_or_else(|| "[]".to_string());
            let stdin_rows = filtered.join("\n") + "\n";
            let sorted = io::lib_call_with_stdin(
                &home,
                "queue_sort_rows",
                &[&rp, &rbase_sha],
                Some(&stdin_rows),
            );
            let Some(sorted) = sorted else { continue };
            let rows = io::bd_rows(Some(pj_raw)).unwrap_or_default();
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
    if let Some(repos) = io::lib_call(&home, "spira_repos", &[]) {
        for rname in repos.split_whitespace() {
            let Some(rp) = io::lib_call(&home, "repo_root", &[rname]) else { continue };
            let suite_state_file = std::env::var("SPIRA_SUITE_STATE_FILE").unwrap_or_else(|_| "spira/suite-state".to_string());
            let sf = std::path::Path::new(&rp).join(&suite_state_file);
            if let Ok(content) = std::fs::read_to_string(&sf) {
                quarantine_n += content.lines().filter(|l| l.contains(" | quarantined |")).count();
            }
        }
    }
    push(&mut out, "SP_QUEUE_QUARANTINE_N", quarantine_n.to_string());

    out
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
    fn read_landstate_parses_status_tip_epoch_reason() {
        let dir = testkit::TempDir::new("cc-landstate");
        let f = dir.path().join("sp-1");
        std::fs::write(&f, "RED deadbeef 1700000000 timeout\n").unwrap();
        let ls = read_landstate(&f).unwrap();
        assert_eq!(ls.status, "RED");
        assert_eq!(ls.epoch, Some(1700000000));
        assert_eq!(ls.reason, "timeout");
    }

    #[test]
    fn missing_landstate_dir_renders_question_marks() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let run = testkit::TempDir::new("cc-queue-missing");
        std::env::set_var("SPIRA_RUN", run.path());
        std::env::set_var("SPIRA_QUEUE_DIR", run.path().join("queue"));
        let kv = queue_keys();
        let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        assert_eq!(get("SP_QUEUE_DEPTH"), Some("?".to_string()));
    }
}
