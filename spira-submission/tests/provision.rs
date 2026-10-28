use spira_submission::clock;
use spira_submission::provision::*;
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use testkit::TempDir;

const FIXTURE_CERT: &str = include_str!("fixtures/test-cert.pem");
const FIXTURE_KEY: &str = include_str!("fixtures/test-key.pem");
const NOT_AFTER: i64 = 4_945_546_720;
const DAY: i64 = 86400;
const HASH: &str = "{SHA512-CRYPT}$6$abcdefgh$yIZAF3gQPvtKZO/9qOJKffAKKbtS3ef3qmwyugk4uWVjX8YZf/GV3A8SkFxEPY0T56CcilGrHKLffBsp6dLMG.";
const STATUS_JSON: &str = r#"{"Self":{"DNSName":"box.tail1234.ts.net."}}"#;
const IP_ADDR: &str = "1: lo    inet 127.0.0.1/8 scope host lo\n4: tailscale0    inet 100.64.0.9/32 scope global tailscale0\n";

struct Fake {
    now: i64,
    calls: RefCell<Vec<String>>,
    tailscale_up: bool,
    ufw_status: Result<String, String>,
    fresh_cert: String,
}

impl Host for Fake {
    fn run(&self, argv: &[&str]) -> Result<String, String> {
        let mut named: Vec<&str> = argv.to_vec();
        named[0] = named[0].rsplit('/').next().unwrap_or(named[0]);
        let argv = &named[..];
        self.calls.borrow_mut().push(argv.join(" "));
        match argv {
            ["tailscale", "ip", "-4"] if self.tailscale_up => Ok("100.64.0.9\n".into()),
            ["tailscale", "status", "--json"] if self.tailscale_up => Ok(STATUS_JSON.into()),
            ["tailscale", ..] => Err("tailscale is not running".into()),
            ["ip", ..] => Ok(IP_ADDR.into()),
            ["sudo", "-n", "ufw", "status"] => self.ufw_status.clone(),
            ["spira-config", "set", ..] | ["systemctl", ..] | ["sudo", "-n", "ufw", "allow", ..] => Ok(String::new()),
            _ => Err(format!("unexpected {argv:?}")),
        }
    }
    fn now(&self) -> i64 {
        self.now
    }
}

struct Cert<'a>(&'a Fake);

impl Host for Cert<'_> {
    fn run(&self, argv: &[&str]) -> Result<String, String> {
        if let ["tailscale", "cert", "--cert-file", c, "--key-file", k, name] = argv {
            self.0.calls.borrow_mut().push(argv.join(" "));
            assert_eq!(*name, "box.tail1234.ts.net");
            std::fs::write(c, &self.0.fresh_cert).unwrap();
            std::fs::write(k, FIXTURE_KEY).unwrap();
            return Ok(String::new());
        }
        self.0.run(argv)
    }
    fn now(&self) -> i64 {
        self.0.now
    }
}

fn fake(now: i64) -> Fake {
    Fake { now, calls: RefCell::new(vec![]), tailscale_up: true, ufw_status: Ok("Status: active\n".into()), fresh_cert: FIXTURE_CERT.replace("MII", "MIJ") }
}

struct Box {
    _d: TempDir,
    plan: Plan,
}

fn plan(configured: bool) -> Box {
    let d = TempDir::new("provision");
    let dir = d.join("submission");
    let users = d.join("users");
    std::fs::write(&users, format!("operator:{HASH}:1000:1000::/home/op\nother:{{PLAIN}}x:1:1\n")).unwrap();
    let ufw_conf = d.join("ufw.conf");
    std::fs::write(&ufw_conf, "ENABLED=yes\n").unwrap();
    let p = |n: &str| if configured { dir.join(n).display().to_string() } else { String::new() };
    let plan = Plan {
        toml_spec: format!("{}:{}", d.join("base.toml").display(), d.join("over.toml").display()),
        addr: if configured { "100.64.0.9".into() } else { String::new() },
        cert: p("cert.pem"),
        key: p("key.pem"),
        password_file: p("password"),
        from: if configured { "operator@spira".into() } else { String::new() },
        user: "operator".into(),
        port: 2587,
        rcpt_domain: "spira".into(),
        users_file: users.display().to_string(),
        dir,
        ufw_conf,
        unit: "spira-submission-prod.service".into(),
    };
    Box { _d: d, plan }
}

fn seed_cert(b: &Box, pem: &str) {
    std::fs::create_dir_all(&b.plan.dir).unwrap();
    std::fs::write(&b.plan.cert, pem).unwrap();
    std::fs::write(&b.plan.key, FIXTURE_KEY).unwrap();
}

fn count(f: &Fake, prefix: &str) -> usize {
    f.calls.borrow().iter().filter(|c| c.starts_with(prefix)).count()
}

#[test]
fn the_certificate_expiry_is_read_from_the_der() {
    let b = plan(true);
    seed_cert(&b, FIXTURE_CERT);
    assert_eq!(cert_days(Path::new(&b.plan.cert), NOT_AFTER - 10 * DAY), Some(10));
    assert_eq!(cert_days(Path::new(&b.plan.cert), NOT_AFTER + DAY), Some(-1));
    assert_eq!(cert_days(Path::new("/nonexistent"), 0), None);
    assert_eq!(clock::iso(NOT_AFTER), "2126-09-20T02:58:40Z");
}

#[test]
fn a_certificate_near_expiry_is_renewed_and_the_service_restarted() {
    let b = plan(true);
    seed_cert(&b, FIXTURE_CERT);
    let f = fake(NOT_AFTER - 10 * DAY);
    let r = provision(&Cert(&f), &b.plan);
    assert!(r.errors.is_empty(), "{r:?}");
    assert_eq!(count(&f, "tailscale cert"), 1, "{:?}", f.calls);
    assert_eq!(count(&f, "systemctl --user restart spira-submission-prod.service"), 1, "{:?}", f.calls);
    assert_eq!(std::fs::read_to_string(&b.plan.cert).unwrap(), f.fresh_cert);
    assert_eq!(std::fs::metadata(&b.plan.key).unwrap().permissions().mode() & 0o077, 0);
}

#[test]
fn a_certificate_with_time_left_is_left_alone() {
    let b = plan(true);
    seed_cert(&b, FIXTURE_CERT);
    let f = fake(NOT_AFTER - 60 * DAY);
    let r = provision(&Cert(&f), &b.plan);
    assert!(r.errors.is_empty(), "{r:?}");
    assert_eq!(count(&f, "tailscale cert"), 0, "{:?}", f.calls);
    assert_eq!(count(&f, "systemctl"), 0);
}

#[test]
fn a_renewal_that_changes_nothing_does_not_restart() {
    let b = plan(true);
    seed_cert(&b, FIXTURE_CERT);
    let mut f = fake(NOT_AFTER - 10 * DAY);
    f.fresh_cert = FIXTURE_CERT.to_string();
    let r = provision(&Cert(&f), &b.plan);
    assert_eq!(count(&f, "tailscale cert"), 1);
    assert_eq!(count(&f, "systemctl"), 0, "{r:?}");
}

#[test]
fn a_missing_certificate_is_fetched() {
    let b = plan(true);
    let f = fake(NOT_AFTER - 60 * DAY);
    provision(&Cert(&f), &b.plan);
    assert_eq!(count(&f, "tailscale cert"), 1);
    assert!(Path::new(&b.plan.cert).exists());
}

#[test]
fn an_unconfigured_install_gets_the_tailnet_address_and_its_files_configured() {
    let b = plan(false);
    let f = fake(NOT_AFTER - 60 * DAY);
    let r = provision(&Cert(&f), &b.plan);
    assert!(r.errors.is_empty(), "{r:?}");
    let over = b._d.join("over.toml").display().to_string();
    let d = b.plan.dir.display();
    for (k, v) in [
        ("submission_addr", "100.64.0.9".to_string()),
        ("submission_cert", format!("{d}/cert.pem")),
        ("submission_key", format!("{d}/key.pem")),
        ("submission_password_file", format!("{d}/password")),
        ("submission_from", "operator@spira".to_string()),
    ] {
        let want = format!("spira-config set spira.{k} {v} {over}");
        assert!(f.calls.borrow().contains(&want), "missing {want}: {:?}", f.calls);
    }
    assert_eq!(count(&f, "systemctl --user restart"), 1);
    assert_eq!(std::fs::read_to_string(format!("{d}/password")).unwrap(), format!("operator:{HASH}\n"));
}

#[test]
fn an_address_the_operator_chose_is_not_overwritten() {
    let mut b = plan(true);
    b.plan.addr = "100.64.0.77".into();
    seed_cert(&b, FIXTURE_CERT);
    let f = fake(NOT_AFTER - 60 * DAY);
    provision(&Cert(&f), &b.plan);
    assert_eq!(count(&f, "spira-config set"), 0, "{:?}", f.calls);
}

#[test]
fn without_tailscale_nothing_is_configured() {
    let b = plan(false);
    let mut f = fake(0);
    f.tailscale_up = false;
    let r = provision(&f, &b.plan);
    assert_eq!(count(&f, "spira-config set"), 0);
    assert_eq!(count(&f, "systemctl"), 0);
    assert!(r.errors.is_empty(), "{r:?}");
}

#[test]
fn the_password_file_follows_the_users_file_and_drift_is_seen() {
    let b = plan(true);
    seed_cert(&b, FIXTURE_CERT);
    let pw = b.plan.password_file.clone();
    assert_eq!(password_state(&b.plan, &pw), Password::Missing);

    let f = fake(NOT_AFTER - 60 * DAY);
    provision(&Cert(&f), &b.plan);
    assert_eq!(password_state(&b.plan, &pw), Password::Ok);
    assert_eq!(std::fs::metadata(&pw).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(count(&f, "systemctl"), 0, "the server reads the password per connection");

    std::fs::write(&b.plan.users_file, "operator:{SHA512-CRYPT}$6$zzzzzzzz$rotated:1000:1000::/h\n").unwrap();
    assert_eq!(password_state(&b.plan, &pw), Password::Drifted);
    provision(&Cert(&f), &b.plan);
    assert_eq!(password_state(&b.plan, &pw), Password::Ok);

    std::fs::set_permissions(&pw, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(password_state(&b.plan, &pw), Password::Mode);
    provision(&Cert(&f), &b.plan);
    assert_eq!(password_state(&b.plan, &pw), Password::Ok);

    std::fs::write(&b.plan.users_file, "other:x:1:1\n").unwrap();
    assert!(matches!(password_state(&b.plan, &pw), Password::NoSource(_)));
}

#[test]
fn the_derived_line_is_a_credential_the_server_accepts() {
    let line = derive_line(&format!("operator:{HASH}:1000:1000::/h\n"), "operator").unwrap();
    let c = spira_submission::credential::Credential::parse(&line, "operator").unwrap();
    assert_eq!(c, spira_submission::credential::Credential::parse(HASH, "operator").unwrap());
    assert_eq!(derive_line("operator::1:1\n", "operator"), None);
}

#[test]
fn a_closed_port_is_opened_on_the_tailnet_interface_once() {
    let b = plan(true);
    seed_cert(&b, FIXTURE_CERT);
    let f = fake(NOT_AFTER - 60 * DAY);
    provision(&Cert(&f), &b.plan);
    assert_eq!(count(&f, "sudo -n ufw allow in on tailscale0 to any port 2587 proto tcp"), 1, "{:?}", f.calls);

    let mut open = fake(NOT_AFTER - 60 * DAY);
    open.ufw_status = Ok("Status: active\n\nTo   Action   From\n2587/tcp on tailscale0   ALLOW   Anywhere\n".into());
    provision(&Cert(&open), &b.plan);
    assert_eq!(count(&open, "sudo -n ufw allow"), 0, "{:?}", open.calls);
}

#[test]
fn a_firewall_that_cannot_be_opened_is_an_error_carrying_the_root_command() {
    let b = plan(true);
    seed_cert(&b, FIXTURE_CERT);
    let mut f = fake(NOT_AFTER - 60 * DAY);
    f.ufw_status = Err("sudo: a password is required".into());
    let r = provision(&Cert(&f), &b.plan);
    assert!(r.errors.iter().any(|e| e.contains("ufw allow in on tailscale0 to any port 2587 proto tcp")), "{r:?}");
}

#[test]
fn ufw_rules_are_matched_by_port_and_interface() {
    let st = "To  Action  From\n2587/tcp on tailscale0  ALLOW  Anywhere\n22/tcp  ALLOW  Anywhere\n";
    assert!(ufw_allows(st, 2587, "tailscale0"));
    assert!(!ufw_allows(st, 2587, "eth0"));
    assert!(!ufw_allows(st, 2588, "tailscale0"));
    assert!(ufw_allows(st, 22, "tailscale0"));
    assert!(!ufw_allows("2587/tcp on tailscale0  DENY  Anywhere\n", 2587, "tailscale0"));
}

#[test]
fn the_firewall_state_distinguishes_absent_closed_open_and_unreadable() {
    let b = plan(true);
    let f = fake(0);
    assert_eq!(firewall_state(&f, &b.plan, "tailscale0"), Firewall::Closed);
    let mut unreadable = fake(0);
    unreadable.ufw_status = Err("denied".into());
    assert!(matches!(firewall_state(&unreadable, &b.plan, "tailscale0"), Firewall::Unknown(_)));
    std::fs::write(&b.plan.ufw_conf, "ENABLED=no\n").unwrap();
    assert_eq!(firewall_state(&f, &b.plan, "tailscale0"), Firewall::Absent);
    let mut open = fake(0);
    std::fs::write(&b.plan.ufw_conf, "ENABLED=yes\n").unwrap();
    open.ufw_status = Ok("2587/tcp on tailscale0 ALLOW Anywhere\n".into());
    assert_eq!(firewall_state(&open, &b.plan, "tailscale0"), Firewall::Open);
}

#[test]
fn status_reports_what_doctor_reads() {
    let b = plan(true);
    seed_cert(&b, FIXTURE_CERT);
    let f = fake(NOT_AFTER - 10 * DAY);
    let s = status(&f, &b.plan);
    for want in ["configured=1", "tailnet=1", "listening=0", "iface=tailscale0", "firewall=closed", "cert_days=10", "password=missing"] {
        assert!(s.lines().any(|l| l == want), "missing {want}: {s}");
    }
    assert_eq!(status(&f, &plan(false).plan), "configured=0\ntailnet=1\n");
}


struct NoOperator<'a>(&'a Fake);

impl Host for NoOperator<'_> {
    fn run(&self, argv: &[&str]) -> Result<String, String> {
        if argv.starts_with(&["tailscale", "cert"]) {
            self.0.calls.borrow_mut().push(argv.join(" "));
            return Err("tailscale cert exited 1: Use 'sudo tailscale cert ...'. To not require root, use 'sudo tailscale set --operator=$USER' once.".into());
        }
        if argv.starts_with(&["tailscale", "debug", "prefs"]) {
            return Ok(r#"{"OperatorUser":""}"#.into());
        }
        self.0.run(argv)
    }
    fn now(&self) -> i64 {
        self.0.now
    }
}

#[test]
fn a_failed_certificate_leaves_the_submission_keys_unset() {
    let b = plan(false);
    let f = fake(NOT_AFTER - 60 * DAY);
    let r = provision(&NoOperator(&f), &b.plan);
    assert_eq!(count(&f, "tailscale cert"), 1, "{:?}", f.calls);
    assert_eq!(count(&f, "spira-config set"), 0, "{:?}", f.calls);
    assert_eq!(count(&f, "systemctl"), 0);
    assert!(r.errors.is_empty(), "{r:?}");
    assert!(r.notes.iter().any(|n| n.contains("sudo tailscale set --operator=$USER")), "{r:?}");
}

#[test]
fn the_address_is_written_last() {
    let b = plan(false);
    let f = fake(NOT_AFTER - 60 * DAY);
    provision(&Cert(&f), &b.plan);
    let sets: Vec<String> = f.calls.borrow().iter().filter(|c| c.starts_with("spira-config set")).cloned().collect();
    assert_eq!(sets.len(), 5, "{sets:?}");
    assert!(sets[4].contains("spira.submission_addr"), "{sets:?}");
}

#[test]
fn status_reports_a_missing_operator_grant() {
    let b = plan(false);
    let f = fake(0);
    let _env = testkit::env(&[("USER", Some("someone"))]);
    let s = status(&NoOperator(&f), &b.plan);
    assert!(s.lines().any(|l| l == "operator=missing"), "{s}");
}
