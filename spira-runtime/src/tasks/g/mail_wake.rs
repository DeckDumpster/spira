//! Task `mail.wake`: a registered reader is woken while its mailbox has unread mail. Replaces
//! the wake half of `spira/spira-mail-deliver.sh` (`_settle_wait`, `_kick`, `_wake_loop`); the
//! watch half is the `mail.arrived` source. Pattern P3: no timer of its own.
//!
//! # Tick
//!
//! A [`mail_arrived::ARRIVED`] on `mail/<mailbox>` for a mailbox `SPIRA_MAIL_READERS` registers
//! arms one [`DUE`] timer on that stream after the settle window — `SPIRA_MAIL_SETTLE` seconds,
//! `SPIRA_MAIL_SETTLE_EVENT` when the newest mail is a machine `--kind event` — so a burst is one
//! wake: arming the timer again replaces the one waiting.
//!
//! Its own [`BOOT`] tick, every last `SPIRA_MAIL_WAKE_BACKOFF` step, is the sweep: it sends a
//! [`DUE`] at the last attempt to every registered mailbox, so a retry chain a restart forgot
//! (timers live in memory) resumes at the slowest step. One chain per mailbox survives: the
//! in-flight check, and the timer an outcome arms replacing the one waiting.
//!
//! # Observe
//!
//! On [`DUE`], the memory-only probe [`OBSERVE`] reads `<SPIRA_MAIL>/<mailbox>/new`: how many
//! messages are unread and how old the oldest is. Nothing else; reading is the ack.
//!
//! # Decide
//!
//! [`decide`]: nothing unread is silence and ends the retries (a mailbox with nothing unread is
//! never woken); unread mail is one [`WAKE`] with the script's line. A wake already in flight on
//! the stream is not doubled. The wake's outcome, success or not, arms the next [`DUE`] after
//! `SPIRA_MAIL_WAKE_BACKOFF`'s step for that attempt, the last step repeating, for as long as mail
//! stays unread.
//!
//! # Act
//!
//! [`WAKE`] runs the reader's operator-declared wake command (the `SPIRA_MAIL_READERS` value,
//! split on whitespace, as the script's unquoted `$wake_cmd`) with the line as its last
//! argument, through `runtime::ports::Proc` under [`WAKE_DEADLINE_MS`], class host. The command is
//! the operator's program; nothing here types into a pane. A halt holds it like every effect.
//!
//! # Replaces
//!
//! `spira-mail-deliver-<instance>.service` (with the source), its `flock` per mailbox (the
//! in-flight check), `sleep` backoff (timers), `mail unread-age` and `ls | wc -l` (the probe).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::mail_arrived::{self, Arrived};
use crate::tasks::Task;

pub const NAME: &str = "mail.wake";
/// The task's own stream: its boot tick lands here; the work happens on `mail/<mailbox>`.
pub const STREAM: &str = "task.mail.wake";
pub const BOOT: EventKindTag = "mail.wake.boot";
/// A wake attempt is due for one mailbox: body [`Due`].
pub const DUE: EventKindTag = "mail.wake.due";
pub const OBSERVE: &str = "mail.wake.observe";
pub const WAKE: &str = "mail.wake.wake";
pub const EFFECTS: &[&str] = &[OBSERVE, WAKE];
pub const KEYS: &[&str] = &["SPIRA_MAIL_READERS", "SPIRA_MAIL", "SPIRA_MAIL_SETTLE", "SPIRA_MAIL_SETTLE_EVENT", "SPIRA_MAIL_WAKE_BACKOFF"];

const OBSERVE_DEADLINE_MS: u64 = 5_000;
/// A reader's wake may hold until its input line is empty (`concierge.sh wake`): minutes.
pub const WAKE_DEADLINE_MS: u64 = 700_000;

/// The body of [`DUE`]: which mailbox, and which attempt this is (0 is the first).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Due {
    pub mailbox: String,
    pub attempt: usize,
}

/// The probe's input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Look {
    pub mailbox: String,
    pub attempt: usize,
    pub new_dir: PathBuf,
}

/// The probe's answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unread {
    pub count: usize,
    pub oldest_age_s: u64,
}

/// The wake's input: the argv to run, and for which attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wake {
    pub mailbox: String,
    pub attempt: usize,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailWake {
    /// Mailbox → its reader's wake command, from `SPIRA_MAIL_READERS`.
    pub readers: BTreeMap<String, String>,
    pub mail_root: PathBuf,
    pub settle_ms: u64,
    pub settle_event_ms: u64,
    /// `SPIRA_MAIL_WAKE_BACKOFF`, in milliseconds; never empty.
    pub backoff_ms: Vec<u64>,
}

/// `<mailbox>=<wake command>` lines, `#` comments and blanks skipped, the first line for a
/// mailbox winning (as the source's own reading of the same key).
pub fn readers(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let Some((mailbox, cmd)) = line.split_once('=') else { continue };
        let (mailbox, cmd) = (mailbox.trim(), cmd.trim());
        if !mailbox.is_empty() && !cmd.is_empty() {
            out.entry(mailbox.to_string()).or_insert_with(|| cmd.to_string());
        }
    }
    out
}

fn secs(key: &str, v: &str) -> Result<u64, String> {
    v.trim().parse::<u64>().map(|s| s * 1000).map_err(|_| format!("{key}={v:?} is not a whole number of seconds — refusing"))
}

/// The line the script's wake carried, word for word.
pub fn line(mailbox: &str, u: Unread) -> String {
    format!("You have {} unread messages in {mailbox}, oldest {}s old — mail list {mailbox} --unread", u.count, u.oldest_age_s)
}

/// What one look at a mailbox calls for: a wake (the argv to run), or nothing when the mailbox
/// is unregistered or has nothing unread.
pub fn decide(cfg: &MailWake, look: &Look, u: Unread) -> Option<Wake> {
    if u.count == 0 {
        return None;
    }
    let cmd = cfg.readers.get(&look.mailbox)?;
    let mut argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    argv.push(line(&look.mailbox, u));
    Some(Wake { mailbox: look.mailbox.clone(), attempt: look.attempt, argv })
}

impl MailWake {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        let p = super::pinned(settings, KEYS)?;
        let get = |k: &str| p.get(k).cloned().unwrap_or_default();
        let backoff_ms = get("SPIRA_MAIL_WAKE_BACKOFF").split_whitespace().map(|s| secs("SPIRA_MAIL_WAKE_BACKOFF", s)).collect::<Result<Vec<_>, _>>()?;
        if backoff_ms.is_empty() {
            return Err("SPIRA_MAIL_WAKE_BACKOFF is empty — a wake needs at least one retry step; refusing".into());
        }
        Ok(MailWake {
            readers: readers(&get("SPIRA_MAIL_READERS")),
            mail_root: PathBuf::from(get("SPIRA_MAIL")),
            settle_ms: secs("SPIRA_MAIL_SETTLE", &get("SPIRA_MAIL_SETTLE"))?,
            settle_event_ms: secs("SPIRA_MAIL_SETTLE_EVENT", &get("SPIRA_MAIL_SETTLE_EVENT"))?,
            backoff_ms,
        })
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("mail.wake/boot", STREAM, EventKind::new(BOOT, ""))
    }

    /// The sweep's period: the last backoff step.
    pub fn cadence_ms(&self) -> u64 {
        self.backoff(usize::MAX)
    }

    /// The wait before attempt `attempt + 1`, the last step repeating.
    pub fn backoff(&self, attempt: usize) -> u64 {
        self.backoff_ms[attempt.min(self.backoff_ms.len() - 1)]
    }

    fn due(mailbox: &str, attempt: usize) -> EventKind {
        EventKind::new(DUE, &serde_json::to_string(&Due { mailbox: mailbox.into(), attempt }).unwrap_or_default())
    }

    /// The request event `ev` answers, when it is one of this task's: its effect name and input.
    fn answered(view: &Projection, ev: &Event) -> Option<(String, String)> {
        let cause = ev.cause.as_ref()?;
        let req = view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)?;
        let body: serde_json::Value = serde_json::from_str(&req.kind.body).ok()?;
        Some((body["spec"]["name"].as_str()?.to_string(), body["spec"]["input"].as_str()?.to_string()))
    }

    /// Whether a [`WAKE`] requested on `stream` has no outcome yet.
    fn wake_in_flight(view: &Projection, stream: &StreamKey) -> bool {
        let events = view.events(stream);
        events.iter().filter(|e| e.kind.tag == EFFECT_REQUESTED && serde_json::from_str::<serde_json::Value>(&e.kind.body).ok().is_some_and(|b| b["spec"]["name"] == WAKE)).any(|req| {
            !events.iter().any(|o| [EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT].contains(&o.kind.tag.as_str()) && o.cause.as_ref() == Some(&req.id))
        })
    }
}

impl Handler for MailWake {
    fn interests(&self) -> &[EventKindTag] {
        &[BOOT, mail_arrived::ARRIVED, DUE, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        match ev.kind.tag.as_str() {
            BOOT if ev.stream.0 == STREAM => {
                let last = self.backoff_ms.len() - 1;
                let mut cmds: Vec<Command> = self.readers.keys().map(|m| Command::EmitTo { stream: StreamKey(mail_arrived::stream(m)), kind: Self::due(m, last) }).collect();
                cmds.push(Command::Timer { at: ev.at + self.cadence_ms(), kind: EventKind::new(BOOT, "") });
                cmds
            }
            mail_arrived::ARRIVED => {
                let Some(a) = Arrived::from_event_kind(&ev.kind) else { return vec![] };
                if ev.stream.0 != mail_arrived::stream(&a.mailbox) || !self.readers.contains_key(&a.mailbox) {
                    return vec![];
                }
                let settle = if a.kind_event { self.settle_event_ms } else { self.settle_ms };
                vec![Command::Timer { at: ev.at + settle, kind: Self::due(&a.mailbox, 0) }]
            }
            DUE => {
                let Ok(d) = serde_json::from_str::<Due>(&ev.kind.body) else { return vec![] };
                if ev.stream.0 != mail_arrived::stream(&d.mailbox) || !self.readers.contains_key(&d.mailbox) || Self::wake_in_flight(view, &ev.stream) {
                    return vec![];
                }
                let look = Look { new_dir: self.mail_root.join(&d.mailbox).join("new"), mailbox: d.mailbox, attempt: d.attempt };
                vec![Command::Probe(EffectSpec::probe(OBSERVE, serde_json::to_string(&look).unwrap_or_default(), OBSERVE_DEADLINE_MS))]
            }
            _ => match Self::answered(view, ev) {
                Some((name, input)) if name == OBSERVE && ev.kind.tag == EFFECT_SUCCEEDED => {
                    let (Ok(look), Ok(u)) = (serde_json::from_str::<Look>(&input), serde_json::from_str::<Unread>(&ev.kind.body)) else { return vec![] };
                    decide(self, &look, u).map(|w| vec![Command::Effect(EffectSpec::keyed(WAKE, format!("mail.wake/{}/{}", w.mailbox, w.attempt), serde_json::to_string(&w).unwrap_or_default(), WAKE_DEADLINE_MS))]).unwrap_or_default()
                }
                Some((name, input)) if name == WAKE => {
                    let Ok(w) = serde_json::from_str::<Wake>(&input) else { return vec![] };
                    vec![Command::Timer { at: ev.at + self.backoff(w.attempt), kind: Self::due(&w.mailbox, w.attempt + 1) }]
                }
                _ => vec![],
            },
        }
    }
}

/// [`OBSERVE`]: the unread count and the oldest unread message's age, from the files in `new/`.
/// A missing directory is nothing unread.
pub fn observe(input: &str) -> Result<String, String> {
    let look: Look = serde_json::from_str(input).map_err(|e| format!("{OBSERVE}: bad input: {e}"))?;
    let u = unread(&look.new_dir, std::time::SystemTime::now())?;
    serde_json::to_string(&u).map_err(|e| e.to_string())
}

/// [`observe`]'s reading of `new_dir` against `now`.
pub fn unread(new_dir: &Path, now: std::time::SystemTime) -> Result<Unread, String> {
    let rd = match std::fs::read_dir(new_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Unread::default()),
        Err(e) => return Err(format!("{}: {e}", new_dir.display())),
    };
    let mut u = Unread::default();
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        u.count += 1;
        let age = md.modified().ok().and_then(|m| now.duration_since(m).ok()).map_or(0, |d| d.as_secs());
        u.oldest_age_s = u.oldest_age_s.max(age);
    }
    Ok(u)
}

/// [`WAKE`]: the reader's own wake command, run through the process port under its deadline.
/// Its stderr is the reason a refusal gives (the script's `wake failed: <why>`).
pub fn wake(proc: &dyn runtime::ports::Proc, input: &str) -> Result<String, String> {
    let w: Wake = serde_json::from_str(input).map_err(|e| format!("{WAKE}: bad input: {e}"))?;
    let (prog, args) = w.argv.split_first().ok_or_else(|| format!("{WAKE}: {} has no wake command — refused", w.mailbox))?;
    let spec = runtime::ports::ProcSpec { prog: prog.clone(), args: args.to_vec(), stdin: vec![], deadline: std::time::Duration::from_millis(WAKE_DEADLINE_MS) };
    let out = proc.run(&spec)?;
    match out.code {
        Some(0) => Ok(format!("{}: wake sent (attempt {})", w.mailbox, w.attempt + 1)),
        code => {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(format!("{}: wake failed (attempt {}, exit {}): {}", w.mailbox, w.attempt + 1, code.map_or("signal".into(), |c| c.to_string()), if why.is_empty() { "reader not running?" } else { &why }))
        }
    }
}

/// The row that registers this task: `tasks::g::TASKS` lists it.
pub const ROW: Task = Task {
    name: NAME,
    plane: runtime::Plane::Signal,
    signal: &[],
    boot: MailWake::boot,
    cadence_ms: |s| super::refuse(NAME, MailWake::from_settings(s)).cadence_ms(),
    effects: EFFECTS,
    runners: &[(OBSERVE, |_, _, s| observe(&s.input)), (WAKE, |_, _, s| wake(&runtime::ports::SysProc, &s.input))],
    keys: &[KEYS],
    build: |s| super::refuse(NAME, MailWake::from_settings(s)).registration(),
};

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> MailWake {
        MailWake {
            readers: readers("# comment\nconcierge=concierge.sh wake\nops = ops-wake --quiet\n\nbad\nconcierge=second\n"),
            mail_root: PathBuf::from("/m"),
            settle_ms: 2_000,
            settle_event_ms: 0,
            backoff_ms: vec![15_000, 60_000, 300_000, 900_000],
        }
    }

    fn event(stream: &str, id: &str, at: u64, kind: EventKind, cause: Option<&str>) -> Event {
        Event { id: EventId(id.into()), stream: StreamKey(stream.into()), kind, at, cause: cause.map(|c| EventId(c.into())), correlation: CorrelationId("c".into()) }
    }

    #[test]
    fn readers_parse_like_the_script_first_line_wins() {
        let r = cfg().readers;
        assert_eq!(r.get("concierge").map(String::as_str), Some("concierge.sh wake"));
        assert_eq!(r.get("ops").map(String::as_str), Some("ops-wake --quiet"));
        assert_eq!(r.len(), 2);
    }

    // covers: UC-operator-channel-10

    #[test]
    fn unread_mail_is_one_wake_with_the_scripts_line_and_nothing_unread_is_silence() {
        let look = Look { mailbox: "concierge".into(), attempt: 2, new_dir: "/m/concierge/new".into() };
        let w = decide(&cfg(), &look, Unread { count: 3, oldest_age_s: 42 }).unwrap();
        assert_eq!(w.argv, vec!["concierge.sh", "wake", "You have 3 unread messages in concierge, oldest 42s old — mail list concierge --unread"]);
        assert_eq!(w.attempt, 2);
        assert_eq!(decide(&cfg(), &look, Unread::default()), None, "a mailbox with nothing unread is never woken");
        assert_eq!(decide(&cfg(), &Look { mailbox: "stranger".into(), ..look }, Unread { count: 1, oldest_age_s: 0 }), None);
    }

    // covers: UC-operator-channel-10

    #[test]
    fn the_backoff_steps_then_repeats_its_last() {
        let c = cfg();
        assert_eq!((c.backoff(0), c.backoff(1), c.backoff(3), c.backoff(9)), (15_000, 60_000, 900_000, 900_000));
    }

    // covers: UC-operator-channel-10

    #[test]
    fn an_arrival_settles_then_the_due_looks_then_the_look_wakes_then_the_outcome_rearms() {
        let c = cfg();
        let s = mail_arrived::stream("concierge");
        let mut view = Projection::default();
        let ctx = Ctx::new(0, 0);

        let arrived = event(&s, "a1", 1_000, Arrived { mailbox: "concierge".into(), kind_event: false }.event_kind(), None);
        view.push(&arrived);
        let cmds = c.handle(&ctx, &view, &arrived);
        assert!(matches!(&cmds[..], [Command::Timer { at: 3_000, kind }] if kind.tag == DUE), "the settle window");
        let event_mail = event(&s, "a2", 1_000, Arrived { mailbox: "concierge".into(), kind_event: true }.event_kind(), None);
        assert!(matches!(&c.handle(&ctx, &view, &event_mail)[..], [Command::Timer { at: 1_000, .. }]), "an event-kind mail does not sit through the window");

        let due = event(&s, "d1", 3_000, MailWake::due("concierge", 0), None);
        view.push(&due);
        let cmds = c.handle(&ctx, &view, &due);
        let [Command::Probe(spec)] = &cmds[..] else { panic!("one look") };
        assert_eq!(spec.name, OBSERVE);
        let look: Look = serde_json::from_str(&spec.input).unwrap();
        assert_eq!(look.new_dir, PathBuf::from("/m/concierge/new"));

        let req = event(&s, "r1", 3_000, EventKind::new(EFFECT_REQUESTED, &serde_json::json!({ "spec": spec, "deadline_at": 8_000 }).to_string()), Some("d1"));
        view.push(&req);
        let seen = event(&s, "o1", 3_010, EventKind::new(EFFECT_SUCCEEDED, &serde_json::to_string(&Unread { count: 2, oldest_age_s: 9 }).unwrap()), Some("r1"));
        view.push(&seen);
        let cmds = c.handle(&ctx, &view, &seen);
        let [Command::Effect(wake_spec)] = &cmds[..] else { panic!("one wake") };
        assert_eq!((wake_spec.name.as_str(), wake_spec.deadline_ms), (WAKE, WAKE_DEADLINE_MS));

        let wreq = event(&s, "w1", 3_010, EventKind::new(EFFECT_REQUESTED, &serde_json::json!({ "spec": wake_spec, "deadline_at": 703_010 }).to_string()), Some("o1"));
        view.push(&wreq);
        let again = event(&s, "d2", 3_500, MailWake::due("concierge", 0), None);
        assert!(c.handle(&ctx, &view, &again).is_empty(), "a wake in flight is not doubled");

        let failed = event(&s, "wf", 5_000, EventKind::new(EFFECT_FAILED, "reader not running"), Some("w1"));
        view.push(&failed);
        let cmds = c.handle(&ctx, &view, &failed);
        assert!(matches!(&cmds[..], [Command::Timer { at: 20_000, kind }] if serde_json::from_str::<Due>(&kind.body).unwrap() == Due { mailbox: "concierge".into(), attempt: 1 }), "a failed wake retries after the first step");
    }

    // covers: UC-operator-channel-10

    #[test]
    fn the_sweep_looks_at_every_registered_mailbox_at_the_slowest_step_and_rearms() {
        let c = cfg();
        let boot = event(STREAM, "b", 1_000, EventKind::new(BOOT, ""), None);
        let mut view = Projection::default();
        view.push(&boot);
        let cmds = c.handle(&Ctx::new(0, 0), &view, &boot);
        let sent: Vec<(String, Due)> = cmds.iter().filter_map(|c| match c {
            Command::EmitTo { stream, kind } if kind.tag == DUE => Some((stream.0.clone(), serde_json::from_str(&kind.body).unwrap())),
            _ => None,
        }).collect();
        assert_eq!(sent, vec![("mail/concierge".to_string(), Due { mailbox: "concierge".into(), attempt: 3 }), ("mail/ops".to_string(), Due { mailbox: "ops".into(), attempt: 3 })]);
        assert!(cmds.iter().any(|c| matches!(c, Command::Timer { at: 901_000, kind } if kind.tag == BOOT)), "the sweep re-arms one last step later");
    }

    #[test]
    fn an_unregistered_mailbox_is_never_woken() {
        let s = mail_arrived::stream("stranger");
        let arrived = event(&s, "a1", 1_000, Arrived { mailbox: "stranger".into(), kind_event: false }.event_kind(), None);
        let mut view = Projection::default();
        view.push(&arrived);
        assert!(cfg().handle(&Ctx::new(0, 0), &view, &arrived).is_empty());
    }

    #[test]
    fn the_look_counts_new_files_and_the_oldest_age_and_a_missing_dir_is_nothing() {
        let d = testkit::TempDir::new("mail-wake");
        let new = d.path().join("new");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("1"), "x").unwrap();
        std::fs::write(new.join("2"), "y").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(100);
        let u = unread(&new, later).unwrap();
        assert_eq!(u.count, 2);
        assert!(u.oldest_age_s >= 100, "{u:?}");
        assert_eq!(unread(&d.path().join("absent"), later).unwrap(), Unread::default());
    }

    struct Ran(std::sync::Mutex<Vec<runtime::ports::ProcSpec>>, i32);

    impl runtime::ports::Proc for Ran {
        fn run(&self, spec: &runtime::ports::ProcSpec) -> Result<runtime::ports::ProcOutput, String> {
            self.0.lock().unwrap().push(spec.clone());
            Ok(runtime::ports::ProcOutput { code: Some(self.1), stdout: vec![], stderr: if self.1 == 0 { vec![] } else { b"pane gone".to_vec() } })
        }
    }

    #[test]
    fn the_wake_runs_the_readers_own_command_under_its_deadline_and_says_why_it_failed() {
        let w = Wake { mailbox: "concierge".into(), attempt: 0, argv: vec!["concierge.sh".into(), "wake".into(), "line".into()] };
        let ok = Ran(std::sync::Mutex::new(vec![]), 0);
        assert!(wake(&ok, &serde_json::to_string(&w).unwrap()).unwrap().contains("wake sent"));
        let ran = ok.0.lock().unwrap()[0].clone();
        assert_eq!((ran.prog.as_str(), ran.args.clone(), ran.deadline.as_millis() as u64), ("concierge.sh", vec!["wake".to_string(), "line".into()], WAKE_DEADLINE_MS));
        let bad = Ran(std::sync::Mutex::new(vec![]), 3);
        assert!(wake(&bad, &serde_json::to_string(&w).unwrap()).unwrap_err().contains("pane gone"));
    }
}
