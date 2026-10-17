use spira_submission::{connect_tls, health, serve, Config, SendmailSink};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use testkit::TempDir;

const PASSWORD: &str = "correct horse";
const HASH: &str = "{SHA512-CRYPT}$6$abcdefgh$yIZAF3gQPvtKZO/9qOJKffAKKbtS3ef3qmwyugk4uWVjX8YZf/GV3A8SkFxEPY0T56CcilGrHKLffBsp6dLMG.";

struct Harness {
    dir: TempDir,
    addr: SocketAddr,
}

fn b64(s: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for ch in s.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= ch.len() { T[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

struct LogSink {
    inner: SendmailSink,
    lines: Arc<Mutex<Vec<String>>>,
}

impl spira_submission::smtp::Sink for LogSink {
    fn deliver(&self, message: &[u8]) -> Result<(), String> {
        self.inner.deliver(message)
    }
    fn log(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_string());
    }
}

fn start(sendmail_body: &str) -> Harness {
    start_logged(sendmail_body).0
}

fn start_logged(sendmail_body: &str) -> (Harness, Arc<Mutex<Vec<String>>>) {
    let dir = TempDir::new("submission");
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let pw = dir.join("pw");
    std::fs::write(&pw, format!("{HASH}\n")).unwrap();
    std::fs::set_permissions(&pw, std::fs::Permissions::from_mode(0o600)).unwrap();
    let script = dir.join("fake-sendmail");
    testkit::write_exe(&script, &format!("#!/bin/sh\n{sendmail_body}\n"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = Config {
        addr: addr.ip(),
        port: addr.port(),
        cert: fixtures.join("test-cert.pem"),
        key: fixtures.join("test-key.pem"),
        password_file: pw,
        user: "operator".into(),
        from: "operator@example.org".into(),
        rcpt_domain: "spira".into(),
        sendmail: vec![script.display().to_string()],
    };
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(LogSink { inner: SendmailSink { argv: cfg.sendmail.clone() }, lines: lines.clone() });
    std::thread::spawn(move || serve(listener, cfg, sink, std::sync::Arc::new(spira_otel::metrics::MetricsSink::new(None, "t", std::time::Duration::from_secs(1)))));
    (Harness { dir, addr }, lines)
}

struct Client<S: Read + Write> {
    s: S,
}

impl<S: Read + Write> Client<S> {
    fn line(&mut self) -> String {
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        while self.s.read(&mut b).map(|n| n == 1).unwrap_or(false) {
            out.push(b[0]);
            if b[0] == b'\n' {
                break;
            }
        }
        String::from_utf8_lossy(&out).trim_end().to_string()
    }
    fn reply(&mut self) -> String {
        loop {
            let l = self.line();
            if l.len() < 4 || l.as_bytes()[3] != b'-' {
                return l;
            }
        }
    }
    fn say(&mut self, cmd: &str) -> String {
        self.s.write_all(format!("{cmd}\r\n").as_bytes()).unwrap();
        self.reply()
    }
}

fn client(h: &Harness) -> Client<impl Read + Write> {
    let mut c = Client { s: connect_tls(h.addr).unwrap() };
    assert!(c.reply().starts_with("220 "));
    assert!(c.say("EHLO test").starts_with("250"));
    c
}

fn login(c: &mut Client<impl Read + Write>) {
    let r = c.say(&format!("AUTH PLAIN {}", b64(format!("\0operator\0{PASSWORD}").as_bytes())));
    assert!(r.starts_with("235"), "{r}");
}

const MSG: &str = "From: Ryan <operator@example.org>\r\nTo: concierge@spira\r\nSubject: Re: ask\r\nMessage-ID: <new-1@phone>\r\nIn-Reply-To: <ask-77@spira>\r\nReferences: <root@spira> <ask-77@spira>\r\n\r\nyes, go\r\n.. a dot line\r\n.\r\n";

fn send(c: &mut Client<impl Read + Write>) -> String {
    assert!(c.say("MAIL FROM:<operator@example.org>").starts_with("250"));
    assert!(c.say("RCPT TO:<concierge@spira>").starts_with("250"));
    assert!(c.say("DATA").starts_with("354"));
    c.s.write_all(MSG.as_bytes()).unwrap();
    c.reply()
}

#[test]
fn mail_is_refused_before_authentication() {
    let h = start("cat >/dev/null");
    let mut c = client(&h);
    assert!(c.say("MAIL FROM:<operator@example.org>").starts_with("530"));
    assert!(c.say("RCPT TO:<concierge@spira>").starts_with("530"));
    assert!(c.say("DATA").starts_with("530"));
}

#[test]
fn a_wrong_password_is_refused_and_the_right_one_is_not() {
    let h = start("cat >/dev/null");
    let mut c = client(&h);
    let r = c.say(&format!("AUTH PLAIN {}", b64(b"\0operator\0wrong")));
    assert!(r.starts_with("535"), "{r}");
    let r = c.say(&format!("AUTH PLAIN {}", b64(format!("\0someone\0{PASSWORD}").as_bytes())));
    assert!(r.starts_with("535"), "{r}");
    login(&mut c);
}

#[test]
fn auth_login_works_and_repeated_failures_drop_the_connection() {
    let h = start("cat >/dev/null");
    let mut c = client(&h);
    assert!(c.say("AUTH LOGIN").starts_with("334"));
    assert!(c.say(&b64(b"operator")).starts_with("334"));
    assert!(c.say(&b64(PASSWORD.as_bytes())).starts_with("235"));

    let mut c = client(&h);
    assert!(c.say(&format!("AUTH PLAIN {}", b64(b"\0operator\0a"))).starts_with("535"));
    assert!(c.say(&format!("AUTH PLAIN {}", b64(b"\0operator\0b"))).starts_with("535"));
    assert!(c.say(&format!("AUTH PLAIN {}", b64(b"\0operator\0c"))).starts_with("421"));
}

#[test]
fn a_non_spira_recipient_is_refused_at_rcpt_and_so_is_a_foreign_sender() {
    let h = start("cat >/dev/null");
    let mut c = client(&h);
    login(&mut c);
    let r = c.say("MAIL FROM:<stranger@example.org>");
    assert!(r.starts_with("550"), "{r}");
    assert!(c.say("MAIL FROM:<operator@example.org>").starts_with("250"));
    for bad in ["victim@example.com", "concierge@spira.local", "concierge", "a@spira@evil.invalid", "<>"] {
        let r = c.say(&format!("RCPT TO:<{bad}>"));
        assert!(r.starts_with("550") || r.starts_with("501"), "{bad}: {r}");
    }
    assert!(c.say("DATA").starts_with("503"), "no accepted recipient, no DATA");
    assert!(c.say("RCPT TO:<concierge@spira>").starts_with("250"));
}

#[test]
fn plaintext_is_refused() {
    let h = start("cat >/dev/null");
    let mut tcp = TcpStream::connect(h.addr).unwrap();
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let _ = tcp.write_all(b"EHLO plain\r\n");
    let mut got = Vec::new();
    let _ = tcp.read_to_end(&mut got);
    assert!(!String::from_utf8_lossy(&got).contains("220"), "an SMTP greeting went out in the clear: {got:?}");
    assert!(!String::from_utf8_lossy(&got).contains("250"), "{got:?}");
}

#[test]
fn an_accepted_message_reaches_sendmail_with_its_thread_headers_intact() {
    let h = start("cat > \"$(dirname \"$0\")/got\"");
    let mut c = client(&h);
    login(&mut c);
    let r = send(&mut c);
    assert!(r.starts_with("250"), "{r}");
    let got = std::fs::read_to_string(h.dir.join("got")).unwrap();
    assert!(got.contains("\nIn-Reply-To: <ask-77@spira>\n"), "{got}");
    assert!(got.contains("\nReferences: <root@spira> <ask-77@spira>\n"), "{got}");
    assert!(got.contains("\nMessage-ID: <new-1@phone>\n"), "{got}");
    assert!(got.contains("\n\nyes, go\n. a dot line\n"), "dot-unstuffed, LF line ends: {got:?}");
    assert!(!got.contains('\r'));
}

#[test]
fn a_sendmail_failure_is_a_4xx_so_the_client_retries() {
    let h = start("cat >/dev/null; echo boom >&2; exit 1");
    let mut c = client(&h);
    login(&mut c);
    let r = send(&mut c);
    assert!(r.starts_with("451"), "{r}");
}

#[test]
fn health_sees_the_banner() {
    let h = start("cat >/dev/null");
    assert!(health(h.addr).unwrap().contains("spira-submission"));
}

#[test]
fn an_unconfigured_or_wildcard_address_never_listens() {
    let vals = |addr: &'static str| move |k: &str| -> Result<String, String> {
        Ok(match k {
            "SPIRA_SUBMISSION_ADDR" => addr,
            "SPIRA_SUBMISSION_PORT" => "2587",
            _ => "x",
        }
        .to_string())
    };
    assert!(Config::from_values(&vals(""), vec![]).unwrap().is_none());
    assert!(Config::from_values(&vals("0.0.0.0"), vec![]).unwrap_err().contains("wildcard"));
    assert!(Config::from_values(&vals("100.64.0.1"), vec![]).unwrap().is_some());
    let no_cert = |k: &str| -> Result<String, String> {
        Ok(match k {
            "SPIRA_SUBMISSION_ADDR" => "100.64.0.1",
            "SPIRA_SUBMISSION_PORT" => "2587",
            "SPIRA_SUBMISSION_CERT" => "",
            _ => "x",
        }
        .to_string())
    };
    assert!(Config::from_values(&no_cert, vec![]).unwrap_err().contains("SPIRA_SUBMISSION_CERT"));
}

fn logged(lines: &Arc<Mutex<Vec<String>>>, needle: &str) -> bool {
    for _ in 0..50 {
        if lines.lock().unwrap().iter().any(|l| l.contains(needle)) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

#[test]
fn every_attempt_leaves_one_line_naming_peer_user_sender_and_outcome() {
    let (h, lines) = start_logged("cat >/dev/null");
    let mut c = client(&h);
    let bad = c.say(&format!("AUTH PLAIN {}", b64(b"\0operator\0wrong")));
    assert!(bad.starts_with("535"), "{bad}");
    assert!(logged(&lines, "user=\"operator\" from=\"\" auth: refused: authentication failed"), "{:?}", lines.lock().unwrap());
    login(&mut c);
    assert!(c.say("MAIL FROM:<stranger@example.org>").starts_with("550"));
    assert!(logged(&lines, "from=\"stranger@example.org\" mail: refused: sender is not the operator address"), "{:?}", lines.lock().unwrap());
    assert!(c.say("MAIL FROM:<operator@example.org>").starts_with("250"));
    assert!(c.say("RCPT TO:<victim@example.com>").starts_with("550"));
    assert!(logged(&lines, "rcpt: refused: relay denied to \"victim@example.com\""), "{:?}", lines.lock().unwrap());
    assert!(c.say("RCPT TO:<concierge@spira>").starts_with("250"));
    assert!(c.say("DATA").starts_with("354"));
    c.s.write_all(MSG.as_bytes()).unwrap();
    assert!(c.reply().starts_with("250"));
    assert!(logged(&lines, "from=\"operator@example.org\" data: accepted to=concierge@spira message-id=\"<new-1@phone>\""), "{:?}", lines.lock().unwrap());
    let all = lines.lock().unwrap();
    assert!(all.iter().all(|l| l.contains("peer=127.0.0.1:") && l.starts_with("ts=20")), "{all:?}");
}

#[test]
fn a_failed_hand_off_is_logged_as_a_refusal_with_its_reason() {
    let (h, lines) = start_logged("echo boom >&2; exit 3");
    let mut c = client(&h);
    login(&mut c);
    assert!(send(&mut c).starts_with("451"));
    assert!(logged(&lines, "data: refused: delivery failed:"), "{:?}", lines.lock().unwrap());
}

#[test]
fn a_connection_that_never_finishes_tls_is_logged() {
    let (h, lines) = start_logged("cat >/dev/null");
    let mut tcp = TcpStream::connect(h.addr).unwrap();
    tcp.write_all(b"EHLO plaintext\r\n").unwrap();
    assert!(logged(&lines, "tls: refused"), "{:?}", lines.lock().unwrap());
}
