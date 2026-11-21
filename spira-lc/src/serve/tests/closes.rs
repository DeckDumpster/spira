use super::*;

#[test]
fn the_reply_is_on_the_stream_before_an_owed_store_close_finishes() {
    use std::io::Read;
    let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let (started, started_rx) = std::sync::mpsc::channel::<()>();
    let settler = std::thread::spawn(move || {
        let ans = super::Answer { owed: vec![("sp-d".into(), "why".into())].into(), ..Default::default() };
        super::reply_then_settle(&server, &ans, false, || {}, &mut |id, reason| {
            assert_eq!((id, reason), ("sp-d", "why"));
            let _ = started.send(());
            let _ = release_rx.recv();
            Ok(())
        });
    });
    started_rx.recv().unwrap();
    let mut head = [0u8; 1];
    client.read_exact(&mut head).unwrap();
    assert_eq!(head[0], b'{', "the reply was written while the settle still blocks");
    drop(release);
    settler.join().unwrap();
}

#[test]
fn close_reads_its_reason_from_the_request_body_and_writes_bd_on_the_server() {
    let (w, c) = (World::new(), Content::default());
    filed(&w, "sp-a");
    let (code, _, err) = over(&w, &c, &["close", "sp-a", "--reason-file", "-", "--dropped", "--actor", "queue"], None);
    assert_eq!(code, 2, "no body sent: {err}");
    assert!(err.contains("--reason-file: stdin: nothing was sent"), "{err}");
    assert!(w.writes.borrow().is_empty());
    let (code, _, err) = over(&w, &c, &["close", "sp-a", "--reason-file", "-", "--dropped", "--actor", "queue"], Some("not needed after all\n"));
    assert_eq!(code, 0, "{err}");
    assert_eq!(*w.writes.borrow(), ["close sp-a"], "the server wrote bd");
    assert!(w.lc(&["show", "sp-a"]).1.contains("DROPPED"));
}

#[test]
fn close_ask_closes_the_ask_and_settles_its_decision_bead() {
    let (w, c) = (World::new(), Content::default());
    w.file("sp-k", "task", None, &[]);
    w.ok(&["create-ask", "sp-k"]);
    let (code, out, err) = over(&w, &c, &["close-ask", "sp-k", "--exit", "answered", "--quote", "yes", "--actor", "operator", "--channel", "mail"], None);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(*w.writes.borrow(), ["close sp-k"], "the decision bead closed with it");
}

fn settle_pass(settler: &crate::settle::Settler, w: &World) {
    settler.pass(&mut &*w, &mut &*w, &mut || Ok(1));
}

#[test]
fn a_delivered_close_is_owed_to_the_settler_and_not_forked_in_the_request() {
    let (w, c) = (World::new(), Content::default());
    filed(&w, "sp-d");
    let settler = crate::settle::install_for_test();
    let (code, _, err) = over(&w, &c, &["close", "sp-d", "--reason", "shipped", "--delivered", "--actor", "concierge"], None);
    assert_eq!(code, 0, "{err}");
    assert!(w.writes.borrow().is_empty(), "bd was not written on the request's thread");
    settle_pass(&settler, &w);
    assert_eq!(*w.writes.borrow(), ["close sp-d"]);
}

#[test]
fn a_reopen_that_races_an_owed_close_wins() {
    let w = World::new();
    filed(&w, "sp-d");
    let settler = crate::settle::install_for_test();
    assert!(crate::settle::owe_closes(&[("sp-d".to_string(), "shipped".to_string())]));
    assert!(settler.owes());
    settle_pass(&settler, &w);
    assert!(!w.writes.borrow().iter().any(|x| x.starts_with("close")), "{:?}", w.writes.borrow());
    assert!(!settler.owes());
}

#[test]
fn an_owed_close_of_a_bead_nothing_records_is_dropped() {
    let w = World::new();
    let settler = crate::settle::install_for_test();
    assert!(crate::settle::owe_closes(&[("sp-none".to_string(), "r".to_string())]));
    settle_pass(&settler, &w);
    assert!(w.writes.borrow().is_empty());
    assert!(!settler.owes());
}

#[test]
fn an_ask_close_owes_its_decision_beads_close_to_the_settler() {
    let (w, c) = (World::new(), Content::default());
    w.file("sp-q", "task", None, &[]);
    w.ok(&["create-ask", "sp-q"]);
    let settler = crate::settle::install_for_test();
    let (code, out, err) = over(&w, &c, &["close", "sp-q", "--reason", "decided"], None);
    assert_eq!(code, 0, "{out}{err}");
    assert!(w.writes.borrow().is_empty(), "{:?}", w.writes.borrow());
    settle_pass(&settler, &w);
    assert_eq!(*w.writes.borrow(), ["close sp-q"]);
}

#[test]
fn a_failing_store_close_is_retried_three_passes_then_given_up() {
    struct Fails<'w>(&'w World, std::cell::Cell<u32>);
    impl crate::callers::Bd for Fails<'_> {
        fn issue_type(&mut self, id: &str) -> Result<String, String> {
            let mut w = self.0;
            w.issue_type(id)
        }
        fn close(&mut self, _: &str, _: &str) -> Result<(), String> {
            self.1.set(self.1.get() + 1);
            Err("bd is down".into())
        }
        fn closed(&mut self, ids: &[String]) -> Result<Vec<(String, String)>, String> {
            let mut w = self.0;
            w.closed(ids)
        }
        fn known(&mut self, ids: &[String]) -> Result<Vec<String>, String> {
            let mut w = self.0;
            w.known(ids)
        }
        fn open_among(&mut self, ids: &[String]) -> Result<Vec<String>, String> {
            let mut w = self.0;
            w.open_among(ids)
        }
        fn reopen(&mut self, id: &str) -> Result<(), String> {
            let mut w = self.0;
            w.reopen(id)
        }
        fn work_beads(&mut self, id: &str) -> Result<Vec<String>, String> {
            let mut w = self.0;
            w.work_beads(id)
        }
        fn external(&mut self, id: &str) -> Result<bool, String> {
            let mut w = self.0;
            w.external(id)
        }
        fn blocking_dependents(&mut self, id: &str) -> Result<Vec<String>, String> {
            let mut w = self.0;
            w.blocking_dependents(id)
        }
        fn planning_watermark(&mut self) -> Result<Option<u64>, String> {
            let mut w = self.0;
            w.planning_watermark()
        }
    }
    let (w, c) = (World::new(), Content::default());
    filed(&w, "sp-d");
    let settler = crate::settle::install_for_test();
    assert_eq!(over(&w, &c, &["close", "sp-d", "--reason", "shipped", "--delivered"], None).0, 0);
    let mut bd = Fails(&w, Default::default());
    for pass in 1..=3 {
        assert!(settler.owes(), "still owed before pass {pass}");
        settler.pass(&mut &w, &mut bd, &mut || Ok(1));
    }
    assert_eq!(bd.1.get(), 3);
    assert!(!settler.owes(), "given up to reconcile-content");
}

#[test]
fn closes_that_owe_nothing_stay_in_the_request() {
    let (w, c) = (World::new(), Content::default());
    filed(&w, "sp-m");
    w.ok(&["event", "bead", "sp-m", "--expect", "READY", "--version", "0", "--actor", "t", "--kind", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#]);
    let settler = crate::settle::install_for_test();
    let (code, _, _) = over(&w, &c, &["close", "sp-m", "--reason", "r", "--actor", "mail"], None);
    assert_eq!(code, 3);
    assert!(!settler.owes());
    w.file("sp-alert", "task", None, &[]);
    assert_eq!(over(&w, &c, &["close", "sp-alert", "--reason", "r"], None).0, 0);
    assert_eq!(*w.writes.borrow(), ["close sp-alert"], "an unowned bead closes in the request");
    assert!(!settler.owes());
}
