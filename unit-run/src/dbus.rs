//! The slice of the D-Bus wire protocol a systemd listener needs: EXTERNAL auth, method calls with
//! a string argument, and little-endian message decoding into [`Val`]. No dependency, so the
//! listener builds wherever the workspace does.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const CALL_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    U(u64),
    I(i64),
    B(bool),
    S(String),
    Arr(Vec<Val>),
    Struct(Vec<Val>),
    Var(Box<Val>),
}

impl Val {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::S(s) => Some(s),
            Val::Var(v) => v.as_str(),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Val::U(u) => Some(*u),
            Val::Var(v) => v.as_u64(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::I(i) => Some(*i),
            Val::U(u) => i64::try_from(*u).ok(),
            Val::Var(v) => v.as_i64(),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Val::Arr(a) => a.iter().map(|v| v.as_u64().and_then(|u| u8::try_from(u).ok())).collect(),
            Val::Var(v) => v.as_bytes(),
            _ => None,
        }
    }

    /// An `a{sv}` as (name, value) pairs; empty for any other shape.
    pub fn dict(&self) -> Vec<(String, Val)> {
        match self {
            Val::Var(v) => v.dict(),
            Val::Arr(entries) => entries
                .iter()
                .filter_map(|e| match e {
                    Val::Struct(kv) if kv.len() == 2 => Some((kv[0].as_str()?.to_string(), kv[1].clone())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub kind: u8,
    pub serial: u32,
    pub reply_serial: Option<u32>,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error: Option<String>,
    pub body: Vec<Val>,
}

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

fn align(buf: &mut Vec<u8>, n: usize) {
    while !buf.len().is_multiple_of(n) {
        buf.push(0);
    }
}

fn put_u32(buf: &mut Vec<u8>, v: u32) {
    align(buf, 4);
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    put_u32(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
}

fn put_field(buf: &mut Vec<u8>, code: u8, sig: &str, s: &str) {
    align(buf, 8);
    buf.push(code);
    buf.push(sig.len() as u8);
    buf.extend_from_slice(sig.as_bytes());
    buf.push(0);
    put_str(buf, s);
}

/// A METHOD_CALL, optionally carrying one string argument.
pub fn encode_call(serial: u32, dest: &str, path: &str, iface: &str, member: &str, arg: Option<&str>) -> Vec<u8> {
    let mut body = Vec::new();
    if let Some(a) = arg {
        put_str(&mut body, a);
    }
    let mut fields = Vec::new();
    put_field(&mut fields, 1, "o", path);
    put_field(&mut fields, 2, "s", iface);
    put_field(&mut fields, 3, "s", member);
    put_field(&mut fields, 6, "s", dest);
    if arg.is_some() {
        align(&mut fields, 8);
        fields.extend_from_slice(&[8, 1, b'g', 0, 1, b's', 0]);
    }
    let mut m = vec![b'l', METHOD_CALL, 0, 1];
    m.extend_from_slice(&(body.len() as u32).to_le_bytes());
    m.extend_from_slice(&serial.to_le_bytes());
    m.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    m.extend_from_slice(&fields);
    align(&mut m, 8);
    m.extend_from_slice(&body);
    m
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.buf.len()).ok_or("message truncated")?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn align(&mut self, n: usize) -> Result<(), String> {
        let to = self.pos.div_ceil(n) * n;
        self.take(to - self.pos).map(|_| ())
    }

    fn u32(&mut self) -> Result<u32, String> {
        self.align(4)?;
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| "u32")?))
    }

    fn string(&mut self, long: bool) -> Result<String, String> {
        let n = if long { self.u32()? as usize } else { self.take(1)?[0] as usize };
        let s = String::from_utf8_lossy(self.take(n)?).into_owned();
        self.take(1)?;
        Ok(s)
    }
}

fn type_end(sig: &[u8], i: usize) -> Result<usize, String> {
    match sig.get(i).ok_or("signature truncated")? {
        b'a' => type_end(sig, i + 1),
        open @ (b'(' | b'{') => {
            let close = if *open == b'(' { b')' } else { b'}' };
            let mut j = i + 1;
            while sig.get(j).ok_or("signature truncated")? != &close {
                j = type_end(sig, j)?;
            }
            Ok(j + 1)
        }
        _ => Ok(i + 1),
    }
}

fn alignment(c: u8) -> usize {
    match c {
        b'y' | b'g' | b'v' => 1,
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b's' | b'o' | b'a' => 4,
        _ => 8,
    }
}

fn value(sig: &[u8], i: &mut usize, r: &mut Reader, depth: u32) -> Result<Val, String> {
    if depth > 16 {
        return Err("nesting too deep".into());
    }
    let c = *sig.get(*i).ok_or("signature truncated")?;
    let start = *i;
    *i += 1;
    r.align(alignment(c))?;
    Ok(match c {
        b'y' => Val::U(r.take(1)?[0] as u64),
        b'b' => Val::B(r.u32()? != 0),
        b'n' => Val::I(i16::from_le_bytes(r.take(2)?.try_into().map_err(|_| "i16")?) as i64),
        b'q' => Val::U(u16::from_le_bytes(r.take(2)?.try_into().map_err(|_| "u16")?) as u64),
        b'i' => Val::I(i32::from_le_bytes(r.take(4)?.try_into().map_err(|_| "i32")?) as i64),
        b'u' => Val::U(r.u32()? as u64),
        b'x' => Val::I(i64::from_le_bytes(r.take(8)?.try_into().map_err(|_| "i64")?)),
        b't' => Val::U(u64::from_le_bytes(r.take(8)?.try_into().map_err(|_| "u64")?)),
        b'd' => Val::U(u64::from_le_bytes(r.take(8)?.try_into().map_err(|_| "f64")?)),
        b's' | b'o' => Val::S(r.string(true)?),
        b'g' => Val::S(r.string(false)?),
        b'v' => {
            let inner = r.string(false)?;
            let mut j = 0;
            Val::Var(Box::new(value(inner.as_bytes(), &mut j, r, depth + 1)?))
        }
        b'a' => {
            let n = r.u32()? as usize;
            let elem = *i;
            *i = type_end(sig, elem)?;
            r.align(alignment(*sig.get(elem).ok_or("signature truncated")?))?;
            let end = r.pos.checked_add(n).filter(|e| *e <= r.buf.len()).ok_or("array truncated")?;
            let mut out = Vec::new();
            while r.pos < end {
                let mut j = elem;
                out.push(value(sig, &mut j, r, depth + 1)?);
            }
            Val::Arr(out)
        }
        b'(' | b'{' => {
            let close = if c == b'(' { b')' } else { b'}' };
            let mut out = Vec::new();
            while sig.get(*i).ok_or("signature truncated")? != &close {
                out.push(value(sig, i, r, depth + 1)?);
            }
            *i += 1;
            debug_assert!(*i > start);
            Val::Struct(out)
        }
        other => return Err(format!("unsupported type {:?}", other as char)),
    })
}

/// Total length of the message starting at `buf[0]`, once its fixed header has arrived.
fn message_len(buf: &[u8]) -> Result<Option<usize>, String> {
    if buf.len() < 16 {
        return Ok(None);
    }
    if buf[0] != b'l' {
        return Err("big-endian peer".into());
    }
    let body = u32::from_le_bytes(buf[4..8].try_into().map_err(|_| "len")?) as usize;
    let fields = u32::from_le_bytes(buf[12..16].try_into().map_err(|_| "len")?) as usize;
    let total = (16 + fields).div_ceil(8) * 8 + body;
    if total > MAX_MESSAGE {
        return Err(format!("message of {total} bytes refused"));
    }
    Ok(Some(total))
}

pub fn decode(buf: &[u8]) -> Result<Msg, String> {
    if message_len(buf)?.is_none_or(|t| buf.len() < t) {
        return Err("message truncated".into());
    }
    let body_len = u32::from_le_bytes(buf[4..8].try_into().map_err(|_| "len")?) as usize;
    let fields_len = u32::from_le_bytes(buf[12..16].try_into().map_err(|_| "len")?) as usize;
    let mut m = Msg { kind: buf[1], serial: u32::from_le_bytes(buf[8..12].try_into().map_err(|_| "serial")?), reply_serial: None, path: None, interface: None, member: None, error: None, body: Vec::new() };
    let mut sig = String::new();
    let mut r = Reader { buf: &buf[..16 + fields_len], pos: 16 };
    while r.pos < 16 + fields_len {
        r.align(8)?;
        if r.pos >= 16 + fields_len {
            break;
        }
        let code = r.take(1)?[0];
        let vsig = r.string(false)?;
        match (code, vsig.as_str()) {
            (1, "o") => m.path = Some(r.string(true)?),
            (2, "s") => m.interface = Some(r.string(true)?),
            (3, "s") => m.member = Some(r.string(true)?),
            (4, "s") => m.error = Some(r.string(true)?),
            (5, "u") => m.reply_serial = Some(r.u32()?),
            (8, "g") => sig = r.string(false)?,
            (_, "s" | "o") => {
                r.string(true)?;
            }
            (_, "g") => {
                r.string(false)?;
            }
            (_, "u") => {
                r.u32()?;
            }
            (_, other) => return Err(format!("header field {code} of type {other:?}")),
        }
    }
    let start = (16 + fields_len).div_ceil(8) * 8;
    let mut br = Reader { buf: &buf[start..start + body_len], pos: 0 };
    let mut i = 0;
    while i < sig.len() {
        m.body.push(value(sig.as_bytes(), &mut i, &mut br, 0)?);
    }
    Ok(m)
}

pub struct Conn {
    sock: UnixStream,
    buf: Vec<u8>,
    serial: u32,
    pending: VecDeque<Msg>,
}

pub fn bus_path() -> Result<String, String> {
    if let Ok(a) = std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        if let Some(p) = a.strip_prefix("unix:path=") {
            return Ok(p.split(',').next().unwrap_or(p).to_string());
        }
    }
    let rt = std::env::var("XDG_RUNTIME_DIR").map_err(|_| "neither DBUS_SESSION_BUS_ADDRESS nor XDG_RUNTIME_DIR is set".to_string())?;
    Ok(format!("{rt}/bus"))
}

fn uid() -> Result<u32, String> {
    let s = std::fs::read_to_string("/proc/self/status").map_err(|e| format!("/proc/self/status: {e}"))?;
    s.lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|u| u.parse().ok())
        .ok_or_else(|| "no Uid in /proc/self/status".to_string())
}

impl Conn {
    pub fn connect(path: &str) -> Result<Conn, String> {
        let mut sock = UnixStream::connect(path).map_err(|e| format!("{path}: {e}"))?;
        sock.set_read_timeout(Some(CALL_DEADLINE)).map_err(|e| e.to_string())?;
        sock.set_write_timeout(Some(CALL_DEADLINE)).map_err(|e| e.to_string())?;
        let hex: String = uid()?.to_string().bytes().map(|b| format!("{b:02x}")).collect();
        sock.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes()).map_err(|e| e.to_string())?;
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while !line.ends_with(b"\r\n") {
            sock.read_exact(&mut b).map_err(|e| format!("auth: {e}"))?;
            line.push(b[0]);
            if line.len() > 512 {
                return Err("auth reply too long".into());
            }
        }
        if !line.starts_with(b"OK") {
            return Err(format!("auth refused: {}", String::from_utf8_lossy(&line).trim()));
        }
        sock.write_all(b"BEGIN\r\n").map_err(|e| e.to_string())?;
        let mut c = Conn { sock, buf: Vec::new(), serial: 0, pending: VecDeque::new() };
        c.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", None)?;
        Ok(c)
    }

    pub fn call(&mut self, dest: &str, path: &str, iface: &str, member: &str, arg: Option<&str>) -> Result<Vec<Val>, String> {
        self.serial += 1;
        let serial = self.serial;
        self.sock.write_all(&encode_call(serial, dest, path, iface, member, arg)).map_err(|e| format!("{member}: {e}"))?;
        let deadline = Instant::now() + CALL_DEADLINE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(m) = self.read_msg(left)? else { return Err(format!("{member}: no reply within {}s", CALL_DEADLINE.as_secs())) };
            match (m.kind, m.reply_serial) {
                (METHOD_RETURN, Some(s)) if s == serial => return Ok(m.body),
                (ERROR, Some(s)) if s == serial => return Err(format!("{member}: {}", m.error.unwrap_or_default())),
                (SIGNAL, _) => self.pending.push_back(m),
                _ => {}
            }
        }
    }

    /// The next signal, waiting up to `timeout`; `None` when it passes with none.
    pub fn next_signal(&mut self, timeout: Duration) -> Result<Option<Msg>, String> {
        if let Some(m) = self.pending.pop_front() {
            return Ok(Some(m));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.read_msg(left)? {
                Some(m) if m.kind == SIGNAL => return Ok(Some(m)),
                Some(_) => continue,
                None => return Ok(None),
            }
        }
    }

    fn read_msg(&mut self, timeout: Duration) -> Result<Option<Msg>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(total) = message_len(&self.buf)? {
                if self.buf.len() >= total {
                    let raw: Vec<u8> = self.buf.drain(..total).collect();
                    return decode(&raw).map(Some);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.sock.set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
            let mut chunk = [0u8; 8192];
            match self.sock.read(&mut chunk) {
                Ok(0) => return Err("bus closed the connection".into()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(None),
                Err(e) => return Err(format!("bus read: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(path: &str, body_sig: &str, body: &[u8]) -> Vec<u8> {
        let mut fields = Vec::new();
        put_field(&mut fields, 1, "o", path);
        put_field(&mut fields, 2, "s", "org.freedesktop.DBus.Properties");
        put_field(&mut fields, 3, "s", "PropertiesChanged");
        align(&mut fields, 8);
        fields.extend_from_slice(&[8, 1, b'g', 0, body_sig.len() as u8]);
        fields.extend_from_slice(body_sig.as_bytes());
        fields.push(0);
        let mut m = vec![b'l', SIGNAL, 0, 1];
        m.extend_from_slice(&(body.len() as u32).to_le_bytes());
        m.extend_from_slice(&7u32.to_le_bytes());
        m.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        m.extend_from_slice(&fields);
        align(&mut m, 8);
        m.extend_from_slice(body);
        m
    }

    #[test]
    fn a_properties_changed_signal_decodes_to_its_interface_and_changed_dict() {
        let mut b = Vec::new();
        put_str(&mut b, "org.freedesktop.systemd1.Unit");
        let mut dict = Vec::new();
        align(&mut dict, 8);
        put_str(&mut dict, "ActiveState");
        dict.extend_from_slice(&[1, b's', 0]);
        put_str(&mut dict, "failed");
        align(&mut dict, 8);
        put_str(&mut dict, "InvocationID");
        dict.extend_from_slice(&[2, b'a', b'y', 0]);
        put_u32(&mut dict, 2);
        dict.extend_from_slice(&[0xab, 0xcd]);
        align(&mut b, 4);
        put_u32(&mut b, dict.len() as u32);
        align(&mut b, 8);
        b.extend_from_slice(&dict);
        put_u32(&mut b, 0);
        let raw = signal("/org/freedesktop/systemd1/unit/x_2eservice", "sa{sv}as", &b);
        assert_eq!(message_len(&raw).unwrap(), Some(raw.len()));
        let m = decode(&raw).unwrap();
        assert_eq!((m.path.as_deref(), m.member.as_deref()), (Some("/org/freedesktop/systemd1/unit/x_2eservice"), Some("PropertiesChanged")));
        assert_eq!(m.body[0].as_str(), Some("org.freedesktop.systemd1.Unit"));
        let d = m.body[1].dict();
        assert_eq!(d[0].0, "ActiveState");
        assert_eq!(d[0].1.as_str(), Some("failed"));
        assert_eq!(d[1].1.as_bytes(), Some(vec![0xab, 0xcd]));
    }

    #[test]
    fn a_truncated_message_is_an_error_not_a_panic() {
        let mut b = Vec::new();
        put_str(&mut b, "org.freedesktop.systemd1.Unit");
        let raw = signal("/p", "sa{sv}as", &b);
        assert!(decode(&raw).is_err());
        assert!(decode(&raw[..raw.len() - 3]).is_err());
        assert!(decode(&raw[..10]).is_err());
    }

    #[test]
    fn a_call_encodes_a_header_of_the_declared_length() {
        let raw = encode_call(3, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", Some("type='signal'"));
        assert_eq!(message_len(&raw).unwrap(), Some(raw.len()));
        let m = decode(&raw).unwrap();
        assert_eq!((m.kind, m.serial, m.member.as_deref()), (METHOD_CALL, 3, Some("AddMatch")));
        assert_eq!(m.body[0].as_str(), Some("type='signal'"));
    }
}
