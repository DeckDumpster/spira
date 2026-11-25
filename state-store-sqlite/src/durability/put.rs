//! The off-box destination, written with a key that can only upload: one signed `PUT` per
//! object, under a fixed name, so a retry is the same request. Nothing here lists, reads or
//! deletes the bucket (law-off-box-backups-are-private-and-write-only).

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Uploads the file at `file` as `name` (`revlog/<from>-<to>`, `snapshot/<rev>`).
pub trait Put: Send + Sync {
    fn put(&self, file: &Path, name: &str) -> Result<(), String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dest {
    /// `file://dir`: a copy into `dir`.
    File(PathBuf),
    /// `s3://bucket/prefix/`.
    S3 { bucket: String, prefix: String },
}

impl Dest {
    pub fn parse(url: &str) -> Result<Dest, String> {
        if let Some(dir) = url.strip_prefix("file://") {
            return Ok(Dest::File(PathBuf::from(dir)));
        }
        let rest = url
            .strip_prefix("s3://")
            .ok_or_else(|| format!("unsupported destination scheme in {url}"))?;
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return Err(format!("{url} names no bucket"));
        }
        let prefix = prefix.trim_matches('/');
        Ok(Dest::S3 {
            bucket: bucket.into(),
            prefix: if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}/")
            },
        })
    }
}

/// The write-only key and where it signs for, from the service's environment.
#[derive(Clone)]
pub struct S3Creds {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    pub region: String,
    /// `AWS_ENDPOINT_URL_S3`: path-style requests to this endpoint instead of AWS's.
    pub endpoint: Option<String>,
    /// `SPIRA_LC_BACKUP_PART_BYTES`: the size of each part of a multipart upload.
    pub part_bytes: u64,
}

impl S3Creds {
    pub fn from_env() -> Result<S3Creds, String> {
        let creds = Self::from_vars(|k| std::env::var(k).ok())?;
        creds.with_part_bytes(spira_config::process::cfg_parse::<u64>("SPIRA_LC_BACKUP_PART_BYTES")?)
    }

    /// These keys cutting a multipart upload into `n`-byte parts, `n` within S3's bounds.
    pub fn with_part_bytes(self, n: u64) -> Result<S3Creds, String> {
        if !(MIN_PART_BYTES..=MAX_PUT_BYTES).contains(&n) {
            return Err(format!("SPIRA_LC_BACKUP_PART_BYTES={n} is not a size from {MIN_PART_BYTES} to {MAX_PUT_BYTES}"));
        }
        Ok(S3Creds { part_bytes: n, ..self })
    }

    /// The same keys read through `lookup`, so a caller can resolve them from a unit's
    /// environment file rather than from this process's environment.
    pub fn from_vars(lookup: impl Fn(&str) -> Option<String>) -> Result<S3Creds, String> {
        let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
        let (Some(access_key), Some(secret_key)) =
            (var("AWS_ACCESS_KEY_ID"), var("AWS_SECRET_ACCESS_KEY"))
        else {
            return Err(
                "AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY are not in the environment".into(),
            );
        };
        let region = var("AWS_DEFAULT_REGION")
            .or_else(|| var("AWS_REGION"))
            .ok_or("AWS_DEFAULT_REGION is not in the environment")?;
        Ok(S3Creds {
            part_bytes: PART_BYTES,
            access_key,
            secret_key,
            session_token: var("AWS_SESSION_TOKEN"),
            region,
            endpoint: var("AWS_ENDPOINT_URL_S3"),
        })
    }
}

/// S3's ceiling on one `PutObject`; a larger file goes up as a multipart upload.
pub const MAX_PUT_BYTES: u64 = 5 << 30;
/// The default size of each part of a multipart upload.
const PART_BYTES: u64 = 256 << 20;
/// S3's floor on every part but the last.
const MIN_PART_BYTES: u64 = 5 << 20;
/// S3's ceiling on the parts of one upload.
const MAX_PARTS: u64 = 10_000;

/// Why a put-only upload did not land.
#[derive(Debug, PartialEq, Eq)]
pub enum PutNew {
    /// Something is already stored under the name, and it is left as it is.
    Exists,
    Failed(String),
}

pub struct Uploader {
    pub dest: Dest,
    /// Needed for an `s3://` destination only.
    pub creds: Option<S3Creds>,
    pub deadline: Duration,
}

/// `connect` bound of every request: a batch-job bound.
const CONNECT_BOUND: Duration = Duration::from_secs(5);

fn encode_path(key: &str) -> String {
    key.split('/').map(query_value).collect::<Vec<_>>().join("/")
}

fn object_url(c: &S3Creds, bucket: &str, key: &str) -> String {
    let key = encode_path(key);
    match &c.endpoint {
        Some(e) => format!("{}/{bucket}/{key}", e.trim_end_matches('/')),
        None => format!("https://{bucket}.s3.{}.amazonaws.com/{key}", c.region),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// `YYYYMMDDTHHMMSSZ` for `secs` since the epoch.
fn amz_date(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn now_amz_date() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    amz_date(secs)
}

/// The `Authorization` header (AWS Signature Version 4, S3) for a request to `url` carrying
/// `headers`, every one of which is signed along with `host`, `x-amz-content-sha256` and
/// `x-amz-date`. `url`'s path and query arrive already percent-encoded.
fn sign(c: &S3Creds, method: &str, url: &str, headers: &[(&str, &str)], payload_sha256: &str, amz_date: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (host, target) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut params: Vec<String> = query.split('&').filter(|p| !p.is_empty()).map(|p| if p.contains('=') { p.to_string() } else { format!("{p}=") }).collect();
    params.sort();
    let mut signed: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string())).collect();
    signed.push(("host".into(), host.into()));
    signed.push(("x-amz-content-sha256".into(), payload_sha256.into()));
    signed.push(("x-amz-date".into(), amz_date.into()));
    signed.sort();
    let canonical_headers: String = signed.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let names = signed.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical = format!("{method}\n{path}\n{}\n{canonical_headers}\n{names}\n{payload_sha256}", params.join("&"));
    let day = &amz_date[..8];
    let scope = format!("{day}/{}/s3/aws4_request", c.region);
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let k = hmac(format!("AWS4{}", c.secret_key).as_bytes(), day);
    let k = hmac(&k, &c.region);
    let k = hmac(&k, "s3");
    let k = hmac(&k, "aws4_request");
    let signature = hex(&hmac(&k, &to_sign));
    format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}", c.access_key)
}

fn query_value(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let from = text.find(open)? + open.len();
    let to = text[from..].find(close)? + from;
    Some(&text[from..to])
}

/// A request body: `len` bytes of `file` from `offset`, whose SHA-256 is `sha256`.
struct Body<'a> {
    file: &'a Path,
    offset: u64,
    len: u64,
    sha256: String,
}

struct Reply {
    etag: Option<String>,
    body: String,
}

fn hash_range(file: &Path, offset: u64, len: u64) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom};
    let io = |e: std::io::Error| format!("{}: {e}", file.display());
    let mut src = std::fs::File::open(file).map_err(io)?;
    src.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut hash = Sha256::new();
    let mut left = len;
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        let take = left.min(buf.len() as u64) as usize;
        src.read_exact(&mut buf[..take]).map_err(io)?;
        hash.update(&buf[..take]);
        left -= take as u64;
    }
    Ok(hex(&hash.finalize()))
}

impl Uploader {
    /// One signed request. A non-2xx answer, or a 2xx whose body holds an `<Error>`, is a
    /// failure; the status it carries is S3's, and 0 when no answer came.
    fn send(&self, method: &str, url: &str, what: &str, extra: &[(&str, &str)], payload: Result<Body, &[u8]>) -> Result<Reply, (u16, String)> {
        use std::io::{Read, Seek, SeekFrom};
        let unsent = |e: String| (0, e);
        let c = self.creds.as_ref().ok_or_else(|| unsent("an s3:// destination with no credentials".into()))?;
        let (sum, len) = match &payload {
            Ok(b) => (b.sha256.clone(), b.len),
            Err(bytes) => (sha256_hex(bytes), bytes.len() as u64),
        };
        let date = now_amz_date();
        let mut headers: Vec<(&str, &str)> = extra.to_vec();
        let token = c.session_token.clone();
        if let Some(t) = &token {
            headers.push(("x-amz-security-token", t));
        }
        let auth = sign(c, method, url, &headers, &sum, &date);
        let agent = ureq::AgentBuilder::new().timeout_connect(CONNECT_BOUND).timeout(self.deadline).build();
        let mut req = agent.request(method, url).set("x-amz-content-sha256", &sum).set("x-amz-date", &date).set("Authorization", &auth).set("Content-Length", &len.to_string());
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let sent = match payload {
            Err(bytes) => req.send_bytes(bytes),
            Ok(b) => {
                let mut src = std::fs::File::open(b.file).map_err(|e| unsent(format!("{}: {e}", b.file.display())))?;
                src.seek(SeekFrom::Start(b.offset)).map_err(|e| unsent(format!("{}: {e}", b.file.display())))?;
                req.send(src.take(b.len))
            }
        };
        match sent {
            Ok(resp) => {
                let etag = resp.header("etag").map(|v| v.trim().to_string());
                let status = resp.status();
                let body = resp.into_string().unwrap_or_default();
                if body.contains("<Error>") {
                    return Err((0, format!("{method} {what}: HTTP {status}: {}", body.trim())));
                }
                Ok(Reply { etag, body })
            }
            Err(ureq::Error::Status(status, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                Err((status, format!("{method} {what}: HTTP {status}: {}", body.trim())))
            }
            Err(e) => Err((0, format!("{method} {what}: {e}"))),
        }
    }

    /// Uploads `file` of `len` bytes as an S3 multipart upload of `part_bytes` parts, each
    /// streamed from `file`; a failed upload is left for the bucket's own expiry, since the
    /// write-only key cannot abort it.
    fn multipart(&self, bucket: &str, key: &str, file: &Path, len: u64, part_bytes: u64) -> Result<(), (u16, String)> {
        let unsent = |e: String| (0, e);
        let c = self.creds.as_ref().ok_or_else(|| unsent("an s3:// destination with no credentials".into()))?;
        let count = len.div_ceil(part_bytes);
        if count > MAX_PARTS {
            return Err(unsent(format!("PUT {key}: {len} bytes needs {count} parts, over S3's limit of {MAX_PARTS}")));
        }
        let base = object_url(c, bucket, key);
        let started = self.send("POST", &format!("{base}?uploads"), &format!("{key}?uploads"), &[], Err(b"")).map_err(|(_, e)| unsent(e))?.body;
        let id = between(&started, "<UploadId>", "</UploadId>")
            .ok_or_else(|| unsent(format!("POST {key}?uploads: no UploadId in {started:?}")))?
            .to_string();
        let upload = format!("uploadId={}", query_value(&id));
        let mut etags = Vec::new();
        for n in 1..=count {
            let offset = (n - 1) * part_bytes;
            let want = part_bytes.min(len - offset);
            let sha256 = hash_range(file, offset, want).map_err(unsent)?;
            let body = Body { file, offset, len: want, sha256 };
            let reply = self.send("PUT", &format!("{base}?partNumber={n}&{upload}"), &format!("{key} part {n}"), &[], Ok(body)).map_err(|(_, e)| unsent(e))?;
            etags.push(reply.etag.ok_or_else(|| unsent(format!("PUT {key} part {n}: no ETag in the response")))?);
        }
        let parts: String = etags
            .iter()
            .enumerate()
            .map(|(i, e)| format!("<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>", i + 1, e.replace('"', "&quot;")))
            .collect();
        let xml = format!("<CompleteMultipartUpload>{parts}</CompleteMultipartUpload>");
        self.send("POST", &format!("{base}?{upload}"), &format!("{key} complete"), &[("content-type", "application/xml")], Err(xml.as_bytes())).map_err(|(_, e)| unsent(e))?;
        Ok(())
    }
}

impl Uploader {
    fn copy(dir: &Path, file: &Path, name: &str) -> Result<(), String> {
        let to = dir.join(name);
        let parent = to.parent().unwrap_or(dir);
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        let tmp = PathBuf::from(format!("{}.tmp", to.display()));
        std::fs::copy(file, &tmp)
            .and_then(|_| std::fs::rename(&tmp, &to))
            .map_err(|e| format!("{}: {e}", to.display()))
    }

    /// A copy that fails rather than replace what `name` already holds: the link is the
    /// atomic claim, so two writers cannot both land.
    fn copy_new(dir: &Path, file: &Path, name: &str) -> Result<(), PutNew> {
        let to = dir.join(name);
        if to.exists() {
            return Err(PutNew::Exists);
        }
        let parent = to.parent().unwrap_or(dir);
        let fail = |e: std::io::Error| PutNew::Failed(format!("{}: {e}", to.display()));
        std::fs::create_dir_all(parent).map_err(fail)?;
        let tmp = PathBuf::from(format!("{}.tmp", to.display()));
        std::fs::copy(file, &tmp).map_err(fail)?;
        let linked = std::fs::hard_link(&tmp, &to);
        let _ = std::fs::remove_file(&tmp);
        match linked {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(PutNew::Exists),
            Err(e) => Err(fail(e)),
        }
    }

    fn signed_put(&self, bucket: &str, key: &str, file: &Path) -> Result<(), String> {
        self.signed(bucket, key, file, false).map_err(|(_, e)| e)
    }

    /// One signed PUT; with `if_absent`, conditional on no object under `key`, and a failure
    /// carries the HTTP status S3 answered (0 when none came) so a refusal can be told
    /// apart.
    fn signed(&self, bucket: &str, key: &str, file: &Path, if_absent: bool) -> Result<(), (u16, String)> {
        let unsent = |e: String| (0, e);
        let c = self
            .creds
            .as_ref()
            .ok_or_else(|| unsent("an s3:// destination with no credentials".into()))?;
        let io = |e: std::io::Error| unsent(format!("{}: {e}", file.display()));
        let len = std::fs::metadata(file).map_err(io)?.len();
        if len > MAX_PUT_BYTES {
            if if_absent {
                return Err(unsent(format!(
                    "PUT {key}: {len} bytes is over S3's single-PUT limit of {MAX_PUT_BYTES}, and a conditional upload has no multipart form"
                )));
            }
            return self.multipart(bucket, key, file, len, c.part_bytes);
        }
        let sha256 = hash_range(file, 0, len).map_err(unsent)?;
        let url = object_url(c, bucket, key);
        let mut extra = vec![("x-amz-server-side-encryption", "AES256")];
        if if_absent {
            extra.push(("if-none-match", "*"));
        }
        self.send("PUT", &url, key, &extra, Ok(Body { file, offset: 0, len, sha256 })).map(|_| ())
    }
}

impl Uploader {
    /// Uploads `file` as `name` only when nothing is stored under it: never an overwrite. On
    /// S3 that is `If-None-Match: *`, which the write-only key can send without reading the
    /// bucket; S3 answers 412 when the object exists.
    pub fn put_new(&self, file: &Path, name: &str) -> Result<(), PutNew> {
        match &self.dest {
            Dest::File(dir) => Self::copy_new(dir, file, name),
            Dest::S3 { bucket, prefix } => {
                let key = format!("{prefix}{name}");
                match self.signed(bucket, &key, file, true) {
                    Ok(()) => Ok(()),
                    Err((412, _)) => Err(PutNew::Exists),
                    Err((_, e)) => Err(PutNew::Failed(e)),
                }
            }
        }
    }
}

impl Put for Uploader {
    fn put(&self, file: &Path, name: &str) -> Result<(), String> {
        match &self.dest {
            Dest::File(dir) => Self::copy(dir, file, name),
            Dest::S3 { bucket, prefix } => {
                self.signed_put(bucket, &format!("{prefix}{name}"), file)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test_id(t1tg0er)]
    #[test]
    fn a_destination_parses_to_a_bucket_and_a_slash_terminated_prefix() {
        assert_eq!(
            Dest::parse("s3://b/spira/lc").unwrap(),
            Dest::S3 {
                bucket: "b".into(),
                prefix: "spira/lc/".into()
            }
        );
        assert_eq!(
            Dest::parse("s3://b").unwrap(),
            Dest::S3 {
                bucket: "b".into(),
                prefix: String::new()
            }
        );
        assert_eq!(Dest::parse("file://x").unwrap(), Dest::File("x".into()));
        let root = testkit::TempDir::new("put-dest-abs");
        assert_eq!(Dest::parse(&format!("file://{}/x", root.display())).unwrap(), Dest::File(format!("{}/x", root.display()).into()));
        assert!(Dest::parse("gs://b").is_err());
        assert!(Dest::parse("s3:///p").is_err());
    }

    #[derive(Clone)]
    struct Req {
        method: String,
        target: String,
        headers: std::collections::BTreeMap<String, String>,
        body: Vec<u8>,
    }

    /// A loopback HTTP listener standing in for S3: records each request and answers it with
    /// `reply`. No process is started.
    struct Listener {
        seen: Arc<std::sync::Mutex<Vec<Req>>>,
        port: u16,
    }

    type Reply = Box<dyn Fn(&Req) -> (u16, String, String) + Send + Sync>;

    impl Listener {
        fn start(reply: Reply) -> Listener {
            use std::io::{BufRead, Read, Write};
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let seen: Arc<std::sync::Mutex<Vec<Req>>> = Default::default();
            let (log, port, reply) = (seen.clone(), l.local_addr().unwrap().port(), Arc::new(reply));
            std::thread::spawn(move || {
                for conn in l.incoming() {
                    let (log, reply) = (log.clone(), reply.clone());
                    std::thread::spawn(move || {
                        let conn = conn.unwrap();
                        let mut r = std::io::BufReader::new(conn.try_clone().unwrap());
                        let mut w = conn;
                        loop {
                            let mut line = String::new();
                            if r.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            let mut parts = line.split_whitespace();
                            let (method, target) = (parts.next().unwrap().to_string(), parts.next().unwrap().to_string());
                            let mut headers = std::collections::BTreeMap::new();
                            loop {
                                let mut h = String::new();
                                r.read_line(&mut h).unwrap();
                                let Some((k, v)) = h.trim_end().split_once(':') else { break };
                                headers.insert(k.to_ascii_lowercase(), v.trim().to_string());
                            }
                            let mut body = vec![0u8; headers.get("content-length").map_or(0, |n| n.parse().unwrap())];
                            r.read_exact(&mut body).unwrap();
                            let req = Req { method, target, headers, body };
                            log.lock().unwrap().push(req.clone());
                            let (status, extra, text) = reply(&req);
                            let out = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{extra}\r\n{text}", text.len());
                            if w.write_all(out.as_bytes()).is_err() {
                                return;
                            }
                        }
                    });
                }
            });
            Listener { seen, port }
        }

        fn requests(&self) -> Vec<Req> {
            self.seen.lock().unwrap().clone()
        }

        fn uploader(&self) -> Uploader {
            let creds = S3Creds { access_key: "AK".into(), secret_key: "sk".into(), session_token: None, region: "r".into(), endpoint: Some(format!("http://127.0.0.1:{}", self.port)), part_bytes: PART_BYTES };
            Uploader { dest: Dest::parse("s3://b/p/").unwrap(), creds: Some(creds), deadline: Duration::from_secs(5) }
        }
    }

    fn answering(status: u16) -> Listener {
        Listener::start(Box::new(move |_| (status, String::new(), if status < 300 { String::new() } else { "<Error/>".into() })))
    }

    #[test_id(tem4z0x)]
    #[test]
    fn a_put_new_to_s3_is_conditional_on_no_object_and_a_412_reads_as_exists() {
        let dir = testkit::TempDir::new("put-new-s3");
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        let ok = answering(200);
        assert_eq!(ok.uploader().put_new(&f, "archive/k"), Ok(()));
        let req = ok.requests().remove(0);
        assert_eq!(req.method, "PUT");
        assert_eq!(req.target, "/b/p/archive/k"); // not a path: an HTTP request target
        assert_eq!(req.headers["if-none-match"], "*");
        assert!(req.headers["authorization"].contains("if-none-match"), "signed");
        assert_eq!(req.headers["x-amz-server-side-encryption"], "AES256");
        assert_eq!(answering(412).uploader().put_new(&f, "archive/k"), Err(PutNew::Exists));
        let refused = answering(403).uploader().put_new(&f, "archive/k");
        assert!(matches!(refused, Err(PutNew::Failed(m)) if m.starts_with("PUT p/archive/k: HTTP 403: <Error/>")));
    }

    #[test_id(t2hezsj)]
    #[test]
    fn a_put_new_to_a_directory_never_replaces_what_is_there() {
        let dir = testkit::TempDir::new("put-new-file");
        let up = Uploader { dest: Dest::File(dir.path().join("off")), creds: None, deadline: Duration::from_secs(5) };
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, b"first").unwrap();
        std::fs::write(&b, b"second").unwrap();
        assert_eq!(up.put_new(&a, "archive/k"), Ok(()));
        assert_eq!(up.put_new(&b, "archive/k"), Err(PutNew::Exists));
        assert_eq!(std::fs::read(dir.path().join("off/archive/k")).unwrap(), b"first");
        let left: Vec<_> = std::fs::read_dir(dir.path().join("off/archive")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["k"], "no temporary left beside it");
    }

    /// An object store's multipart calls, answered as S3 does.
    fn multipart_store(fail_part: Option<u64>, fail_create: bool, error_on_complete: bool) -> Listener {
        Listener::start(Box::new(move |r| {
            if r.target.ends_with("?uploads") {
                if fail_create {
                    return (503, String::new(), "<Error/>".into());
                }
                return (200, String::new(), "<InitiateMultipartUploadResult><UploadId>up/1 2</UploadId></InitiateMultipartUploadResult>".into());
            }
            if r.method == "PUT" {
                let n: u64 = between(&r.target, "partNumber=", "&").unwrap().parse().unwrap();
                assert!(r.target.ends_with("uploadId=up%2F1%202"), "{}", r.target);
                if fail_part == Some(n) {
                    return (503, String::new(), "<Error/>".into());
                }
                return (200, format!("ETag: \"etag{n}\"\r\n"), String::new());
            }
            let text = if error_on_complete { "<Error><Code>InternalError</Code></Error>" } else { "<CompleteMultipartUploadResult/>" };
            (200, String::new(), text.into())
        }))
    }

    #[test_id(t6z6600)]
    #[test]
    fn a_file_over_the_single_put_limit_goes_up_as_ordered_parts_that_rejoin_to_the_file() {
        let dir = testkit::TempDir::new("put-multipart");
        let f = dir.path().join("snap");
        let bytes: Vec<u8> = (0..2_500u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&f, &bytes).unwrap();
        let store = multipart_store(None, false, false);
        store.uploader().multipart("b", "p/snapshot/1", &f, bytes.len() as u64, 1_000).unwrap();
        let calls = store.requests();
        let parts: Vec<&Req> = calls.iter().filter(|r| r.method == "PUT").collect();
        assert_eq!(parts.iter().map(|r| r.body.len()).collect::<Vec<_>>(), [1_000, 1_000, 500]);
        assert_eq!(parts.iter().flat_map(|r| r.body.clone()).collect::<Vec<u8>>(), bytes);
        for p in &parts {
            assert_eq!(p.headers["x-amz-content-sha256"], sha256_hex(&p.body));
        }
        let complete = calls.last().unwrap();
        let xml = String::from_utf8(complete.body.clone()).unwrap();
        assert!(xml.contains("<PartNumber>3</PartNumber><ETag>&quot;etag3&quot;</ETag>"), "{xml}");
        assert!(xml.find("PartNumber>1<").unwrap() < xml.find("PartNumber>2<").unwrap());
        assert_eq!(complete.headers["content-type"], "application/xml");
        assert_eq!(calls.first().unwrap().target, "/b/p/snapshot/1?uploads"); // not a path: an HTTP request target
        assert_eq!(calls.first().unwrap().method, "POST");
        assert!(complete.method == "POST" && complete.target.ends_with("uploadId=up%2F1%202"));
    }

    #[test_id(tzxg6wx)]
    #[test]
    fn a_part_the_store_refuses_fails_the_upload_without_an_abort_the_key_could_not_send() {
        let dir = testkit::TempDir::new("put-multipart-abort");
        let f = dir.path().join("snap");
        std::fs::write(&f, vec![7u8; 2_500]).unwrap();
        let store = multipart_store(Some(2), false, false);
        let err = store.uploader().multipart("b", "k", &f, 2_500, 1_000).unwrap_err();
        assert!(err.1.contains("HTTP 503"), "{err:?}");
        let calls = store.requests();
        assert!(calls.iter().all(|c| c.method != "DELETE"), "no abort");
        assert!(calls.iter().all(|c| !(c.method == "POST" && c.target.contains("uploadId"))), "never completed");
    }

    #[test_id(tpd0skn)]
    #[test]
    fn a_complete_that_answers_200_with_an_error_body_is_a_failed_upload() {
        let dir = testkit::TempDir::new("put-multipart-error-body");
        let f = dir.path().join("snap");
        std::fs::write(&f, vec![7u8; 2_000]).unwrap();
        let err = multipart_store(None, false, true).uploader().multipart("b", "k", &f, 2_000, 1_000).unwrap_err();
        assert!(err.1.contains("<Error>"), "{err:?}");
    }

    #[test_id(t8jjcl2)]
    #[test]
    fn a_snapshot_past_the_single_put_limit_is_sent_by_multipart_not_refused() {
        let dir = testkit::TempDir::new("put-over-limit");
        let f = dir.path().join("snap");
        let file = std::fs::File::create(&f).unwrap();
        file.set_len(MAX_PUT_BYTES + 1).unwrap();
        let store = multipart_store(None, true, false);
        let err = store.uploader().put(&f, "snapshot/1").unwrap_err();
        assert!(!err.contains("single-PUT limit"), "{err}");
        assert!(store.requests()[0].target.ends_with("?uploads"), "{:?}", store.requests().len());
    }

    #[test_id(t04iyrb)]
    #[test]
    fn the_signer_reproduces_the_signature_aws_prints_for_its_get_object_example() {
        let c = S3Creds { access_key: "AKIAIOSFODNN7EXAMPLE".into(), secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(), session_token: None, region: "us-east-1".into(), endpoint: None, part_bytes: PART_BYTES };
        let auth = sign(&c, "GET", "https://examplebucket.s3.amazonaws.com/test.txt", &[("Range", "bytes=0-9")], &sha256_hex(b""), "20130524T000000Z");
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test_id(tib07na)]
    #[test]
    fn the_signer_signs_a_bare_subresource_query_as_aws_prints_for_its_lifecycle_example() {
        let c = S3Creds { access_key: "AKIAIOSFODNN7EXAMPLE".into(), secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(), session_token: None, region: "us-east-1".into(), endpoint: None, part_bytes: PART_BYTES };
        let auth = sign(&c, "GET", "https://examplebucket.s3.amazonaws.com/?lifecycle", &[], &sha256_hex(b""), "20130524T000000Z");
        assert!(auth.ends_with("Signature=fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"), "{auth}");
    }

    #[test_id(t7qj2wg)]
    #[test]
    fn a_single_put_answered_200_with_an_error_body_or_not_answered_at_all_is_a_failure() {
        let dir = testkit::TempDir::new("put-failure-shapes");
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        let body = Listener::start(Box::new(|_| (200, String::new(), "<Error><Code>Slow</Code></Error>".into())));
        let err = body.uploader().put(&f, "k").unwrap_err();
        assert!(err.starts_with("PUT p/k: HTTP 200: <Error>"), "{err}");
        let gone = {
            let up = answering(200).uploader();
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            drop(l);
            let mut creds = up.creds.clone().unwrap();
            creds.endpoint = Some(format!("http://127.0.0.1:{port}"));
            Uploader { creds: Some(creds), ..up }
        };
        let err = gone.signed("b", "p/k", &f, true).unwrap_err();
        assert_eq!(err.0, 0, "{err:?}");
        assert!(err.1.starts_with("PUT p/k: "), "{err:?}");
    }

    #[test_id(tdlih9g)]
    #[test]
    fn the_request_date_is_the_utc_time_in_amz_basic_format() {
        assert_eq!(amz_date(0), "19700101T000000Z");
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        assert_eq!(amz_date(1_782_345_678), "20260625T000118Z");
    }

    #[test_id(treensq)]
    #[test]
    fn the_part_size_defaults_to_256_mib_and_refuses_what_s3_would() {
        let base = S3Creds::from_vars(|k| Some(if k == "AWS_REGION" { "r".into() } else { "x".into() })).unwrap();
        assert_eq!(base.part_bytes, 256 << 20);
        assert_eq!(base.clone().with_part_bytes(10 << 20).unwrap().part_bytes, 10 << 20);
        for bad in [0, 1024, MAX_PUT_BYTES + 1] {
            assert!(base.clone().with_part_bytes(bad).is_err(), "{bad}");
        }
    }
}
