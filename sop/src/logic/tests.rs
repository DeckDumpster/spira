use super::*;
use crate::ports::{Bd, Clock, Proc};
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Default)]
struct FakeBd {
    shelf: RefCell<BTreeMap<String, String>>,
    forgotten: RefCell<Vec<String>>,
    notes: RefCell<Vec<(String, String)>>,
    unreadable: RefCell<bool>,
    remember_err: RefCell<Option<String>>,
    forget_fails: RefCell<Vec<String>>,
}

impl FakeBd {
    fn seed(&self, key: &str, text: &str) {
        self.shelf.borrow_mut().insert(key.to_string(), text.to_string());
    }
}

impl Bd for FakeBd {
    fn remember(&self, key: &str, text: &str) -> Result<(), String> {
        if let Some(e) = self.remember_err.borrow().clone() {
            return Err(e);
        }
        self.shelf.borrow_mut().insert(key.to_string(), text.to_string());
        Ok(())
    }
    fn recall(&self, key: &str) -> Option<String> {
        self.shelf.borrow().get(key).cloned()
    }
    fn forget(&self, key: &str) -> bool {
        if self.forget_fails.borrow().iter().any(|k| k == key) {
            return false;
        }
        if self.shelf.borrow_mut().remove(key).is_some() {
            self.forgotten.borrow_mut().push(key.to_string());
            true
        } else {
            false
        }
    }
    fn memories_json(&self) -> Option<String> {
        if *self.unreadable.borrow() {
            return None;
        }
        Some(serde_json::to_string(&*self.shelf.borrow()).unwrap())
    }
    fn note(&self, bead: &str, text: &str) -> bool {
        self.notes.borrow_mut().push((bead.to_string(), text.to_string()));
        true
    }
}

#[derive(Default)]
struct FakeProc {
    inventory_hits: RefCell<Vec<String>>,
    metric_value: RefCell<Option<String>>,
}
impl Proc for FakeProc {
    fn inventory_scan(&self, _text: &str) -> Result<Vec<String>, String> {
        Ok(self.inventory_hits.borrow().clone())
    }
    fn metric_probe(&self, _bin: &str, _bash_prefix: bool, _subcmd: &str, _timeout_secs: u64) -> Option<String> {
        self.metric_value.borrow().clone()
    }
}

struct FakeClock;
impl Clock for FakeClock {
    fn now(&self) -> (u64, String) {
        (1_790_812_800, "2026-09-30T00:00:00Z".to_string())
    }
    fn today(&self) -> String {
        "2026-09-30".to_string()
    }
}

fn env() -> Env {
    Env { word_cap: 250, why_cap: 400, actor: "tester".to_string(), cockpit_bin: "cockpit-collect".to_string(), cockpit_bash_prefix: false }
}

const GOOD: &str = "SYMPTOM: disk is full\nCHECK: df -h\nFIX: clear /tmp\n";

// ───────────────────────────── write ─────────────────────────────

#[test]
fn write_refuses_empty_text() {
    let bd = FakeBd::default();
    let proc = FakeProc::default();
    let r = write(&bd, &proc, &env(), "x", "   ", false);
    assert_eq!(r.code, 1);
    assert!(r.err.iter().any(|l| l.contains("empty SOP")));
}

#[test]
fn write_failure_surfaces_the_cause() {
    let bd = FakeBd::default();
    *bd.remember_err.borrow_mut() = Some("fence: blocked\nexit=3".to_string());
    let proc = FakeProc::default();
    let text = "SYMPTOM: s\nCHECK: c\nFIX: f\nESCALATE: e\nMATCH: m\nREF: r\n";
    let r = write(&bd, &proc, &env(), "x", text, false);
    assert_eq!(r.code, 1);
    assert!(r.err.iter().any(|l| l.contains("fence: blocked") && l.contains("exit=3")), "{:?}", r.err);
}

#[test]
fn write_refuses_missing_fields() {
    let bd = FakeBd::default();
    let proc = FakeProc::default();
    let r = write(&bd, &proc, &env(), "x", "SYMPTOM: only this\n", false);
    assert_eq!(r.code, 1);
    assert!(r.err.iter().any(|l| l.contains("missing required field: CHECK")));
    assert!(bd.shelf.borrow().is_empty());
}

#[test]
fn write_refuses_operator_infrastructure() {
    let bd = FakeBd::default();
    let proc = FakeProc::default();
    *proc.inventory_hits.borrow_mut() = vec!["FIX: /opt/operator-secrets/whatever".to_string()];
    let r = write(&bd, &proc, &env(), "x", GOOD, false);
    assert_eq!(r.code, 1);
    assert!(r.err.iter().any(|l| l.contains("operator infrastructure")));
    assert!(bd.shelf.borrow().is_empty());
}

#[test]
fn write_stores_a_clean_sop_and_slugifies_the_key() {
    let bd = FakeBd::default();
    let proc = FakeProc::default();
    let r = write(&bd, &proc, &env(), "disk-full", GOOD, false);
    assert_eq!(r.code, 0, "{:?}", r.err);
    assert_eq!(bd.shelf.borrow().get("sop-disk-full").map(String::as_str), Some(GOOD));
    assert!(r.out.iter().any(|l| l.contains("wrote sop-disk-full")));
}

#[test]
fn write_is_idempotent_on_the_prefix() {
    let bd = FakeBd::default();
    let proc = FakeProc::default();
    let r = write(&bd, &proc, &env(), "sop-disk-full", GOOD, false);
    assert_eq!(r.code, 0);
    assert_eq!(bd.shelf.borrow().len(), 1);
    assert!(bd.shelf.borrow().contains_key("sop-disk-full"));
}

// ───────────────────────────── show / list / match ─────────────────────────────

#[test]
fn show_prints_the_text_or_refuses() {
    let bd = FakeBd::default();
    bd.seed("sop-x", GOOD);
    assert_eq!(show(&bd, "x").out, vec![GOOD.trim_end().to_string()]);
    let r = show(&bd, "missing");
    assert_eq!(r.code, 1);
    assert!(r.err.iter().any(|l| l.contains("no such SOP: sop-missing")));
}

#[test]
fn list_counts_and_shows_symptom() {
    let bd = FakeBd::default();
    bd.seed("sop-a", GOOD);
    let r = list(&bd, false);
    assert!(r.out.iter().any(|l| l.contains("sop-a") && l.contains("disk is full")));
    assert!(r.out.last().unwrap().contains("1 SOP(s) on the shelf"));
}

#[test]
fn match_refuses_on_an_unreadable_shelf() {
    let bd = FakeBd::default();
    *bd.unreadable.borrow_mut() = true;
    let r = match_cmd(&bd, "anything");
    assert_eq!(r.code, 1, "an unreadable shelf is an error for match, never an empty shelf");
    assert!(r.out.is_empty());
}

#[test]
fn match_reports_whether_a_sop_declares_run_in_column_four() {
    let bd = FakeBd::default();
    bd.seed("sop-runs", "MATCH: FAIL\nSYMPTOM: sym-a\nCHECK: c\nFIX: f\nRUN: recert --batch {batch}\nDEADLINE: 60");
    bd.seed("sop-plain", "MATCH: FAIL\nSYMPTOM: sym-b\nCHECK: c\nFIX: f");
    let r = match_cmd(&bd, "FAIL here");
    let cols = |key: &str| r.out.iter().find(|l| l.starts_with(key)).unwrap().split('\t').map(str::to_string).collect::<Vec<_>>();
    assert_eq!((cols("sop-runs")[3].as_str(), cols("sop-runs")[4].as_str()), ("run", "sym-a"));
    assert_eq!((cols("sop-plain")[3].as_str(), cols("sop-plain")[4].as_str()), ("-", "sym-b"));
}

// ───────────────────────────── applied ─────────────────────────────

fn applied_args<'a>(key: &'a str, bead: Option<&'a str>, check: &'a str, held: &'a str) -> AppliedArgs<'a> {
    AppliedArgs { key, bead, pass: None, check, held, why: None }
}

#[test]
fn applied_needs_bead_or_pass() {
    let bd = FakeBd::default();
    let proc = FakeProc::default();
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("applied.jsonl");
    let a = AppliedArgs { key: "x", bead: None, pass: None, check: "pass", held: "yes", why: None };
    let r = applied(&bd, &proc, &clock, &env(), &lp, &a);
    assert_eq!(r.code, 1);
    assert!(r.err.iter().any(|l| l.contains("needs --bead")));
}

#[test]
fn applied_refuses_check_fail_held_yes() {
    let bd = FakeBd::default();
    let proc = FakeProc::default();
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("applied.jsonl");
    let a = applied_args("x", Some("sp-1"), "fail", "yes");
    let r = applied(&bd, &proc, &clock, &env(), &lp, &a);
    assert_eq!(r.code, 1);
    assert!(r.err.iter().any(|l| l.contains("check fail --held yes")));
}

#[test]
fn applied_refuses_an_unknown_slug_when_the_shelf_is_readable() {
    let bd = FakeBd::default();
    bd.seed("sop-other", GOOD);
    let proc = FakeProc::default();
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("applied.jsonl");
    let a = applied_args("sop-missing", Some("sp-1"), "pass", "yes");
    let r = applied(&bd, &proc, &clock, &env(), &lp, &a);
    assert_eq!(r.code, 1);
    assert!(r.err.iter().any(|l| l.contains("no such SOP")));
}

#[test]
fn applied_records_a_ledger_line_and_a_bead_note() {
    let bd = FakeBd::default();
    bd.seed("sop-x", GOOD);
    let proc = FakeProc::default();
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("applied.jsonl");
    let a = applied_args("x", Some("sp-1"), "pass", "yes");
    let r = applied(&bd, &proc, &clock, &env(), &lp, &a);
    assert_eq!(r.code, 0, "{:?}", r.err);
    let text = std::fs::read_to_string(&lp).unwrap();
    assert_eq!(text.lines().count(), 1);
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(v["bead"], "sp-1");
    assert_eq!(v["held"], "yes");
    assert_eq!(bd.notes.borrow().len(), 1);
    assert_eq!(bd.notes.borrow()[0].0, "sp-1");
}

#[test]
fn applied_accepts_a_pass_record_with_no_bead() {
    let bd = FakeBd::default();
    bd.seed("sop-x", GOOD);
    let proc = FakeProc::default();
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("applied.jsonl");
    let a = AppliedArgs { key: "x", bead: None, pass: Some("pass-7"), check: "pass", held: "unknown", why: None };
    let r = applied(&bd, &proc, &clock, &env(), &lp, &a);
    assert_eq!(r.code, 0, "{:?}", r.err);
    assert!(bd.notes.borrow().is_empty(), "a pass record has no bead to note");
    let text = std::fs::read_to_string(&lp).unwrap();
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(v["pass"], "pass-7");
    assert!(v.get("bead").is_none());
}

#[test]
fn applied_downgrades_held_yes_to_unknown_when_the_metric_has_not_cleared() {
    let bd = FakeBd::default();
    bd.seed("sop-x", "SYMPTOM: s\nCHECK: c\nFIX: f\nMETRIC: SP_UNADOPTED unsent\n");
    let proc = FakeProc::default();
    *proc.metric_value.borrow_mut() = Some("SP_UNADOPTED=7\n".to_string());
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("applied.jsonl");
    let a = applied_args("x", Some("sp-1"), "pass", "yes");
    let r = applied(&bd, &proc, &clock, &env(), &lp, &a);
    assert_eq!(r.code, 0, "{:?}", r.err);
    let text = std::fs::read_to_string(&lp).unwrap();
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(v["held"], "unknown", "held must be downgraded, not left at yes");
    let note = &bd.notes.borrow()[0].1;
    assert!(note.contains("downgraded from yes to unknown"), "{note}");
}

#[test]
fn applied_confirms_held_yes_when_the_metric_reads_zero() {
    let bd = FakeBd::default();
    bd.seed("sop-x", "SYMPTOM: s\nCHECK: c\nFIX: f\nMETRIC: SP_UNADOPTED unsent\n");
    let proc = FakeProc::default();
    *proc.metric_value.borrow_mut() = Some("SP_UNADOPTED=0\n".to_string());
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("applied.jsonl");
    let a = applied_args("x", Some("sp-1"), "pass", "yes");
    let r = applied(&bd, &proc, &clock, &env(), &lp, &a);
    assert_eq!(r.code, 0, "{:?}", r.err);
    let text = std::fs::read_to_string(&lp).unwrap();
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(v["held"], "yes");
    assert!(bd.notes.borrow()[0].1.contains("fix confirmed"));
}

#[test]
fn applied_on_an_unreadable_shelf_still_records_marked_unreadable() {
    let bd = FakeBd::default();
    *bd.unreadable.borrow_mut() = true;
    let proc = FakeProc::default();
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("applied.jsonl");
    let a = applied_args("x", Some("sp-1"), "pass", "yes");
    let r = applied(&bd, &proc, &clock, &env(), &lp, &a);
    assert_eq!(r.code, 0, "an unreadable shelf must not block the record");
    let text = std::fs::read_to_string(&lp).unwrap();
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(v["shelf"], "unreadable");
}

// ───────────────────────────── log / digest / ledger-init ─────────────────────────────

fn empty_filter() -> crate::ledger::LogFilter {
    crate::ledger::LogFilter { bead: None, pass: None, sop: None, check: None, since: None }
}

#[test]
fn log_distinguishes_absent_empty_and_corrupt() {
    let f = empty_filter();
    assert_eq!(log(None, &f).code, 2);
    assert_eq!(log(Some(""), &f).code, 1);
    assert_eq!(log(Some("not json\n"), &f).code, 2);
}

#[test]
fn digest_distinguishes_unreadable_from_empty() {
    let bd = FakeBd::default();
    *bd.unreadable.borrow_mut() = true;
    assert_eq!(digest(&bd).code, 2);
    *bd.unreadable.borrow_mut() = false;
    let r = digest(&bd);
    assert_eq!(r.code, 0);
    assert!(r.out.is_empty());
}

#[test]
fn ledger_init_creates_once_and_says_present_after() {
    let dir = testkit::TempDir::new("sop-test");
    let lp = dir.path().join("sop").join("applied.jsonl");
    let r1 = ledger_init(&lp);
    assert_eq!(r1.code, 0);
    assert!(lp.exists());
    let r2 = ledger_init(&lp);
    assert!(r2.out[0].contains("ledger present"));
}

// ───────────────────────────── retire / synth / lint / validate ─────────────────────────────

fn shelved(bd: &FakeBd) {
    bd.seed("sop-x", GOOD);
}

fn retire_x(bd: &FakeBd, reason: &str) -> Report {
    retire(bd, &FakeClock, &env(), "x", reason)
}

#[test]
fn retire_without_a_reason_removes_nothing() {
    let bd = FakeBd::default();
    shelved(&bd);
    for reason in ["", "   "] {
        let r = retire_x(&bd, reason);
        assert_eq!(r.code, 1);
        assert!(r.err.iter().any(|l| l.contains("--reason")), "{:?}", r.err);
    }
    assert!(bd.shelf.borrow().contains_key("sop-x"));
    assert!(bd.shelf.borrow().keys().all(|k| !k.starts_with("retired-")));
}

#[test]
fn retire_records_actor_time_reason_and_last_text() {
    let bd = FakeBd::default();
    shelved(&bd);
    let r = retire_x(&bd, "superseded");
    assert_eq!(r.code, 0, "{:?}", r.err);
    assert_eq!(r.out[0], "retired sop-x by tester at 2026-09-30T00:00:00Z: superseded");
    assert!(!bd.shelf.borrow().contains_key("sop-x"));
    let raw = bd.memories_json().unwrap();
    let rec = &shelf::parse_retired(&raw).unwrap()["sop-x"];
    assert_eq!(
        (rec.by.as_str(), rec.at.as_str(), rec.epoch, rec.reason.as_str(), rec.text.as_str()),
        ("tester", "2026-09-30T00:00:00Z", 1_790_812_800, "superseded", GOOD.trim())
    );
}

#[test]
fn a_retired_sop_leaves_every_reader_but_list_retired() {
    let bd = FakeBd::default();
    bd.seed("sop-x", "SYMPTOM: s\nCHECK: c\nFIX: f\nMATCH: test\n");
    bd.seed("sop-y", GOOD);
    assert!(match_cmd(&bd, "test").out.iter().any(|l| l.starts_with("sop-x")));
    assert_eq!(retire_x(&bd, "stray").code, 0);
    assert!(!list(&bd, false).out.join("\n").contains("sop-x"));
    assert!(!match_cmd(&bd, "test").out.join("\n").contains("sop-x"));
    assert!(!digest(&bd).out.join("\n").contains("sop-x"));
    assert!(!lint(&bd, 250).out.join("\n").contains("sop-x"));
    let l = list(&bd, true);
    assert!(l.out[0].contains("sop-x") && l.out[0].contains("tester") && l.out[0].contains("stray"), "{:?}", l.out);
    assert_eq!(l.out.last().unwrap(), "1 retired SOP(s)");
}

#[test]
fn show_of_a_retired_slug_says_who_and_why() {
    let bd = FakeBd::default();
    shelved(&bd);
    retire_x(&bd, "superseded");
    let r = show(&bd, "x");
    assert_eq!(r.code, 1);
    let err = r.err.join("\n");
    assert!(err.contains("retired") && err.contains("tester") && err.contains("superseded"), "{err}");
    assert!(show(&bd, "never").err[0].contains("no such SOP"));
}

#[test]
fn write_over_a_retired_slug_needs_revive() {
    let bd = FakeBd::default();
    shelved(&bd);
    retire_x(&bd, "superseded");
    let before = bd.shelf.borrow().clone();
    let r = write(&bd, &FakeProc::default(), &env(), "x", GOOD, false);
    assert_eq!(r.code, 1);
    assert!(r.err[0].contains("--revive") && r.err[0].contains("superseded"), "{:?}", r.err);
    assert_eq!(*bd.shelf.borrow(), before);
    let r = write(&bd, &FakeProc::default(), &env(), "x", GOOD, true);
    assert_eq!(r.code, 0, "{:?}", r.err);
    assert!(r.out.iter().any(|l| l.starts_with("revived sop-x (retired by tester")), "{:?}", r.out);
    assert!(bd.shelf.borrow().contains_key("sop-x"));
    assert!(!bd.shelf.borrow().contains_key("retired-sop-x"));
}

#[test]
fn a_second_retire_names_the_first() {
    let bd = FakeBd::default();
    shelved(&bd);
    retire_x(&bd, "first reason");
    let r = retire_x(&bd, "again");
    assert_eq!(r.code, 1);
    assert!(r.err[0].contains("already retired by tester") && r.err[0].contains("first reason"), "{:?}", r.err);
    assert_eq!(retire_x(&bd, "").code, 1);
    assert_eq!(retire(&bd, &FakeClock, &env(), "never", "r").err[0], "sop: no such SOP: sop-never");
}

#[test]
fn retire_of_an_unreadable_shelf_writes_nothing() {
    let bd = FakeBd::default();
    shelved(&bd);
    *bd.unreadable.borrow_mut() = true;
    let r = retire_x(&bd, "why");
    assert_eq!(r.code, 1);
    assert!(r.err[0].contains("cannot read the shelf; nothing retired"));
    assert_eq!(bd.shelf.borrow().len(), 1);
}

#[test]
fn a_failed_forget_rolls_the_record_back() {
    let bd = FakeBd::default();
    shelved(&bd);
    bd.forget_fails.borrow_mut().push("sop-x".to_string());
    let r = retire_x(&bd, "why");
    assert_eq!(r.code, 1);
    assert!(r.err[0].contains("could not remove sop-x; nothing retired"));
    assert_eq!(bd.shelf.borrow().keys().collect::<Vec<_>>(), vec!["sop-x"]);
}

#[test]
fn a_failed_record_forget_on_revive_is_named() {
    let bd = FakeBd::default();
    shelved(&bd);
    retire_x(&bd, "why");
    bd.forget_fails.borrow_mut().push("retired-sop-x".to_string());
    let r = write(&bd, &FakeProc::default(), &env(), "x", GOOD, true);
    assert_eq!(r.code, 1);
    assert!(r.err[0].contains("retired-sop-x"), "{:?}", r.err);
}

#[test]
fn write_on_an_unreadable_shelf_refuses_rather_than_skip_the_retired_guard() {
    let bd = FakeBd::default();
    *bd.unreadable.borrow_mut() = true;
    let r = write(&bd, &FakeProc::default(), &env(), "x", GOOD, false);
    assert_eq!(r.code, 1);
    assert!(r.err[0].contains("cannot read the shelf"));
    assert!(bd.shelf.borrow().is_empty());
}

#[test]
fn a_failed_record_write_leaves_the_sop_in_place() {
    let bd = FakeBd::default();
    shelved(&bd);
    *bd.remember_err.borrow_mut() = Some("fence: blocked".to_string());
    let r = retire_x(&bd, "why");
    assert_eq!(r.code, 1);
    assert!(r.err[0].contains("fence: blocked"));
    assert!(bd.shelf.borrow().contains_key("sop-x"));
}

#[test]
fn synth_with_no_out_path_says_so_and_does_not_fail() {
    let bd = FakeBd::default();
    let clock = FakeClock;
    let r = synth(&bd, &clock, None);
    assert_eq!(r.code, 0);
    assert!(r.out.iter().any(|l| l.contains("no wiki configured")));
}

#[test]
fn synth_writes_the_page() {
    let bd = FakeBd::default();
    bd.seed("sop-x", GOOD);
    let clock = FakeClock;
    let dir = testkit::TempDir::new("sop-test");
    let out = dir.path().join("page.md");
    let r = synth(&bd, &clock, Some(out.to_str().unwrap()));
    assert_eq!(r.code, 0, "{:?}", r.err);
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("sop-x"));
}

#[test]
fn lint_reports_failures_and_refuses_an_unreadable_shelf() {
    let bd = FakeBd::default();
    bd.seed("sop-bad", "SYMPTOM: only this\n");
    let r = lint(&bd, 250);
    assert_eq!(r.code, 1);
    assert!(r.out.iter().any(|l| l.contains("FAIL  sop-bad")));

    let bd2 = FakeBd::default();
    *bd2.unreadable.borrow_mut() = true;
    let r2 = lint(&bd2, 250);
    assert_eq!(r2.code, 1);
    assert!(r2.err.iter().any(|l| l.contains("could not read the shelf")));
}

#[test]
fn validate_cmd_reports_ok_and_scans_for_infrastructure() {
    let proc = FakeProc::default();
    let r = validate_cmd(&proc, "x", GOOD, 250);
    assert_eq!(r.code, 0);
    assert!(r.out.iter().any(|l| l.contains("ok    sop-x")));

    let proc2 = FakeProc::default();
    *proc2.inventory_hits.borrow_mut() = vec!["FIX: /opt/operator-secrets/x".to_string()];
    let r2 = validate_cmd(&proc2, "x", GOOD, 250);
    assert_eq!(r2.code, 1);
    assert!(r2.out.iter().any(|l| l.contains("operator infrastructure")));
}

