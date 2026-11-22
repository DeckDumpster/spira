use super::*;

#[test_id(treyxbz)]
#[test]
fn content_is_served_and_keeps_its_refusal_text() {
    let (w, c) = (World::new(), Content::default());
    assert_eq!(over(&w, &c, &["content", "show", "sp-a", "--json"], None), (0, "read show sp-a --json".into(), String::new()));
    assert_eq!(over(&w, &c, &["content", "config", "get", "issue_prefix"], None).1, "read config get issue_prefix");
    let (code, _, err) = over(&w, &c, &["content", "delete", "sp-a"], None);
    assert_eq!(code, 3);
    assert_eq!(err, "spira-lc content: `delete sp-a` is not a content verb\n");
    assert_eq!(over(&w, &c, &["bd-note", "sp-a", "hello"], None).0, 0);
    assert_eq!(*c.0.borrow(), ["note sp-a hello"]);
}

#[test_id(tjpc09m)]
#[test]
fn a_scheduler_task_is_refused_the_whole_history_over_the_served_door() {
    let (w, c) = (World::new(), Content::default());
    let whole = ["content", "list", "--all", "--limit", "0", "--json"];
    crate::slow::set_caller("spira-runtime");
    let (code, out, err) = over(&w, &c, &whole, None);
    assert_eq!((code, out.as_str()), (3, ""), "{err}");
    assert!(err.contains("exit 3"), "{err}");
    assert!(c.0.borrow().is_empty(), "bd was never asked");
    crate::slow::set_caller("spira-lc");
    assert_eq!(over(&w, &c, &whole, None).0, 0, "any other caller is served");
}

/// A content write's guard variables ride the request to the server, which refuses the
/// czar's shadow-stage mutation with bdq's exit code and text, and writes nothing.
#[test_id(tw6xe3d)]
#[test]
fn a_czar_content_write_is_judged_on_the_callers_variables_on_the_server() {
    let (w, c) = (World::new(), Content::default());
    let caller: std::collections::BTreeMap<String, String> =
        [("SPIRA_RUNE", "czar"), ("SPIRA_CZAR_CLASS", "deadlock"), ("SPIRA_CZAR_TRIGGER_BEAD", "sp-t")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let argv: Vec<String> = ["content", "update", "sp-a", "--add-label", "x"].iter().map(|s| s.to_string()).collect();
    let line = spira_store::lc_call::request_head(&argv, None, &caller, None, None).unwrap();
    let mut wire = std::io::Cursor::new(format!("{line}\n").into_bytes());
    let (req, _) = super::read_request(&mut wire).unwrap().unwrap();
    assert_eq!(req.caller, caller, "the variables survive the wire");
    assert_eq!(req.stdin, None, "no len, no body");
    let (mut m, mut b) = (&w, &w);
    let ans = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
    assert_eq!((ans.code, ans.stderr.as_str()), (1, "czar-fence: deadlock is shadow — mutation refused (SPIRA_CZAR_STAGE_DEADLOCK=act to enable)\n"));
    assert!(c.0.borrow().is_empty(), "nothing was written");

    let mut acting = caller.clone();
    acting.insert("SPIRA_CZAR_STAGE_DEADLOCK".into(), "act".into());
    let req = super::Request { argv: argv.clone(), stdin: None, caller: acting, task: None, request_id: None };
    let ans = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
    assert_eq!(ans.code, 0, "{ans:?}");
    assert_eq!(*c.0.borrow(), ["update sp-a --add-label x"]);

    let line = spira_store::lc_call::request_head(&argv, Some(2), &caller, None, None).unwrap();
    let mut wire = std::io::Cursor::new(format!("{line}\nhi").into_bytes());
    assert_eq!(super::read_request(&mut wire).unwrap().unwrap().0.stdin.as_deref(), Some("hi"), "a body still rides with its len");
    let mut wire = std::io::Cursor::new(b"{\"argv\":[\"list\"]}\n".to_vec());
    assert!(super::read_request(&mut wire).is_err(), "an object with neither len nor caller is still refused");
}

#[test_id(ta6rw4g)]
#[test]
fn a_tasked_request_is_counted_under_its_task() {
    let w = World::new();
    let c = Content::default();
    for (head, task) in [(r#"{"argv":["tasked-probe","sp-a"],"task":"asks"}"#, "asks"), (r#"["tasked-probe","sp-a"]"#, "-")] {
        let mut wire = std::io::Cursor::new(format!("{head}\n").into_bytes());
        let (req, _) = super::read_request(&mut wire).unwrap().unwrap();
        assert_eq!(req.stdin, None, "a task needs no len");
        let task_seen = req.task.clone();
        let (mut m, mut b) = (&w, &w);
        let _ = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
        crate::slow::request_done("tasked-probe", "tasked-probe", "probe", task_seen.as_deref(), std::time::Duration::from_millis(5), 0);
        let s: serde_json::Value = serde_json::from_str(&crate::slow::stats_json()).unwrap();
        let row = s["callers"].as_array().unwrap().iter().find(|r| r["verb"] == "tasked-probe" && r["task"] == task).unwrap_or_else(|| panic!("no row for {task}: {s}"));
        assert_eq!((row["count"].as_u64(), row["total_ms"].as_u64()), (Some(1), Some(5)));
    }
}

#[test_id(ta1j0k2)]
#[test]
fn a_min_rev_key_is_ignored_and_the_reply_header_names_no_revision() {
    use std::io::Read;
    let read = |wire: &[u8]| super::read_request(&mut std::io::Cursor::new(wire.to_vec()));
    let shown = |wire: &[u8]| read(wire).map(|r| r.map(|r| r.0.argv)).map_err(|e| e.1);
    let (with, _) = read(b"{\"argv\":[\"show\",\"sp-a\"],\"len\":0,\"min_rev\":42}\n").unwrap().unwrap();
    let (without, _) = read(b"{\"argv\":[\"show\",\"sp-a\"],\"len\":0}\n").unwrap().unwrap();
    assert_eq!(format!("{with:?}"), format!("{without:?}"), "a min_rev is a key read_request does not know");
    assert_eq!(shown(b"{\"argv\":[\"show\"],\"min_rev\":42}\n"), shown(b"{\"argv\":[\"show\"]}\n"));
    assert_eq!(shown(b"{\"argv\":[\"show\"],\"min_rev\":\"x\"}\n"), shown(b"{\"argv\":[\"show\"]}\n"));

    let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
    super::write_response(&a, 0, "{}\n", "", false).unwrap();
    drop(a);
    let mut got = String::new();
    b.read_to_string(&mut got).unwrap();
    let head: serde_json::Value = serde_json::from_str(got.lines().next().unwrap()).unwrap();
    assert!(head.get("rev").is_none(), "{got}");
}
