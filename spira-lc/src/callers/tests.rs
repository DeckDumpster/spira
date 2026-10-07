//! The caller verbs against an in-memory machine that applies the REAL transition tables
//! (`lifecycle::bead::apply`, `lifecycle::delivery::apply`) and answers `show`/`list`/
//! `event` in the shape `dolt -r json` gives them — every column string-valued, JSON
//! columns as JSON text. What these prove is the composition (which row is read, which
//! event is built, under which CAS); the SQL and the CAS race are spira-lc's own and are
//! proven against a real server by the container suites (test-lc-hold.sh and friends).

use std::collections::BTreeMap;
use std::process::Command;

use lifecycle::bead::{BeadEvent, BeadRow, BeadState, HoldKind};
use lifecycle::delivery::{DeliveryEvent, DeliveryRow, DeliveryState};
use serde_json::{json, Value};

use super::*;

#[derive(Default)]
struct Fake {
    beads: BTreeMap<String, BeadRow>,
    deliveries: BTreeMap<String, DeliveryRow>,
    /// Every `event` call, as `(machine, key, expect, version, actor, kind-json)`.
    events: Vec<(String, String, String, String, String, String)>,
    calls: usize,
    /// Answer every call with this (a machine that cannot be reached).
    down: bool,
}

fn opt(v: &Option<String>) -> Value {
    v.clone().map(Value::String).unwrap_or(Value::Null)
}

impl Fake {
    fn bead(&mut self, id: &str, state: BeadState) -> &mut BeadRow {
        let mut r = BeadRow::filed(id);
        r.state = state;
        self.beads.insert(id.into(), r);
        self.beads.get_mut(id).unwrap()
    }
    fn row_json(r: &BeadRow) -> Value {
        let holds: Vec<&str> = r.holds.iter().map(|h| h.as_str()).collect();
        json!({
            "bead_id": r.bead_id, "state": r.state.as_str(), "tip": opt(&r.tip), "gate_key": opt(&r.gate_key),
            "holder": opt(&r.holder), "lease_until": r.lease_until.map(|n| Value::String(n.to_string())).unwrap_or(Value::Null),
            "holds": serde_json::to_string(&holds).unwrap(), "reason": opt(&r.reason), "version": r.version.to_string(),
        })
    }
    fn state(&self, id: &str) -> &'static str {
        self.beads[id].state.as_str()
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

impl Machine for Fake {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        self.calls += 1;
        if self.down {
            return (CANNOT_TELL, "cannot tell: down".into());
        }
        match args[0].as_str() {
            "show" => match self.beads.get(&args[1]) {
                None => (1, "{}".into()),
                Some(r) => {
                    let d = self.deliveries.get(&args[1]).map(|d| {
                        json!({"bead_id": d.bead_id, "mode": d.mode.as_str(), "state": d.state.as_str(), "version": d.version.to_string()})
                    });
                    (0, json!({"bead": Self::row_json(r), "delivery": d}).to_string())
                }
            },
            "list" => {
                let st = flag(args, "--state");
                let hold = flag(args, "--hold");
                let rows: Vec<Value> = self
                    .beads
                    .values()
                    .filter(|r| st.as_deref().is_none_or(|s| r.state.as_str() == s))
                    .filter(|r| hold.as_deref().is_none_or(|h| r.holds.iter().any(|x| x.as_str() == h)))
                    .map(Self::row_json)
                    .collect();
                (0, Value::Array(rows).to_string())
            }
            "event" => {
                let (machine, key) = (args[1].clone(), args[2].clone());
                let (expect, version, actor, kind) = (
                    flag(args, "--expect").unwrap(),
                    flag(args, "--version").unwrap(),
                    flag(args, "--actor").unwrap(),
                    flag(args, "--kind").unwrap(),
                );
                self.events.push((machine.clone(), key.clone(), expect.clone(), version.clone(), actor.clone(), kind.clone()));
                let ver: u64 = version.parse().unwrap();
                if machine == "bead" {
                    let Some(row) = self.beads.get(&key) else { return (2, "no row".into()) };
                    let ev = BeadEvent { expect: BeadState::from_str(&expect).unwrap(), version: ver, kind: serde_json::from_str(&kind).unwrap(), actor, at: None };
                    let o = lifecycle::bead::apply(row, &ev);
                    if !o.applied {
                        return (REFUSED, format!("refused: {:?}", o.refusal));
                    }
                    self.beads.insert(key, o.row);
                } else {
                    let Some(row) = self.deliveries.get(&key) else { return (2, "no row".into()) };
                    let ev = DeliveryEvent { expect: DeliveryState::from_str(&expect).unwrap(), version: ver, kind: serde_json::from_str(&kind).unwrap(), actor };
                    let o = lifecycle::delivery::apply(row, &ev);
                    if !o.applied {
                        return (REFUSED, format!("refused: {:?}", o.refusal));
                    }
                    self.deliveries.insert(key, o.row);
                }
                (0, String::new())
            }
            other => panic!("unexpected primitive {other}"),
        }
    }
}

fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

fn go(f: &mut Fake, verb: &str, args: &[&str]) -> Answer {
    run(verb, &v(args), f)
}

#[test]
fn every_verb_is_a_caller_verb_and_no_primitive_is() {
    for verb in VERBS {
        assert!(is_verb(verb));
    }
    assert!(!is_verb("show") && !is_verb("event") && !is_verb("list"), "primitives are not caller verbs");
}

// ---- holds -----------------------------------------------------------------------------

#[test]
fn hold_suspends_without_moving_state_and_unhold_restores_it() {
    let mut f = Fake::default();
    f.bead("sp-h", BeadState::Working);
    for kind in ["poison", "wait", "operator", "ask"] {
        assert_eq!(go(&mut f, "hold", &["sp-h", kind, "held for it", "t"]).code, APPLIED, "{kind}");
        assert_eq!(f.state("sp-h"), "WORKING");
        assert_eq!(go(&mut f, "holds", &["sp-h"]).stdout, kind);
        assert_eq!(go(&mut f, "held", &["sp-h", kind]).code, 0);
        assert_eq!(go(&mut f, "held", &["sp-h", "other"]).code, 1);
        if kind == "ask" {
            assert_ne!(go(&mut f, "unhold", &["sp-h", kind, "t"]).code, APPLIED, "an ask lifts only on a reply event");
            assert_eq!(go(&mut f, "holds", &["sp-h"]).stdout, kind);
            continue;
        }
        assert_eq!(go(&mut f, "unhold", &["sp-h", kind, "t"]).code, APPLIED);
        assert_eq!(go(&mut f, "holds", &["sp-h"]).stdout, "");
    }
}

#[test]
fn hold_event_is_byte_identical_to_the_shell_librarys() {
    let mut f = Fake::default();
    f.bead("sp-j", BeadState::Ready);
    go(&mut f, "hold", &["sp-j", "poison", "seed \"quoted\"", "test"]);
    go(&mut f, "unhold", &["sp-j", "poison"]);
    go(&mut f, "hold", &["sp-j", "wait", ""]);
    let kinds: Vec<&str> = f.events.iter().map(|e| e.5.as_str()).collect();
    assert_eq!(
        kinds,
        [
            r#"{"Hold":{"kind":"Poison","cause":"attempts-exhausted","detail":"seed \"quoted\""}}"#,
            r#"{"Unhold":{"kind":"Poison"}}"#,
            r#"{"Hold":{"kind":"Wait","cause":"unlanded-blocker","detail":""}}"#,
        ]
    );
    // CAS from the same read; default actor "sentinel", as lc.sh.
    assert_eq!((f.events[0].2.as_str(), f.events[0].3.as_str(), f.events[0].4.as_str()), ("READY", "0", "test"));
    assert_eq!((f.events[1].3.as_str(), f.events[1].4.as_str()), ("1", "sentinel"));
}

#[test]
fn hold_refusals_and_absences_keep_the_shell_exit_codes() {
    let mut f = Fake::default();
    f.bead("sp-t", BeadState::Landed);
    assert_eq!(go(&mut f, "hold", &["sp-t", "poison", "x"]).code, REFUSED, "terminal refuses");
    assert_eq!(go(&mut f, "hold", &["sp-none", "poison", "x"]).code, NO_ROW, "no row");
    let a = go(&mut f, "hold", &["sp-t", "bogus", "x"]);
    assert_eq!((a.code, a.stderr.as_str()), (CANNOT_TELL, "lc: unknown hold kind 'bogus'\n"));
    assert_eq!(go(&mut f, "holds", &["sp-none"]), Answer::code(0));
    assert_eq!(go(&mut f, "held", &["sp-none", "poison"]).code, 1);
    let mut down = Fake { down: true, ..Default::default() };
    assert_eq!(go(&mut down, "hold", &["sp-t", "poison", "x"]).code, CANNOT_TELL);
    assert_eq!(go(&mut down, "list-all", &[]), Answer::code(0), "an unreachable machine lists nothing, rc 0");
}

// ---- releases, drops, returns, content-on-base -----------------------------------------

#[test]
fn release_and_holder_dead_return_working_to_ready_and_refuse_elsewhere() {
    let mut f = Fake::default();
    f.bead("sp-r", BeadState::Working).holder = Some("aeon-1".into());
    assert_eq!(go(&mut f, "release", &["sp-r", "t"]).code, APPLIED);
    assert_eq!(f.state("sp-r"), "READY");
    assert_eq!(go(&mut f, "release", &["sp-r"]).code, REFUSED);
    f.bead("sp-d", BeadState::Working).holder = Some("aeon-2".into());
    assert_eq!(go(&mut f, "holder-dead", &["sp-d", "slay"]).code, APPLIED);
    assert_eq!(f.state("sp-d"), "READY");
    assert_eq!(f.events.last().unwrap().5, r#""HolderDead""#);
}

#[test]
fn drop_is_orthogonal_and_terminal() {
    let mut f = Fake::default();
    f.bead("sp-x", BeadState::Ready);
    assert_eq!(go(&mut f, "drop", &["sp-x", "operator decided", "slay"]).code, APPLIED);
    assert_eq!(f.state("sp-x"), "DROPPED");
    assert_eq!(f.events[0].5, r#"{"Drop":{"reason":"unwanted"}}"#);
    assert_eq!(go(&mut f, "drop", &["sp-x", "again"]).code, REFUSED);
}

#[test]
fn returned_moves_in_delivery_to_rework_as_batch_ejected() {
    let mut f = Fake::default();
    f.bead("sp-e", BeadState::InDelivery);
    assert_eq!(go(&mut f, "returned", &["sp-e", "ejected from open batch", "queue"]).code, APPLIED);
    assert_eq!(f.state("sp-e"), "REWORK");
    assert_eq!(f.events[0].5, r#"{"Returned":{"reason":"batch-ejected"}}"#);
}

#[test]
fn content_on_base_lands_any_non_terminal_state_with_its_proof() {
    let mut f = Fake::default();
    f.bead("sp-c", BeadState::Working);
    assert_eq!(go(&mut f, "content-on-base", &["sp-c", "merge-tree:abc"]).code, APPLIED);
    assert_eq!(f.state("sp-c"), "LANDED");
    assert_eq!(f.beads["sp-c"].reason.as_deref(), Some("merge-tree:abc"));
    assert_eq!(f.events[0].4, "sending", "lc_content_on_base's default actor");
    assert_eq!(go(&mut f, "content-on-base", &["sp-c", "again"]).code, REFUSED);
}

// ---- reads -----------------------------------------------------------------------------

#[test]
fn state_and_the_bulk_lists_keep_their_line_shapes() {
    let mut f = Fake::default();
    let w = f.bead("sp-1", BeadState::Working);
    w.holder = Some("aeon-1".into());
    w.lease_until = Some(1_700_000_000);
    w.holds.insert(HoldKind::Wait);
    w.holds.insert(HoldKind::Poison);
    f.bead("sp-2", BeadState::Ready).holds.insert(HoldKind::Poison);
    f.bead("sp-3", BeadState::Working);
    assert_eq!(go(&mut f, "state", &["sp-1"]).stdout, "WORKING");
    assert_eq!(go(&mut f, "state", &["sp-9"]).code, NO_ROW);
    assert_eq!(go(&mut f, "list-held", &["poison"]).stdout, "sp-1\nsp-2");
    assert_eq!(go(&mut f, "list-state", &["WORKING"]).stdout, "sp-1\t1700000000\tpoison,wait\nsp-3\t\t");
    assert_eq!(go(&mut f, "list-all", &[]).stdout, "sp-1\tWORKING\taeon-1\nsp-2\tREADY\t\nsp-3\tWORKING\t");
}

// ---- delivery exits --------------------------------------------------------------------

#[test]
fn deliver_skips_without_a_row_or_in_the_wrong_state_and_applies_in_the_right_one() {
    let mut f = Fake::default();
    let a = go(&mut f, "deliver", &["push-delivered", "sp-p", "abc"]);
    assert_eq!(a.code, NO_ROW);
    assert!(a.stdout.contains("lc: no delivery row for sp-p — not recording landing.sh's event"), "{}", a.stdout);
    f.bead("sp-p", BeadState::InDelivery);
    let a = go(&mut f, "deliver", &["push-delivered", "sp-p", "abc"]);
    assert!(a.stdout.contains("no delivery row for sp-p"), "a bead row with no delivery row is still no delivery row");
    f.deliveries.insert("sp-p".into(), DeliveryRow::start_push("sp-p"));
    let a = go(&mut f, "deliver", &["pr-closed", "sp-p", "closed"]);
    assert_eq!(a.code, NO_ROW);
    assert!(a.stdout.contains("lc: sp-p delivery is PUSHING, not PR_OPEN — not recording pr-pass-branch's event"), "{}", a.stdout);
    let a = go(&mut f, "deliver", &["push-delivered", "sp-p", "abc"]);
    assert_eq!(a.code, APPLIED, "{}", a.stdout);
    assert!(a.stdout.contains("lc: sp-p delivery PUSHING -> applied (landing.sh)"));
    assert_eq!(f.events.last().unwrap().5, r#"{"Delivered":{"merge_sha":"abc","proof":"ancestry"}}"#);
    let a = go(&mut f, "deliver", &["push-delivered", "sp-p", "abc"]);
    assert!(a.stdout.contains("delivery is EXITED, not PUSHING"), "a second sighting refuses: {}", a.stdout);
}

#[test]
fn deliver_push_requeued_and_returned_build_the_typed_exits() {
    for (sub, extra, want) in [
        ("push-requeued", "cafe", r#"{"Requeued":{"tip":"cafe"}}"#),
        ("push-returned", "genuinely conflicts", r#"{"Returned":{"reason":"push-rejected"}}"#),
    ] {
        let mut f = Fake::default();
        f.bead("sp-q", BeadState::InDelivery);
        f.deliveries.insert("sp-q".into(), DeliveryRow::start_push("sp-q"));
        assert_eq!(go(&mut f, "deliver", &[sub, "sp-q", extra]).code, APPLIED, "{sub}");
        assert_eq!(f.events[0].5, want);
        assert_eq!(f.deliveries["sp-q"].state, DeliveryState::Exited);
    }
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn deliver_pr_merged_proves_by_merge_tree_when_the_squash_holds_the_diff() {
    let t = testkit::TempDir::new("spira-lc-prmerged");
    let r = t.path();
    git(r, &["init", "-q", "-b", "main"]);
    git(r, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(r, &["checkout", "-q", "-b", "spira/sp-s"]);
    std::fs::write(r.join("f"), "x\n").unwrap();
    git(r, &["add", "f"]);
    git(r, &["commit", "-q", "-m", "sp-s: work"]);
    git(r, &["checkout", "-q", "main"]);
    std::fs::write(r.join("f"), "x\n").unwrap();
    git(r, &["add", "f"]);
    git(r, &["commit", "-q", "-m", "squash (#1)"]);
    let squash = git(r, &["rev-parse", "HEAD"]);
    std::fs::write(r.join("g"), "unrelated\n").unwrap();
    git(r, &["checkout", "-q", "-b", "spira/sp-u", "main~1"]);
    std::fs::write(r.join("h"), "never landed\n").unwrap();
    git(r, &["add", "h"]);
    git(r, &["commit", "-q", "-m", "sp-u: work"]);
    let repo = r.to_str().unwrap();

    assert!(crate::git_evidence::content_on_base(r, "spira/sp-s", &squash), "squash holds the diff");
    assert!(!crate::git_evidence::content_on_base(r, "spira/sp-u", &squash), "a branch with work outstanding is not landed");
    for (id, br, proof) in [("sp-s", "spira/sp-s", "merge-tree"), ("sp-u", "spira/sp-u", "gh-merged")] {
        let mut f = Fake::default();
        f.bead(id, BeadState::InDelivery);
        f.deliveries.insert(id.into(), DeliveryRow::start_pr(id, 7));
        let a = go(&mut f, "deliver", &["pr-merged", repo, id, br, &squash]);
        assert_eq!(a.code, APPLIED, "{}", a.stdout);
        assert_eq!(f.events[0].5, format!(r#"{{"Delivered":{{"merge_sha":"{squash}","proof":"{proof}"}}}}"#));
        assert_eq!(f.events[0].4, "pr-pass-branch");
    }
    let mut f = Fake::default();
    f.bead("sp-s", BeadState::InDelivery);
    f.deliveries.insert("sp-s".into(), DeliveryRow::start_pr("sp-s", 7));
    assert_eq!(go(&mut f, "deliver", &["pr-closed", "sp-s", "closed unmerged"]).code, APPLIED);
    assert_eq!(f.events[0].5, r#"{"Returned":{"reason":"pr-closed-unmerged"}}"#);
}

// ---- certification ---------------------------------------------------------------------

#[test]
fn certify_submits_a_working_bead_before_the_verdict_lands() {
    let mut f = Fake::default();
    f.bead("sp-w", BeadState::Working).holder = Some("aeon".into());
    let a = go(&mut f, "certify", &["sp-w", "fff666", "pass", "keyB", "gate"]);
    assert_eq!((a.code, a.cert_log.clone()), (APPLIED, Some(("applied".into(), "pass tip=fff666".into()))));
    assert_eq!(f.state("sp-w"), "CERTIFIED");
    assert_eq!(f.beads["sp-w"].gate_key.as_deref(), Some("keyB"));
    let kinds: Vec<&str> = f.events.iter().map(|e| e.5.as_str()).collect();
    assert_eq!(kinds, [r#"{"Submit":{"tip":"fff666"}}"#, r#"{"GatePass":{"tip":"fff666","gate_key":"keyB"}}"#]);
    assert!(f.events.iter().all(|e| e.4 == "gate"));
}

#[test]
fn certify_voids_a_stale_certification_first_and_leaves_a_current_one_alone() {
    let mut f = Fake::default();
    let r = f.bead("sp-s", BeadState::Certified);
    r.tip = Some("aaa111".into());
    r.gate_key = Some("keyA".into());
    // Same tip: the same pass again is idempotent — applied as "already", no event. A red
    // verdict on it is still not a SUBMITTED transition — skipped, rc 3.
    let a = go(&mut f, "certify", &["sp-s", "aaa111", "pass", "keyA"]);
    assert_eq!((a.code, a.cert_log.unwrap().0), (APPLIED, "already".into()));
    let a = go(&mut f, "certify", &["sp-s", "aaa111", "red", "branch-red"]);
    assert_eq!((a.code, a.cert_log.unwrap().0), (REFUSED, "skip".into()));
    assert!(f.events.is_empty());
    // Moved tip: Submit voids it, then the verdict lands on SUBMITTED.
    assert_eq!(go(&mut f, "certify", &["sp-s", "bbb222", "pass", "keyA"]).code, APPLIED);
    assert_eq!((f.state("sp-s"), f.beads["sp-s"].tip.as_deref()), ("CERTIFIED", Some("bbb222")));
    assert_eq!(f.events[0].4, "lifecycle-cert", "default actor");
}

#[test]
fn certify_maps_red_reasons_and_infra_and_refuses_what_it_cannot_reach() {
    for (raw, want) in [("branch-red", "suites-failed"), ("syntax", "syntax"), ("beads-data", "policy-violation"), ("foreign-harness", "policy-violation"), ("no-rebase", "no-rebase"), ("timeout", "timeout"), ("confine", "confine"), ("a-reason-never-heard-of", "suites-failed")] {
        let mut f = Fake::default();
        f.bead("sp-r", BeadState::Working);
        assert_eq!(go(&mut f, "certify", &["sp-r", "ddd", "red", raw]).code, APPLIED, "{raw}");
        assert_eq!(f.events[1].5, format!(r#"{{"GateRed":{{"tip":"ddd","reason":"{want}"}}}}"#));
        assert_eq!(f.state("sp-r"), "REWORK");
    }
    let mut f = Fake::default();
    f.bead("sp-i", BeadState::Working);
    assert_eq!(go(&mut f, "certify", &["sp-i", "eee", "infra", "-"]).code, APPLIED);
    assert_eq!(f.events[1].5, r#"{"GateInfra":{"tip":"eee"}}"#);
    assert_eq!(f.state("sp-i"), "SUBMITTED", "an infra verdict is a retry: the row stays SUBMITTED");
    // REWORK: lifecycle-cert.sh tried Submit from REWORK too, but the machine has no such
    // transition (a REWORK bead is re-claimed first) — the attempt is refused, the row is
    // not SUBMITTED, and the verdict is skipped: rc 3, exactly as the shell answered.
    f.bead("sp-rw", BeadState::Rework);
    let a = go(&mut f, "certify", &["sp-rw", "eee", "pass", "k"]);
    assert_eq!((a.code, a.cert_log.unwrap().0), (REFUSED, "skip".into()));
    assert_eq!(f.state("sp-rw"), "REWORK");
    let a = go(&mut f, "certify", &["sp-none", "eee", "pass", "k"]);
    assert_eq!((a.code, a.cert_log.unwrap()), (CANNOT_TELL, ("cannot-tell".into(), "no lifecycle row yet".into())));
    f.bead("sp-rd", BeadState::Ready);
    assert_eq!(go(&mut f, "certify", &["sp-rd", "eee", "pass", "k"]).code, REFUSED, "READY never reaches SUBMITTED");
    f.bead("sp-o", BeadState::Working);
    let a = go(&mut f, "certify", &["sp-o", "eee", "bogus", "k"]);
    assert_eq!((a.code, a.cert_log.unwrap().1), (CANNOT_TELL, "unknown outcome bogus".into()));
}

#[test]
fn resubmit_records_a_moved_tip_with_no_verdict() {
    let mut f = Fake::default();
    let r = f.bead("sp-m", BeadState::Certified);
    r.tip = Some("aaa".into());
    let a = go(&mut f, "resubmit", &["sp-m", "ccc333"]);
    assert_eq!((a.code, a.cert_log.unwrap()), (APPLIED, ("applied".into(), "resubmit tip=ccc333".into())));
    assert_eq!((f.state("sp-m"), f.beads["sp-m"].tip.as_deref()), ("SUBMITTED", Some("ccc333")));
    let a = go(&mut f, "resubmit", &["sp-m", "ddd"]);
    assert_eq!((a.code, a.cert_log.unwrap().0), (REFUSED, "skip".into()));
    assert_eq!(go(&mut f, "resubmit", &["sp-none", "ddd"]).code, CANNOT_TELL);
}

#[test]
fn every_event_verb_needs_its_arguments() {
    let mut f = Fake::default();
    for (verb, args) in [("hold", vec!["sp-a"]), ("drop", vec!["sp-a"]), ("returned", vec!["sp-a"]), ("content-on-base", vec!["sp-a"]), ("certify", vec!["sp-a", "t"]), ("deliver", vec!["push-delivered", "sp-a"]), ("state", vec![])] {
        let a = go(&mut f, verb, &args);
        assert_eq!(a.code, CANNOT_TELL, "{verb}");
        assert!(a.stderr.starts_with("usage: spira-lc "), "{verb}: {}", a.stderr);
    }
    assert_eq!(f.calls, 0, "a malformed call reads nothing");
}

#[test]
fn log_lines_carry_libsh_logs_timestamp() {
    assert_eq!(fmt_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(fmt_utc(1_790_000_000), "2026-09-21T14:13:20Z");
    assert_eq!(fmt_utc(951_782_400), "2000-02-29T00:00:00Z");
    let l = log_line("x");
    assert!(l.len() == "2026-09-21T14:13:20Z spira: x\n".len() && l.ends_with("Z spira: x\n"), "{l}");
}

#[test]
fn a_push_landing_with_no_delivery_round_records_landed_on_a_certified_bead() {
    // Push mode creates no delivery row; the landing is recorded on the bead (sp-51lgh).
    let mut f = Fake::default();
    f.bead("sp-q", BeadState::Certified);
    let a = go(&mut f, "deliver", &["push-delivered", "sp-q", "abc"]);
    assert_eq!(a.code, APPLIED, "{}", a.stdout);
    assert!(a.stdout.contains("lc: sp-q landed by push — CERTIFIED -> LANDED"), "{}", a.stdout);
    let last = f.events.last().unwrap();
    assert_eq!(last.5, r#"{"ContentOnBase":{"proof":"ancestry:abc"}}"#);
    // Not CERTIFIED (e.g. a queue-mode bead mid-delivery): still no row, nothing recorded.
    let n = f.events.len();
    f.bead("sp-r", BeadState::Submitted);
    assert_eq!(go(&mut f, "deliver", &["push-delivered", "sp-r", "abc"]).code, NO_ROW);
    assert_eq!(f.events.len(), n, "no event for a bead that is not CERTIFIED");
}

// ---- unclaim / close-epic (sp-hyo5e) ---------------------------------------------------

#[derive(Default)]
struct FakeBd {
    /// bead id -> (issue_type, assignee)
    rows: BTreeMap<String, (String, Option<String>)>,
    closed: Vec<(String, String)>,
    /// bead id -> close reason, for beads bd reports closed.
    reasons: BTreeMap<String, String>,
    down: bool,
}

impl Bd for FakeBd {
    fn issue_type(&mut self, id: &str) -> Result<String, String> {
        if self.down {
            return Err("bd down".into());
        }
        self.rows.get(id).map(|r| r.0.clone()).ok_or_else(|| "no such issue".into())
    }
    fn close(&mut self, id: &str, reason: &str) -> Result<(), String> {
        self.closed.push((id.into(), reason.into()));
        Ok(())
    }
    fn closed(&mut self, ids: &[String]) -> Result<Vec<(String, String)>, String> {
        if self.down {
            return Err("bd down".into());
        }
        Ok(ids.iter().filter_map(|i| self.reasons.get(i).map(|r| (i.clone(), r.clone()))).collect())
    }
}

#[test]
fn unclaim_releases_the_row_under_the_holders_name() {
    let mut f = Fake::default();
    f.bead("sp-u", BeadState::Working).holder = Some("aeon-1".into());
    assert_eq!(unclaim(&v(&["sp-u", "aeon-1"]), &mut f).code, APPLIED);
    assert_eq!(f.state("sp-u"), "READY");
    assert_eq!(f.events.last().unwrap().5, r#""Release""#);
}

#[test]
fn unclaim_never_robs_another_holder_and_a_claim_already_over_is_success() {
    let mut f = Fake::default();
    f.bead("sp-s", BeadState::Submitted);
    assert_eq!(unclaim(&v(&["sp-s", "aeon-1"]), &mut f).code, APPLIED);
    assert!(f.events.is_empty(), "past WORKING: nothing to release, no event");
    // Reaped and handed to another aeon in between: refused, the new holder keeps it.
    f.bead("sp-h", BeadState::Working).holder = Some("aeon-2".into());
    let a = unclaim(&v(&["sp-h", "aeon-1"]), &mut f);
    assert_eq!(a.code, NO_ROW, "{}", a.stderr);
    assert_eq!(f.state("sp-h"), "WORKING");
    assert!(f.events.is_empty());
    assert_eq!(unclaim(&v(&["sp-none", "aeon-1"]), &mut f).code, NO_ROW);
}

#[test]
fn unclaim_needs_an_actor() {
    let mut f = Fake::default();
    assert_eq!(unclaim(&v(&["sp-o"]), &mut f).code, CANNOT_TELL, "no actor: usage");
    assert_eq!(unclaim(&v(&["sp-o", ""]), &mut f).code, CANNOT_TELL, "empty actor: usage");
    assert_eq!(f.calls, 0);
}

#[test]
fn close_epic_closes_only_an_epic() {
    let mut bd = FakeBd::default();
    bd.rows.insert("sp-e".into(), ("epic".into(), None));
    bd.rows.insert("sp-w".into(), ("task".into(), None));
    assert_eq!(close_epic(&v(&["sp-e", "Pilgrimage complete"]), &mut bd).code, APPLIED);
    assert_eq!(bd.closed, vec![("sp-e".to_string(), "Pilgrimage complete".to_string())]);
    let a = close_epic(&v(&["sp-w", "x"]), &mut bd);
    assert_eq!(a.code, REFUSED, "a work bead is never closed through bd");
    assert!(a.stderr.contains("not an epic"));
    assert_eq!(bd.closed.len(), 1);
    bd.down = true;
    assert_eq!(close_epic(&v(&["sp-e", "x"]), &mut bd).code, CANNOT_TELL);
    assert_eq!(close_epic(&v(&["sp-e"]), &mut bd).code, CANNOT_TELL, "no reason: usage");
}

// ---- renew (sp-2jf0a): the working aeon's lease renewal --------------------------------

#[test]
fn renew_advances_the_holders_lease_and_keeps_the_row_working() {
    let mut f = Fake::default();
    let r = f.bead("sp-n", BeadState::Working);
    r.holder = Some("aeon-1".into());
    r.lease_until = Some(600);
    for until in ["660", "720", "780"] {
        let a = go(&mut f, "renew", &["sp-n", "aeon-1", until]);
        assert_eq!(a.code, APPLIED, "{}", a.stderr);
        assert_eq!(f.state("sp-n"), "WORKING");
        assert_eq!(f.beads["sp-n"].lease_until, Some(until.parse().unwrap()));
    }
    let (_, _, expect, _, actor, kind) = f.events.last().unwrap().clone();
    assert_eq!((expect.as_str(), actor.as_str(), kind.as_str()), ("WORKING", "aeon-1", r#"{"Renew":{"lease_until":780}}"#));
}

#[test]
fn renew_by_a_non_holder_is_refused_and_leaves_the_lease_alone() {
    let mut f = Fake::default();
    let r = f.bead("sp-h", BeadState::Working);
    r.holder = Some("aeon-2".into());
    r.lease_until = Some(600);
    let a = go(&mut f, "renew", &["sp-h", "aeon-1", "9999"]);
    assert_eq!(a.code, NO_ROW, "{}", a.stderr);
    assert!(a.stderr.contains("aeon-2"), "{}", a.stderr);
    assert_eq!(f.beads["sp-h"].lease_until, Some(600));
    assert!(f.events.is_empty(), "a renewal that is not ours sends nothing");
    // Past WORKING (submitted, or reaped back to READY): nothing to renew, and nothing sent.
    f.bead("sp-s", BeadState::Submitted);
    assert_eq!(go(&mut f, "renew", &["sp-s", "aeon-1", "9999"]).code, NO_ROW);
    f.bead("sp-r", BeadState::Ready);
    assert_eq!(go(&mut f, "renew", &["sp-r", "aeon-1", "9999"]).code, NO_ROW);
    assert!(f.events.is_empty());
    assert_eq!(go(&mut f, "renew", &["sp-none", "aeon-1", "9999"]).code, NO_ROW);
}

#[test]
fn renew_needs_a_numeric_deadline_and_reads_nothing_without_one() {
    let mut f = Fake::default();
    for args in [vec!["sp-a", "aeon-1"], vec!["sp-a", "aeon-1", "soon"], vec!["sp-a", "", "5"]] {
        let a = go(&mut f, "renew", &args);
        assert_eq!(a.code, CANNOT_TELL, "{args:?}");
        assert!(a.stderr.starts_with("usage: spira-lc "), "{}", a.stderr);
    }
    assert_eq!(f.calls, 0);
    let mut down = Fake { down: true, ..Default::default() };
    assert_eq!(go(&mut down, "renew", &["sp-a", "aeon-1", "5"]).code, CANNOT_TELL);
}

// ---- reconcile-closed (sp-bc0rlt) ------------------------------------------------------

#[test]
fn reconcile_closed_moves_a_closed_beads_ready_row_to_the_terminal_its_reason_earns() {
    let mut f = Fake::default();
    f.bead("sp-d", BeadState::Ready);
    f.bead("sp-s", BeadState::Ready);
    f.bead("sp-open", BeadState::Ready);
    f.bead("sp-w", BeadState::Working);
    let mut b = FakeBd::default();
    b.reasons.insert("sp-d".into(), "Answered: yes".into());
    b.reasons.insert("sp-s".into(), "OUTCOME: duplicate\nSuperseded by sp-zz, same work".into());
    b.reasons.insert("sp-w".into(), "closed by hand".into());

    let dry = reconcile_closed(&[], &mut f, &mut b);
    assert_eq!(dry.code, APPLIED, "{}", dry.stderr);
    assert!(dry.stdout.contains("would move sp-d READY -> DROPPED"), "{}", dry.stdout);
    assert!(f.events.is_empty(), "a dry run writes nothing");
    assert_eq!(f.state("sp-d"), "READY");

    let ans = reconcile_closed(&v(&["--apply"]), &mut f, &mut b);
    assert_eq!(ans.code, APPLIED, "{}", ans.stdout);
    assert_eq!(f.state("sp-d"), "DROPPED");
    assert_eq!(f.state("sp-s"), "SUPERSEDED");
    assert_eq!(f.state("sp-open"), "READY", "bd says open: untouched");
    assert_eq!(f.state("sp-w"), "WORKING", "only never-claimed READY rows are rewritten");
}

#[test]
fn reconcile_closed_with_ids_touches_only_those_and_cannot_tell_when_bd_is_down() {
    let mut f = Fake::default();
    f.bead("sp-a", BeadState::Ready);
    f.bead("sp-b", BeadState::Ready);
    let mut b = FakeBd::default();
    b.reasons.insert("sp-a".into(), "x".into());
    b.reasons.insert("sp-b".into(), "x".into());
    assert_eq!(reconcile_closed(&v(&["--apply", "sp-a"]), &mut f, &mut b).code, APPLIED);
    assert_eq!((f.state("sp-a"), f.state("sp-b")), ("DROPPED", "READY"));
    b.down = true;
    assert_eq!(reconcile_closed(&v(&["--apply"]), &mut f, &mut b).code, CANNOT_TELL);
    assert_eq!(reconcile_closed(&v(&["--bogus"]), &mut f, &mut b).code, 2);
}

#[test]
fn reconcile_closed_reports_an_ask_hold_as_refused() {
    let mut f = Fake::default();
    f.bead("sp-h", BeadState::Ready).holds.insert(HoldKind::Ask);
    let mut b = FakeBd::default();
    b.reasons.insert("sp-h".into(), "x".into());
    let ans = reconcile_closed(&v(&["--apply", "sp-h"]), &mut f, &mut b);
    assert_eq!(ans.code, REFUSED, "{}", ans.stdout);
    assert_eq!(f.state("sp-h"), "READY");
}

#[test]
fn a_close_reason_naming_a_successor_supersedes_otherwise_drops() {
    assert_eq!(terminal_event_for("dup. superseded by sp-9x."), BeadEventKind::Supersede { by: "sp-9x".into() });
    assert_eq!(terminal_event_for("superseded by nobody"), BeadEventKind::Drop { reason: DropReason::ClosedNoBranch });
    assert_eq!(terminal_event_for(""), BeadEventKind::Drop { reason: DropReason::ClosedNoBranch });
}

// ---- close (sp-3fue0j): the one door for closing a bead -----------------------------------

fn no_file(_: &str) -> Result<String, String> {
    Err("no reason file in this test".into())
}

#[test]
fn close_records_the_end_on_the_row_before_the_store_closes() {
    let mut f = Fake::default();
    f.bead("sp-d", BeadState::Ready);
    let mut bd = FakeBd::default();
    let a = close(&v(&["sp-d", "--reason", "Decided by the Concierge: neither"]), &mut f, &mut bd, &mut no_file);
    assert_eq!(a.code, APPLIED, "{}", a.stderr);
    assert_eq!(f.state("sp-d"), "DROPPED", "a closed bead never stays READY on its row");
    assert_eq!(bd.closed, vec![("sp-d".to_string(), "Decided by the Concierge: neither".to_string())]);
    assert_eq!(f.events.last().unwrap().4, "close", "default actor");
}

#[test]
fn close_withdraws_an_open_ask_first_and_names_a_successor() {
    let mut f = Fake::default();
    f.bead("sp-a", BeadState::Ready).holds.insert(HoldKind::Ask);
    let mut bd = FakeBd::default();
    let a = close(&v(&["sp-a", "--reason", "answered", "--superseded-by", "sp-b", "--actor", "operator"]), &mut f, &mut bd, &mut no_file);
    assert_eq!(a.code, APPLIED, "{}", a.stderr);
    assert_eq!(f.state("sp-a"), "SUPERSEDED");
    let kinds: Vec<&str> = f.events.iter().map(|e| e.5.as_str()).collect();
    assert_eq!(kinds, vec![r#""AskWithdrawn""#, r#"{"Supersede":{"by":"sp-b"}}"#]);
    assert!(f.events.iter().all(|e| e.4 == "operator"));
    // A successor named in the prose earns SUPERSEDED as reconcile-closed reads it.
    f.bead("sp-c", BeadState::Rework);
    assert_eq!(close(&v(&["sp-c", "--reason", "Superseded by sp-z (landed)"]), &mut f, &mut bd, &mut no_file).code, APPLIED);
    assert_eq!(f.state("sp-c"), "SUPERSEDED");
    assert_eq!(f.beads["sp-c"].reason.as_deref(), Some("sp-z"));
}

#[test]
fn close_of_a_terminal_or_rowless_bead_closes_the_store_alone() {
    let mut f = Fake::default();
    f.bead("sp-l", BeadState::Landed);
    let mut bd = FakeBd::default();
    assert_eq!(close(&v(&["sp-l", "--reason", "landed at abc"]), &mut f, &mut bd, &mut no_file).code, APPLIED);
    assert_eq!(close(&v(&["sp-none", "--reason", "an alert"]), &mut f, &mut bd, &mut no_file).code, APPLIED);
    assert!(f.events.is_empty(), "nothing to record: already terminal, or no row");
    assert_eq!(bd.closed.len(), 2);
}

#[test]
fn close_never_closes_the_store_when_the_row_cannot_be_read_or_moved() {
    let mut f = Fake { down: true, ..Default::default() };
    let mut bd = FakeBd::default();
    let a = close(&v(&["sp-d", "--reason", "x"]), &mut f, &mut bd, &mut no_file);
    assert_eq!(a.code, CANNOT_TELL, "{}", a.stderr);
    assert!(bd.closed.is_empty(), "fail closed: no store close without the row's end");
}

#[test]
fn close_reads_its_reason_from_a_file_and_refuses_without_one() {
    let mut f = Fake::default();
    f.bead("sp-r", BeadState::Ready);
    let mut bd = FakeBd::default();
    let mut file = |p: &str| -> Result<String, String> { if p == "-" { Ok("from stdin\n".into()) } else { Err("nope".into()) } };
    assert_eq!(close(&v(&["sp-r", "--reason-file", "-"]), &mut f, &mut bd, &mut file).code, APPLIED);
    assert_eq!(bd.closed, vec![("sp-r".to_string(), "from stdin".to_string())]);
    for bad in [&["sp-r"][..], &["--reason", "x"], &["sp-r", "--reason", "  "], &["sp-r", "--reason", "x", "--bogus", "y"]] {
        assert_eq!(close(&v(bad), &mut f, &mut bd, &mut no_file).code, CANNOT_TELL, "{bad:?}: usage");
    }
    assert_eq!(close(&v(&["sp-r", "--reason-file", "/nope"]), &mut f, &mut bd, &mut file).code, CANNOT_TELL);
    assert_eq!(bd.closed.len(), 1);
}

#[test]
fn a_landing_close_leaves_the_row_to_the_delivery_and_is_refused_outside_it() {
    let mut f = Fake::default();
    let mut bd = FakeBd::default();
    for (id, st) in [("sp-c", BeadState::Certified), ("sp-s", BeadState::Submitted), ("sp-l", BeadState::Landed)] {
        f.bead(id, st);
        let a = close(&v(&[id, "--reason", "landed at abc", "--landing"]), &mut f, &mut bd, &mut no_file);
        assert_eq!(a.code, APPLIED, "{id}: {}", a.stderr);
    }
    assert!(f.events.is_empty(), "the landing records LANDED itself");
    assert_eq!((f.state("sp-c"), f.state("sp-s")), ("CERTIFIED", "SUBMITTED"));
    assert_eq!(bd.closed.len(), 3);
    f.bead("sp-r", BeadState::Ready);
    let a = close(&v(&["sp-r", "--reason", "x", "--landing"]), &mut f, &mut bd, &mut no_file);
    assert_eq!(a.code, REFUSED, "{}", a.stderr);
    assert_eq!(f.state("sp-r"), "READY");
    assert_eq!(bd.closed.len(), 3, "nothing closed");
    assert_eq!(close(&v(&["sp-r", "--reason", "x", "--landing", "--superseded-by", "sp-z"]), &mut f, &mut bd, &mut no_file).code, CANNOT_TELL);
}
