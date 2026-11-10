//! Acceptance T1–T6 for `submission-provision`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/submission-provision.md`, Acceptance) on `TaskRig`. Red
//! until the port lands: `decide`, `after_renewal` and `step` are `unimplemented!`, and the row
//! waits in `tasks::a::PENDING`. The shared T6 runs two virtual days; the contract's own T6, thirty
//! days with the certificate due on day 10, is `t6_thirty_days_renew_once_and_restart_once`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: submission_provision,
    registration: SubmissionProvision::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: BOOT_DELAY_MS,
}

fn plan() -> Plan {
    Plan {
        port: "587".into(),
        addr: "100.64.0.7:587".into(),
        cert: "/c/cert.pem".into(),
        key: "/c/key.pem".into(),
        password_file: "/c/pw".into(),
        from: "spira@example.org".into(),
        user: "spira".into(),
        rcpt_domain: "example.org".into(),
        users_file: "/c/users".into(),
        unit: "spira-submission".into(),
    }
}

/// The contract's idle fixture: tailscale up, cert 60 days, password ok, ufw open.
fn idle() -> Snapshot {
    Snapshot { plan: plan(), tailnet_ip: Some("100.64.0.7".into()), dns_name: Some("mini.tail.ts.net".into()), cert_days: Some(60), password: Password::Ok, iface: Some("tailscale0".into()), firewall: Firewall::Open, day: "2026-11-03".into() }
}

/// The certificate is 10 days from expiry: renew, then restart.
fn busy() -> Snapshot {
    Snapshot { cert_days: Some(10), ..idle() }
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_a_config_set_before_the_renewal_outcome_is_refused_naming_renewing() {
    let refused = step(State::Renewing, Trigger::ConfigSet).expect_err("config_set waits on cert_renew");
    assert!(refused.to_lowercase().contains("renewing"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(DAY_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `provision.rs:254-256`: no tailnet ip is a note and nothing else.
#[test]
fn t5_no_tailnet_ip_is_a_note_only() {
    let (_, acts) = decide(&Carry::default(), &Snapshot { tailnet_ip: None, ..idle() });
    assert_eq!(effects(&acts), [INBOX_APPEND], "{acts:?}");
}

/// `provision.rs:282`, `:355-360`: a cert under `RENEW_DAYS` renews, keyed by name and day.
#[test]
fn t5_a_cert_due_renews() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    assert!(acts.iter().any(|a| a.effect == CERT_RENEW && a.key == "cert/mini.tail.ts.net/2026-11-03"), "{acts:?}");
    assert_eq!(carry.state, State::Renewing);
}

/// `provision.rs:307-318`: after a renewal, each empty value is set; a set value is left.
#[test]
fn t5_after_renewal_only_the_empty_values_are_set() {
    let snap = Snapshot { plan: Plan { cert: String::new(), key: String::new(), ..plan() }, ..idle() };
    let (_, acts) = after_renewal(&Carry { state: State::Renewing, last_note: None }, &snap);
    let mut set: Vec<&str> = acts.iter().filter(|a| a.effect == CONFIG_SET).map(|a| a.key.as_str()).collect();
    set.sort();
    assert_eq!(set.len(), 2, "{acts:?}");
    assert!(set.iter().all(|k| k.starts_with("spira.submission_cert=") || k.starts_with("spira.submission_key=")), "{set:?}");
}

/// `provision.rs:339-345`: a closed firewall opens; an absent ufw is left.
#[test]
fn t5_a_closed_firewall_opens_and_an_absent_one_is_left() {
    assert!(effects(&decide(&Carry::default(), &Snapshot { firewall: Firewall::Closed, ..idle() }).1).contains(&FIREWALL_OPEN));
    assert!(!effects(&decide(&Carry::default(), &Snapshot { firewall: Firewall::Absent, ..idle() }).1).contains(&FIREWALL_OPEN));
}

/// `provision.rs:323-336`: a drifted password is rewritten; no users file is refused, not written.
#[test]
fn t5_a_drifted_password_is_rewritten() {
    assert!(effects(&decide(&Carry::default(), &Snapshot { password: Password::Drifted, ..idle() }).1).contains(&PASSWORD_WRITE));
    assert!(!effects(&decide(&Carry::default(), &Snapshot { password: Password::NoSource, ..idle() }).1).contains(&PASSWORD_WRITE));
}

/// The contract's T6: thirty days, the certificate due on day 10 → one renewal and one restart.
#[test]
fn t6_thirty_days_renew_once_and_restart_once() {
    let days_left = Arc::new(Mutex::new(39u32));
    let (seen, renewed) = (days_left.clone(), days_left.clone());
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, move |_| {
            let mut d = seen.lock().unwrap();
            let snap = Snapshot { cert_days: Some(*d), ..idle() };
            *d = d.saturating_sub(1);
            Ok(serde_json::to_string(&snap).unwrap())
        })
        .double(CERT_RENEW, move |_| {
            *renewed.lock().unwrap() = 90;
            Ok("renewed".into())
        });
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, CERT_RENEW].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    let (_, stream, kind) = SubmissionProvision::boot();
    r.tick(stream, kind).unwrap();
    r.advance(30 * DAY_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let count = |n: &str| r.requested().iter().filter(|q| q.name == n).count();
    assert_eq!((count(CERT_RENEW), count(UNIT_RESTART)), (1, 1));
}

fn at_work() -> TaskRig {
    let mut r = rig(1, Snapshot { password: Password::NoSource, firewall: Firewall::Closed, ..idle() });
    r.advance(DAY_MS).unwrap();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&at_work(), NAME);
    assert!(rows.iter().any(|k| k == "last_note"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}
