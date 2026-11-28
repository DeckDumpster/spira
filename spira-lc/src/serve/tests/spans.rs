use super::*;

#[test_id(tq5yuhc)]
#[test]
fn a_served_request_writes_one_span_and_a_dead_sink_changes_nothing() {
    let _env = testkit::env_read();
    let spans = Spans::new();
    let req = "[\"no-such-verb\"]\n";
    let (reply, code) = ask(Some(&spans.sink), req);
    let got = spans.received();
    assert_eq!(got.len(), 1, "exactly one span per request");
    let span = &got[0];
    assert_eq!(span.name, "no-such-verb");
    let body_len = reply.split_once('\n').unwrap().1.len() as i64;
    assert!(span.attributes.iter().any(|a| a.key == "spira.queue_wait_us"), "queue wait is recorded on every span");
    let answered = span.attributes.iter().find(|a| a.key == "spira.answer_us").expect("the answer time is recorded on a served request");
    let us = |a: &opentelemetry_proto::tonic::common::v1::KeyValue| match a.value.as_ref().and_then(|v| v.value.as_ref()) {
        Some(opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(n)) => *n as u64,
        other => panic!("{other:?}"),
    };
    assert!(us(answered) <= (span.end_time_unix_nano - span.start_time_unix_nano) / 1000, "the answer is written before the span ends");
    let size = span.attributes.iter().find(|a| a.key == "rpc.response.size").unwrap();
    assert_eq!(format!("{:?}", size.value), format!("Some(AnyValue {{ value: Some(IntValue({body_len})) }})"));

    let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = gone.local_addr().unwrap().to_string();
    drop(gone);
    let dead = spira_otel::Sink::buffered(&addr, "spira-lc");
    let (reply2, code2) = ask(Some(&dead), req);
    assert_eq!((reply2, code2), (reply, code), "a failing sink leaves the reply and exit code unchanged");
    dead.flush();
    assert_eq!(dead.dropped_here(), 1);
}

/// A sink pushing to its own fixture Collector.
struct Spans {
    collector: spira_otel::fixture::Collector,
    sink: spira_otel::Sink,
}

impl Spans {
    fn new() -> Spans {
        let collector = spira_otel::fixture::Collector::start();
        let sink = spira_otel::Sink::buffered(collector.addr(), "spira-lc");
        Spans { collector, sink }
    }

    fn received(&self) -> Vec<opentelemetry_proto::tonic::trace::v1::Span> {
        self.sink.flush();
        self.collector.spans()
    }
}

fn span_of(snapshot: &state_store::phases::Snapshot) -> opentelemetry_proto::tonic::trace::v1::Span {
    let now = std::time::SystemTime::now();
    let call = spira_otel::Call {
        method: "renew",
        caller: "-",
        request_size: 1,
        response_size: 1,
        exit_code: 0,
        start: now,
        end: now,
        queue_wait: std::time::Duration::ZERO,
        before_work: std::time::Duration::ZERO,
        answer_wait: std::time::Duration::ZERO,
        after_answer: std::time::Duration::ZERO,
        cut: false,
        answered: None,
        cut_at: None,
        phases: &snapshot.phases,
        commits_us: &snapshot.commits_us,
        reads: snapshot.reads,
        read_max_us: snapshot.read_max_us,
        reads_ended: snapshot.reads_ended,
        reads_overran: snapshot.reads_overran,
        open_phase: snapshot.open,
        content_queries: snapshot.content_queries,
        content_query_max_us: snapshot.content_query_max_us,
        content_query_max_shape: &snapshot.content_query_max_shape,
        task: None,
        children: snapshot.children,
        programs: &snapshot.programs,
        subverb: None,
    };
    spira_otel::span_request("spira-lc", &call).resource_spans[0].scope_spans[0].spans[0].clone()
}

#[test_id(t0nfoaq)]
#[test]
fn a_span_from_a_recorder_with_two_commits_names_both() {
    let _env = testkit::env_read();
    use state_store::phases::{self, Phase};
    let rec = std::sync::Arc::new(phases::Recorder::default());
    phases::track(rec.clone());
    phases::time(Phase::Commit, || ());
    phases::time(Phase::Commit, || ());
    phases::time(Phase::Read, || ());
    phases::record_read(std::time::Duration::from_micros(70), phases::ReadEnd::Ended);
    phases::record_read(std::time::Duration::from_micros(9), phases::ReadEnd::Overran);
    std::mem::forget(phases::begin(Phase::WriterWait));
    phases::untrack();
    let span = span_of(&rec.snapshot());
    let attr = |k: &str| span.attributes.iter().find(|a| a.key == k).and_then(|a| a.value.clone()).and_then(|v| v.value);
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    assert!(matches!(attr("spira.store.commits"), Some(Value::IntValue(2))));
    assert!(matches!(attr("spira.store.reads"), Some(Value::IntValue(1))));
    assert!(matches!(attr("spira.store.read_max_us"), Some(Value::IntValue(70))));
    assert!(matches!(attr("spira.store.reads_ended"), Some(Value::IntValue(1))));
    assert!(matches!(attr("spira.store.reads_overran"), Some(Value::IntValue(1))));
    assert!(matches!(attr("spira.phase.commits_us"), Some(Value::ArrayValue(a)) if a.values.len() == 2));
    assert!(attr("spira.phase.commit_us").is_some() && attr("spira.phase.read_us").is_some());
    assert!(matches!(attr("spira.phase.open"), Some(Value::StringValue(s)) if s == "writer_wait"));
    assert!(attr("spira.queue_wait_us").is_some());
}

#[test_id(tzac7dh)]
#[test]
fn a_span_names_its_children_and_content_queries() {
    let _env = testkit::env_read();
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use state_store::phases::{self, Phase};
    let rec = std::sync::Arc::new(phases::Recorder::default());
    phases::track(rec.clone());
    phases::child("bd", || ());
    phases::time(Phase::ContentQuery, || ());
    phases::time(Phase::ContentQuery, || ());
    phases::time(Phase::ContentWait, || ());
    phases::untrack();
    let span = span_of(&rec.snapshot());
    let attr = |k: &str| span.attributes.iter().find(|a| a.key == k).and_then(|a| a.value.clone()).and_then(|v| v.value);
    assert!(matches!(attr("spira.children"), Some(Value::IntValue(1))));
    assert!(matches!(attr("spira.children.programs"), Some(Value::StringValue(s)) if s == "bd"));
    assert!(matches!(attr("spira.content.queries"), Some(Value::IntValue(2))));
    for k in ["spira.phase.child_us", "spira.phase.content_query_us", "spira.phase.content_wait_us"] {
        assert!(attr(k).is_some(), "{k}");
    }
}

#[test_id(t9gpgjg)]
#[test]
fn a_span_names_its_longest_content_statement() {
    let _env = testkit::env_read();
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use state_store::phases::{self, Phase};
    let rec = std::sync::Arc::new(phases::Recorder::default());
    phases::track(rec.clone());
    phases::time(Phase::ContentQuery, || ());
    phases::record_content_statement(std::time::Duration::from_micros(40), || "SELECT 1".into());
    phases::record_content_statement(std::time::Duration::from_micros(900), || "SELECT a FROM t WHERE label = ?".into());
    phases::record_content_statement(std::time::Duration::from_micros(10), || "SELECT 2".into());
    phases::untrack();
    let span = span_of(&rec.snapshot());
    let attr = |k: &str| span.attributes.iter().find(|a| a.key == k).and_then(|a| a.value.clone()).and_then(|v| v.value);
    assert!(matches!(attr("spira.content.query_max_us"), Some(Value::IntValue(900))));
    assert!(matches!(attr("spira.content.query_max_shape"), Some(Value::StringValue(s)) if s.contains("WHERE label")));
}

#[test_id(td40q2j)]
#[test]
fn a_request_that_names_its_task_carries_it_on_the_span() {
    let _env = testkit::env_read();
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    let spans = Spans::new();
    ask(Some(&spans.sink), "{\"argv\":[\"no-such-verb\"],\"task\":\"t\"}\n");
    ask(Some(&spans.sink), "[\"no-such-verb\"]\n");
    let spans = spans.received();
    let task = |i: usize| spans[i].attributes.iter().find(|a| a.key == "spira.task").and_then(|a| a.value.clone()).and_then(|v| v.value);
    assert!(matches!(task(0), Some(Value::StringValue(s)) if s == "t"));
    assert!(task(1).is_none());
}

#[test_id(tkmj6qp)]
#[test]
fn the_subverb_is_a_plain_second_word_and_never_an_id_or_free_text() {
    let _env = testkit::env_read();
    let argv = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(super::subverb_of(&argv(&["content", "note", "sp-a", "x"])), Some("note"));
    assert_eq!(super::subverb_of(&argv(&["history", "sp-a1b2"])), None);
    assert_eq!(super::subverb_of(&argv(&["content", "Free text"])), None);
    assert_eq!(super::subverb_of(&argv(&["content"])), None);
}

#[test_id(tmmn47y)]
#[test]
fn a_request_cut_at_its_deadline_leaves_the_phase_it_was_in_on_the_recorder() {
    let _env = testkit::env_read();
    use state_store::phases::{begin, track_scoped, Phase, Recorder};
    use std::{sync::Arc, time::Duration};
    let rec = Arc::new(Recorder::default());
    let worker = Arc::clone(&rec);
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let out = super::within(Duration::from_millis(30), move || {
        let _tracking = track_scoped(worker);
        let _open = begin(Phase::WriterWait);
        let _ = release_rx.recv();
    });
    assert!(out.is_none());
    let patience = std::time::Instant::now() + Duration::from_secs(5);
    while rec.snapshot().open.is_none() && std::time::Instant::now() < patience { // clock-safe: caps the wait for the worker to open its phase
        std::thread::yield_now();
    }
    assert_eq!(rec.snapshot().open, Some("writer_wait"));
    drop(release);
}

#[test_id(tsu4sm5)]
#[test]
fn the_answer_time_is_taken_before_the_owed_close_settles() {
    let _env = testkit::env_read();
    let (server, _client) = std::os::unix::net::UnixStream::pair().unwrap();
    let ans = super::Answer { owed: vec![("sp-d".into(), "why".into())].into(), ..Default::default() };
    let (read_at, order) = (std::time::Instant::now(), std::cell::RefCell::new(Vec::new()));
    let mut answered = std::time::Duration::ZERO;
    super::reply_then_settle(
        &server,
        &ans,
        false,
        || {
            answered = read_at.elapsed();
            order.borrow_mut().push("answered");
        },
        &mut |_, _| {
            order.borrow_mut().push("settled");
            Ok(())
        },
    );
    assert_eq!(*order.borrow(), ["answered", "settled"]);
    assert!(answered > std::time::Duration::ZERO, "the answer time was taken");
}

#[test_id(tupt2tz)]
#[test]
fn a_request_that_touched_no_store_carries_no_phase_attribute() {
    let _env = testkit::env_read();
    let span = span_of(&state_store::phases::Snapshot::default());
    let named: Vec<_> = span.attributes.iter().map(|a| a.key.as_str()).filter(|k| k.starts_with("spira.phase.") || k.starts_with("spira.store.")).collect();
    assert!(named.is_empty(), "{named:?}");
    assert!(span.attributes.iter().any(|a| a.key == "spira.queue_wait_us"));
}

fn span_attr(spans: &Spans, verb: &str, key: &str) -> Option<Option<i64>> {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    let span = spans.received().into_iter().rfind(|s| s.name == verb).expect("a span for the verb");
    span.attributes.iter().find(|a| a.key == key).map(|a| match a.value.as_ref()?.value.as_ref()? {
        Value::IntValue(n) => Some(*n),
        _ => None,
    })
}

#[test_id(t70wf5b)]
#[test]
fn a_span_records_the_cutoff_only_for_a_call_the_cutoff_abandoned() {
    let _env = testkit::env_read();
    let spans = Spans::new();
    let r = rig(false, three_seconds());
    let _ = send_spanned(&r.door, Some(&spans.sink), &["show", "sp-none"], None, None);
    assert!(span_attr(&spans, "show", "spira.answer_us").is_some(), "an in-time call records its answer time");
    assert_eq!(span_attr(&spans, "show", "spira.deadline_cut_ms"), None, "an in-time call records no cutoff");

    let parked = rig(true, fifty_ms());
    let cut = send_spanned(&parked.door, Some(&spans.sink), &["content", "note", "sp-a", "x"], None, None);
    assert_eq!(cut, (2, "cannot tell: deadline".to_string()));
    assert_eq!(span_attr(&spans, "content", "spira.deadline_cut_ms"), Some(Some(50)));
    assert!(span_attr(&spans, "content", "spira.answer_us").is_some());
    parked.notes.release();

    let ledgered = rig(true, fifty_ms());
    let cut = send_spanned(&ledgered.door, Some(&spans.sink), &["content", "note", "sp-b", "y"], None, Some("req-cut"));
    assert_eq!(cut, (2, "cannot tell: deadline".to_string()));
    assert_eq!(span_attr(&spans, "content", "spira.deadline_cut_ms"), Some(Some(50)), "a write under a request id is cut the same way");
    ledgered.notes.release();
}

fn serve_value(span: &opentelemetry_proto::tonic::trace::v1::Span, key: &str) -> opentelemetry_proto::tonic::common::v1::any_value::Value {
    span.attributes.iter().find(|a| a.key == key).unwrap_or_else(|| panic!("{key} is on the span")).value.clone().unwrap().value.unwrap()
}

#[test_id(t7w6dtq)]
#[test]
fn a_span_cut_at_its_deadline_says_where_its_time_went() {
    let _env = testkit::env_read();
    let sink = Spans::new();
    let parked = rig(true, fifty_ms());
    let cut = send_spanned(&parked.door, Some(&sink.sink), &["content", "note", "sp-a", "x"], None, None);
    assert_eq!(cut, (2, "cannot tell: deadline".to_string()));
    parked.notes.release();
    let ledgered = rig(true, fifty_ms());
    let cut = send_spanned(&ledgered.door, Some(&sink.sink), &["content", "note", "sp-b", "y"], None, Some("req-stages"));
    assert_eq!(cut, (2, "cannot tell: deadline".to_string()));
    ledgered.notes.release();
    let spans = sink.received();
    assert_eq!(spans.len(), 2);
    for span in &spans {
        check_stages(span);
    }
}

fn check_stages(span: &opentelemetry_proto::tonic::trace::v1::Span) {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    let us = |k: &str| match serve_value(span, k) {
        Value::IntValue(n) => n as u64,
        other => panic!("{k}: {other:?}"),
    };
    assert_eq!(serve_value(span, "spira.serve.cut"), Value::BoolValue(true));
    assert!(us("spira.serve.before_work_us") + us("spira.serve.answer_wait_us") >= 50_000, "the cutoff was waited out, counted from receipt");
    let stages = us("spira.serve.before_work_us") + us("spira.serve.answer_wait_us") + us("spira.serve.after_answer_us");
    assert!(stages <= (span.end_time_unix_nano - span.start_time_unix_nano) / 1000, "the stages sit inside the span");
}

#[test_id(t0v9mng)]
#[test]
fn a_call_answered_in_time_writes_cut_false() {
    let _env = testkit::env_read();
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    let spans = Spans::new();
    let r = rig(false, three_seconds());
    let _ = send_spanned(&r.door, Some(&spans.sink), &["show", "sp-none"], None, None);
    let mut got = spans.received();
    assert_eq!(got.len(), 1, "one span");
    let span = got.remove(0);
    assert_eq!(serve_value(&span, "spira.serve.cut"), Value::BoolValue(false));
}

fn head_of(door: &Arc<super::Door>) -> serde_json::Value {
    use std::io::{Read, Write};
    let argv = vec!["list".to_string()];
    let head = spira_store::lc_call::request_head(&argv, None, &Default::default(), None, None).unwrap();
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    (&client).write_all(format!("{head}\n").as_bytes()).unwrap();
    let mut got = String::new();
    std::thread::scope(|scope| {
        scope.spawn(|| super::serve_one(server, None, std::time::Instant::now(), door));
        (&client).read_to_string(&mut got).unwrap();
    });
    serde_json::from_str(got.lines().next().unwrap()).unwrap()
}

fn door_answering(code: i32, ended: Option<state_store::phases::ReadEnd>) -> Arc<super::Door> {
    let work: super::Work = Arc::new(move |_| {
        if let Some(end) = ended {
            state_store::phases::record_read(std::time::Duration::from_millis(150), end);
        }
        crate::callers::Answer { code, stdout: "cannot tell: list".into(), ..Default::default() }
    });
    Arc::new(super::Door { ledger: super::Ledger::new(std::time::Duration::from_secs(60)), work, personas: Default::default(), deadlines: test_deadlines() })
}

#[test_id(t8o880r)]
#[test]
fn a_cannot_tell_whose_store_read_was_ended_at_the_limit_says_so_in_its_head() {
    let _env = testkit::env_read();
    use state_store::phases::ReadEnd;
    for end in [ReadEnd::Ended, ReadEnd::Overran] {
        assert_eq!(head_of(&door_answering(2, Some(end)))["read_ended"], true, "{end:?}");
    }
    assert!(head_of(&door_answering(2, None)).get("read_ended").is_none(), "another failure is not marked");
    assert!(head_of(&door_answering(2, Some(ReadEnd::Finished))).get("read_ended").is_none(), "a read that finished is not marked");
    assert!(head_of(&door_answering(0, Some(ReadEnd::Ended))).get("read_ended").is_none(), "an answer that applied is not marked");
}

#[test_id(tyvvzq9)]
#[test]
fn a_request_whose_read_a_forced_wal_restart_cut_pages_the_slow_query_log_with_its_verb() {
    use state_store::phases;
    let dir = testkit::TempDir::new("lc-serve-wal-page");
    let log = dir.join("slow.log");
    let _env = testkit::env(&[("SPIRA_SLOW_QUERY_LOG", Some(log.to_str().unwrap()))]);
    let work: super::Work = Arc::new(|_| {
        let since = phases::forced_stamp();
        drop(phases::forced_reset_scoped());
        phases::record_read(std::time::Duration::from_millis(150), phases::ReadEnd::Ended);
        phases::record_reset_ended(since, phases::ReadEnd::Ended);
        crate::callers::Answer { code: 2, stdout: "cannot tell: list".into(), ..Default::default() }
    });
    let door = Arc::new(super::Door { ledger: super::Ledger::new(std::time::Duration::from_secs(60)), work, personas: Default::default(), deadlines: test_deadlines() });
    head_of(&door);
    let got = std::fs::read_to_string(&log).unwrap();
    let f: Vec<&str> = got.lines().find(|l| l.ends_with("WAL-RESET-ENDED")).unwrap_or_else(|| panic!("{got}")).split('\t').collect();
    assert_eq!(f[1], "list", "{got}");
}

#[test_id(tasjkm8)]
#[test]
fn a_request_logs_its_own_verb_and_caller_for_a_query_made_before_any_dispatch() {
    let dir = testkit::TempDir::new("lc-serve-verb-first");
    let log = dir.join("slow.log");
    let _env = testkit::env(&[("SPIRA_SLOW_QUERY_LOG", Some(log.to_str().unwrap()))]);
    let work: super::Work = Arc::new(|_| {
        crate::slow::record(std::time::Duration::from_millis(5000), "SELECT 1");
        crate::callers::Answer::out(0, String::new())
    });
    let door = super::Door { ledger: super::Ledger::new(std::time::Duration::from_secs(60)), work, personas: Default::default(), deadlines: test_deadlines() };
    let me = std::process::id() as i32;
    for verb in ["list", "show"] {
        let req = super::Request { argv: vec![verb.to_string()], stdin: None, caller: Default::default(), task: None, request_id: None };
        let now = std::time::Instant::now();
        super::call_request(req, super::Peer { pid: Some(me), uid: None }, &door, now, now, super::Cut::ByCaller);
    }
    let got = std::fs::read_to_string(&log).unwrap();
    let who = crate::slow::caller_of_pid(me);
    let fields: Vec<(String, String)> = got.lines().map(|l| (l.split('\t').nth(1).unwrap().to_string(), l.split('\t').nth(2).unwrap().to_string())).collect();
    assert_eq!(fields, [("list".to_string(), who.clone()), ("show".to_string(), who)], "{got}");
}

#[test_id(tcezk56)]
#[test]
fn the_metrics_pass_pushes_the_dropped_span_count_for_spira_lc() {
    let _env = testkit::env_read();
    let store = spira_otel::fixture::Sink::start();
    let metrics = spira_otel::metrics::MetricsSink::new(Some(store.addr()), "spira-lc", std::time::Duration::from_secs(30));
    let before = spira_otel::dropped();
    super::metrics_pass(&metrics, &mut super::MetricsPushed::default());
    let seen = store.received(std::time::Duration::from_secs(30));
    let dropped = seen.iter().find(|s| s.name == "spira_otel_spans_dropped_total").unwrap_or_else(|| panic!("{seen:?}"));
    assert_eq!(dropped.label("service"), Some("spira-lc"));
    assert!(dropped.value >= before as f64 && dropped.value <= spira_otel::dropped() as f64, "the cumulative count: {}", dropped.value);
}

#[test_id(tx4qwbh)]
#[test]
fn lc_serve_refuses_a_missing_or_empty_collector_endpoint_naming_the_key() {
    let _env = testkit::env_read();
    for read in [Err("missing".to_string()), Ok(String::new()), Ok("  ".to_string())] {
        let e = super::collector_endpoint(|_| read.clone()).unwrap_err();
        assert!(e.contains("spira.otel_collector_endpoint"), "{e}");
    }
    assert_eq!(super::collector_endpoint(|k| Ok(format!("{k}:1"))), Ok("SPIRA_OTEL_COLLECTOR_ENDPOINT:1".to_string()));
}
