use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use super::*;

#[derive(Default)]
struct Fake {
    base: Option<String>,
    conflicts: BTreeMap<String, Vec<String>>,
    patch: BTreeMap<String, String>,
    stacked: BTreeMap<String, Vec<String>>,
    states: BTreeMap<String, String>,
    broken: Vec<String>,
    merges: Cell<u32>,
}

impl Probe for Fake {
    fn base(&self) -> Result<String, String> {
        self.base.clone().ok_or_else(|| "no base".to_string())
    }
    fn merge(&self, tip: &str) -> Result<Merge, String> {
        self.merges.set(self.merges.get() + 1);
        if self.broken.iter().any(|b| b == tip) {
            return Err("git exploded".into());
        }
        Ok(match self.conflicts.get(tip) {
            Some(f) => Merge::Conflict(f.clone()),
            None => Merge::Clean(format!("merged-{tip}")),
        })
    }
    fn patch_id(&self, tip: &str) -> Result<String, String> {
        Ok(self.patch.get(tip).cloned().unwrap_or_else(|| format!("p-{tip}")))
    }
    fn stacked_on(&self, id: &str, _: &str) -> Result<Vec<String>, String> {
        Ok(self.stacked.get(id).cloned().unwrap_or_default())
    }
    fn state(&self, id: &str) -> Result<String, String> {
        self.states.get(id).cloned().ok_or_else(|| "unknown".to_string())
    }
}

#[derive(Default)]
struct Mem {
    verdicts: RefCell<BTreeMap<(String, String), Verdict>>,
    counts: RefCell<BTreeMap<String, u32>>,
    reported: RefCell<Vec<(String, String)>>,
}

impl Store for Mem {
    fn get(&self, id: &str, tip: &str) -> Option<Verdict> {
        self.verdicts.borrow().get(&(id.to_string(), tip.to_string())).cloned()
    }
    fn put(&self, v: &Verdict) -> Result<(), String> {
        self.verdicts.borrow_mut().insert((v.id.clone(), v.tip.clone()), v.clone());
        Ok(())
    }
    fn send_backs(&self, id: &str) -> u32 {
        self.counts.borrow().get(id).copied().unwrap_or(0)
    }
    fn record_screened(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn record_send_back(&self, id: &str, _: &str, _: &str) -> Result<u32, String> {
        let mut c = self.counts.borrow_mut();
        let n = c.entry(id.to_string()).or_insert(0);
        *n += 1;
        Ok(*n)
    }
    fn first_cap_report(&self, id: &str, tip: &str) -> bool {
        let k = (id.to_string(), tip.to_string());
        let seen = self.reported.borrow().contains(&k);
        self.reported.borrow_mut().push(k);
        !seen
    }
}

#[derive(Default)]
struct Rec {
    log: Vec<String>,
    fail_note: bool,
}

impl Acts for Rec {
    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        if self.fail_note {
            return Err("bead.sh down".into());
        }
        self.log.push(format!("note {id}: {text}"));
        Ok(())
    }
    fn gate_red(&mut self, id: &str, tip: &str, reason: &str) -> Result<(), String> {
        self.log.push(format!("red {id} {tip} {reason}"));
        Ok(())
    }
    fn supersede(&mut self, id: &str, keeper: &str) -> Result<(), String> {
        self.log.push(format!("supersede {id} by {keeper}"));
        Ok(())
    }
    fn pass(&mut self, id: &str, tip: &str) -> Result<(), String> {
        self.log.push(format!("pass {id} {tip}"));
        Ok(())
    }
    fn tell(&mut self, msg: &str) {
        self.log.push(format!("tell {msg}"));
    }
}

fn c(id: &str) -> Candidate {
    Candidate { id: id.into(), tip: format!("t-{id}") }
}

fn fake() -> Fake {
    Fake { base: Some("b1".into()), ..Default::default() }
}

fn reds(r: &Rec) -> Vec<&String> {
    r.log.iter().filter(|l| l.starts_with("red ")).collect()
}

#[test]
fn a_conflict_is_sent_back_as_no_rebase_and_a_clean_candidate_passes() {
    let mut f = fake();
    f.conflicts.insert("t-a".into(), vec!["x.rs".into()]);
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &[c("a"), c("b")], &[]);
    assert_eq!((o.pass.as_slice(), o.sent_back.as_slice()), (&["b".to_string()][..], &["a".to_string()][..]));
    assert_eq!(reds(&r), ["red a t-a no-rebase"]);
}

#[test]
fn the_evidence_note_is_written_before_the_gate_red() {
    let mut f = fake();
    f.conflicts.insert("t-a".into(), vec!["x.rs".into()]);
    let mut r = Rec::default();
    screen(&f, &Mem::default(), &mut r, &[c("a")], &[]);
    let note = r.log.iter().position(|l| l.starts_with("note a: ") && l.contains("x.rs")).unwrap();
    let red = r.log.iter().position(|l| l.starts_with("red a")).unwrap();
    assert!(note < red);
}

#[test]
fn no_note_means_no_send_back_and_the_candidate_stays_out_of_the_cut() {
    let mut f = fake();
    f.conflicts.insert("t-a".into(), vec!["x.rs".into()]);
    let mut r = Rec { fail_note: true, ..Default::default() };
    let s = Mem::default();
    let o = screen(&f, &s, &mut r, &[c("a")], &[]);
    assert!(reds(&r).is_empty());
    assert_eq!((o.held, o.pass), (vec!["a".to_string()], vec![]));
    assert_eq!(s.send_backs("a"), 0);
}

#[test]
fn a_duplicate_is_superseded_by_the_smallest_id_and_an_open_round_member_keeps() {
    let mut f = fake();
    f.patch.insert("t-a".into(), "same".into());
    f.patch.insert("t-b".into(), "same".into());
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &[c("a"), c("b")], &[]);
    assert_eq!((o.pass, o.superseded), (vec!["a".to_string()], vec!["b".to_string()]));
    assert!(r.log.contains(&"supersede b by a".to_string()));

    let mut f = fake();
    f.patch.insert("t-a".into(), "same".into());
    f.patch.insert("t-z".into(), "same".into());
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &[c("a")], &[c("z")]);
    assert_eq!(o.superseded, ["a"]);
    assert!(r.log.contains(&"supersede a by z".to_string()));
}

#[test]
fn stacked_on_a_bead_in_rework_is_sent_back_naming_it_but_a_live_prerequisite_is_not() {
    let mut f = fake();
    f.stacked.insert("a".into(), vec!["sp-x".into()]);
    f.stacked.insert("b".into(), vec!["sp-y".into()]);
    f.states.insert("sp-x".into(), "REWORK".into());
    f.states.insert("sp-y".into(), "SUBMITTED".into());
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &[c("a"), c("b")], &[]);
    assert_eq!((o.pass, o.sent_back), (vec!["b".to_string()], vec!["a".to_string()]));
    assert!(r.log.iter().any(|l| l.starts_with("note a:") && l.contains("sp-x")));
}

#[test]
fn a_verdict_is_cached_by_tip_until_the_base_moves() {
    let f = fake();
    let s = Mem::default();
    let mut r = Rec::default();
    screen(&f, &s, &mut r, &[c("a")], &[]);
    screen(&f, &s, &mut r, &[c("a")], &[]);
    assert_eq!(f.merges.get(), 1);
    let moved = Fake { base: Some("b2".into()), ..fake() };
    screen(&moved, &s, &mut r, &[c("a")], &[]);
    assert_eq!(moved.merges.get(), 1);
}

#[test]
fn a_cached_stacked_verdict_is_judged_against_the_prerequisites_current_state() {
    let mut f = fake();
    f.stacked.insert("a".into(), vec!["sp-x".into()]);
    f.states.insert("sp-x".into(), "SUBMITTED".into());
    let s = Mem::default();
    let mut r = Rec::default();
    assert_eq!(screen(&f, &s, &mut r, &[c("a")], &[]).pass, ["a"]);
    f.states.insert("sp-x".into(), "REWORK".into());
    assert_eq!(screen(&f, &s, &mut r, &[c("a")], &[]).sent_back, ["a"]);
    assert_eq!(f.merges.get(), 1);
}

#[test]
fn after_three_send_backs_the_bead_is_excluded_and_the_concierge_told_once() {
    let mut f = fake();
    f.conflicts.insert("t-a".into(), vec!["x.rs".into()]);
    let s = Mem::default();
    s.counts.borrow_mut().insert("a".into(), SEND_BACK_CAP);
    let mut r = Rec::default();
    let o = screen(&f, &s, &mut r, &[c("a")], &[]);
    assert_eq!((o.capped, o.pass), (vec!["a".to_string()], vec![]));
    assert!(reds(&r).is_empty());
    screen(&f, &s, &mut r, &[c("a")], &[]);
    assert_eq!(r.log.iter().filter(|l| l.starts_with("tell SIFT REPEAT a")).count(), 1);
}

#[test]
fn each_send_back_counts_toward_the_cap() {
    let mut f = fake();
    f.conflicts.insert("t-a".into(), vec!["x.rs".into()]);
    let s = Mem::default();
    let mut r = Rec::default();
    screen(&f, &s, &mut r, &[c("a")], &[]);
    assert_eq!(s.send_backs("a"), 1);
}

#[test]
fn a_screen_that_cannot_run_cuts_the_whole_pool_unfiltered_and_reports_it() {
    let f = Fake::default();
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &[c("a"), c("b")], &[]);
    assert_eq!(o.pass, ["a", "b"]);
    assert!(o.errors[0].contains("unfiltered"));
}

#[test]
fn a_candidate_the_screen_errors_on_passes_unscreened_and_the_error_is_reported() {
    let mut f = fake();
    f.broken.push("t-a".into());
    f.conflicts.insert("t-b".into(), vec!["y".into()]);
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &[c("a"), c("b")], &[]);
    assert_eq!((o.pass, o.sent_back), (vec!["a".to_string()], vec!["b".to_string()]));
    assert!(o.errors[0].contains("a screening failed"));
}

#[test]
fn the_fixture_round_excludes_the_conflict_supersedes_the_duplicate_and_cuts_the_clean_one() {
    let mut f = fake();
    f.conflicts.insert("t-conflict".into(), vec!["x.rs".into()]);
    f.patch.insert("t-dup".into(), "same".into());
    f.patch.insert("t-alpha".into(), "same".into());
    let pool = [c("alpha"), c("clean"), c("conflict"), c("dup")];
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &pool, &[]);
    let cut = filter(pool.to_vec(), |m| m.id.as_str(), &o);
    assert_eq!(cut.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["alpha", "clean"]);
    assert_eq!((o.sent_back, o.superseded), (vec!["conflict".to_string()], vec!["dup".to_string()]));
}

#[test]
fn stacked_ids_come_from_land_subjects_and_bare_ids_but_never_the_beads_own() {
    assert_eq!(parse_stacked_id("spira: land sp-abc12 — thing", "sp-me"), Some("sp-abc12".into()));
    assert_eq!(parse_stacked_id("sp-abc.1: thing", "sp-me"), Some("sp-abc.1".into()));
    assert_eq!(parse_stacked_id("sp-me: thing", "sp-me"), None);
    assert_eq!(parse_stacked_id("fix the thing", "sp-me"), None);
    assert_eq!(parse_stacked_id("sp-abc", "sp-me"), None);
}

fn passes(r: &Rec) -> Vec<&String> {
    r.log.iter().filter(|l| l.starts_with("pass ")).collect()
}

#[test]
fn a_clean_candidate_has_its_pass_recorded_at_its_tip_and_a_red_one_has_none() {
    let mut f = fake();
    f.conflicts.insert("t-a".into(), vec!["x.rs".into()]);
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &[c("a"), c("b")], &[]);
    assert_eq!(passes(&r), ["pass b t-b"]);
    assert_eq!(o.sifted, ["b"]);
}

#[test]
fn a_candidate_the_screen_could_not_judge_records_no_pass() {
    let mut f = fake();
    f.broken.push("t-a".into());
    let mut r = Rec::default();
    let o = screen(&f, &Mem::default(), &mut r, &[c("a")], &[]);
    assert!(passes(&r).is_empty() && o.sifted.is_empty(), "{:?}", r.log);
    let mut none = fake();
    none.base = None;
    let mut r = Rec::default();
    let o = screen(&none, &Mem::default(), &mut r, &[c("a")], &[]);
    assert!(passes(&r).is_empty() && o.sifted.is_empty());
}

#[test]
fn a_moved_tip_is_screened_again_and_judged_on_its_own() {
    let f = fake();
    let store = Mem::default();
    let mut r = Rec::default();
    screen(&f, &store, &mut r, &[Candidate { id: "a".into(), tip: "t1".into() }], &[]);
    let after_first = f.merges.get();
    screen(&f, &store, &mut r, &[Candidate { id: "a".into(), tip: "t1".into() }], &[]);
    assert_eq!(f.merges.get(), after_first, "the same tip on the same base is not screened twice");
    screen(&f, &store, &mut r, &[Candidate { id: "a".into(), tip: "t2".into() }], &[]);
    assert_eq!(f.merges.get(), after_first + 1);
    assert_eq!(passes(&r), ["pass a t1", "pass a t1", "pass a t2"]);
}

#[test]
fn a_pass_the_store_refuses_is_not_a_sifted_candidate() {
    struct Refusing(Rec);
    impl Acts for Refusing {
        fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
            self.0.note(id, text)
        }
        fn gate_red(&mut self, id: &str, tip: &str, reason: &str) -> Result<(), String> {
            self.0.gate_red(id, tip, reason)
        }
        fn supersede(&mut self, id: &str, keeper: &str) -> Result<(), String> {
            self.0.supersede(id, keeper)
        }
        fn pass(&mut self, _: &str, _: &str) -> Result<(), String> {
            Err("tip moved".into())
        }
        fn tell(&mut self, msg: &str) {
            self.0.tell(msg)
        }
    }
    let mut acts = Refusing(Rec::default());
    let o = screen(&fake(), &Mem::default(), &mut acts, &[c("a")], &[]);
    assert!(o.sifted.is_empty() && o.errors.iter().any(|e| e.contains("stays unscreened")));
}

mod machine_tests {
    use crate::machine::{SiftEvent, SiftState, Sifted};
    use crate::{FileStore, Store, SEND_BACK_CAP};
    use testkit::TempDir;

    fn screened() -> SiftEvent {
        SiftEvent::Screened { tip: "t1".into() }
    }
    fn back(n: u32) -> SiftEvent {
        SiftEvent::SentBack { n, tip: "t1".into(), reason: "no-rebase".into() }
    }
    fn capped() -> SiftEvent {
        SiftEvent::Capped { tip: "t1".into() }
    }

    fn at(events: &[SiftEvent]) -> Sifted {
        let mut s = Sifted::new("sp-a");
        for (i, e) in events.iter().enumerate() {
            s.record(e.clone(), i as u64 + 1).unwrap();
        }
        s
    }

    #[test]
    fn every_legal_move_applies_and_is_recorded() {
        let mut s = Sifted::new("sp-a");
        assert_eq!(s.state(), None);
        assert!(s.record(screened(), 1).unwrap());
        assert_eq!(s.state(), Some(SiftState::Screened));
        for n in 1..=SEND_BACK_CAP {
            assert!(s.record(back(n), 10 + n as u64).unwrap());
            assert_eq!(s.state(), Some(SiftState::SentBack(n)));
        }
        assert!(s.record(screened(), 20).unwrap(), "a fixed resubmit screens again");
        assert_eq!(s.state(), Some(SiftState::Screened));
        assert_eq!(s.send_backs(), SEND_BACK_CAP);
        assert!(s.record(capped(), 21).unwrap());
        assert_eq!(s.state(), Some(SiftState::Capped));
        assert_eq!(s.events.len(), 1 + SEND_BACK_CAP as usize + 2);
    }

    #[test]
    fn an_illegal_move_is_refused_naming_the_state_and_writes_nothing() {
        let cases: Vec<(Vec<SiftEvent>, SiftEvent, &str)> = vec![
            (vec![], back(2), "unrecorded"),
            (vec![], capped(), "unrecorded"),
            (vec![screened()], back(3), "screened"),
            (vec![screened()], capped(), "screened"),
            (vec![back(1)], back(1), "sent_back(1)"),
            (vec![back(1)], capped(), "sent_back(1)"),
            (vec![back(1), back(2), back(3)], back(4), "sent_back(3)"),
            (vec![back(1), back(2), back(3), capped()], back(4), "capped"),
        ];
        for (history, event, state) in cases {
            let mut s = at(&history);
            let before = s.clone();
            let e = s.record(event.clone(), 99).unwrap_err();
            assert!(e.contains(state), "{event:?}: {e}");
            assert_eq!(s, before);
        }
    }

    #[test]
    fn the_same_event_twice_is_not_a_move() {
        let mut s = at(&[screened()]);
        assert!(!s.record(screened(), 5).unwrap());
        assert_eq!(s.events.len(), 1);
    }

    #[test]
    fn the_read_returns_exactly_the_recorded_state() {
        let s = at(&[screened(), back(1), back(2)]);
        let v = s.status_json();
        assert_eq!(v["state"], "sent_back(2)");
        assert_eq!(v["send_backs"], 2);
        assert_eq!(v["events"].as_array().unwrap().len(), 3);
        assert_eq!(Sifted::from_json(&s.to_json().to_string()).unwrap(), s);
        assert_eq!(Sifted::new("sp-b").status_json()["state"], "unrecorded");
    }

    #[test]
    fn the_file_store_backs_the_screen_with_events_and_counts_from_them() {
        let t = TempDir::new("sift-events");
        let store = FileStore::new(t.path().to_path_buf());
        assert_eq!(store.send_backs("sp-a"), 0);
        store.record_screened("sp-a", "t1").unwrap();
        assert_eq!(store.record_send_back("sp-a", "t1", "no-rebase").unwrap(), 1);
        assert_eq!(store.record_send_back("sp-a", "t2", "no-rebase").unwrap(), 2);
        assert_eq!(store.send_backs("sp-a"), 2);
        assert_eq!(FileStore::new(t.path().to_path_buf()).load("sp-a").unwrap().state(), Some(SiftState::SentBack(2)));
        assert!(!store.first_cap_report("sp-a", "t2"), "a bead short of the cap cannot be capped");
        store.record_send_back("sp-a", "t3", "no-rebase").unwrap();
        assert!(store.first_cap_report("sp-a", "t3"));
        assert!(!store.first_cap_report("sp-a", "t3"), "the same tip is reported once");
        assert!(store.first_cap_report("sp-a", "t4"), "a new tip is reported again");
        assert_eq!(store.all().len(), 1);
    }

    #[test]
    fn a_bead_counted_before_events_existed_keeps_its_count() {
        let t = TempDir::new("sift-legacy");
        std::fs::create_dir_all(t.path().join("send-backs")).unwrap();
        std::fs::write(t.path().join("send-backs/sp-old"), "2").unwrap();
        let store = FileStore::new(t.path().to_path_buf());
        assert_eq!(store.send_backs("sp-old"), 2);
        assert_eq!(store.record_send_back("sp-old", "t", "r").unwrap(), 3);
        assert_eq!(store.load("sp-old").unwrap().events.len(), 3);
    }
}
