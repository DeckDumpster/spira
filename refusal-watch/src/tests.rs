use super::*;

const NOW: i64 = 1_791_576_000;

fn cfg() -> Config {
    Config {
        window_secs: 86400,
        baseline: 5,
        repeat_secs: 86400,
        max_files: 2,
    }
}

fn row(actor: &str, event: &str, n: u64) -> String {
    format!(
        r#"{{"actor":"{actor}","event":"{event}","from_state":"WORKING","refusal":"wrong_state","n":"{n}","first_at":"{}","last_at":"{}","examples":"sp-a@{},sp-b@{}"}}"#,
        NOW - 600,
        NOW - 60,
        NOW - 60,
        NOW - 600
    )
}

fn answer(rows: &[String]) -> String {
    format!(r#"{{"now":{NOW},"classes":[{}]}}"#, rows.join(","))
}

fn run(
    rows: &[String],
    state: BTreeMap<String, i64>,
    c: &Config,
) -> (Outcome, Vec<(String, String)>) {
    let (now, classes) = parse(&answer(rows)).unwrap();
    let mut filed = Vec::new();
    let o = pass(&classes, now, c, state, &mut |t, b| {
        filed.push((t.to_string(), b.to_string()));
        Ok(())
    });
    (o, filed)
}

#[test]
fn a_planted_class_over_baseline_is_filed_naming_caller_examples_and_times() {
    let (o, filed) = run(
        &[row("dwarf:sp-x", "Submit", 9), row("quiet", "Claim", 2)],
        BTreeMap::new(),
        &cfg(),
    );
    assert_eq!(filed.len(), 1, "{filed:?}");
    let (title, body) = &filed[0];
    assert!(
        title.contains("Submit") && title.contains("dwarf:sp-x"),
        "{title}"
    );
    assert!(
        body.contains("Caller: dwarf:sp-x") && body.contains("- sp-a at 2026-10-09T"),
        "{body}"
    );
    assert_eq!(o.filed.len(), 1);
}

#[test]
fn a_class_at_or_under_baseline_is_not_filed() {
    let (_, filed) = run(
        &[row("a", "Submit", 5), row("b", "Claim", 1)],
        BTreeMap::new(),
        &cfg(),
    );
    assert!(filed.is_empty(), "{filed:?}");
}

#[test]
fn a_class_repeats_once_per_window_and_not_per_event() {
    let rows = [row("a", "Submit", 50)];
    let (first, f1) = run(&rows, BTreeMap::new(), &cfg());
    assert_eq!(f1.len(), 1);
    let (second, f2) = run(&rows, first.state.clone(), &cfg());
    assert!(
        f2.is_empty(),
        "same day, more refusals, still one filing: {f2:?}"
    );
    let mut later = first.state.clone();
    for t in later.values_mut() {
        *t -= 86400;
    }
    let (_, f3) = run(&rows, later, &cfg());
    assert_eq!(f3.len(), 1, "a day on it is filed again");
    assert_eq!(second.state, first.state);
}

#[test]
fn a_failed_filing_is_not_recorded_and_is_retried() {
    let (now, classes) = parse(&answer(&[row("a", "Submit", 9)])).unwrap();
    let o = pass(&classes, now, &cfg(), BTreeMap::new(), &mut |_, _| {
        Err("db down".into())
    });
    assert_eq!((o.filed.len(), o.failed.len(), o.state.len()), (0, 1, 0));
}

#[test]
fn a_batch_is_bounded_worst_class_first() {
    let rows = [
        row("a", "Submit", 7),
        row("b", "Submit", 30),
        row("c", "Submit", 9),
    ];
    let (o, filed) = run(&rows, BTreeMap::new(), &cfg());
    assert_eq!(filed.len(), 2);
    assert!(
        filed[0].0.contains(" by b") && filed[1].0.contains(" by c"),
        "{filed:?}"
    );
    assert_eq!(o.deferred, 1);
}

#[test]
fn an_answer_that_is_not_the_expected_shape_is_an_error() {
    assert!(parse("").is_err());
    assert!(parse(r#"{"classes":[]}"#).is_err());
    assert!(parse(r#"{"now":1}"#).is_err());
    assert_eq!(parse(r#"{"now":1,"classes":[]}"#).unwrap().1.len(), 0);
}

#[test]
fn state_round_trips() {
    let mut s = BTreeMap::new();
    s.insert("a|b|c|d".to_string(), 5);
    assert_eq!(parse_state(&render_state(&s)), s);
}

#[test]
fn iso_formats_epochs() {
    assert_eq!(iso(0), "1970-01-01T00:00:00Z");
    assert_eq!(iso(1_791_576_000), "2026-10-09T20:00:00Z");
}

struct Scripted(std::sync::Mutex<Vec<Vec<String>>>, i32);

impl spira_store::lc_call::Transport for Scripted {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
        self.0.lock().unwrap().push(argv.to_vec());
        Ok(spira_store::lc_call::Reply { code: self.1, out: "{\"now\":1}".into(), err: String::new() })
    }
}

/// sp-ipwlic: the refusals are read over the store client; a non-zero answer is an error.
#[test]
fn refusals_are_read_through_the_store_client() {
    let t = std::sync::Arc::new(Scripted(Default::default(), 0));
    let c = spira_store::lc_call::LcClient::over(t.clone(), true);
    assert_eq!(read_refusals(&c, 86400).unwrap(), "{\"now\":1}");
    assert_eq!(*t.0.lock().unwrap(), vec![vec!["ops-refusals".to_string(), "86400".to_string()]]);
    let refused = spira_store::lc_call::LcClient::over(std::sync::Arc::new(Scripted(Default::default(), 2)), true);
    assert!(read_refusals(&refused, 60).unwrap_err().contains("exited 2"));
}
