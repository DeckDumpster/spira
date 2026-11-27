use super::*;

#[test_id(tmy5go1)]
#[test]
fn a_label_or_note_write_is_one_mutation_and_forks_no_bd() {
    let r = rig(false, three_seconds());
    send(&r.door, &["content", "update", "sp-a", "--remove-label", "branch:spira/sp-a"], None);
    send(&r.door, &["content", "label", "add", "sp-a", "overseer"], None);
    assert!(r.notes.writes().is_empty(), "{:?}", r.notes.writes());
    assert_eq!(r.notes.mutations(), ["update sp-a --remove-label branch:spira/sp-a", "label add sp-a overseer"]);
}

#[test_id(tvd4k0k)]
#[test]
fn a_stdin_note_is_one_mutation_carrying_its_body() {
    let r = rig(false, three_seconds());
    let ans = send_body(&r.door, &["content", "note", "sp-a", "--stdin"], Some("the body"), None);
    assert!(r.notes.writes().is_empty(), "{ans:?}");
    assert_eq!(r.notes.mutations(), ["note sp-a --stdin <<the body"]);
}

#[test_id(tsueqwc)]
#[test]
fn the_state_and_key_value_writes_are_mutations_and_fork_no_bd() {
    let r = rig(false, three_seconds());
    send(&r.door, &["content", "set-state", "sp-a", "branch=spira/sp-a"], None);
    send(&r.door, &["content", "kv", "set", "k", "v"], None);
    assert!(r.notes.writes().is_empty(), "{:?}", r.notes.writes());
    assert_eq!(r.notes.mutations(), ["set-state sp-a branch=spira/sp-a", "kv set k v"]);
}

/// `argv` (after `content`) over the socket: the answer, and what the server wrote to bd.
fn content_write(argv: &[&str], stdin: Option<&str>) -> ((i32, String, String), Vec<String>) {
    let (w, c) = (World::new(), Content::default());
    let full: Vec<&str> = std::iter::once("content").chain(argv.iter().copied()).collect();
    let ans = over(&w, &c, &full, stdin);
    let wrote = c.0.borrow().clone();
    (ans, wrote)
}

fn written(argv: &[&str], stdin: Option<&str>, want: &str) {
    let ((code, out, err), wrote) = content_write(argv, stdin);
    assert_eq!(code, 0, "{argv:?}: {err}");
    assert_eq!(out, format!("wrote {}", argv.join(" ")), "bd's stdout is the answer");
    assert_eq!(wrote, [want], "the server wrote bd once, with bd's own argv");
}

#[test_id(tfhos8f)]
#[test]
fn content_label_add_and_remove_are_written_by_the_server() {
    written(&["label", "add", "sp-a", "spira:x"], None, "label add sp-a spira:x");
    written(&["label", "remove", "sp-a", "spira:x"], None, "label remove sp-a spira:x");
    written(&["update", "sp-a", "--add-label", "x", "--remove-label", "y"], None, "update sp-a --add-label x --remove-label y");
}

/// A close is a state verb: refused unless the caller overrides, and an overridden close
/// leaves the row terminal.
#[test_id(tp0uuco)]
#[test]
fn a_content_close_is_refused_unless_overridden_and_then_its_row_is_terminal() {
    let w = World::new();
    filed(&w, "sp-a");
    let f = Filing(&w, "sp-a");
    let ans = served(&w, &f, &["close", "sp-a", "--reason", "landed"], &[]);
    assert_eq!(ans.code, 1);
    assert!(ans.stderr.starts_with("spira: bdq refuses `close`"), "{ans:?}");
    assert_eq!(w.status("sp-a"), "open", "nothing written");
    let ans = served(&w, &f, &["close", "sp-a", "--reason", "landed"], &[("SPIRA_BDQ_STATE_WRITE", "test")]);
    assert_eq!(ans.code, 0, "{ans:?}");
    assert_eq!(ans.stderr, "spira-lc content: STATE WRITE OVERRIDE (close sp-a --reason landed) — bd's status moved outside the lifecycle machine: test\n", "the override is logged");
    assert_eq!(w.status("sp-a"), "closed");
    assert!(state(&w, "sp-a").is_some_and(|r| r.terminal()), "the row follows the close: {:?}", state(&w, "sp-a"));
}

#[test_id(tgi7lqu)]
#[test]
fn content_note_is_written_by_the_server_with_its_text_or_the_request_body() {
    written(&["note", "sp-a", "a note"], None, "note sp-a a note");
    written(&["note", "sp-a", "--stdin"], Some("line one\nline two\n"), "note sp-a --stdin <<line one\nline two\n");
    let ((code, _, err), wrote) = content_write(&["note", "sp-a", "--stdin"], None);
    assert_eq!((code, err.as_str()), (2, "spira-lc content: stdin: nothing was sent\n"), "no body, no write");
    assert!(wrote.is_empty());
}

#[test_id(ttv60py)]
#[test]
fn content_update_set_metadata_is_written_by_the_server() {
    written(&["update", "sp-a", "--set-metadata", "claimdesc_hash=abc", "--set-metadata", "n=1"], None, "update sp-a --set-metadata claimdesc_hash=abc --set-metadata n=1");
}

#[test_id(t4xek3b)]
#[test]
fn content_create_is_written_by_the_server_with_the_request_body() {
    written(&["create", "T", "--type", "task", "--label", "spira", "--silent", "--priority", "3", "--body-file", "-"], Some("## Outcome\n"), "create T --type task --label spira --silent --priority 3 --body-file - <<## Outcome\n");
    written(&["create", "--parent", "sp-e", "--silent", "--title", "child"], None, "create --parent sp-e --silent --title child");
}

#[test_id(tgs25os)]
#[test]
fn content_sub_verbs_not_served_still_refuse_with_their_text_and_write_nothing() {
    for (argv, why) in [
        (&["delete", "sp-a"][..], "`delete sp-a` is not a content verb"),
        (&["reopen", "sp-a"], "`reopen sp-a` is not a content verb"),
        (&["set-state", "sp-a", "rune=x"], "`set-state sp-a rune=x` is not a content verb"),
        (&["dep", "add", "sp-a"], "`dep add sp-a` is not a content verb"),
        (&["close", "sp-a", "--reason", "r", "--force"], "`close sp-a --reason r --force` is not a content verb"),
        (&["update", "sp-a", "--status", "open"], "update takes only --add-label/--remove-label <label>, --set-metadata <key=value>, --priority 0-4 and --force-claimed <reason> with a body, not `--status`"),
        (&["create", "T", "--body-file", system_paths::ETC_PASSWD], "create takes its body only as --body-file - (the request body)"),
    ] {
        let ((code, out, err), wrote) = content_write(argv, None);
        assert_eq!((code, out.as_str(), err), (3, "", format!("spira-lc content: {why}\n")), "{argv:?}");
        assert!(wrote.is_empty(), "{argv:?} wrote {wrote:?}");
    }
}

#[test_id(t22pime)]
#[test]
fn content_writes_keep_the_configured_default_under_the_clients_five() {
    for argv in [&["label", "add", "sp-a", "x"][..], &["close", "sp-a"], &["note", "sp-a", "--stdin"], &["update", "sp-a", "--set-metadata", "k=v"], &["create", "T"]] {
        let full: Vec<String> = std::iter::once("content").chain(argv.iter().copied()).map(str::to_string).collect();
        assert_eq!(dl(&full), test_deadlines().default, "{argv:?}");
        assert!(dl(&full) < crate::client::TIMEOUT, "{argv:?}");
    }
    assert_eq!(crate::client::TIMEOUT, std::time::Duration::from_secs(5));
}

/// A primitive's stdin body is carried inline, exactly as the CLI carries its own stdin.
#[test_id(tqsq45y)]
#[test]
fn a_primitives_body_is_inlined_as_its_json() {
    struct Rec(Vec<Vec<String>>);
    impl Machine for Rec {
        fn call(&mut self, args: &[String]) -> (i32, String) {
            self.0.push(args.to_vec());
            (0, String::new())
        }
    }
    let w = World::new();
    let (mut rec, mut bd) = (Rec(Vec::new()), &w);
    let req = super::Request { argv: vec!["batch-progress".into()], stdin: Some("[{\"bead_id\":\"sp-a\"}]".into()), caller: Default::default(), task: None, request_id: None };
    super::answer(req, &mut rec, &mut bd, &Content::default(), &|| Err(String::new()));
    assert_eq!(rec.0, vec![vec!["batch-progress".to_string(), "--json".into(), "[{\"bead_id\":\"sp-a\"}]".into()]]);
}

#[test_id(tiu5tsd)]
#[test]
fn a_work_requests_body_becomes_its_stdin_trailer_before_actor() {
    struct Rec(Vec<Vec<String>>);
    impl Machine for Rec {
        fn call(&mut self, args: &[String]) -> (i32, String) {
            self.0.push(args.to_vec());
            (0, String::new())
        }
    }
    let w = World::new();
    let (mut rec, mut bd) = (Rec(Vec::new()), &w);
    let a = |v: &[&str], body: Option<&str>| super::Request {
        argv: v.iter().map(|s| s.to_string()).collect(),
        stdin: body.map(String::from),
        caller: Default::default(),
        task: None,
        request_id: None,
    };
    let mut run = |req| {
        super::answer(req, &mut rec, &mut bd, &Content::default(), &|| Err(String::new()));
    };
    run(a(&["work", "sp-a", "note", "-", "--actor", "sift"], Some("evidence\nline two")));
    run(a(&["work", "sp-a", "note", "-"], Some("evidence\nline two")));
    run(a(&["work", "sp-a", "note", "-"], None));
    run(a(&["work", "-", "file", "t", "--body-file", "-", "--actor", "ops"], Some("b")));
    run(a(&["work", "sp-a", "note", "-", "--stdin", "x"], Some("y")));
    let sv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(rec.0[0], sv(&["work", "sp-a", "note", "-", "--stdin", "evidence\nline two", "--actor", "sift"]));
    let c = crate::work::split_reserved(&rec.0[0][2..]);
    assert_eq!((c.stdin.as_deref(), c.actor.as_deref()), (Some("evidence\nline two"), Some("sift")));
    assert_eq!(rec.0[1], sv(&["work", "sp-a", "note", "-", "--stdin", "evidence\nline two"]));
    assert_eq!(rec.0[2], sv(&["work", "sp-a", "note", "-"]));
    assert_eq!(rec.0[3], sv(&["work", "-", "file", "t", "--body-file", "-", "--stdin", "b", "--actor", "ops"]));
    assert_eq!(rec.0[4], sv(&["work", "sp-a", "note", "-", "--stdin", "x"]));
}
