use crate::credential::Credential;
use std::io::{Read, Write};
use std::path::PathBuf;

const MAX_COMMAND_LINE: usize = 4096;
const MAX_DATA_LINE: usize = 256 * 1024;
const MAX_RCPT: usize = 20;
const MAX_AUTH_FAILURES: u32 = 3;

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

/// The audit line for one session outcome, and, for a refusal, the sink's [`Sink::refused`].
pub fn attempt(sink: &dyn Sink, peer: &str, user: &str, from: &str, outcome: &str) {
    sink.log(&format!("ts={} peer={peer} user={user:?} from={from:?} {outcome}", crate::clock::iso(crate::clock::now())));
    if let Some((_, reason)) = outcome.split_once("refused: ") {
        sink.refused(peer, user, from, reason);
    }
}

struct Conn<S> {
    s: S,
    buf: Vec<u8>,
}

enum Line {
    Text(Vec<u8>),
    TooLong,
    Closed,
}

impl<S: Read + Write> Conn<S> {
    fn read_line(&mut self, max: usize) -> Line {
        loop {
            if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=i).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Line::Text(line);
            }
            if self.buf.len() > max {
                self.buf.clear();
                return Line::TooLong;
            }
            let mut chunk = [0u8; 4096];
            match self.s.read(&mut chunk) {
                Ok(0) | Err(_) => return Line::Closed,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    fn reply(&mut self, text: &str) -> bool {
        self.s.write_all(format!("{text}\r\n").as_bytes()).and_then(|_| self.s.flush()).is_ok()
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

fn b64_encode(s: &str) -> String {
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

#[derive(PartialEq)]
enum Auth {
    No,
    Yes,
}

pub fn serve_session<S: Read + Write>(stream: S, p: &Policy, sink: &dyn Sink) {
    let mut c = Conn { s: stream, buf: Vec::new() };
    if !c.reply(&format!("220 {} spira-submission ESMTP", p.host)) {
        return;
    }
    let mut auth = Auth::No;
    let mut who = String::new();
    let mut failures = 0u32;
    let mut from: Option<String> = None;
    let mut rcpts: Vec<String> = Vec::new();
    loop {
        let line = match c.read_line(MAX_COMMAND_LINE) {
            Line::Text(l) => String::from_utf8_lossy(&l).into_owned(),
            Line::TooLong => {
                c.reply("500 5.5.2 line too long");
                return;
            }
            Line::Closed => return,
        };
        let (verb, rest) = match line.split_once(' ') {
            Some((v, r)) => (v.to_ascii_uppercase(), r),
            None => (line.to_ascii_uppercase(), ""),
        };
        let ok = match verb.as_str() {
            "EHLO" | "HELO" => {
                from = None;
                rcpts.clear();
                if verb == "HELO" {
                    c.reply(&format!("250 {}", p.host))
                } else {
                    c.reply(&format!("250-{}\r\n250-AUTH PLAIN LOGIN\r\n250-SIZE {}\r\n250 8BITMIME", p.host, p.max_message_bytes))
                }
            }
            "NOOP" => c.reply("250 2.0.0 ok"),
            "RSET" => {
                from = None;
                rcpts.clear();
                c.reply("250 2.0.0 ok")
            }
            "QUIT" => {
                c.reply("221 2.0.0 bye");
                return;
            }
            "STARTTLS" => c.reply("502 5.5.1 already encrypted"),
            "AUTH" => {
                if auth == Auth::Yes {
                    c.reply("503 5.5.1 already authenticated")
                } else {
                    let mut tried = String::new();
                    match do_auth(&mut c, p, rest, &mut tried) {
                        Some(Some(true)) => {
                            auth = Auth::Yes;
                            who = tried;
                            attempt(sink, &p.peer, &who, "", "auth: accepted");
                            c.reply("235 2.7.0 authenticated")
                        }
                        Some(None) => {
                            attempt(sink, &p.peer, "", "", "auth: refused: mechanism not supported");
                            c.reply("504 5.5.4 mechanism not supported")
                        }
                        Some(Some(false)) => {
                            attempt(sink, &p.peer, &clean(&tried), "", "auth: refused: authentication failed");
                            failures += 1;
                            if failures >= MAX_AUTH_FAILURES {
                                c.reply("421 4.7.0 too many authentication failures");
                                return;
                            }
                            c.reply("535 5.7.8 authentication failed")
                        }
                        None => return,
                    }
                }
            }
            "MAIL" => {
                if auth != Auth::Yes {
                    c.reply("530 5.7.0 authentication required")
                } else if from.is_some() {
                    c.reply("503 5.5.1 sender already given")
                } else {
                    match rest.get(..5).filter(|k| k.eq_ignore_ascii_case("FROM:")).and_then(|_| path_arg(&rest[5..])) {
                        None => c.reply("501 5.5.4 syntax: MAIL FROM:<address>"),
                        Some(a) if a.eq_ignore_ascii_case(&p.from) => {
                            from = Some(a);
                            c.reply("250 2.1.0 ok")
                        }
                        Some(a) => {
                            attempt(sink, &p.peer, &who, &clean(&a), "mail: refused: sender is not the operator address");
                            c.reply("550 5.7.1 sender is not the operator address")
                        }
                    }
                }
            }
            "RCPT" => {
                if auth != Auth::Yes {
                    c.reply("530 5.7.0 authentication required")
                } else if from.is_none() {
                    c.reply("503 5.5.1 MAIL first")
                } else {
                    match rest.get(..3).filter(|k| k.eq_ignore_ascii_case("TO:")).and_then(|_| path_arg(&rest[3..])) {
                        None => c.reply("501 5.5.4 syntax: RCPT TO:<address>"),
                        Some(a) if !recipient_allowed(&a, &p.rcpt_domain) => {
                            attempt(sink, &p.peer, &who, from.as_deref().unwrap_or(""), &format!("rcpt: refused: relay denied to {:?}", clean(&a)));
                            c.reply("550 5.7.1 relay denied: only Spira mailbox addresses are accepted")
                        }
                        Some(_) if rcpts.len() >= MAX_RCPT => c.reply("452 4.5.3 too many recipients"),
                        Some(a) => {
                            rcpts.push(a);
                            c.reply("250 2.1.5 ok")
                        }
                    }
                }
            }
            "DATA" => {
                if auth != Auth::Yes {
                    c.reply("530 5.7.0 authentication required")
                } else if rcpts.is_empty() {
                    c.reply("503 5.5.1 RCPT first")
                } else if !c.reply("354 end data with <CR><LF>.<CR><LF>") {
                    return;
                } else {
                    let sender = from.take().unwrap_or_default();
                    let to = std::mem::take(&mut rcpts);
                    match read_data(&mut c, p.max_message_bytes) {
                        Err(DataErr::Closed) => return,
                        Err(DataErr::TooBig) => {
                            attempt(sink, &p.peer, &who, &clean(&sender), "data: refused: message too large");
                            c.reply("552 5.3.4 message too large")
                        }
                        Ok(raw) => {
                            let text = String::from_utf8_lossy(&raw).into_owned();
                            let (mid, irt) = (header(&text, "message-id"), header(&text, "in-reply-to"));
                            let sender_clean = clean(&sender);
                            let envelope = Envelope { peer: &p.peer, user: &who, from: &sender_clean, to: &to, message_id: &mid, in_reply_to: &irt };
                            let outcome = sink.accept(&envelope, &raw);
                            let state = match &outcome {
                                Ok(()) => "accepted".to_string(),
                                Err(e) => format!("refused: delivery failed: {}", clean(e)),
                            };
                            attempt(sink, &p.peer, &who, &clean(&sender), &format!("data: {state} to={} message-id={mid:?} in-reply-to={irt:?}", clean(&to.join(","))));
                            match outcome {
                                Ok(()) => c.reply("250 2.0.0 queued"),
                                Err(_) => c.reply("451 4.3.0 local delivery failed, try again"),
                            }
                        }
                    }
                }
            }
            "" => c.reply("500 5.5.2 empty command"),
            _ => c.reply("502 5.5.1 command not implemented"),
        };
        if !ok {
            return;
        }
    }
}

/// Outer `None`: the connection ended mid-exchange. Inner `None`: unknown mechanism.
fn do_auth<S: Read + Write>(c: &mut Conn<S>, p: &Policy, rest: &str, tried: &mut String) -> Option<Option<bool>> {
    let mut words = rest.split_whitespace();
    let mech = words.next().unwrap_or("").to_ascii_uppercase();
    let initial = words.next();
    let prompt = |c: &mut Conn<S>, text: &str| -> Option<String> {
        if !c.reply(&format!("334 {text}")) {
            return None;
        }
        match c.read_line(MAX_COMMAND_LINE) {
            Line::Text(l) => Some(String::from_utf8_lossy(&l).into_owned()),
            _ => None,
        }
    };
    let (user, pass) = match mech.as_str() {
        "PLAIN" => {
            let blob = match initial {
                Some(i) if i != "=" => i.to_string(),
                _ => prompt(c, "")?,
            };
            let Some(raw) = b64_decode(&blob) else { return Some(Some(false)) };
            let parts: Vec<&[u8]> = raw.split(|&b| b == 0).collect();
            if parts.len() != 3 {
                return Some(Some(false));
            }
            let authz = String::from_utf8_lossy(parts[0]).into_owned();
            let user = String::from_utf8_lossy(parts[1]).into_owned();
            *tried = user.clone();
            if !authz.is_empty() && authz != user {
                return Some(Some(false));
            }
            (user, String::from_utf8_lossy(parts[2]).into_owned())
        }
        "LOGIN" => {
            let u = match initial {
                Some(i) => i.to_string(),
                None => prompt(c, &b64_encode("Username:"))?,
            };
            let pw = prompt(c, &b64_encode("Password:"))?;
            let (Some(u), Some(pw)) = (b64_decode(&u), b64_decode(&pw)) else { return Some(Some(false)) };
            *tried = String::from_utf8_lossy(&u).into_owned();
            (tried.clone(), String::from_utf8_lossy(&pw).into_owned())
        }
        _ => {
            return Some(None);
        }
    };
    let cred = match Credential::load(&p.password_file, &p.user) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spira-submission: password file unusable: {e}");
            return Some(Some(false));
        }
    };
    let user_ok = user == p.user;
    Some(Some(cred.verify(&pass) && user_ok))
}

enum DataErr {
    Closed,
    TooBig,
}

fn read_data<S: Read + Write>(c: &mut Conn<S>, max: usize) -> Result<Vec<u8>, DataErr> {
    let mut out = Vec::new();
    let mut too_big = false;
    loop {
        match c.read_line(MAX_DATA_LINE) {
            Line::Closed => return Err(DataErr::Closed),
            Line::TooLong => too_big = true,
            Line::Text(l) => {
                if l == b"." {
                    break;
                }
                let l = if l.first() == Some(&b'.') { &l[1..] } else { &l[..] };
                if out.len() + l.len() + 1 > max {
                    too_big = true;
                }
                if !too_big {
                    out.extend_from_slice(l);
                    out.push(b'\n');
                }
            }
        }
    }
    if too_big {
        Err(DataErr::TooBig)
    } else {
        Ok(out)
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

    #[test]
    fn a_refused_outcome_reaches_the_sink_with_its_reason_and_an_accepted_one_does_not() {
        let s = Recording::default();
        attempt(&s, "10.0.0.3:6666", "ryan", "x@y", "rcpt: refused: relay denied to \"a@b\"");
        attempt(&s, "10.0.0.3:6666", "ryan", "", "auth: accepted");
        assert_eq!(*s.refused.borrow(), ["10.0.0.3:6666|ryan|x@y|relay denied to \"a@b\""]);
    }

    #[test]
    fn the_default_accept_delivers_the_message_alone() {
        let s = Recording::default();
        let to = vec!["c@spira".to_string()];
        let env = Envelope { peer: "p", user: "u", from: "f", to: &to, message_id: "<m>", in_reply_to: "" };
        assert_eq!(s.accept(&env, b"hello"), Ok(()));
        assert_eq!(*s.delivered.borrow(), [b"hello".to_vec()]);
    }
}
