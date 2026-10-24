use super::*;
use std::cell::RefCell;

fn host() -> Host {
    Host { name: "prod".into(), ssh: "u@h".into(), home: "/d".into() }
}

#[derive(Default)]
struct Fake {
    outbox: RefCell<BTreeMap<String, String>>,
    open: RefCell<BTreeSet<String>>,
    filed: RefCell<Vec<String>>,
    appended: RefCell<Vec<String>>,
    ack_fails: bool,
}

impl World for Fake {
    fn list(&self, _: &Host) -> Result<Vec<String>, String> {
        Ok(self.outbox.borrow().keys().cloned().collect())
    }
    fn read(&self, _: &Host, n: &str) -> Result<String, String> {
        Ok(self.outbox.borrow()[n].clone())
    }
    fn ack(&self, _: &Host, n: &str) -> Result<(), String> {
        if self.ack_fails {
            return Err("host went away".into());
        }
        self.outbox.borrow_mut().remove(n);
        Ok(())
    }
    fn is_open(&self, id: &str) -> Result<bool, String> {
        Ok(self.open.borrow().contains(id))
    }
    fn file(&self, f: &Filing, _: &str) -> Result<String, String> {
        let id = format!("sp-b{}", self.filed.borrow().len());
        self.filed.borrow_mut().push(f.title.clone());
        self.open.borrow_mut().insert(id.clone());
        Ok(id)
    }
    fn append(&self, id: &str, _: &str, _: &str) -> Result<(), String> {
        self.appended.borrow_mut().push(id.into());
        Ok(())
    }
}

fn js(action: &str) -> String {
    format!(r#"{{"class":"c","action":"{action}","repo":"r","title":"t","body":"b"}}"#)
}

fn go(w: &Fake, st: &mut State) -> Outcome {
    pass(w, &[host()], st, &mut |_| Ok(()))
}

#[test]
fn an_unacknowledged_filing_is_never_applied_twice() {
    let w = Fake { ack_fails: true, ..Fake::default() };
    w.outbox.borrow_mut().insert("a.json".into(), js("open"));
    let mut st = State::default();
    let first = go(&w, &mut st);
    assert_eq!(first.filed.len(), 1);
    assert!(first.failed[0].contains("prod/a.json") && first.failed[0].contains("not acknowledged"), "{first:?}");
    let second = go(&w, &mut st);
    assert_eq!((w.filed.borrow().len(), w.appended.borrow().len()), (1, 0), "{second:?}");
}

#[test]
fn a_closed_bead_is_not_appended_to_and_the_next_filing_opens_a_new_one() {
    let w = Fake::default();
    let mut st = State::default();
    w.outbox.borrow_mut().insert("a.json".into(), js("open"));
    go(&w, &mut st);
    w.open.borrow_mut().clear();
    w.outbox.borrow_mut().insert("b.json".into(), js("open"));
    let o = go(&w, &mut st);
    assert_eq!((o.filed.len(), o.amended.len()), (1, 0));
    assert_eq!(st.beads["prod/c"], "sp-b1");
}

#[test]
fn state_round_trips() {
    let mut s = State::default();
    s.beads.insert("prod/c".into(), "sp-x".into());
    s.applied.insert("prod/a.json".into());
    assert_eq!(State::parse(&s.render()), s);
}

#[test]
fn hosts_parse_and_refuse_malformed_entries() {
    assert_eq!(parse_hosts("prod=u@h:/d").unwrap(), vec![host()]);
    assert!(parse_hosts("").unwrap().is_empty());
    for bad in ["prod", "prod=u@h", "prod=u@h:rel", "prod=-oProxy:/d", "a=u@h:/d a=u@h:/e"] {
        assert!(parse_hosts(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_filing_missing_a_field_or_naming_an_unknown_action_is_refused() {
    assert!(parse_filing(&js("open")).is_ok());
    assert!(parse_filing(&js("close")).is_err());
    assert!(parse_filing(r#"{"class":"c","action":"open","repo":"r","title":"t"}"#).is_err());
}
