//! `backfill-repo`: give a lifecycle row that has no repo the repo its bead's `repo:` label
//! names. A row without one files the bead's Claim, Submit and Delivered under the scope `-`.
//! Safe to run again: a row that has a repo is never touched, and a WORKING row, which
//! `set-repo` refuses, is left for the next run.

use serde_json::Value;

use crate::callers::{s, usage, Answer, Machine, APPLIED, CANNOT_TELL, REFUSED};

const CHUNK: usize = 100;
const USE: &str = "backfill-repo [--apply | --dry-run] [<bead-id>...]";
/// Rows one call writes: a sweep answers inside the service's query deadline, so a backlog is
/// worked down over repeated calls, each skipping what the last set.
const PER_CALL: usize = 200;
/// What `set-repo` exits with on a WORKING row.
const KEPT: i32 = 1;

fn cannot(why: String) -> Answer {
    Answer { code: CANNOT_TELL, stderr: format!("spira-lc backfill-repo: {why}\n"), ..Default::default() }
}

fn rows(m: &mut dyn Machine) -> Result<Vec<Value>, String> {
    let (rc, out) = m.call(&["list".into()]);
    match serde_json::from_str::<Value>(out.trim()) {
        Ok(Value::Array(rows)) if rc == 0 => Ok(rows),
        _ => Err(format!("lifecycle list failed: {}", out.trim())),
    }
}

/// The repo each of `ids` names in its `repo:` label; a bead with none is absent.
fn named(m: &mut dyn Machine, ids: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        let (rc, text) = m.call(&["plan".into(), "items".into(), "--ids".into(), chunk.join(",")]);
        let items = serde_json::from_str::<Value>(text.trim()).ok().and_then(|mut v| v.get_mut("items").map(Value::take));
        let Some(Value::Array(items)) = items.filter(|_| rc == 0) else {
            return Err(format!("content store unreadable: {}", text.trim()));
        };
        for item in &items {
            let labels = item["labels"].as_array().into_iter().flatten().filter_map(Value::as_str);
            if let Some(repo) = labels.filter_map(|l| l.strip_prefix("repo:")).find(|r| !r.is_empty()) {
                out.push((s(item, "id"), repo.to_string()));
            }
        }
    }
    Ok(out)
}

/// `backfill-repo [--apply | --dry-run] [<bead-id>...]`. Dry run unless `--apply`.
///
/// Exit: 0 done (or listed) · 2 cannot tell · 3 a set-repo failed.
pub fn backfill_repo(args: &[String], m: &mut dyn Machine) -> Answer {
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
    let wanted = |r: &&Value| s(r, "repo").is_empty() && (only.is_empty() || only.iter().any(|i| **i == s(r, "bead_id")));
    let unset: Vec<String> = all.iter().filter(wanted).map(|r| s(r, "bead_id")).collect();
    let working: Vec<String> = all.iter().filter(wanted).filter(|r| s(r, "state") == "WORKING").map(|r| s(r, "bead_id")).collect();
    let named = match named(m, &unset) {
        Ok(n) => n,
        Err(e) => return cannot(e),
    };
    if apply {
        if let Err((code, why)) = crate::skew::guard(m) {
            return Answer { code, stderr: format!("spira-lc backfill-repo: {why}\n"), ..Default::default() };
        }
    }
    let (mut lines, mut code, mut done, mut kept, mut tried) = (Vec::new(), APPLIED, 0usize, 0usize, 0usize);
    for (id, repo) in &named {
        if !apply {
            if working.contains(id) {
                lines.push(format!("would keep {id}: WORKING"));
                kept += 1;
            } else {
                lines.push(format!("would set {id} -> {repo}"));
                done += 1;
            }
            continue;
        }
        if tried == PER_CALL {
            lines.push(format!("stopped after {PER_CALL} rows; run again for the rest"));
            break;
        }
        tried += 1;
        match m.call(&["set-repo".into(), id.clone(), repo.clone()]) {
            (0, _) => {
                lines.push(format!("set {id} -> {repo}"));
                done += 1;
            }
            (KEPT, _) => {
                lines.push(format!("kept {id}: WORKING; run again once it leaves"));
                kept += 1;
            }
            (_, why) => {
                lines.push(format!("failed {id}: {}", why.trim()));
                code = REFUSED;
            }
        }
    }
    let verb = if apply { "set" } else { "would set (dry run; --apply to write)" };
    lines.push(format!("backfill-repo: {done} {verb}, {kept} kept while WORKING, {} without a repo: label", unset.len() - named.len()));
    Answer::out(code, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct Lc {
        rows: Vec<Value>,
        labels: Vec<(&'static str, Vec<&'static str>)>,
        working: Vec<&'static str>,
        sets: Vec<Vec<String>>,
    }

    impl Machine for Lc {
        fn call(&mut self, a: &[String]) -> (i32, String) {
            match a[0].as_str() {
                "list" => (0, Value::Array(self.rows.clone()).to_string()),
                "plan" => {
                    let asked = a[3].split(',').collect::<Vec<_>>();
                    let items: Vec<Value> = self.labels.iter().filter(|(id, _)| asked.contains(id)).map(|(id, l)| json!({"id": id, "labels": l})).collect();
                    (0, json!({"items": items}).to_string())
                }
                "set-repo" => {
                    self.sets.push(a.to_vec());
                    if self.working.contains(&a[1].as_str()) { (KEPT, "WORKING".into()) } else { (0, String::new()) }
                }
                _ => (0, String::new()),
            }
        }
    }

    fn fixture() -> Lc {
        Lc {
            rows: vec![
                json!({"bead_id": "sp-ready", "state": "READY", "repo": null}),
                json!({"bead_id": "sp-landed", "state": "LANDED", "repo": ""}),
                json!({"bead_id": "sp-working", "state": "WORKING", "repo": null}),
                json!({"bead_id": "sp-has", "state": "READY", "repo": "other"}),
                json!({"bead_id": "sp-bare", "state": "READY", "repo": null}),
            ],
            labels: vec![
                ("sp-ready", vec!["plan", "repo:deck"]),
                ("sp-landed", vec!["repo:harness"]),
                ("sp-working", vec!["repo:deck"]),
                ("sp-has", vec!["repo:deck"]),
                ("sp-bare", vec!["plan"]),
            ],
            working: vec!["sp-working"],
            ..Default::default()
        }
    }

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn backfill_repo_sets_the_label_repo_on_rows_without_one_and_leaves_working_and_unlabelled_rows() {
        let mut m = fixture();
        let a = backfill_repo(&v(&["--apply"]), &mut m);
        assert_eq!(a.code, APPLIED, "{}", a.stderr);
        assert_eq!(
            m.sets,
            [v(&["set-repo", "sp-ready", "deck"]), v(&["set-repo", "sp-landed", "harness"]), v(&["set-repo", "sp-working", "deck"])],
            "a row with a repo or no repo: label is never written"
        );
        assert!(a.stdout.contains("kept sp-working"), "{}", a.stdout);
        assert!(a.stdout.contains("2 set, 1 kept while WORKING, 1 without"), "{}", a.stdout);
    }

    #[test]
    fn backfill_repo_writes_nothing_without_apply() {
        let mut m = fixture();
        let a = backfill_repo(&[], &mut m);
        assert_eq!(a.code, APPLIED);
        assert!(m.sets.is_empty());
        assert!(a.stdout.contains("would set sp-ready -> deck"), "{}", a.stdout);
    }

    #[test]
    fn backfill_repo_named_ids_limit_the_sweep() {
        let mut m = fixture();
        backfill_repo(&v(&["--apply", "sp-landed"]), &mut m);
        assert_eq!(m.sets, [v(&["set-repo", "sp-landed", "harness"])]);
    }

    #[test]
    fn backfill_repo_dry_run_does_not_promise_to_set_a_working_row() {
        let a = backfill_repo(&[], &mut fixture());
        assert!(a.stdout.contains("would keep sp-working: WORKING"), "{}", a.stdout);
        assert!(a.stdout.contains("2 would set (dry run; --apply to write), 1 kept"), "{}", a.stdout);
    }

    #[test]
    fn backfill_repo_stops_at_the_per_call_bound_and_says_to_run_again() {
        let mut m = Lc::default();
        for i in 0..PER_CALL + 1 {
            m.rows.push(json!({"bead_id": format!("sp-{i}"), "state": "READY", "repo": null}));
            m.labels.push((Box::leak(format!("sp-{i}").into_boxed_str()), vec!["repo:deck"]));
        }
        let a = backfill_repo(&v(&["--apply"]), &mut m);
        assert_eq!(m.sets.len(), PER_CALL);
        assert!(a.stdout.contains("run again"), "{}", a.stdout);
    }

    struct Down(&'static str);

    impl Machine for Down {
        fn call(&mut self, a: &[String]) -> (i32, String) {
            if a[0] == self.0 { (2, "cannot tell: down".into()) } else { Lc::default().call(a) }
        }
    }

    #[test]
    fn backfill_repo_cannot_tell_when_the_rows_or_the_labels_are_unreadable() {
        assert_eq!(backfill_repo(&[], &mut Down("list")).code, CANNOT_TELL);
        let mut lc = fixture();
        struct Both<'a>(&'a mut Lc);
        impl Machine for Both<'_> {
            fn call(&mut self, a: &[String]) -> (i32, String) {
                if a[0] == "plan" { (2, "down".into()) } else { self.0.call(a) }
            }
        }
        assert_eq!(backfill_repo(&[], &mut Both(&mut lc)).code, CANNOT_TELL);
    }

    #[test]
    fn backfill_repo_reports_a_failed_set_and_goes_on() {
        struct Fails(Lc);
        impl Machine for Fails {
            fn call(&mut self, a: &[String]) -> (i32, String) {
                if a[0] == "set-repo" && a[1] == "sp-ready" { (2, "boom".into()) } else { self.0.call(a) }
            }
        }
        let mut m = Fails(fixture());
        let a = backfill_repo(&v(&["--apply"]), &mut m);
        assert_eq!(a.code, REFUSED);
        assert!(a.stdout.contains("failed sp-ready: boom"), "{}", a.stdout);
        assert!(m.0.sets.iter().any(|s| s[1] == "sp-landed"), "the sweep goes on past a failure");
    }

    #[test]
    fn backfill_repo_refuses_an_unknown_flag() {
        assert_eq!(backfill_repo(&v(&["--force"]), &mut fixture()).code, CANNOT_TELL);
    }
}
