use crate::credential::Credential;
use std::io::{Read, Write};
use std::path::PathBuf;

const MAX_COMMAND_LINE: usize = 4096;
const MAX_DATA_LINE: usize = 256 * 1024;
const MAX_RCPT: usize = 20;
const MAX_AUTH_FAILURES: u32 = 3;

#[derive(Clone, Debug)]
pub struct Policy {
    pub host: String,
    pub user: String,
    pub from: String,
    pub rcpt_domain: String,
    pub password_file: PathBuf,
    pub max_message_bytes: usize,
    pub peer: String,
}

/// The envelope of one finished DATA phase: who sent what to whom, as the session saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope<'a> {
    pub peer: &'a str,
    pub user: &'a str,
    pub from: &'a str,
    pub to: &'a [String],
    pub message_id: &'a str,
    pub in_reply_to: &'a str,
}

pub trait Sink {
    fn deliver(&self, message: &[u8]) -> Result<(), String>;

    /// One accepted message with its envelope: the client is answered `250` on `Ok` and `451`
    /// on `Err`. A sink that records the session (the scheduler's `submission` source) overrides
    /// this; the default delivers the message alone.
    fn accept(&self, envelope: &Envelope, message: &[u8]) -> Result<(), String> {
        let _ = envelope;
        self.deliver(message)
    }

    /// A session outcome the client was refused (auth, sender, relay, size, TLS or delivery),
    /// with the reason as the audit line names it. The default records nothing beyond the log.
    fn refused(&self, peer: &str, user: &str, from: &str, reason: &str) {
        let _ = (peer, user, from, reason);
    }

    fn log(&self, line: &str) {
        eprintln!("spira-submission: {line}");
    }
}

/// The audit line for one session outcome.
pub fn attempt_line(peer: &str, user: &str, from: &str, outcome: &str) -> String {
    format!("ts={} peer={peer} user={user:?} from={from:?} {outcome}", crate::clock::iso(crate::clock::now()))
}

/// The reason in an outcome that was a refusal.
pub fn refusal_reason(outcome: &str) -> Option<&str> {
    outcome.split_once("refused: ").map(|(_, reason)| reason)
}

/// The audit line for one session outcome, and, for a refusal, the sink's [`Sink::refused`].
pub fn attempt(sink: &dyn Sink, peer: &str, user: &str, from: &str, outcome: &str) {
    sink.log(&attempt_line(peer, user, from, outcome));
    if let Some(reason) = refusal_reason(outcome) {
        sink.refused(peer, user, from, reason);
    }
}

/// An [`Envelope`] a session owns, handed out with an accepted message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedEnvelope {
    pub peer: String,
    pub user: String,
    pub from: String,
    pub to: Vec<String>,
    pub message_id: String,
    pub in_reply_to: String,
}

impl OwnedEnvelope {
    pub fn as_envelope(&self) -> Envelope<'_> {
        Envelope { peer: &self.peer, user: &self.user, from: &self.from, to: &self.to, message_id: &self.message_id, in_reply_to: &self.in_reply_to }
    }
}

/// What a [`Session`] asks its driver to do. `Verify` and `Accept` park the session until the
/// driver answers with [`Session::verified`] or [`Session::accepted`]; bytes fed meanwhile wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Bytes for the client, CRLF included.
    Send(String),
    /// Check this password against the credential file; `user` is judged by the session.
    Verify { password: String },
    /// A finished DATA phase: deliver it, answer `250` on `Ok` and `451` on `Err`.
    Accept { envelope: OwnedEnvelope, message: Vec<u8> },
    /// One outcome for the audit line; a refusal also reaches [`Sink::refused`] (see [`attempt`]).
    Attempt { user: String, from: String, outcome: String },
    /// End the connection.
    Close,
}

enum Line {
    Text(Vec<u8>),
    TooLong,
}

enum Mode {
    Command,
    AuthPlain,
    AuthLoginUser,
    AuthLoginPass { user_b64: String },
    Verifying { tried: String, user_ok: bool },
    Data { out: Vec<u8>, too_big: bool, sender: String, to: Vec<String> },
    Accepting { sender: String, to: Vec<String>, message_id: String, in_reply_to: String },
}

/// The SMTP protocol of one submission connection with no IO: bytes in, [`Step`]s out. The
/// steps are the same however the bytes are cut.
pub struct Session {
    p: Policy,
    buf: Vec<u8>,
    skipping: bool,
    mode: Mode,
    authed: bool,
    who: String,
    failures: u32,
    from: Option<String>,
    rcpts: Vec<String>,
    closed: bool,
    out: Vec<Step>,
}

impl Session {
    pub fn new(p: Policy) -> Session {
        Session { p, buf: Vec::new(), skipping: false, mode: Mode::Command, authed: false, who: String::new(), failures: 0, from: None, rcpts: Vec::new(), closed: false, out: Vec::new() }
    }

    /// The banner.
    pub fn start(&mut self) -> Vec<Step> {
        self.reply(&format!("220 {} spira-submission ESMTP", self.p.host));
        self.take()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Step> {
        if !self.closed {
            self.buf.extend_from_slice(bytes);
            self.pump();
        }
        self.take()
    }

    /// The driver's answer to [`Step::Verify`].
    pub fn verified(&mut self, ok: bool) -> Vec<Step> {
        if let Mode::Verifying { tried, user_ok } = std::mem::replace(&mut self.mode, Mode::Command) {
            self.auth_done(ok && user_ok, tried);
            self.pump();
        }
        self.take()
    }

    /// The driver's answer to [`Step::Accept`].
    pub fn accepted(&mut self, outcome: Result<(), String>) -> Vec<Step> {
        if let Mode::Accepting { sender, to, message_id, in_reply_to } = std::mem::replace(&mut self.mode, Mode::Command) {
            let state = match &outcome {
                Ok(()) => "accepted".to_string(),
                Err(e) => format!("refused: delivery failed: {}", clean(e)),
            };
            let user = self.who.clone();
            self.attempt(&user, &sender, &format!("data: {state} to={} message-id={message_id:?} in-reply-to={in_reply_to:?}", clean(&to.join(","))));
            match outcome {
                Ok(()) => self.reply("250 2.0.0 queued"),
                Err(_) => self.reply("451 4.3.0 local delivery failed, try again"),
            }
            self.pump();
        }
        self.take()
    }

    fn take(&mut self) -> Vec<Step> {
        std::mem::take(&mut self.out)
    }

    fn reply(&mut self, text: &str) {
        self.out.push(Step::Send(format!("{text}\r\n")));
    }

    fn attempt(&mut self, user: &str, from: &str, outcome: &str) {
        self.out.push(Step::Attempt { user: user.into(), from: from.into(), outcome: outcome.into() });
    }

    fn close(&mut self) {
        self.closed = true;
        self.out.push(Step::Close);
    }

    fn waiting(&self) -> bool {
        matches!(self.mode, Mode::Verifying { .. } | Mode::Accepting { .. })
    }

    fn next_line(&mut self) -> Option<Line> {
        let max = if matches!(self.mode, Mode::Data { .. }) { MAX_DATA_LINE } else { MAX_COMMAND_LINE };
        loop {
            let nl = self.buf.iter().position(|&b| b == b'\n');
            if self.skipping {
                match nl {
                    Some(i) => {
                        self.buf.drain(..=i);
                        self.skipping = false;
                        continue;
                    }
                    None => {
                        self.buf.clear();
                        return None;
                    }
                }
            }
            return match nl {
                Some(i) if i > max => {
                    self.buf.drain(..=i);
                    Some(Line::TooLong)
                }
                Some(i) => {
                    let mut line: Vec<u8> = self.buf.drain(..=i).collect();
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    Some(Line::Text(line))
                }
                None if self.buf.len() > max => {
                    self.buf.clear();
                    self.skipping = true;
                    Some(Line::TooLong)
                }
                None => None,
            };
        }
    }

    fn pump(&mut self) {
        while !self.closed && !self.waiting() {
            let Some(line) = self.next_line() else { return };
            match line {
                Line::Text(l) => self.line(String::from_utf8_lossy(&l).into_owned(), l),
                Line::TooLong => match &mut self.mode {
                    Mode::Data { too_big, .. } => *too_big = true,
                    Mode::Command => {
                        self.reply("500 5.5.2 line too long");
                        self.close();
                    }
                    _ => self.close(),
                },
            }
        }
    }

    fn line(&mut self, text: String, raw: Vec<u8>) {
        match std::mem::replace(&mut self.mode, Mode::Command) {
            Mode::Command => self.command(&text),
            Mode::AuthPlain => self.auth_plain(&text),
            Mode::AuthLoginUser => self.auth_login_user(text),
            Mode::AuthLoginPass { user_b64 } => self.auth_login_pass(&user_b64, &text),
            Mode::Data { out, too_big, sender, to } => self.data_line(raw, out, too_big, sender, to),
            m @ (Mode::Verifying { .. } | Mode::Accepting { .. }) => self.mode = m,
        }
    }

    fn command(&mut self, line: &str) {
        let (verb, rest) = match line.split_once(' ') {
            Some((v, r)) => (v.to_ascii_uppercase(), r),
            None => (line.to_ascii_uppercase(), ""),
        };
        match verb.as_str() {
            "EHLO" | "HELO" => {
                self.from = None;
                self.rcpts.clear();
                if verb == "HELO" {
                    self.reply(&format!("250 {}", self.p.host));
                } else {
                    self.reply(&format!("250-{}\r\n250-AUTH PLAIN LOGIN\r\n250-SIZE {}\r\n250 8BITMIME", self.p.host, self.p.max_message_bytes));
                }
            }
            "NOOP" => self.reply("250 2.0.0 ok"),
            "RSET" => {
                self.from = None;
                self.rcpts.clear();
                self.reply("250 2.0.0 ok");
            }
            "QUIT" => {
                self.reply("221 2.0.0 bye");
                self.close();
            }
            "STARTTLS" => self.reply("502 5.5.1 already encrypted"),
            "AUTH" => {
                if self.authed {
                    self.reply("503 5.5.1 already authenticated");
                } else {
                    self.begin_auth(rest);
                }
            }
            "MAIL" => {
                if !self.authed {
                    self.reply("530 5.7.0 authentication required");
                } else if self.from.is_some() {
                    self.reply("503 5.5.1 sender already given");
                } else {
                    match rest.get(..5).filter(|k| k.eq_ignore_ascii_case("FROM:")).and_then(|_| path_arg(&rest[5..])) {
                        None => self.reply("501 5.5.4 syntax: MAIL FROM:<address>"),
                        Some(a) if a.eq_ignore_ascii_case(&self.p.from) => {
                            self.from = Some(a);
                            self.reply("250 2.1.0 ok");
                        }
                        Some(a) => {
                            let user = self.who.clone();
                            self.attempt(&user, &clean(&a), "mail: refused: sender is not the operator address");
                            self.reply("550 5.7.1 sender is not the operator address");
                        }
                    }
                }
            }
            "RCPT" => {
                if !self.authed {
                    self.reply("530 5.7.0 authentication required");
                } else if self.from.is_none() {
                    self.reply("503 5.5.1 MAIL first");
                } else {
                    match rest.get(..3).filter(|k| k.eq_ignore_ascii_case("TO:")).and_then(|_| path_arg(&rest[3..])) {
                        None => self.reply("501 5.5.4 syntax: RCPT TO:<address>"),
                        Some(a) if !recipient_allowed(&a, &self.p.rcpt_domain) => {
                            let (user, from) = (self.who.clone(), self.from.clone().unwrap_or_default());
                            self.attempt(&user, &from, &format!("rcpt: refused: relay denied to {:?}", clean(&a)));
                            self.reply("550 5.7.1 relay denied: only Spira mailbox addresses are accepted");
                        }
                        Some(_) if self.rcpts.len() >= MAX_RCPT => self.reply("452 4.5.3 too many recipients"),
                        Some(a) => {
                            self.rcpts.push(a);
                            self.reply("250 2.1.5 ok");
                        }
                    }
                }
            }
            "DATA" => {
                if !self.authed {
                    self.reply("530 5.7.0 authentication required");
                } else if self.rcpts.is_empty() {
                    self.reply("503 5.5.1 RCPT first");
                } else {
                    self.reply("354 end data with <CR><LF>.<CR><LF>");
                    self.mode = Mode::Data { out: Vec::new(), too_big: false, sender: self.from.take().unwrap_or_default(), to: std::mem::take(&mut self.rcpts) };
                }
            }
            "" => self.reply("500 5.5.2 empty command"),
            _ => self.reply("502 5.5.1 command not implemented"),
        }
    }

    fn begin_auth(&mut self, rest: &str) {
        let mut words = rest.split_whitespace();
        let mech = words.next().unwrap_or("").to_ascii_uppercase();
        let initial = words.next();
        match mech.as_str() {
            "PLAIN" => match initial {
                Some(i) if i != "=" => self.auth_plain(i),
                _ => {
                    self.reply("334 ");
                    self.mode = Mode::AuthPlain;
                }
            },
            "LOGIN" => match initial {
                Some(i) => self.auth_login_user(i.to_string()),
                None => {
                    self.reply(&format!("334 {}", b64_encode("Username:")));
                    self.mode = Mode::AuthLoginUser;
                }
            },
            _ => {
                self.attempt("", "", "auth: refused: mechanism not supported");
                self.reply("504 5.5.4 mechanism not supported");
            }
        }
    }

    fn auth_plain(&mut self, blob: &str) {
        let Some(raw) = b64_decode(blob) else { return self.auth_done(false, String::new()) };
        let parts: Vec<&[u8]> = raw.split(|&b| b == 0).collect();
        if parts.len() != 3 {
            return self.auth_done(false, String::new());
        }
        let authz = String::from_utf8_lossy(parts[0]).into_owned();
        let user = String::from_utf8_lossy(parts[1]).into_owned();
        if !authz.is_empty() && authz != user {
            return self.auth_done(false, user);
        }
        let password = String::from_utf8_lossy(parts[2]).into_owned();
        self.verify(user, password);
    }

    fn auth_login_user(&mut self, user_b64: String) {
        self.reply(&format!("334 {}", b64_encode("Password:")));
        self.mode = Mode::AuthLoginPass { user_b64 };
    }

    fn auth_login_pass(&mut self, user_b64: &str, pass_b64: &str) {
        let (Some(u), Some(pw)) = (b64_decode(user_b64), b64_decode(pass_b64)) else { return self.auth_done(false, String::new()) };
        self.verify(String::from_utf8_lossy(&u).into_owned(), String::from_utf8_lossy(&pw).into_owned());
    }

    fn verify(&mut self, tried: String, password: String) {
        let user_ok = tried == self.p.user;
        self.mode = Mode::Verifying { tried, user_ok };
        self.out.push(Step::Verify { password });
    }

    fn auth_done(&mut self, ok: bool, tried: String) {
        if ok {
            self.authed = true;
            self.who = tried;
            let user = self.who.clone();
            self.attempt(&user, "", "auth: accepted");
            self.reply("235 2.7.0 authenticated");
            return;
        }
        self.attempt(&clean(&tried), "", "auth: refused: authentication failed");
        self.failures += 1;
        if self.failures >= MAX_AUTH_FAILURES {
            self.reply("421 4.7.0 too many authentication failures");
            self.close();
        } else {
            self.reply("535 5.7.8 authentication failed");
        }
    }

    fn data_line(&mut self, l: Vec<u8>, mut out: Vec<u8>, mut too_big: bool, sender: String, to: Vec<String>) {
        if l != b"." {
            let l = if l.first() == Some(&b'.') { &l[1..] } else { &l[..] };
            if out.len() + l.len() + 1 > self.p.max_message_bytes {
                too_big = true;
            }
            if !too_big {
                out.extend_from_slice(l);
                out.push(b'\n');
            }
            self.mode = Mode::Data { out, too_big, sender, to };
            return;
        }
        let sender = clean(&sender);
        if too_big {
            let user = self.who.clone();
            self.attempt(&user, &sender, "data: refused: message too large");
            self.reply("552 5.3.4 message too large");
            return;
        }
        let text = String::from_utf8_lossy(&out).into_owned();
        let (message_id, in_reply_to) = (header(&text, "message-id"), header(&text, "in-reply-to"));
        let envelope = OwnedEnvelope { peer: self.p.peer.clone(), user: self.who.clone(), from: sender.clone(), to: to.clone(), message_id: message_id.clone(), in_reply_to: in_reply_to.clone() };
        self.mode = Mode::Accepting { sender, to, message_id, in_reply_to };
        self.out.push(Step::Accept { envelope, message: out });
    }
}

/// `password` against the credential file `p` names. A file that cannot be used is a refusal.
pub fn verify_credential(p: &Policy, password: &str) -> bool {
    match Credential::load(&p.password_file, &p.user) {
        Ok(c) => c.verify(password),
        Err(e) => {
            eprintln!("spira-submission: password file unusable: {e}");
            false
        }
    }
}

/// [`Session`] driven over a blocking stream.
pub fn serve_session<S: Read + Write>(mut stream: S, p: &Policy, sink: &dyn Sink) {
    let mut session = Session::new(p.clone());
    let mut steps: std::collections::VecDeque<Step> = session.start().into();
    loop {
        while let Some(step) = steps.pop_front() {
            match step {
                Step::Send(text) => {
                    if stream.write_all(text.as_bytes()).and_then(|_| stream.flush()).is_err() {
                        return;
                    }
                }
                Step::Attempt { user, from, outcome } => attempt(sink, &p.peer, &user, &from, &outcome),
                Step::Verify { password } => steps.extend(session.verified(verify_credential(p, &password))),
                Step::Accept { envelope, message } => steps.extend(session.accepted(sink.accept(&envelope.as_envelope(), &message))),
                Step::Close => return,
            }
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => steps.extend(session.feed(&chunk[..n])),
        }
    }
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.trim().trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn b64_encode(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for ch in s.as_bytes().chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= ch.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `<addr>` out of `MAIL FROM:<addr> SIZE=...`; the empty string for the null sender.
fn path_arg(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let inner = rest.strip_prefix('<')?;
    let end = inner.find('>')?;
    Some(inner[..end].to_string())
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

fn header(raw: &str, name: &str) -> String {
    let mut value = String::new();
    let mut found = false;
    for l in raw.lines() {
        if l.is_empty() {
            break;
        }
        if found {
            if l.starts_with(' ') || l.starts_with('\t') {
                value.push(' ');
                value.push_str(l.trim());
                continue;
            }
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case(name) {
                found = true;
                value = v.trim().to_string();
            }
        }
    }
    clean(&value)
}

fn recipient_allowed(addr: &str, domain: &str) -> bool {
    match addr.rsplit_once('@') {
        Some((local, d)) => {
            !local.is_empty()
                && local.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
                && d.eq_ignore_ascii_case(domain)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recording {
        delivered: RefCell<Vec<Vec<u8>>>,
        refused: RefCell<Vec<String>>,
    }

    impl Sink for Recording {
        fn deliver(&self, message: &[u8]) -> Result<(), String> {
            self.delivered.borrow_mut().push(message.to_vec());
            Ok(())
        }

        fn refused(&self, peer: &str, user: &str, from: &str, reason: &str) {
            self.refused.borrow_mut().push(format!("{peer}|{user}|{from}|{reason}"));
        }

        fn log(&self, _: &str) {}
    }

    #[test_id(taiwme9)]
    #[test]
    fn a_refused_outcome_reaches_the_sink_with_its_reason_and_an_accepted_one_does_not() {
        let s = Recording::default();
        attempt(&s, "10.0.0.3:6666", "ryan", "x@y", "rcpt: refused: relay denied to \"a@b\"");
        attempt(&s, "10.0.0.3:6666", "ryan", "", "auth: accepted");
        assert_eq!(*s.refused.borrow(), ["10.0.0.3:6666|ryan|x@y|relay denied to \"a@b\""]);
    }

    #[test_id(tqnt2h4)]
    #[test]
    fn the_default_accept_delivers_the_message_alone() {
        let s = Recording::default();
        let to = vec!["c@spira".to_string()];
        let env = Envelope { peer: "p", user: "u", from: "f", to: &to, message_id: "<m>", in_reply_to: "" };
        assert_eq!(s.accept(&env, b"hello"), Ok(()));
        assert_eq!(*s.delivered.borrow(), [b"hello".to_vec()]);
    }

    fn policy() -> Policy {
        Policy { host: "h".into(), user: "ryan".into(), from: "me@example.org".into(), rcpt_domain: "spira".into(), password_file: "unused".into(), max_message_bytes: 16, peer: "p:1".into() }
    }

    fn b64(s: &str) -> String {
        b64_encode(s)
    }

    /// Every step the session takes over `script`, fed in `cut`-byte pieces (the whole script when 0).
    /// A `Verify` is answered by the password `hunter2`; an `Accept` is answered `Ok`.
    fn transcript(script: &[u8], cut: usize) -> Vec<Step> {
        let mut s = Session::new(policy());
        let mut all = s.start();
        let mut todo: std::collections::VecDeque<Step> = Default::default();
        let pieces: Vec<&[u8]> = if cut == 0 { vec![script] } else { script.chunks(cut).collect() };
        for piece in pieces {
            todo.extend(s.feed(piece));
            while let Some(step) = todo.pop_front() {
                let next = match &step {
                    Step::Verify { password } => s.verified(password == "hunter2"),
                    Step::Accept { .. } => s.accepted(Ok(())),
                    _ => Vec::new(),
                };
                for n in next.into_iter().rev() {
                    todo.push_front(n);
                }
                all.push(step);
            }
        }
        all
    }

    fn same_however_cut(script: &[u8]) -> Vec<Step> {
        same_cut_at(script, &[1, 2, 3, 7])
    }

    fn same_cut_at(script: &[u8], cuts: &[usize]) -> Vec<Step> {
        let whole = transcript(script, 0);
        for &cut in cuts {
            assert_eq!(transcript(script, cut), whole, "cut every {cut} bytes");
        }
        whole
    }

    fn sent(steps: &[Step]) -> String {
        steps.iter().filter_map(|s| if let Step::Send(t) = s { Some(t.as_str()) } else { None }).collect()
    }

    #[test_id(trmqk2r)]
    #[test]
    fn a_failed_authentication_reads_the_same_fed_whole_or_a_byte_at_a_time() {
        let script = format!("EHLO c\r\nAUTH PLAIN {}\r\nAUTH LOGIN\r\n{}\r\n{}\r\nAUTH PLAIN {}\r\n", b64("\0ryan\0wrong"), b64("ryan"), b64("nope"), b64("\0ryan\0worse"));
        let steps = same_however_cut(script.as_bytes());
        let text = sent(&steps);
        assert_eq!(text.matches("535 5.7.8").count(), 2, "{text}");
        assert!(text.contains("421 4.7.0 too many authentication failures"), "{text}");
        assert_eq!(steps.last(), Some(&Step::Close));
    }

    #[test_id(tguj2g3)]
    #[test]
    fn a_relay_refusal_reads_the_same_fed_whole_or_a_byte_at_a_time() {
        let script = format!("AUTH PLAIN {}\r\nMAIL FROM:<me@example.org>\r\nRCPT TO:<a@elsewhere>\r\nRCPT TO:<c@spira>\r\nQUIT\r\n", b64("\0ryan\0hunter2"));
        let steps = same_however_cut(script.as_bytes());
        assert!(steps.iter().any(|s| matches!(s, Step::Attempt { outcome, .. } if outcome.starts_with("rcpt: refused: relay denied"))), "{steps:?}");
        let text = sent(&steps);
        assert!(text.contains("550 5.7.1 relay denied") && text.contains("250 2.1.5 ok") && text.ends_with("221 2.0.0 bye\r\n"), "{text}");
    }

    #[test_id(tut2gok)]
    #[test]
    fn an_oversize_message_reads_the_same_fed_whole_or_a_byte_at_a_time() {
        let script = format!("AUTH PLAIN {}\r\nMAIL FROM:<me@example.org>\r\nRCPT TO:<c@spira>\r\nDATA\r\n{}\r\n.\r\nNOOP\r\n", b64("\0ryan\0hunter2"), "0123456789abcdef\r\nmore\r\n");
        let steps = same_however_cut(script.as_bytes());
        assert!(steps.iter().all(|s| !matches!(s, Step::Accept { .. })), "{steps:?}");
        let text = sent(&steps);
        assert!(text.contains("552 5.3.4 message too large\r\n250 2.0.0 ok"), "{text}");
    }

    #[test_id(tjx9o23)]
    #[test]
    fn an_accepted_message_is_handed_out_with_an_owned_envelope_and_answered_when_the_driver_says() {
        let script = format!("AUTH PLAIN {}\r\nMAIL FROM:<me@example.org>\r\nRCPT TO:<c@spira>\r\nDATA\r\nMessage-ID: <m>\r\n..x\r\n.\r\n", b64("\0ryan\0hunter2"));
        let mut s = Session::new(Policy { max_message_bytes: 1024, ..policy() });
        s.start();
        let steps = s.feed(script.as_bytes());
        let Some(Step::Verify { .. }) = steps.last() else { panic!("{steps:?}") };
        let steps = s.verified(true);
        let Some(Step::Accept { envelope, message }) = steps.last() else { panic!("{steps:?}") };
        assert_eq!((envelope.user.as_str(), envelope.from.as_str(), envelope.message_id.as_str(), envelope.to.as_slice()), ("ryan", "me@example.org", "<m>", ["c@spira".to_string()].as_slice()));
        assert_eq!(message, b"Message-ID: <m>\n.x\n");
        assert_eq!(sent(&s.accepted(Err("disk full".into()))), "451 4.3.0 local delivery failed, try again\r\n");
    }

    #[test_id(txd3eki)]
    #[test]
    fn an_over_long_command_line_is_a_500_and_a_close_however_the_bytes_are_cut() {
        let script = format!("NOOP\r\n{}\r\nNOOP\r\n", "a".repeat(MAX_COMMAND_LINE + 1));
        let steps = same_however_cut(script.as_bytes());
        assert_eq!(sent(&steps), "220 h spira-submission ESMTP\r\n250 2.0.0 ok\r\n500 5.5.2 line too long\r\n");
        assert_eq!(steps.last(), Some(&Step::Close));
    }

    #[test_id(taqcbxc)]
    #[test]
    fn an_over_long_data_line_refuses_the_message_and_the_session_goes_on_however_the_bytes_are_cut() {
        let script = format!("AUTH PLAIN {}\r\nMAIL FROM:<me@example.org>\r\nRCPT TO:<c@spira>\r\nDATA\r\n{}\r\nbody\r\n.\r\nNOOP\r\n", b64("\0ryan\0hunter2"), "x".repeat(MAX_DATA_LINE + 1));
        let steps = same_cut_at(script.as_bytes(), &[4096, 100_000]);
        assert!(steps.iter().all(|s| !matches!(s, Step::Accept { .. })), "{steps:?}");
        assert!(sent(&steps).ends_with("552 5.3.4 message too large\r\n250 2.0.0 ok\r\n"), "{}", sent(&steps));
    }
}
