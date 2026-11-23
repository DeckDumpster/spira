//! Runs the shared scenario through a localhost recorder to a real Margin reader and writes
//! each request, status and reply to a fixture file. The bearer is never written.
//!
//! `record_margin --base <reader url> --token-file <file> --out <fixture.json>`

use margin_client::{scenario, HttpMargin};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn arg(name: &str) -> Result<String, String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned().ok_or_else(|| format!("usage: record_margin --base <url> --token-file <file> --out <file>; {name} is missing"))
}

struct Request {
    method: String,
    target: String,
    bearer: Option<String>,
    body: String,
}

fn read_request(s: &mut TcpStream) -> Result<Request, String> {
    let mut seen = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = s.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("the client closed before its request was whole".into());
        }
        seen.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&seen).to_string();
        let Some(at) = text.find("\r\n\r\n") else { continue };
        let header = |name: &str| text[..at].lines().find_map(|l| l.to_ascii_lowercase().starts_with(&format!("{name}:")).then(|| l[name.len() + 1..].trim().to_string()));
        let want: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        if seen.len() < at + 4 + want {
            continue;
        }
        let mut first = text.lines().next().unwrap_or("").split(' ');
        let (method, target) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
        let bearer = header("authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string));
        return Ok(Request { method, target, bearer, body: String::from_utf8_lossy(&seen[at + 4..at + 4 + want]).to_string() });
    }
}

fn forward(base: &str, r: &Request) -> (u16, String) {
    let req = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build().request(&r.method, &format!("{base}{}", r.target));
    let req = match &r.bearer {
        Some(b) => req.set("Authorization", &format!("Bearer {b}")),
        None => req,
    };
    let sent = if r.body.is_empty() { req.call() } else { req.set("Content-Type", "application/json").send_string(&r.body) };
    match sent {
        Ok(resp) => (resp.status(), resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => (code, resp.into_string().unwrap_or_default()),
        Err(e) => (502, e.to_string()),
    }
}

fn main() -> Result<(), String> {
    let (base, token_file, out) = (arg("--base")?, arg("--token-file")?, arg("--out")?);
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let local = format!("http://{}", listener.local_addr().map_err(|e| e.to_string())?);
    let log: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (tape, upstream) = (log.clone(), base.trim_end_matches('/').to_string());
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let Ok(r) = read_request(&mut s) else { continue };
            let (status, reply) = forward(&upstream, &r);
            let (path, query) = r.target.split_once('?').unwrap_or((&r.target, ""));
            let body: Value = if r.body.is_empty() { Value::Null } else { serde_json::from_str(&r.body).unwrap_or(Value::Null) };
            tape.lock().unwrap().push(json!({"method": r.method, "path": path, "query": query, "body": body, "status": status, "reply": reply}));
            let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
        }
    });
    let reader = HttpMargin { base: local, token_file, timeout: Duration::from_secs(10) };
    scenario::run(&reader)?;
    let tape = log.lock().unwrap();
    std::fs::write(&out, serde_json::to_string_pretty(&*tape).map_err(|e| e.to_string())? + "\n").map_err(|e| format!("writing {out}: {e}"))?;
    println!("recorded {} exchanges to {out}", tape.len());
    Ok(())
}
