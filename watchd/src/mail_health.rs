//! The unread-age check over every registered mailbox and the operator's reply path, run in the
//! scheduler's process. Exit 0: nothing to report. 1: mailed. 3: could not check.
//!
//! State under `$SPIRA_RUN/mail-health/<name>` holds what was last mailed; `mail tidy` reads it
//! through [`mail::tidy::mail_health_raised`] to end an alarm whose state is gone.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const FROM: &str = "Mail health <health@spira>";
const OUTBOUND: &str = "outbound";

pub struct Config {
    pub mail_root: PathBuf,
    pub run_dir: PathBuf,
    pub readers: Vec<String>,
    pub unread_age_s: u64,
    pub accounts_conf: PathBuf,
}

impl Config {
    pub fn load() -> Result<Config, String> {
        use spira_config::process::{cfg, cfg_parse};
        let readers = cfg("SPIRA_MAIL_READERS")?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| Some(l.split('=').next()?.trim().to_string()).filter(|m| !m.is_empty()))
            .collect();
        Ok(Config {
            mail_root: PathBuf::from(cfg("SPIRA_MAIL")?),
            run_dir: PathBuf::from(cfg("SPIRA_RUN")?),
            readers,
            unread_age_s: cfg_parse("SPIRA_MAIL_UNREAD_AGE")?,
            accounts_conf: accounts_conf_path(),
        })
    }

    fn state_dir(&self) -> PathBuf {
        self.run_dir.join("mail-health")
    }
}

fn accounts_conf_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))).unwrap_or_default();
    base.join("aerc/accounts.conf")
}

pub trait Mailer {
    fn send(&self, subject: &str, condition: &str, body: &str) -> Result<(), String>;
}

/// The operator mail, sent in process through the `mail` library.
pub struct LibMailer;

impl Mailer for LibMailer {
    fn send(&self, subject: &str, condition: &str, body: &str) -> Result<(), String> {
        let env = mail::env::Env::load()?;
        let bd = mail::bead::BdCli::configured(env.bd_conn_retries)?;
        alert(&bd, env, subject, condition, body)
    }
}

fn alert(bd: &dyn mail::bead::Bd, mut env: mail::env::Env, subject: &str, condition: &str, body: &str) -> Result<(), String> {
    env.condition = Some(condition.to_string());
    env.operator_considered = Some("mail-health alerts the operator about unread mailboxes".into());
    env.repeat_considered = Some("mail-health has own dedup".into());
    mail::maildir::mailbox_valid("operator")?;
    let args = mail::cmds::SendArgs { mailbox: "operator", from: Some(FROM), subject, kind: "alert", default: "", class: "", bead: "", urgent: false, digest: false, dry_run: false, requested_by: "" };
    mail::cmds::send_recorded(bd, &env, &args, body.to_string()).map(|_| ())
}

/// What `(mtime, len)` of a file said when `spira-sendmail --check` last passed.
type Stamp = (SystemTime, u64);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Passed {
    conf: Stamp,
    exe: Stamp,
}

/// Remembers the stamps of the last passing `--check`, so an unchanged install spawns nothing.
#[derive(Default)]
pub struct CheckMemo(Mutex<Option<Passed>>);

static PROCESS_MEMO: CheckMemo = CheckMemo(Mutex::new(None));

/// The check as production runs it: in process mail, the process-wide memo, the real binary.
pub fn run() -> i32 {
    match Config::load() {
        Ok(cfg) => check(&cfg, &LibMailer, &PROCESS_MEMO, &sendmail_check),
        Err(e) => {
            eprintln!("mail-health: {e}");
            3
        }
    }
}

fn sendmail_check(exe: &Path) -> bool {
    // FORK-EDGE: spira-sendmail's own self-check is the only judge of its transport
    spira_config::bounded::bounded(exe).arg("--check").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn check(cfg: &Config, mailer: &dyn Mailer, memo: &CheckMemo, sendmail_check: &dyn Fn(&Path) -> bool) -> i32 {
    let (mut found, mut err) = (false, false);
    match outbound(cfg, mailer, memo, sendmail_check) {
        Outbound::Fine => {}
        Outbound::Mailed => found = true,
        Outbound::Unsent => err = true,
    }
    for mailbox in &cfg.readers {
        match mailbox_pass(cfg, mailer, mailbox) {
            Pass::Quiet => {}
            Pass::Mailed => found = true,
            Pass::Failed => err = true,
        }
    }
    if err {
        3
    } else if found {
        1
    } else {
        0
    }
}

enum Outbound {
    Fine,
    Mailed,
    Unsent,
}

fn outbound(cfg: &Config, mailer: &dyn Mailer, memo: &CheckMemo, sendmail_check: &dyn Fn(&Path) -> bool) -> Outbound {
    let Some(command) = aerc_outgoing(&cfg.accounts_conf) else { return Outbound::Fine };
    let fault = outbound_fault(&command, &cfg.accounts_conf, memo, sendmail_check);
    let state = cfg.state_dir().join(OUTBOUND);
    let Some(fault) = fault else {
        let _ = std::fs::remove_file(&state);
        return Outbound::Fine;
    };
    if read_state(&state).as_deref() == Some(fault.as_str()) {
        return Outbound::Fine;
    }
    let body = format!("## Alert\nYou cannot reply to mail: {fault}. Re-run spira-install to rewrite aerc's outgoing command.\n");
    if let Err(e) = mailer.send("reply path is broken: aerc cannot send", "mail-health:outbound", &body) {
        eprintln!("mail-health: could not mail the operator about the reply path: {e}");
        return Outbound::Unsent;
    }
    write_state(&state, &fault);
    Outbound::Mailed
}

fn aerc_outgoing(conf: &Path) -> Option<String> {
    let text = std::fs::read_to_string(conf).ok()?;
    text.lines().find_map(|l| {
        let (key, value) = l.split_once('=')?;
        key.starts_with("outgoing").then(|| value.trim().to_string())
    })
}

fn outbound_fault(command: &str, conf: &Path, memo: &CheckMemo, sendmail_check: &dyn Fn(&Path) -> bool) -> Option<String> {
    if command.is_empty() {
        return Some("aerc has an empty outgoing command".into());
    }
    let exe = command.split(' ').next().unwrap_or_default();
    let exe_path = Path::new(exe);
    if !is_executable_file(exe_path) {
        return Some(format!("aerc outgoing command {exe} is not an executable file"));
    }
    if exe_path.file_name().and_then(|n| n.to_str()) != Some("spira-sendmail") {
        return None;
    }
    let stamps = stamp(conf).zip(stamp(exe_path)).map(|(conf, exe)| Passed { conf, exe });
    let mut memo = memo.0.lock().unwrap_or_else(|p| p.into_inner());
    if stamps.is_some() && *memo == stamps {
        return None;
    }
    if sendmail_check(exe_path) {
        *memo = stamps;
        return None;
    }
    *memo = None;
    Some(format!("{exe} --check failed"))
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    path.is_file() && unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0
}

enum Pass {
    Quiet,
    Mailed,
    Failed,
}

fn mailbox_pass(cfg: &Config, mailer: &dyn Mailer, mailbox: &str) -> Pass {
    let state = cfg.state_dir().join(mailbox);
    let dir = cfg.mail_root.join(mailbox);
    if mail::maildir::mailbox_valid(mailbox).is_err() || !mail::maildir::mailbox_exists(&dir) {
        eprintln!("mail-health: mailbox {mailbox} cannot be read");
        return Pass::Failed;
    }
    let unread = mail::maildir::sorted_entries(&dir.join("new"));
    let Some(oldest) = unread.iter().map(|p| mail::maildir::mtime_secs(p)).min() else {
        let _ = std::fs::remove_file(&state);
        return Pass::Quiet;
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let age = now.saturating_sub(oldest);
    if age < cfg.unread_age_s || read_state(&state).as_deref() == Some(oldest.to_string().as_str()) {
        return Pass::Quiet;
    }
    let body = format!("## Alert\n{mailbox} is not reading its mail ({} message(s), oldest {} minutes unread).\n", unread.len(), age / 60);
    if let Err(e) = mailer.send(&format!("{mailbox} is not reading its mail"), &format!("mail-health:{mailbox}"), &body) {
        eprintln!("mail-health: could not mail the operator about {mailbox}: {e}");
        return Pass::Failed;
    }
    write_state(&state, &oldest.to_string());
    Pass::Mailed
}

fn read_state(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.lines().next().unwrap_or_default().to_string())
}

fn write_state(path: &Path, value: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("{value}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::time::Duration;

    #[derive(Default)]
    struct Outbox(RefCell<Vec<(String, String)>>, bool);

    impl Mailer for Outbox {
        fn send(&self, subject: &str, condition: &str, _body: &str) -> Result<(), String> {
            if self.1 {
                return Err("mail refused".into());
            }
            self.0.borrow_mut().push((subject.into(), condition.into()));
            Ok(())
        }
    }

    struct Fx {
        t: testkit::TempDir,
    }

    impl Fx {
        fn new(tag: &str) -> Fx {
            let t = testkit::TempDir::new(tag);
            fs::create_dir_all(t.path().join("home")).unwrap();
            Fx { t }
        }

        fn cfg(&self, readers: &[&str]) -> Config {
            let d = self.t.path();
            Config { mail_root: d.join("mail"), run_dir: d.join("run"), readers: readers.iter().map(|s| s.to_string()).collect(), unread_age_s: 60, accounts_conf: d.join("home/accounts.conf") }
        }

        fn deliver(&self, mailbox: &str, name: &str, age_s: u64) {
            let dir = self.t.path().join("mail").join(mailbox);
            for sub in ["new", "cur"] {
                fs::create_dir_all(dir.join(sub)).unwrap();
            }
            let f = fs::File::create(dir.join("new").join(name)).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(age_s)).unwrap();
        }

        fn drain(&self, mailbox: &str) {
            for e in fs::read_dir(self.t.path().join("mail").join(mailbox).join("new")).unwrap() {
                fs::remove_file(e.unwrap().path()).unwrap();
            }
        }

        fn state(&self, name: &str) -> PathBuf {
            self.t.path().join("run/mail-health").join(name)
        }

        fn aerc_outgoing(&self, cmd: &str) {
            fs::write(self.t.path().join("home/accounts.conf"), format!("[spira]\noutgoing = {cmd}\n")).unwrap();
        }
    }

    fn run(fx: &Fx, readers: &[&str], out: &Outbox) -> i32 {
        check(&fx.cfg(readers), out, &CheckMemo::default(), &|_| true)
    }

    struct Door {
        env: mail::env::Env,
    }

    impl Mailer for Door {
        fn send(&self, subject: &str, condition: &str, body: &str) -> Result<(), String> {
            alert(&crate::send::tests::NoStore::default(), self.env.clone(), subject, condition, body)
        }
    }

    // covers: UC-operator-channel-09
    #[test_id(t69sp14)]
    #[test]
    fn both_mail_health_alarms_pass_the_lint_and_reach_the_operator_as_alerts() {
        let fx = Fx::new("mh-door");
        std::fs::create_dir_all(fx.t.path().join("run")).unwrap();
        fx.deliver("concierge", "a", 120);
        fx.aerc_outgoing(fx.t.path().join("gone/spira-sendmail").to_str().unwrap());
        let env = mail::env::Env { kinds_dir: testkit::manifest_dir().join("../spira/mail/kinds"), lint_considered: None, ..crate::send::tests::env(fx.t.path()) };
        assert_eq!(check(&fx.cfg(&["concierge"]), &Door { env }, &CheckMemo::default(), &|_| true), 1);
        let sent: Vec<String> = std::fs::read_dir(fx.t.path().join("mail/operator/new")).unwrap().map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap()).collect();
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|m| m.contains("\nX-Spira-Kind: alert\n") && m.contains("\n## Alert\n")), "{sent:?}");
    }

    // covers: UC-operator-channel-09
    #[test_id(t3n7vqa)]
    #[test]
    fn an_unread_backlog_past_the_threshold_mails_the_operator_once_naming_the_mailbox() {
        let fx = Fx::new("mh-once");
        let out = Outbox::default();
        fx.deliver("concierge", "a", 120);
        assert_eq!(run(&fx, &["concierge"], &out), 1);
        assert_eq!(*out.0.borrow(), vec![("concierge is not reading its mail".to_string(), "mail-health:concierge".to_string())]);
        assert_eq!(run(&fx, &["concierge"], &out), 0);
        assert_eq!(out.0.borrow().len(), 1);
    }

    #[test_id(t5d2hzc)]
    #[test]
    fn a_drained_mailbox_clears_the_alarm_state_and_a_recurring_backlog_alerts_again() {
        let fx = Fx::new("mh-rearm");
        let out = Outbox::default();
        fx.deliver("concierge", "a", 120);
        assert_eq!(run(&fx, &["concierge"], &out), 1);
        assert!(fx.state("concierge").exists());
        fx.drain("concierge");
        assert_eq!(run(&fx, &["concierge"], &out), 0);
        assert!(!fx.state("concierge").exists());
        fx.deliver("concierge", "b", 150);
        assert_eq!(run(&fx, &["concierge"], &out), 1);
        assert_eq!(out.0.borrow().len(), 2);
    }

    #[test_id(t9w4kxe)]
    #[test]
    fn fresh_mail_below_the_threshold_never_alerts() {
        let fx = Fx::new("mh-fresh");
        let out = Outbox::default();
        fx.deliver("concierge", "a", 5);
        assert_eq!(run(&fx, &["concierge"], &out), 0);
        assert!(out.0.borrow().is_empty());
    }

    #[test_id(tb1m6ry)]
    #[test]
    fn a_dead_aerc_outgoing_path_mails_the_operator_once_and_no_accounts_conf_is_silent() {
        let fx = Fx::new("mh-dead");
        let out = Outbox::default();
        assert_eq!(run(&fx, &[], &out), 0);
        fx.aerc_outgoing(&format!("{}/gone/mail.sh sendmail", fx.t.path().display()));
        assert_eq!(run(&fx, &[], &out), 1);
        assert_eq!(out.0.borrow().len(), 1);
        assert_eq!(out.0.borrow()[0].1, "mail-health:outbound");
        assert_eq!(run(&fx, &[], &out), 0);
        assert_eq!(out.0.borrow().len(), 1);
    }

    #[test_id(tc8p3ug)]
    #[test]
    fn a_spira_sendmail_failing_its_check_is_a_fault_and_a_working_one_clears_the_state() {
        let fx = Fx::new("mh-check");
        let out = Outbox::default();
        let wr = fx.t.path().join("wr");
        fs::create_dir_all(&wr).unwrap();
        let sendmail = wr.join("spira-sendmail");
        testkit::write_exe(&sendmail, "#!/bin/sh\nexit 0\n");
        fx.aerc_outgoing(sendmail.to_str().unwrap());
        let cfg = fx.cfg(&[]);
        let memo = CheckMemo::default();
        assert_eq!(check(&cfg, &out, &memo, &|_| false), 1);
        assert!(fx.state("outbound").exists());
        assert_eq!(check(&cfg, &out, &memo, &|_| true), 0);
        assert!(!fx.state("outbound").exists());
    }

    #[test_id(td6j9sw)]
    #[test]
    fn spira_sendmail_is_checked_again_only_when_its_config_or_binary_moved_since_a_pass() {
        let fx = Fx::new("mh-memo");
        let out = Outbox::default();
        let sendmail = fx.t.path().join("spira-sendmail");
        testkit::write_exe(&sendmail, "#!/bin/sh\nexit 0\n");
        fx.aerc_outgoing(sendmail.to_str().unwrap());
        let (cfg, memo, spawned) = (fx.cfg(&[]), CheckMemo::default(), std::cell::Cell::new(0));
        let pass = |_: &Path| {
            spawned.set(spawned.get() + 1);
            true
        };
        for _ in 0..3 {
            assert_eq!(check(&cfg, &out, &memo, &pass), 0);
        }
        assert_eq!(spawned.get(), 1, "an unchanged install spawns the check once");
        fx.aerc_outgoing(&format!("{} --flag", sendmail.display()));
        check(&cfg, &out, &memo, &pass);
        assert_eq!(spawned.get(), 2, "a moved accounts.conf is checked again");
        testkit::write_exe(&sendmail, "#!/bin/sh\n# changed\nexit 0\n");
        check(&cfg, &out, &memo, &pass);
        assert_eq!(spawned.get(), 3, "a moved executable is checked again");
        fx.aerc_outgoing(&format!("{} --again", sendmail.display()));
        check(&cfg, &out, &memo, &|_| false);
        check(&cfg, &out, &memo, &pass);
        assert_eq!(spawned.get(), 4, "a failed check is never remembered");
    }

    #[test_id(te2r5hn)]
    #[test]
    fn a_mailbox_that_cannot_be_read_or_a_mail_that_cannot_be_sent_is_could_not_check() {
        let fx = Fx::new("mh-err");
        let out = Outbox::default();
        assert_eq!(run(&fx, &["nobody"], &out), 3, "no such mailbox");
        fx.deliver("concierge", "a", 120);
        let broken = Outbox(RefCell::default(), true);
        assert_eq!(run(&fx, &["concierge"], &broken), 3);
        assert!(!fx.state("concierge").exists(), "an unsent alarm leaves no state, so the next pass retries");
        assert_eq!(run(&fx, &["concierge"], &out), 1);
    }
}
