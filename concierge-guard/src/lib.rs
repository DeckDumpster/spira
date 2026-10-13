//! One check of whether the Concierge session is listening: find the session by argv, look for
//! a waiter among its descendants, and act on the state the previous check left.

use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pane {
    pub socket: String,
    pub pane: String,
}

pub struct Settings {
    pub session_name: String,
    pub waiters: Vec<Vec<String>>,
    pub min_gap: u64,
    pub mail_after: u64,
    pub wake_text: String,
}

pub trait World {
    fn procs(&self) -> Vec<Proc>;
    fn pane_of(&self, pid: u32) -> Option<Pane>;
    fn send_keys(&mut self, pane: &Pane, text: &str) -> Result<(), String>;
    fn send_mail(&mut self, subject: &str, body: &str) -> Result<(), String>;
    fn inbox_tail(&self) -> String;
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub listening: bool,
    pub since: u64,
    pub checked: u64,
    pub session: u32,
    pub missing_since: Option<u64>,
    pub woke_at: Option<u64>,
    pub mailed_at: Option<u64>,
}

impl State {
    pub fn render(&self) -> String {
        let opt = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_default();
        format!(
            "listening={}\nsince={}\nchecked={}\nsession={}\nmissing_since={}\nwoke_at={}\nmailed_at={}\n",
            if self.listening { "yes" } else { "no" },
            self.since,
            self.checked,
            self.session,
            opt(self.missing_since),
            opt(self.woke_at),
            opt(self.mailed_at)
        )
    }

    pub fn parse(text: &str) -> State {
        let kv: HashMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
        let num = |k: &str| kv.get(k).and_then(|v| v.parse::<u64>().ok());
        State {
            listening: kv.get("listening") == Some(&"yes"),
            since: num("since").unwrap_or(0),
            checked: num("checked").unwrap_or(0),
            session: num("session").unwrap_or(0) as u32,
            missing_since: num("missing_since"),
            woke_at: num("woke_at"),
            mailed_at: num("mailed_at"),
        }
    }
}

fn base(a: &str) -> &str {
    a.rsplit('/').next().unwrap_or(a)
}

pub fn find_session(procs: &[Proc], name: &str) -> Option<u32> {
    procs
        .iter()
        .find(|p| {
            p.argv.first().map(|a| base(a)) == Some("claude") && p.argv.windows(2).any(|w| w[0] == "--remote-control" && w[1] == name)
        })
        .map(|p| p.pid)
}

fn is_waiter(p: &Proc, waiters: &[Vec<String>]) -> bool {
    let head: Vec<&str> = p.argv.iter().take(4).map(|a| base(a)).map(|a| a.strip_suffix(".sh").unwrap_or(a)).collect();
    waiters.iter().any(|w| !w.is_empty() && head.windows(w.len()).any(|h| h.iter().zip(w).all(|(a, b)| a == b)))
}

pub fn has_waiter(procs: &[Proc], session: u32, waiters: &[Vec<String>]) -> bool {
    let mut seen: HashSet<u32> = HashSet::from([session]);
    let mut frontier = vec![session];
    while let Some(parent) = frontier.pop() {
        for p in procs.iter().filter(|p| p.ppid == parent) {
            if !seen.insert(p.pid) {
                continue;
            }
            if is_waiter(p, waiters) {
                return true;
            }
            frontier.push(p.pid);
        }
    }
    false
}

pub fn check(cfg: &Settings, world: &mut dyn World, prior: &State, now: u64) -> (State, Vec<String>) {
    let procs = world.procs();
    let session = find_session(&procs, &cfg.session_name);
    let mut s = prior.clone();
    let mut events = Vec::new();
    s.checked = now;
    s.session = session.unwrap_or(0);

    if session.is_some_and(|sid| has_waiter(&procs, sid, &cfg.waiters)) {
        if !prior.listening {
            s.since = now;
            events.push("listening again: an inbox waiter is running".to_string());
        }
        s.listening = true;
        s.missing_since = None;
        s.woke_at = None;
        s.mailed_at = None;
        return (s, events);
    }

    s.listening = false;
    let Some(missing) = prior.missing_since else {
        s.missing_since = Some(now);
        s.since = now;
        events.push(match session {
            Some(sid) => format!("session {sid} has no inbox waiter (first miss; acting on the next)"),
            None => "no Concierge session is running (first miss; acting on the next)".to_string(),
        });
        return (s, events);
    };

    let Some(sid) = session else {
        if prior.mailed_at.is_none() && now.saturating_sub(missing) >= cfg.mail_after {
            let body = mail_body(world, "No Concierge session has been running since the first miss; there is nothing to wake.");
            mail(world, &mut s, now, &mut events, "Concierge session is not running", &body);
        }
        return (s, events);
    };

    if prior.mailed_at.is_none() && prior.woke_at.is_some_and(|w| now.saturating_sub(w) >= cfg.mail_after) {
        let body = mail_body(world, &format!("Session {sid} was woken and still has no inbox waiter; it is dead or wedged."));
        mail(world, &mut s, now, &mut events, "Concierge woke but did not re-arm its inbox waiter", &body);
        return (s, events);
    }

    if prior.woke_at.is_some_and(|w| now.saturating_sub(w) < cfg.min_gap) {
        events.push(format!("session {sid} has no waiter; woken under {}s ago, holding", cfg.min_gap));
        return (s, events);
    }

    s.woke_at = Some(now);
    match world.pane_of(sid) {
        None => events.push(format!("session {sid} has no waiter and its pane is unknown; cannot wake")),
        Some(pane) => match world.send_keys(&pane, &cfg.wake_text) {
            Ok(()) => events.push(format!("woke session {sid} in pane {} on {}", pane.pane, pane.socket)),
            Err(e) => events.push(format!("could not wake session {sid} in pane {}: {e}", pane.pane)),
        },
    }
    (s, events)
}

fn mail_body(world: &dyn World, lead: &str) -> String {
    format!("{lead}\n\nLast inbox lines:\n{}\n", world.inbox_tail())
}

fn mail(world: &mut dyn World, s: &mut State, now: u64, events: &mut Vec<String>, subject: &str, body: &str) {
    match world.send_mail(subject, body) {
        Ok(()) => {
            s.mailed_at = Some(now);
            events.push(format!("mailed the operator: {subject}"));
        }
        Err(e) => events.push(format!("could not mail the operator ({subject}): {e}")),
    }
}

/// `Ok` with the state line when the last check found the Concierge listening and is recent.
pub fn health(st: &State, now: u64, max_age: u64) -> Result<String, String> {
    if st.checked == 0 || now.saturating_sub(st.checked) > max_age {
        return Err("no recent check".to_string());
    }
    if st.listening {
        Ok(format!("listening since {}", st.since))
    } else {
        Err(format!("not listening since {}", st.since))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        procs: Vec<Proc>,
        keys: Vec<(Pane, String)>,
        mails: Vec<(String, String)>,
        mail_fails: bool,
    }

    impl World for Fake {
        fn procs(&self) -> Vec<Proc> {
            self.procs.clone()
        }
        fn pane_of(&self, _: u32) -> Option<Pane> {
            Some(Pane { socket: "/s/sock".into(), pane: "%7".into() })
        }
        fn send_keys(&mut self, p: &Pane, t: &str) -> Result<(), String> {
            self.keys.push((p.clone(), t.to_string()));
            Ok(())
        }
        fn send_mail(&mut self, s: &str, b: &str) -> Result<(), String> {
            if self.mail_fails {
                return Err("down".into());
            }
            self.mails.push((s.into(), b.into()));
            Ok(())
        }
        fn inbox_tail(&self) -> String {
            "inbox line".into()
        }
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn cfg() -> Settings {
        Settings {
            session_name: "concierge".into(),
            waiters: vec![argv(&["inbox-next"]), argv(&["watchd", "next", "concierge-inbox"])],
            min_gap: 600,
            mail_after: 600,
            wake_text: "wake".into(),
        }
    }

    fn session(name: &str) -> Proc {
        Proc { pid: 10, ppid: 1, argv: argv(&["claude", "--model", "x", "--remote-control", name]) }
    }

    fn world(procs: Vec<Proc>) -> Fake {
        Fake { procs, keys: vec![], mails: vec![], mail_fails: false }
    }

    fn waiter_under_shell() -> Vec<Proc> {
        vec![
            session("concierge"),
            Proc { pid: 11, ppid: 10, argv: argv(&["bash", "-c", "long snapshot"]) },
            Proc { pid: 12, ppid: 11, argv: argv(&["bash", "/run/concierge-notes/inbox-next.sh"]) },
        ]
    }

    #[test]
    fn a_waiter_descendant_means_no_wake() {
        let mut w = world(waiter_under_shell());
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        let (s, _) = check(&cfg(), &mut w, &s, 1180);
        assert!(s.listening);
        assert!(w.keys.is_empty() && w.mails.is_empty());
    }

    #[test]
    fn a_watchd_next_waiter_counts() {
        let mut w = world(vec![session("concierge"), Proc { pid: 12, ppid: 10, argv: argv(&["/usr/bin/watchd", "next", "concierge-inbox"]) }]);
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        assert!(s.listening);
    }

    #[test]
    fn a_waiter_outside_the_session_does_not_count() {
        let mut w = world(vec![session("concierge"), Proc { pid: 12, ppid: 1, argv: argv(&["bash", "inbox-next.sh"]) }]);
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        assert!(!s.listening);
    }

    #[test]
    fn first_miss_is_quiet_second_wakes_once_to_the_session_pane_and_a_third_holds() {
        let mut w = world(vec![session("concierge")]);
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        assert!(w.keys.is_empty());
        let (s, _) = check(&cfg(), &mut w, &s, 1180);
        assert_eq!(w.keys, vec![(Pane { socket: "/s/sock".into(), pane: "%7".into() }, "wake".to_string())]);
        let (s, _) = check(&cfg(), &mut w, &s, 1360);
        assert_eq!(w.keys.len(), 1);
        assert!(s.mailed_at.is_none());
    }

    #[test]
    fn a_wake_with_no_waiter_ten_minutes_on_mails_the_operator_once() {
        let mut w = world(vec![session("concierge")]);
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        let (s, _) = check(&cfg(), &mut w, &s, 1180);
        let (s, _) = check(&cfg(), &mut w, &s, 1779);
        assert!(w.mails.is_empty());
        let (s, ev) = check(&cfg(), &mut w, &s, 1780);
        assert_eq!(w.mails.len(), 1);
        assert!(w.mails[0].1.contains("inbox line"), "{ev:?}");
        let (_, _) = check(&cfg(), &mut w, &s, 1960);
        assert_eq!(w.mails.len(), 1);
    }

    #[test]
    fn a_failed_mail_is_retried() {
        let mut w = world(vec![session("concierge")]);
        w.mail_fails = true;
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        let (s, _) = check(&cfg(), &mut w, &s, 1180);
        let (s, _) = check(&cfg(), &mut w, &s, 1780);
        assert!(s.mailed_at.is_none());
        w.mail_fails = false;
        let (_, _) = check(&cfg(), &mut w, &s, 1960);
        assert_eq!(w.mails.len(), 1);
    }

    #[test]
    fn a_session_with_different_argv_is_not_matched() {
        let mut w = world(vec![session("other")]);
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        let (s, _) = check(&cfg(), &mut w, &s, 1180);
        assert_eq!(s.session, 0);
        assert!(w.keys.is_empty());
    }

    #[test]
    fn no_session_for_ten_minutes_mails_and_never_types() {
        let mut w = world(vec![]);
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        let (s, _) = check(&cfg(), &mut w, &s, 1599);
        assert!(w.mails.is_empty());
        let (_, _) = check(&cfg(), &mut w, &s, 1600);
        assert_eq!(w.mails.len(), 1);
        assert!(w.keys.is_empty());
    }

    #[test]
    fn recovery_clears_the_episode() {
        let mut w = world(vec![session("concierge")]);
        let (s, _) = check(&cfg(), &mut w, &State::default(), 1000);
        let (s, _) = check(&cfg(), &mut w, &s, 1180);
        w.procs = waiter_under_shell();
        let (s, ev) = check(&cfg(), &mut w, &s, 1360);
        assert!(s.listening && s.woke_at.is_none() && s.since == 1360 && !ev.is_empty());
    }

    #[test]
    fn state_round_trips_and_health_reads_it() {
        let st = State { listening: true, since: 5, checked: 100, session: 3, missing_since: None, woke_at: Some(4), mailed_at: None };
        assert_eq!(State::parse(&st.render()), st);
        assert!(health(&st, 200, 300).is_ok());
        assert!(health(&st, 500, 300).is_err());
        assert!(health(&State { listening: false, ..st }, 200, 300).is_err());
    }
}
