//! spira-submission: implicit-TLS SMTP submission that hands each accepted message to
//! `mail sendmail`, so a reply from a mail client behaves exactly like a cockpit reply.

pub mod credential;
pub mod smtp;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection, SignatureScheme, StreamOwned};
use smtp::{Policy, Sink};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

pub const IO_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    pub addr: IpAddr,
    pub port: u16,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub password_file: PathBuf,
    pub user: String,
    pub from: String,
    pub rcpt_domain: String,
    pub sendmail: Vec<String>,
}

impl Config {
    /// `Ok(None)` when `SPIRA_SUBMISSION_ADDR` is empty: the service is not enabled on this box.
    pub fn from_spira() -> Result<Option<Config>, String> {
        let get = spira_config::process::cfg;
        Config::from_values(&|k| get(k), vec!["mail".into(), "sendmail".into()])
    }

    pub fn from_values(get: &dyn Fn(&str) -> Result<String, String>, sendmail: Vec<String>) -> Result<Option<Config>, String> {
        let addr = get("SPIRA_SUBMISSION_ADDR")?;
        if addr.trim().is_empty() {
            return Ok(None);
        }
        let need = |k: &str| -> Result<String, String> {
            let v = get(k)?;
            if v.trim().is_empty() {
                Err(format!("{k} is empty — spira-submission refuses to start without it"))
            } else {
                Ok(v.trim().to_string())
            }
        };
        let addr: IpAddr = addr.trim().parse().map_err(|e| format!("SPIRA_SUBMISSION_ADDR={addr:?} is not an IP address: {e}"))?;
        if addr.is_unspecified() {
            return Err(format!("SPIRA_SUBMISSION_ADDR={addr} — never a wildcard"));
        }
        let port: u32 = need("SPIRA_SUBMISSION_PORT")?.parse().map_err(|e| format!("SPIRA_SUBMISSION_PORT does not parse: {e}"))?;
        if !(1025..=65535).contains(&port) {
            return Err(format!("SPIRA_SUBMISSION_PORT={port} — must be 1025-65535"));
        }
        Ok(Some(Config {
            addr,
            port: port as u16,
            cert: need("SPIRA_SUBMISSION_CERT")?.into(),
            key: need("SPIRA_SUBMISSION_KEY")?.into(),
            password_file: need("SPIRA_SUBMISSION_PASSWORD_FILE")?.into(),
            user: need("SPIRA_SUBMISSION_USER")?,
            from: need("SPIRA_SUBMISSION_FROM")?,
            rcpt_domain: need("SPIRA_SUBMISSION_RCPT_DOMAIN")?,
            sendmail,
        }))
    }

    pub fn socket(&self) -> SocketAddr {
        SocketAddr::new(self.addr, self.port)
    }

    fn policy(&self) -> Policy {
        Policy {
            host: self.addr.to_string(),
            user: self.user.clone(),
            from: self.from.clone(),
            rcpt_domain: self.rcpt_domain.clone(),
            password_file: self.password_file.clone(),
            max_message_bytes: MAX_MESSAGE_BYTES,
        }
    }
}

pub struct SendmailSink {
    pub argv: Vec<String>,
}

impl Sink for SendmailSink {
    fn deliver(&self, message: &[u8]) -> Result<(), String> {
        let (prog, args) = self.argv.split_first().ok_or("no sendmail command configured")?;
        // batch-job: a hand-off killed midway could deliver and still report failure, so it runs to its own exit
        let mut child = Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run {prog}: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let msg = message.to_vec();
        let writer = std::thread::spawn(move || {
            let r = stdin.write_all(&msg);
            drop(stdin);
            r
        });
        let out = child.wait_with_output().map_err(|e| format!("waiting for {prog}: {e}"))?;
        let _ = writer.join();
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("{prog} exited {}: {}", out.status.code().map_or("signal".into(), |c| c.to_string()), err.lines().next().unwrap_or("")))
        }
    }
}

pub fn server_config(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>, String> {
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(cert).map_err(|e| format!("{}: {e}", cert.display()))?))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", cert.display()))?;
    if certs.is_empty() {
        return Err(format!("{}: no certificate in file", cert.display()));
    }
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut std::io::BufReader::new(std::fs::File::open(key).map_err(|e| format!("{}: {e}", key.display()))?))
        .map_err(|e| format!("{}: {e}", key.display()))?
        .ok_or_else(|| format!("{}: no private key in file", key.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate/key: {e}"))?;
    Ok(Arc::new(cfg))
}

/// Serves forever on an already-bound listener. The certificate and password are read per
/// connection, so a renewed tailscale cert or a rotated password needs no restart.
pub fn serve(listener: TcpListener, cfg: Config, sink: Arc<dyn Sink + Send + Sync>) {
    let cfg = Arc::new(cfg);
    for conn in listener.incoming() {
        let Ok(tcp) = conn else { continue };
        let (cfg, sink) = (cfg.clone(), sink.clone());
        std::thread::spawn(move || {
            let _ = tcp.set_read_timeout(Some(IO_TIMEOUT));
            let _ = tcp.set_write_timeout(Some(IO_TIMEOUT));
            let tls_cfg = match server_config(&cfg.cert, &cfg.key) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("spira-submission: {e}");
                    return;
                }
            };
            let Ok(conn) = ServerConnection::new(tls_cfg) else { return };
            let mut tls = StreamOwned::new(conn, tcp);
            while tls.conn.is_handshaking() {
                if tls.conn.complete_io(&mut tls.sock).is_err() {
                    return;
                }
            }
            smtp::serve_session(tls, &cfg.policy(), sink.as_ref());
        });
    }
}

#[derive(Debug)]
struct AcceptAny;

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

/// A TLS client that does not authenticate the server: for liveness probes and tests, which
/// care that the endpoint answers, never that it is who it says.
pub fn probe_config() -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth();
    Arc::new(cfg)
}

pub fn connect_tls(addr: SocketAddr) -> Result<StreamOwned<ClientConnection, TcpStream>, String> {
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("connect {addr}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(5))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let conn = ClientConnection::new(probe_config(), ServerName::IpAddress(addr.ip().into())).map_err(|e| e.to_string())?;
    Ok(StreamOwned::new(conn, tcp))
}

/// TLS-connects and requires an SMTP `220` banner naming this service.
pub fn health(addr: SocketAddr) -> Result<String, String> {
    let mut s = connect_tls(addr)?;
    let mut buf = [0u8; 256];
    let n = s.read(&mut buf).map_err(|e| format!("reading banner from {addr}: {e}"))?;
    let banner = String::from_utf8_lossy(&buf[..n]).trim().to_string();
    let _ = s.write_all(b"QUIT\r\n");
    if banner.starts_with("220 ") && banner.contains("spira-submission") {
        Ok(banner)
    } else {
        Err(format!("unexpected banner from {addr}: {banner:?}"))
    }
}
