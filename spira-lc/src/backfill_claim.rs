//! `backfill-claim-inputs`: give a lifecycle record whose `claim` was never written the claim
//! rule's inputs from the content store. Safe to run again: a record with a `claim` is never
//! changed, the check being made inside the store's write (`sync-claim-inputs --only-if-unset`).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::callers::{s, usage, Answer, Machine, APPLIED, CANNOT_TELL, REFUSED};

const CHUNK: usize = 100;
const USE: &str = "backfill-claim-inputs [--apply | --dry-run] [<bead-id>...]";
/// Records one call writes: a sweep answers inside the service's query deadline, so a backlog
/// is worked down over repeated calls, each skipping what the last wrote.
const PER_CALL: usize = 200;
const TERMINAL: [&str; 4] = ["LANDED", "DONE", "DROPPED", "SUPERSEDED"];

fn cannot(why: String) -> Answer {
    Answer { code: CANNOT_TELL, stderr: format!("spira-lc backfill-claim-inputs: {why}\n"), ..Default::default() }
}

fn rows(m: &mut dyn Machine) -> Result<Vec<Value>, String> {
    let (rc, out) = m.call(&["list".into()]);
    match serde_json::from_str::<Value>(out.trim()) {
        Ok(Value::Array(rows)) if rc == 0 => Ok(rows),
        _ => Err(format!("lifecycle list failed: {}", out.trim())),
    }
}

/// `backfill-claim-inputs [--apply | --dry-run] [<bead-id>...]`. Dry run unless `--apply`.
///
/// Exit: 0 done (or listed) · 2 cannot tell · 3 an id failed.
pub fn backfill_claim_inputs(args: &[String], m: &mut dyn Machine) -> Answer {
    let apply = args.iter().any(|a| a == "--apply");
    if apply && args.iter().any(|a| a == "--dry-run") {
        return usage(&format!("{USE} (--apply and --dry-run together)"));
    }
    if let Some(bad) = args.iter().find(|a| a.starts_with('-') && !matches!(a.as_str(), "--apply" | "--dry-run")) {
        return usage(&format!("{USE} (unknown flag {bad})"));
    }
    let only: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let all = match rows(m) {
        Ok(r) => r,
        Err(e) => return cannot(e),
    };
    let mut unset: Vec<(String, String)> = all
        .iter()
        .filter(|r| r.get("claim").is_none_or(Value::is_null) && (only.is_empty() || only.iter().any(|i| **i == s(r, "bead_id"))))
        .map(|r| (s(r, "bead_id"), s(r, "state")))
        .collect();
    unset.sort_by_key(|(_, state)| TERMINAL.contains(&state.as_str()));

    let mut lines = Vec::new();
    if !apply {
        let mut by_state: BTreeMap<&str, usize> = BTreeMap::new();
        for (id, state) in &unset {
            lines.push(format!("would set {id}"));
            *by_state.entry(state).or_default() += 1;
        }
        let counts: Vec<String> = by_state.iter().map(|(st, n)| format!("{st} {n}")).collect();
        let tail = if counts.is_empty() { String::new() } else { format!(": {}", counts.join(", ")) };
        lines.push(format!("backfill-claim-inputs: {} would set (dry run; --apply to write){tail}", unset.len()));
        return Answer::out(APPLIED, lines.join("\n"));
    }
    if let Err((code, why)) = crate::skew::guard(m) {
        return Answer { code, stderr: format!("spira-lc backfill-claim-inputs: {why}\n"), ..Default::default() };
    }
    let more = unset.len() > PER_CALL;
    unset.truncate(PER_CALL);
    let (mut code, mut done, mut kept, mut failed) = (APPLIED, 0usize, 0usize, 0usize);
    for chunk in unset.chunks(CHUNK) {
        let ids: Vec<&str> = chunk.iter().map(|(id, _)| id.as_str()).collect();
        let mut argv = vec![crate::claim_sync::VERB.to_string(), crate::claim_sync::ONLY_IF_UNSET.to_string()];
        argv.extend(ids.iter().map(|i| i.to_string()));
        let (rc, out) = m.call(&argv);
        let named = |prefix: &str, id: &str| out.lines().find_map(|l| l.strip_prefix(&format!("{prefix}{id}:")).map(|why| why.trim().to_string()));
        let (mut chunk_failed, mut chunk_kept) = (0, 0);
        for id in &ids {
            if let Some(why) = named("", id).filter(|_| rc != 0) {
                lines.push(format!("failed {id}: {why}"));
                chunk_failed += 1;
            } else if named("kept ", id).is_some() {
                lines.push(format!("kept {id}: already written"));
                chunk_kept += 1;
            } else {
                lines.push(format!("set {id}"));
            }
        }
        if rc != 0 && chunk_failed == 0 {
            return cannot(format!("{} failed: {}", crate::claim_sync::VERB, out.trim()));
        }
        if chunk_failed > 0 {
            code = REFUSED;
        }
        failed += chunk_failed;
        kept += chunk_kept;
        done += ids.len() - chunk_failed - chunk_kept;
    }
    if more {
        lines.push(format!("stopped after {PER_CALL} records; run again for the rest"));
    }
    lines.push(format!("backfill-claim-inputs: {done} set, {kept} kept, {failed} failed"));
    Answer::out(code, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct Lc {
        rows: Vec<Value>,
        syncs: Vec<Vec<String>>,
        kept: Vec<&'static str>,
        fail: Vec<&'static str>,
        fault: bool,
    }

    impl Machine for Lc {
        fn call(&mut self, a: &[String]) -> (i32, String) {
            match a[0].as_str() {
                "list" => (0, Value::Array(self.rows.clone()).to_string()),
                "sync-claim-inputs" => {
                    self.syncs.push(a.to_vec());
                    if self.fault {
                        return (CANNOT_TELL, "store down".into());
                    }
                    let ids = &a[2..];
                    let said: Vec<String> = ids
                        .iter()
                        .filter_map(|i| {
                            if self.fail.contains(&i.as_str()) {
                                Some(format!("{i}: boom"))
                            } else {
                                self.kept.contains(&i.as_str()).then(|| format!("kept {i}: already written"))
                            }
                        })
                        .collect();
                    (if self.fail.iter().any(|f| ids.iter().any(|i| i == f)) { CANNOT_TELL } else { 0 }, said.join("\n"))
                }
                _ => (0, String::new()),
            }
        }
    }

    fn fixture() -> Lc {
        Lc {
            rows: vec![
                json!({"bead_id": "sp-landed", "state": "LANDED", "claim": null}),
                json!({"bead_id": "sp-ready", "state": "READY", "claim": null}),
                json!({"bead_id": "sp-has", "state": "READY", "claim": {"blocks": [], "parent": null, "labels": []}}),
                json!({"bead_id": "sp-rework", "state": "REWORK"}),
            ],
            ..Default::default()
        }
    }

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_dry_run_names_each_null_row_with_per_state_counts_and_syncs_nothing() {
        let mut m = fixture();
        let a = backfill_claim_inputs(&[], &mut m);
        assert_eq!(a.code, APPLIED, "{}", a.stderr);
        assert!(m.syncs.is_empty());
        assert!(a.stdout.contains("would set sp-ready") && a.stdout.contains("would set sp-landed") && !a.stdout.contains("sp-has"), "{}", a.stdout);
        assert!(a.stdout.contains("3 would set (dry run; --apply to write): LANDED 1, READY 1, REWORK 1"), "{}", a.stdout);
    }

    #[test]
    fn apply_syncs_exactly_the_null_rows_live_states_first_with_only_if_unset() {
        let mut m = fixture();
        let a = backfill_claim_inputs(&v(&["--apply"]), &mut m);
        assert_eq!(a.code, APPLIED, "{}", a.stderr);
        assert_eq!(m.syncs, [v(&["sync-claim-inputs", "--only-if-unset", "sp-ready", "sp-rework", "sp-landed"])]);
        assert!(a.stdout.contains("3 set, 0 kept, 0 failed"), "{}", a.stdout);
    }

    #[test]
    fn a_record_the_store_kept_is_reported_kept_not_set() {
        let mut m = fixture();
        m.kept = vec!["sp-ready"];
        let a = backfill_claim_inputs(&v(&["--apply"]), &mut m);
        assert_eq!(a.code, APPLIED);
        assert!(a.stdout.contains("kept sp-ready: already written") && a.stdout.contains("2 set, 1 kept, 0 failed"), "{}", a.stdout);
    }

    #[test]
    fn named_ids_limit_the_sweep() {
        let mut m = fixture();
        backfill_claim_inputs(&v(&["--apply", "sp-landed", "sp-has"]), &mut m);
        assert_eq!(m.syncs, [v(&["sync-claim-inputs", "--only-if-unset", "sp-landed"])]);
    }

    #[test]
    fn apply_stops_at_the_per_call_bound_and_says_to_run_again() {
        let mut m = Lc::default();
        for i in 0..PER_CALL + 1 {
            m.rows.push(json!({"bead_id": format!("sp-{i}"), "state": "READY", "claim": null}));
        }
        let a = backfill_claim_inputs(&v(&["--apply"]), &mut m);
        assert_eq!(m.syncs.iter().map(|s| s.len() - 2).sum::<usize>(), PER_CALL);
        assert!(m.syncs.iter().all(|s| s.len() - 2 <= CHUNK));
        assert!(a.stdout.contains("run again"), "{}", a.stdout);
    }

    #[test]
    fn a_list_failure_or_a_sync_fault_cannot_tell() {
        struct Down;
        impl Machine for Down {
            fn call(&mut self, _: &[String]) -> (i32, String) {
                (2, "down".into())
            }
        }
        assert_eq!(backfill_claim_inputs(&[], &mut Down).code, CANNOT_TELL);
        let mut m = fixture();
        m.fault = true;
        assert_eq!(backfill_claim_inputs(&v(&["--apply"]), &mut m).code, CANNOT_TELL);
    }

    #[test]
    fn a_failure_on_one_id_is_refused_and_the_sweep_goes_on() {
        let mut m = fixture();
        m.fail = vec!["sp-ready"];
        let a = backfill_claim_inputs(&v(&["--apply"]), &mut m);
        assert_eq!(a.code, REFUSED);
        assert!(a.stdout.contains("failed sp-ready: boom") && a.stdout.contains("set sp-landed") && a.stdout.contains("2 set, 0 kept, 1 failed"), "{}", a.stdout);
    }

    #[test]
    fn an_unknown_flag_is_refused() {
        let mut m = fixture();
        assert_eq!(backfill_claim_inputs(&v(&["--force"]), &mut m).code, CANNOT_TELL);
        assert_eq!(backfill_claim_inputs(&v(&["--apply", "--dry-run"]), &mut m).code, CANNOT_TELL);
        assert!(m.syncs.is_empty());
    }
}
