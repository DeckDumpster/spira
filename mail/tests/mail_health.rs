//! The shell script is the subject (the binary-spira-spawning exception): `mail-health.sh` runs
//! under `bash` beside an empty `conf.sh`, with a stub `mail` that reads and writes a scratch
//! Maildir by file mtime. The real `mail` verbs it calls are proven in `src/channel_tests.rs`.

#[macro_use]
extern crate spira_test_id;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const SCRIPT: &str = include_str!("../../spira/mail-health.sh");
const STUB_MAIL: &str = r#"#!/bin/bash
newest() { ls -tr "$SPIRA_MAIL/$1/new" 2>/dev/null | head -1; }
case "$1" in
unread-age)
    f="$(newest "$2")"; [ -n "$f" ] || exit 0
    echo $(( $(date +%s) - $(stat -c %Y "$SPIRA_MAIL/$2/new/$f") )) ;;
count) ls "$SPIRA_MAIL/$2/new" 2>/dev/null | wc -l ;;
send)
    cat > /dev/null
    shift 2
    while [ $# -gt 0 ]; do [ "$1" = --subject ] && s="$2"; shift; done
    mkdir -p "$SPIRA_MAIL/operator/new"
    printf '%s\n' "$s" > "$SPIRA_MAIL/operator/new/$(date +%s%N)" ;;
esac
"#;

struct Fx {
    t: testkit::TempDir,
}

impl Fx {
    fn new(tag: &str) -> Fx {
        let t = testkit::TempDir::new(tag);
        let d = t.path();
        fs::create_dir_all(d.join("bin")).unwrap();
        fs::create_dir_all(d.join("home")).unwrap();
        fs::write(d.join("conf.sh"), "").unwrap();
        fs::write(d.join("mail-health.sh"), SCRIPT).unwrap();
        testkit::write_exe(d.join("bin/mail"), STUB_MAIL);
        Fx { t }
    }

    fn mail(&self) -> PathBuf {
        self.t.path().join("mail")
    }

    fn deliver(&self, mailbox: &str, name: &str, age_s: u64) {
        let dir = self.mail().join(mailbox).join("new");
        fs::create_dir_all(&dir).unwrap();
        let f = fs::File::create(dir.join(name)).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age_s)).unwrap();
    }

    fn drain(&self, mailbox: &str) {
        for e in fs::read_dir(self.mail().join(mailbox).join("new")).unwrap() {
            fs::remove_file(e.unwrap().path()).unwrap();
        }
    }

    fn operator(&self) -> Vec<String> {
        let dir = self.mail().join("operator/new");
        let mut v: Vec<String> = fs::read_dir(dir).map(|d| d.flatten().map(|f| fs::read_to_string(f.path()).unwrap().trim().to_string()).collect()).unwrap_or_default();
        v.sort();
        v
    }

    fn state(&self, name: &str) -> PathBuf {
        self.t.path().join("run/mail-health").join(name)
    }

    fn run(&self, readers: &str) -> i32 {
        let d = self.t.path();
        let path = format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap());
        Command::new("bash")
            .arg(d.join("mail-health.sh"))
            .env_clear()
            .env("PATH", path)
            .env("HOME", d.join("home"))
            .env("SPIRA_RUN", d.join("run"))
            .env("SPIRA_MAIL", self.mail())
            .env("SPIRA_MAIL_UNREAD_AGE", "60")
            .env("SPIRA_MAIL_READERS", readers)
            .status()
            .unwrap()
            .code()
            .unwrap()
    }

    fn aerc_outgoing(&self, cmd: &str) {
        let dir = self.t.path().join("home/.config/aerc");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("accounts.conf"), format!("[spira]\noutgoing = {cmd}\n")).unwrap();
    }
}

fn exe(path: &Path, body: &str) {
    testkit::write_exe(path, body);
}

const READ: &str = "concierge=echo wake";

// covers: UC-operator-channel-09
#[test_id(tt9s00u)]
#[test]
fn an_unread_backlog_past_the_threshold_mails_the_operator_once_naming_the_mailbox() {
    let fx = Fx::new("mail-health-once");
    fx.deliver("concierge", "a", 120);
    assert_eq!(fx.run(READ), 1);
    let sent = fx.operator();
    assert!(sent.len() == 1 && sent[0].contains("concierge"), "{sent:?}");
    assert_eq!(fx.run(READ), 0);
    assert_eq!(fx.operator().len(), 1);
}

// covers: UC-operator-channel-09
#[test_id(tiybnht)]
#[test]
fn a_drained_mailbox_clears_the_alarm_state_and_a_recurring_backlog_alerts_again() {
    let fx = Fx::new("mail-health-rearm");
    fx.deliver("concierge", "a", 120);
    assert_eq!(fx.run(READ), 1);
    fx.drain("concierge");
    assert_eq!(fx.run(READ), 0);
    assert!(!fx.state("concierge").exists());
    fx.deliver("concierge", "b", 150);
    assert_eq!(fx.run(READ), 1);
    assert_eq!(fx.operator().len(), 2);
}

// covers: UC-operator-channel-09
#[test_id(tlhsyj4)]
#[test]
fn fresh_mail_below_the_threshold_never_alerts() {
    let fx = Fx::new("mail-health-fresh");
    fx.deliver("concierge", "a", 5);
    assert_eq!(fx.run(READ), 0);
    assert!(fx.operator().is_empty());
}

// covers: UC-operator-channel-09
#[test_id(t7h8fph)]
#[test]
fn a_dead_aerc_outgoing_path_mails_the_operator_once_and_no_accounts_conf_is_silent() {
    let fx = Fx::new("mail-health-outbound-dead");
    assert_eq!(fx.run(""), 0);
    fx.aerc_outgoing(&format!("{}/gone/mail.sh sendmail", fx.t.path().display()));
    assert_eq!(fx.run(""), 1);
    assert_eq!(fx.operator().len(), 1);
    assert_eq!(fx.run(""), 0);
    assert_eq!(fx.operator().len(), 1);
}

// covers: UC-operator-channel-09
#[test_id(tvroetq)]
#[test]
fn a_spira_sendmail_failing_its_check_is_a_fault_and_a_working_one_clears_the_state() {
    let fx = Fx::new("mail-health-outbound-check");
    let wr = fx.t.path().join("wr");
    fs::create_dir_all(&wr).unwrap();
    let sendmail = wr.join("spira-sendmail");
    exe(&sendmail, "#!/bin/sh\n[ \"$1\" = --check ] && exit 1\nexit 0\n");
    fx.aerc_outgoing(sendmail.to_str().unwrap());
    assert_eq!(fx.run(""), 1);
    assert!(fx.state("outbound").exists());
    exe(&sendmail, "#!/bin/sh\nexit 0\n");
    assert_eq!(fx.run(""), 0);
    assert!(!fx.state("outbound").exists());
}
