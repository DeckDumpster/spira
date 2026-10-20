//! A stub Proxmox HTTPS API for test-round-vm-e2e.sh only: real TLS, real request/response
//! shapes (round-vm/DESIGN.md §3.3), but next_id/clone/start/stop/destroy answer synchronously
//! (the task-polling loop is round-vm/src/pve.rs's own unit tests' job). agent/exec and
//! agent/file-write are NOT faked: each really execs inside the given podman container, so
//! round-vm's real key-delivery sequence has to land on a real filesystem.
//!
//! Usage: round-vm-stub-pve <port> <container-name> <cert-pem> <key-pem> <reqlog-path>

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

struct Stub {
    container: String,
    reqlog: String,
    vms: HashMap<String, Value>,
    exec_results: HashMap<i64, i64>,
    pid: i64,
    nextid: i64,
}

type Params = Vec<(String, String)>;

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_form(s: &str) -> Params {
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (pct_decode(k), pct_decode(v))
        })
        .collect()
}

fn get<'a>(p: &'a Params, k: &str) -> Option<&'a str> {
    p.iter().rev().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str())
}

fn podman_exec(container: &str, argv: &[&str]) -> i64 {
    Command::new("podman")
        .arg("exec")
        .arg(container)
        .args(argv)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.code().unwrap_or(-1) as i64)
        .unwrap_or(-1)
}

fn podman_write_file(container: &str, path: &str, content: &str) -> i64 {
    let child = Command::new("podman")
        .args(["exec", "-i", container, "sh", "-c", "cat > \"$1\"", "_", path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return -1 };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(content.as_bytes());
    }
    child.wait().map(|s| s.code().unwrap_or(-1) as i64).unwrap_or(-1)
}

impl Stub {
    fn route(&mut self, method: &str, path: &str, params: &Params) -> Result<(u16, Value), String> {
        let ok = |v: Value| Ok((200, v));
        if method == "GET" && path == "/api2/json/cluster/nextid" {
            self.nextid += 1;
            return ok(json!(self.nextid.to_string()));
        }
        let Some(rest) = path.strip_prefix("/api2/json/nodes/") else { return ok(Value::Null) };
        let (_, sub) = rest.split_once('/').ok_or("bad node path")?;
        if method == "GET" && sub == "qemu" {
            let list: Vec<Value> = self
                .vms
                .iter()
                .map(|(v, d)| json!({"vmid": v.parse::<i64>().unwrap_or(0), "name": d["name"], "tags": d.get("tags").cloned().unwrap_or(json!(""))}))
                .collect();
            return ok(Value::Array(list));
        }
        let parts: Vec<&str> = sub.split('/').collect();
        let vmid = parts.get(1).ok_or("no vmid")?.to_string();
        let tail = parts[2..].join("/");
        match (method, tail.as_str()) {
            ("POST", "clone") => {
                let newid = get(params, "newid").ok_or("no newid")?.to_string();
                self.vms.insert(newid, json!({"name": get(params, "name").unwrap_or(""), "running": false}));
                ok(Value::Null)
            }
            ("POST", "config") => {
                if let Some(vm) = self.vms.get_mut(&vmid) {
                    vm["tags"] = json!(get(params, "tags").unwrap_or(""));
                }
                ok(Value::Null)
            }
            ("POST", "status/start") => {
                self.vms.get_mut(&vmid).ok_or("no such vm")?["running"] = json!(true);
                ok(Value::Null)
            }
            ("POST", "status/stop") => {
                if let Some(vm) = self.vms.get_mut(&vmid) {
                    vm["running"] = json!(false);
                }
                ok(Value::Null)
            }
            ("GET", "status/current") => {
                let running = self.vms.get(&vmid).and_then(|v| v["running"].as_bool()).unwrap_or(false);
                ok(json!({"status": if running { "running" } else { "stopped" }}))
            }
            ("DELETE", "") => {
                self.vms.remove(&vmid);
                ok(Value::Null)
            }
            ("GET", "agent/network-get-interfaces") => ok(json!({"result": [{"name": "ens18", "ip-addresses": [{"ip-address-type": "ipv4", "ip-address": "127.0.0.1"}]}]})),
            ("POST", "agent/exec") => {
                let argv: Vec<&str> = params.iter().filter(|(k, _)| k == "command").map(|(_, v)| v.as_str()).collect();
                let rc = podman_exec(&self.container, &argv);
                self.pid += 1;
                self.exec_results.insert(self.pid, rc);
                ok(json!({"pid": self.pid}))
            }
            ("GET", "agent/exec-status") => {
                let pid: i64 = get(params, "pid").and_then(|p| p.parse().ok()).ok_or("no pid")?;
                match self.exec_results.get(&pid) {
                    None => ok(json!({"exited": 0})),
                    Some(rc) => ok(json!({"exited": 1, "exitcode": rc})),
                }
            }
            ("POST", "agent/file-write") => {
                let file = get(params, "file").ok_or("no file")?;
                if podman_write_file(&self.container, file, get(params, "content").unwrap_or("")) != 0 {
                    return Ok((500, json!({"error": "write failed"})));
                }
                ok(Value::Null)
            }
            _ => ok(Value::Null),
        }
    }
}

fn handle(stub: &Mutex<Stub>, conn: &mut dyn ReadWrite) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut it = line.split_whitespace();
    let (method, target) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
    let (mut length, mut auth) = (0usize, Value::Null);
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.trim().split_once(':') {
            match k.to_ascii_lowercase().as_str() {
                "content-length" => length = v.trim().parse().unwrap_or(0),
                "authorization" => auth = json!(v.trim()),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let params = if method == "POST" { parse_form(&String::from_utf8_lossy(&body)) } else { parse_form(query) };
    let (code, data) = {
        let mut s = stub.lock().unwrap();
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&s.reqlog) {
            let _ = writeln!(f, "{}", json!({"method": method, "path": path, "params": params, "auth": auth}));
        }
        s.route(&method, path, &params).unwrap_or_else(|e| (500, json!({"error": e})))
    };
    let out = json!({"data": data}).to_string();
    let head = format!(
        "HTTP/1.0 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        if code == 200 { "OK" } else { "Internal Server Error" },
        out.len()
    );
    let w = reader.get_mut();
    w.write_all(head.as_bytes())?;
    w.write_all(out.as_bytes())?;
    w.flush()
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 5 {
        eprintln!("usage: round-vm-stub-pve <port> <container-name> <cert-pem> <key-pem> <reqlog-path>");
        std::process::exit(2);
    }
    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(File::open(&a[2]).expect("cert"))).filter_map(Result::ok).collect();
    let key = rustls_pemfile::private_key(&mut BufReader::new(File::open(&a[3]).expect("key"))).ok().flatten().expect("private key");
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("cert/key");
    let cfg = Arc::new(cfg);
    let stub = Arc::new(Mutex::new(Stub { container: a[1].clone(), reqlog: a[4].clone(), vms: HashMap::new(), exec_results: HashMap::new(), pid: 1000, nextid: 776 }));
    let listener = TcpListener::bind(("127.0.0.1", a[0].parse().expect("port"))).expect("bind");
    for tcp in listener.incoming().flatten() {
        let (cfg, stub) = (cfg.clone(), stub.clone());
        std::thread::spawn(move || {
            let tcp: TcpStream = tcp;
            let Ok(conn) = rustls::ServerConnection::new(cfg) else { return };
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let _ = handle(&stub, &mut tls);
            tls.conn.send_close_notify();
            let _ = tls.flush();
        });
    }
}
