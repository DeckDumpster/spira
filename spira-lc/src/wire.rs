//! A minimal MySQL client: handshake, `mysql_native_password`, and `COM_QUERY` with
//! multi-statement scripts, over plain TCP. Dolt's `sql-server` speaks exactly this, and
//! forking the `dolt` CLI per query paid a process start (and its telemetry flush) on every
//! lifecycle call. Every column comes back as a string or null, which is the shape `rows.rs`
//! already reads.
//!
//! Every read and write is bounded by `IO_TIMEOUT`, so a server that stops answering is
//! `CannotTell`, never a hung caller.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::db::ScriptFailure;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_PING_AFTER: Duration = Duration::from_secs(30);

const CLIENT_LONG_PASSWORD: u32 = 1;
const CLIENT_CONNECT_WITH_DB: u32 = 0x8;
const CLIENT_LONG_FLAG: u32 = 0x4;
const CLIENT_PROTOCOL_41: u32 = 0x200;
const CLIENT_TRANSACTIONS: u32 = 0x2000;
const CLIENT_SECURE_CONNECTION: u32 = 0x8000;
const CLIENT_MULTI_STATEMENTS: u32 = 1 << 16;
const CLIENT_MULTI_RESULTS: u32 = 1 << 17;
const CLIENT_PLUGIN_AUTH: u32 = 1 << 19;

const MORE_RESULTS_EXISTS: u16 = 0x0008;
const NATIVE_PLUGIN: &str = "mysql_native_password";
const UTF8MB4_GENERAL_CI: u8 = 45;

pub struct Wire {
    stream: TcpStream,
    last_used: Instant,
}

fn cannot(msg: impl std::fmt::Display) -> ScriptFailure {
    ScriptFailure::CannotTell(msg.to_string())
}

fn io_failure(what: &str, e: std::io::Error) -> ScriptFailure {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => cannot(crate::db::DEADLINE_MESSAGE),
        _ => cannot(format!("{what}: {e}")),
    }
}

impl Wire {
    pub fn connect(host: &str, port: u16, user: &str, password: &str, database: Option<&str>) -> Result<Self, ScriptFailure> {
        Self::connect_with(host, port, user, password, database, IO_TIMEOUT)
    }

    /// [`Wire::connect`] with an explicit socket limit — only the admin batch verbs use one
    /// other than [`IO_TIMEOUT`] (see `db::ADMIN_IO_TIMEOUT`).
    pub fn connect_with(host: &str, port: u16, user: &str, password: &str, database: Option<&str>, io: Duration) -> Result<Self, ScriptFailure> {
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| cannot(format!("resolving {host}:{port}: {e}")))?
            .next()
            .ok_or_else(|| cannot(format!("{host}:{port} resolves to nothing")))?;
        let stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT).map_err(|e| cannot(format!("connecting to {host}:{port}: {e}")))?;
        stream.set_read_timeout(Some(io)).map_err(cannot)?;
        stream.set_write_timeout(Some(io)).map_err(cannot)?;
        let _ = stream.set_nodelay(true);
        let mut wire = Wire { stream, last_used: Instant::now() };
        wire.handshake(user, password, database)?;
        Ok(wire)
    }

    pub fn idle_too_long(&self) -> bool {
        self.last_used.elapsed() > IDLE_PING_AFTER
    }

    pub fn ping(&mut self) -> bool {
        self.write_packet(0, &[0x0e]).is_ok() && matches!(self.read_packet(), Ok((_, p)) if p.first() == Some(&0x00))
    }

    /// Run a script of one or more statements; the result sets (rows only — an OK carries
    /// none) in script order. A statement that errors ends the script and is the whole
    /// answer: the caller must discard this connection, since a transaction may be open.
    pub fn exec(&mut self, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        let mut payload = Vec::with_capacity(script.len() + 1);
        payload.push(0x03);
        payload.extend_from_slice(script.as_bytes());
        self.write_packet(0, &payload)?;
        let mut sets = Vec::new();
        loop {
            let (_, first) = self.read_packet()?;
            let more = match first.first() {
                Some(0x00) => ok_status(&first)? & MORE_RESULTS_EXISTS != 0,
                Some(0xff) => return Err(err_packet(&first)),
                Some(0xfb) => return Err(cannot("server asked for a LOCAL INFILE")),
                _ => {
                    let (rows, status) = self.read_result_set(&first)?;
                    sets.push(rows);
                    status & MORE_RESULTS_EXISTS != 0
                }
            };
            if !more {
                break;
            }
        }
        self.last_used = Instant::now();
        Ok(sets)
    }

    fn read_result_set(&mut self, first: &[u8]) -> Result<(Vec<Value>, u16), ScriptFailure> {
        let (ncols, _) = lenenc_int(first).ok_or_else(|| cannot("bad column count"))?;
        let mut names = Vec::with_capacity(ncols as usize);
        for _ in 0..ncols {
            let (_, def) = self.read_packet()?;
            names.push(column_name(&def)?);
        }
        self.expect_eof()?;
        let mut rows = Vec::new();
        loop {
            let (_, p) = self.read_packet()?;
            if p.first() == Some(&0xff) {
                return Err(err_packet(&p));
            }
            if is_eof(&p) {
                let status = u16_at(&p, 3).ok_or_else(|| cannot("short EOF packet"))?;
                return Ok((rows, status));
            }
            let mut obj = Map::new();
            let mut pos = 0;
            for name in &names {
                let value = match p.get(pos) {
                    Some(0xfb) => {
                        pos += 1;
                        Value::Null
                    }
                    _ => {
                        let (len, used) = lenenc_int(&p[pos..]).ok_or_else(|| cannot("bad row cell"))?;
                        let start = pos + used;
                        let cell = p.get(start..start + len as usize).ok_or_else(|| cannot("row cell overruns its packet"))?;
                        pos = start + len as usize;
                        Value::String(String::from_utf8_lossy(cell).into_owned())
                    }
                };
                obj.insert(name.clone(), value);
            }
            rows.push(Value::Object(obj));
        }
    }

    fn expect_eof(&mut self) -> Result<(), ScriptFailure> {
        let (_, p) = self.read_packet()?;
        if is_eof(&p) {
            Ok(())
        } else {
            Err(cannot("column definitions not terminated by EOF"))
        }
    }

    fn handshake(&mut self, user: &str, password: &str, database: Option<&str>) -> Result<(), ScriptFailure> {
        let (_, greeting) = self.read_packet()?;
        if greeting.first() == Some(&0xff) {
            return Err(err_packet(&greeting));
        }
        let hello = parse_greeting(&greeting)?;
        let server_caps = hello.caps;
        let mut caps = CLIENT_LONG_PASSWORD
            | CLIENT_LONG_FLAG
            | CLIENT_PROTOCOL_41
            | CLIENT_TRANSACTIONS
            | CLIENT_SECURE_CONNECTION
            | CLIENT_MULTI_STATEMENTS
            | CLIENT_MULTI_RESULTS
            | CLIENT_PLUGIN_AUTH;
        if database.is_some() {
            caps |= CLIENT_CONNECT_WITH_DB;
        }
        caps &= server_caps;
        let token = native_password_token(password, &hello.scramble);
        let mut resp = Vec::new();
        resp.extend_from_slice(&caps.to_le_bytes());
        resp.extend_from_slice(&(1u32 << 24).to_le_bytes());
        resp.push(UTF8MB4_GENERAL_CI);
        resp.extend_from_slice(&[0u8; 23]);
        resp.extend_from_slice(user.as_bytes());
        resp.push(0);
        resp.push(token.len() as u8);
        resp.extend_from_slice(&token);
        if let Some(db) = database {
            resp.extend_from_slice(db.as_bytes());
            resp.push(0);
        }
        resp.extend_from_slice(NATIVE_PLUGIN.as_bytes());
        resp.push(0);
        self.write_packet(1, &resp)?;

        let (seq, mut p) = self.read_packet()?;
        if p.first() == Some(&0xfe) {
            let body = &p[1..];
            let nul = body.iter().position(|b| *b == 0).ok_or_else(|| cannot("bad auth switch request"))?;
            let plugin = String::from_utf8_lossy(&body[..nul]).into_owned();
            if plugin != NATIVE_PLUGIN {
                return Err(cannot(format!("server wants auth plugin {plugin}; only {NATIVE_PLUGIN} is supported")));
            }
            let data = &body[nul + 1..];
            let scramble = data.strip_suffix(&[0]).unwrap_or(data);
            self.write_packet(seq.wrapping_add(1), &native_password_token(password, scramble))?;
            let next = self.read_packet()?;
            p = next.1;
        }
        match p.first() {
            Some(0x00) => Ok(()),
            Some(0xff) => Err(err_packet(&p)),
            other => Err(cannot(format!("unexpected authentication reply {other:?}"))),
        }
    }

    fn write_packet(&mut self, mut seq: u8, payload: &[u8]) -> Result<(), ScriptFailure> {
        const MAX: usize = 0xff_ffff;
        let mut buf = Vec::with_capacity(payload.len() + 8);
        let mut rest = payload;
        loop {
            let n = rest.len().min(MAX);
            buf.extend_from_slice(&(n as u32).to_le_bytes()[..3]);
            buf.push(seq);
            buf.extend_from_slice(&rest[..n]);
            seq = seq.wrapping_add(1);
            rest = &rest[n..];
            if n < MAX {
                break;
            }
        }
        self.stream.write_all(&buf).and_then(|_| self.stream.flush()).map_err(|e| cannot(format!("writing to server: {e}")))
    }

    fn read_packet(&mut self) -> Result<(u8, Vec<u8>), ScriptFailure> {
        let mut out = Vec::new();
        loop {
            let mut head = [0u8; 4];
            self.stream.read_exact(&mut head).map_err(|e| io_failure("reading from server", e))?;
            let len = head[0] as usize | (head[1] as usize) << 8 | (head[2] as usize) << 16;
            let start = out.len();
            out.resize(start + len, 0);
            self.stream.read_exact(&mut out[start..]).map_err(|e| io_failure("reading from server", e))?;
            if len < 0xff_ffff {
                return Ok((head[3], out));
            }
        }
    }
}

struct Greeting {
    caps: u32,
    scramble: Vec<u8>,
}

fn parse_greeting(p: &[u8]) -> Result<Greeting, ScriptFailure> {
    let bad = || cannot("malformed server greeting");
    if p.first() != Some(&10) {
        return Err(cannot(format!("unsupported protocol version {:?}", p.first())));
    }
    let nul = p[1..].iter().position(|b| *b == 0).ok_or_else(bad)? + 1;
    let mut pos = nul + 1 + 4;
    let part1 = p.get(pos..pos + 8).ok_or_else(bad)?;
    pos += 9;
    let caps_lo = u16_at(p, pos).ok_or_else(bad)? as u32;
    pos += 2 + 1 + 2;
    let caps_hi = u16_at(p, pos).ok_or_else(bad)? as u32;
    pos += 2;
    let auth_len = *p.get(pos).ok_or_else(bad)? as usize;
    pos += 1 + 10;
    let part2_len = auth_len.saturating_sub(8).max(13);
    let part2 = p.get(pos..pos + part2_len).ok_or_else(bad)?;
    let mut scramble = part1.to_vec();
    scramble.extend_from_slice(part2.strip_suffix(&[0]).unwrap_or(part2));
    Ok(Greeting { caps: caps_lo | caps_hi << 16, scramble })
}

fn is_eof(p: &[u8]) -> bool {
    p.first() == Some(&0xfe) && p.len() < 9
}

fn u16_at(p: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*p.get(at)?, *p.get(at + 1)?]))
}

fn lenenc_int(p: &[u8]) -> Option<(u64, usize)> {
    match *p.first()? {
        n @ 0..=0xfa => Some((n as u64, 1)),
        0xfc => Some((u16_at(p, 1)? as u64, 3)),
        0xfd => Some((u32::from_le_bytes([*p.get(1)?, *p.get(2)?, *p.get(3)?, 0]) as u64, 4)),
        0xfe => Some((u64::from_le_bytes(p.get(1..9)?.try_into().ok()?), 9)),
        _ => None,
    }
}

fn ok_status(p: &[u8]) -> Result<u16, ScriptFailure> {
    let (_, a) = lenenc_int(&p[1..]).ok_or_else(|| cannot("bad OK packet"))?;
    let (_, b) = lenenc_int(&p[1 + a..]).ok_or_else(|| cannot("bad OK packet"))?;
    u16_at(p, 1 + a + b).ok_or_else(|| cannot("short OK packet"))
}

fn column_name(def: &[u8]) -> Result<String, ScriptFailure> {
    let mut pos = 0;
    let mut field = Vec::new();
    for _ in 0..5 {
        let (len, used) = lenenc_int(def.get(pos..).unwrap_or_default()).ok_or_else(|| cannot("bad column definition"))?;
        let start = pos + used;
        field = def.get(start..start + len as usize).ok_or_else(|| cannot("bad column definition"))?.to_vec();
        pos = start + len as usize;
    }
    Ok(String::from_utf8_lossy(&field).into_owned())
}

/// 1213 / SQLSTATE 40001: Dolt's commit-time serialization failure, the lost CAS race.
fn err_packet(p: &[u8]) -> ScriptFailure {
    let code = u16_at(p, 1).unwrap_or(0);
    let (state, msg) = match p.get(3) {
        Some(b'#') => (String::from_utf8_lossy(p.get(4..9).unwrap_or_default()).into_owned(), p.get(9..).unwrap_or_default()),
        _ => (String::new(), p.get(3..).unwrap_or_default()),
    };
    let msg = String::from_utf8_lossy(msg);
    if code == 1213 || state == "40001" || msg.contains("serialization failure") {
        ScriptFailure::LostRace
    } else {
        cannot(format!("Error {code} ({state}): {msg}"))
    }
}

/// SHA1(password) XOR SHA1(scramble + SHA1(SHA1(password))); empty for an empty password.
fn native_password_token(password: &str, scramble: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }
    let stage1 = sha1(password.as_bytes());
    let stage2 = sha1(&stage1);
    let mut salted = scramble.to_vec();
    salted.extend_from_slice(&stage2);
    let mix = sha1(&salted);
    stage1.iter().zip(mix.iter()).map(|(a, b)| a ^ b).collect()
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().concat()
    }

    #[test]
    fn sha1_matches_the_published_vectors() {
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(hex(&sha1(long)), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    }

    #[test]
    fn an_empty_password_sends_no_token_and_a_set_one_sends_twenty_bytes() {
        assert!(native_password_token("", b"01234567890123456789").is_empty());
        assert_eq!(native_password_token("pw", b"01234567890123456789").len(), 20);
    }

    #[test]
    fn a_serialization_failure_is_a_lost_race_and_any_other_error_is_not() {
        let mut p = vec![0xff];
        p.extend_from_slice(&1213u16.to_le_bytes());
        p.extend_from_slice(b"#40001serialization failure");
        assert!(matches!(err_packet(&p), ScriptFailure::LostRace));
        let mut q = vec![0xff];
        q.extend_from_slice(&1064u16.to_le_bytes());
        q.extend_from_slice(b"#42000syntax");
        assert!(matches!(err_packet(&q), ScriptFailure::CannotTell(_)));
    }

    #[test]
    fn length_encoded_integers_round_trip_every_width() {
        assert_eq!(lenenc_int(&[5]), Some((5, 1)));
        assert_eq!(lenenc_int(&[0xfc, 0x01, 0x01]), Some((257, 3)));
        assert_eq!(lenenc_int(&[0xfd, 1, 0, 1]), Some((65537, 4)));
        assert_eq!(lenenc_int(&[0xfe, 1, 0, 0, 0, 0, 0, 0, 0]), Some((1, 9)));
        assert_eq!(lenenc_int(&[0xfb]), None);
    }
}
