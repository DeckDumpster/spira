//! The off-box destination, written with a key that can only upload: one signed `PUT` per
//! object, under a fixed name, so a retry is the same request. Nothing here lists, reads or
//! deletes the bucket (law-off-box-backups-are-private-and-write-only).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};
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
    pub proc: Arc<dyn Proc>,
    pub deadline: Duration,
}

/// A curl config line's quoted value: the credentials go to curl on stdin, never on argv.
fn quoted(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

fn object_url(c: &S3Creds, bucket: &str, key: &str) -> String {
    match &c.endpoint {
        Some(e) => format!("{}/{bucket}/{key}", e.trim_end_matches('/')),
        None => format!("https://{bucket}.s3.{}.amazonaws.com/{key}", c.region),
    }
}

fn auth_config(c: &S3Creds) -> String {
    let mut config = format!(
        "user = {}\n",
        quoted(&format!("{}:{}", c.access_key, c.secret_key))
    );
    if let Some(t) = &c.session_token {
        config.push_str(&format!(
            "header = {}\n",
            quoted(&format!("x-amz-security-token: {t}"))
        ));
    }
    config
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A query value with everything but unreserved characters percent-encoded.
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

impl Uploader {
    /// One signed request of a multipart upload. A part (`body` set) answers with its response
    /// headers on stdout; the other calls answer with their body.
    fn call(&self, method: &str, url: &str, body: Option<(&Path, &str)>, xml: bool) -> Result<String, String> {
        let c = self.creds.as_ref().ok_or("an s3:// destination with no credentials")?;
        let secs = self.deadline.as_secs().max(1).to_string();
        let sum = match body {
            Some((_, sum)) => sum.to_string(),
            None => sha256_hex(b""),
        };
        let mut args: Vec<String> = ["-q", "-sS", "--connect-timeout", "5", "--fail-with-body", "--max-time", &secs, "-X", method]
            .map(String::from)
            .to_vec();
        match body {
            Some((path, _)) if xml => args.extend(["--data-binary".into(), format!("@{}", path.display()), "-H".into(), "Content-Type: application/xml".into()]),
            Some((path, _)) => args.extend(["-T".into(), path.display().to_string(), "-D".into(), "-".into(), "-o".into(), "/dev/null".into()]),
            None => args.extend(["--data-binary".into(), String::new()]),
        }
        args.extend(["--aws-sigv4".into(), format!("aws:amz:{}:s3", c.region), "-K".into(), "-".into()]);
        args.extend(["-H".into(), format!("x-amz-content-sha256: {sum}")]);
        args.push(url.into());
        let out = self.proc.run(&ProcSpec { prog: "curl".into(), args, stdin: auth_config(c).into_bytes(), deadline: self.deadline + Duration::from_secs(5) })?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        match out.code {
            Some(0) if !text.contains("<Error>") => Ok(text),
            code => Err(format!(
                "{method} {url}: curl exit {}: {}{}",
                code.map_or("signal".into(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stderr).trim(),
                if text.contains("<Error>") { text.as_str() } else { "" }
            )),
        }
    }

    /// Uploads `file` of `len` bytes as an S3 multipart upload of `part_bytes` parts, each cut
    /// into a temporary beside `file`; a failed upload is aborted so its parts are not kept.
    fn multipart(&self, bucket: &str, key: &str, file: &Path, len: u64, part_bytes: u64) -> Result<(), (u16, String)> {
        let unsent = |e: String| (0, e);
        let c = self.creds.as_ref().ok_or_else(|| unsent("an s3:// destination with no credentials".into()))?;
        let count = len.div_ceil(part_bytes);
        if count > MAX_PARTS {
            return Err(unsent(format!("PUT {key}: {len} bytes needs {count} parts, over S3's limit of {MAX_PARTS}")));
        }
        let base = object_url(c, bucket, key);
        let started = self.call("POST", &format!("{base}?uploads"), None, false).map_err(unsent)?;
        let id = between(&started, "<UploadId>", "</UploadId>")
            .ok_or_else(|| unsent(format!("POST {key}?uploads: no UploadId in {started:?}")))?
            .to_string();
        let upload = format!("uploadId={}", query_value(&id));
        let piece = PathBuf::from(format!("{}.part", file.display()));
        let done = format!("{}.complete", file.display());
        let sent = (|| -> Result<(), String> {
            let mut src = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
            let mut etags = Vec::new();
            for n in 1..=count {
                let want = part_bytes.min(len - (n - 1) * part_bytes);
                let mut out = std::fs::File::create(&piece).map_err(|e| format!("{}: {e}", piece.display()))?;
                let mut hash = Sha256::new();
                let mut left = want;
                let mut buf = vec![0u8; 1 << 20];
                while left > 0 {
                    let take = left.min(buf.len() as u64) as usize;
                    std::io::Read::read_exact(&mut src, &mut buf[..take]).map_err(|e| format!("{}: {e}", file.display()))?;
                    hash.update(&buf[..take]);
                    std::io::Write::write_all(&mut out, &buf[..take]).map_err(|e| format!("{}: {e}", piece.display()))?;
                    left -= take as u64;
                }
                drop(out);
                let sum: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
                let headers = self.call("PUT", &format!("{base}?partNumber={n}&{upload}"), Some((&piece, &sum)), false)?;
                let etag = headers
                    .lines()
                    .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("etag")).map(|(_, v)| v.trim().to_string()))
                    .ok_or_else(|| format!("PUT {key} part {n}: no ETag in {headers:?}"))?;
                etags.push(etag);
            }
            let parts: String = etags
                .iter()
                .enumerate()
                .map(|(i, e)| format!("<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>", i + 1, e.replace('"', "&quot;")))
                .collect();
            let xml = format!("<CompleteMultipartUpload>{parts}</CompleteMultipartUpload>");
            std::fs::write(&done, &xml).map_err(|e| format!("{done}: {e}"))?;
            self.call("POST", &format!("{base}?{upload}"), Some((Path::new(&done), &sha256_hex(xml.as_bytes()))), true)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&piece);
        let _ = std::fs::remove_file(&done);
        if let Err(e) = sent {
            return Err((0, e));
        }
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
    /// carries the HTTP status curl reports (0 when it reports none) so a refusal can be told
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
        let mut body = std::fs::File::open(file).map_err(io)?;
        let mut hash = Sha256::new();
        std::io::copy(&mut body, &mut hash).map_err(io)?;
        let sum: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let url = object_url(c, bucket, key);
        let config = auth_config(c);
        let secs = self.deadline.as_secs().max(1).to_string();
        let mut args: Vec<String> = [
            "-q",
            "-sS",
            "--connect-timeout",
            "5",
            "--fail-with-body",
            "--max-time",
            &secs,
            "-X",
            "PUT",
            "-T",
            &file.display().to_string(),
            "--aws-sigv4",
            &format!("aws:amz:{}:s3", c.region),
            "-K",
            "-",
            "-H",
            &format!("x-amz-content-sha256: {sum}"),
            "-H",
            "x-amz-server-side-encryption: AES256",
            "-o",
            "/dev/null",
        ]
        .map(String::from)
        .to_vec();
        if if_absent {
            args.extend(["-H", "If-None-Match: *", "--write-out", "%{http_code}"].map(String::from));
        }
        args.push(url);
        let out = self
            .proc
            .run(&ProcSpec {
                prog: "curl".into(),
                args,
                stdin: config.into_bytes(),
                deadline: self.deadline + Duration::from_secs(5),
            })
            .map_err(unsent)?;
        match out.code {
            Some(0) => Ok(()),
            code => Err((
                String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0),
                format!(
                    "PUT {key}: curl exit {}: {}",
                    code.map_or("signal".into(), |c| c.to_string()),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            )),
        }
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
        assert_eq!(Dest::parse("file:///x").unwrap(), Dest::File("/x".into()));
        assert!(Dest::parse("gs://b").is_err());
        assert!(Dest::parse("s3:///p").is_err());
    }

    /// A fake curl that answers every PUT with `status`, as `--write-out` prints it, and
    /// records the argv. No process is started.
    struct Answering(u16, std::sync::Mutex<Vec<Vec<String>>>);

    impl Proc for Answering {
        fn run(&self, spec: &ProcSpec) -> Result<runtime::ports::ProcOutput, String> {
            self.1.lock().unwrap().push(spec.args.clone());
            let code = if self.0 < 400 { 0 } else { 22 };
            Ok(runtime::ports::ProcOutput { code: Some(code), stdout: self.0.to_string().into_bytes(), stderr: Vec::new() })
        }
    }

    fn s3(status: u16) -> (Uploader, Arc<Answering>) {
        let curl = Arc::new(Answering(status, Default::default()));
        let creds = S3Creds { access_key: "AK".into(), secret_key: "sk".into(), session_token: None, region: "r".into(), endpoint: Some("http://s3.test".into()), part_bytes: PART_BYTES };
        let up = Uploader { dest: Dest::parse("s3://b/p/").unwrap(), creds: Some(creds), proc: curl.clone(), deadline: Duration::from_secs(5) };
        (up, curl)
    }

    #[test]
    fn a_put_new_to_s3_is_conditional_on_no_object_and_a_412_reads_as_exists() {
        let dir = testkit::TempDir::new("put-new-s3");
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        let (up, curl) = s3(200);
        assert_eq!(up.put_new(&f, "archive/k"), Ok(()));
        let args = curl.1.lock().unwrap()[0].clone();
        assert!(args.windows(2).any(|w| w == ["-H", "If-None-Match: *"]), "{args:?}");
        assert_eq!(args.last().unwrap(), "http://s3.test/b/p/archive/k");
        let (up, _) = s3(412);
        assert_eq!(up.put_new(&f, "archive/k"), Err(PutNew::Exists));
        let (up, _) = s3(403);
        assert!(matches!(up.put_new(&f, "archive/k"), Err(PutNew::Failed(m)) if m.contains("PUT p/archive/k")));
    }

    #[test]
    fn a_put_new_to_a_directory_never_replaces_what_is_there() {
        let dir = testkit::TempDir::new("put-new-file");
        let up = Uploader { dest: Dest::File(dir.path().join("off")), creds: None, proc: Arc::new(Answering(200, Default::default())), deadline: Duration::from_secs(5) };
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, b"first").unwrap();
        std::fs::write(&b, b"second").unwrap();
        assert_eq!(up.put_new(&a, "archive/k"), Ok(()));
        assert_eq!(up.put_new(&b, "archive/k"), Err(PutNew::Exists));
        assert_eq!(std::fs::read(dir.path().join("off/archive/k")).unwrap(), b"first");
        let left: Vec<_> = std::fs::read_dir(dir.path().join("off/archive")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["k"], "no temporary left beside it");
    }

    /// A fake object store behind curl: answers the multipart calls as S3 does and keeps what
    /// each part held.
    #[derive(Default)]
    struct Store {
        calls: std::sync::Mutex<Vec<String>>,
        parts: std::sync::Mutex<Vec<Vec<u8>>>,
        completed: std::sync::Mutex<Option<String>>,
        fail_part: Option<u64>,
        fail_create: bool,
    }

    impl Proc for Store {
        fn run(&self, spec: &ProcSpec) -> Result<runtime::ports::ProcOutput, String> {
            let a = &spec.args;
            let url = a.last().unwrap().clone();
            let method = a[a.iter().position(|x| x == "-X").unwrap() + 1].clone();
            self.calls.lock().unwrap().push(format!("{method} {url}"));
            let ok = |out: String| Ok(runtime::ports::ProcOutput { code: Some(0), stdout: out.into_bytes(), stderr: Vec::new() });
            if url.ends_with("?uploads") {
                if self.fail_create {
                    return Ok(runtime::ports::ProcOutput { code: Some(22), stdout: Vec::new(), stderr: b"503".to_vec() });
                }
                return ok("<InitiateMultipartUploadResult><UploadId>up/1 2</UploadId></InitiateMultipartUploadResult>".into());
            }
            if method == "PUT" {
                let n: u64 = between(&url, "partNumber=", "&").unwrap().parse().unwrap();
                assert!(url.ends_with("uploadId=up%2F1%202"), "{url}");
                if self.fail_part == Some(n) {
                    return Ok(runtime::ports::ProcOutput { code: Some(22), stdout: Vec::new(), stderr: b"503".to_vec() });
                }
                let file = &a[a.iter().position(|x| x == "-T").unwrap() + 1];
                let body = std::fs::read(file).unwrap();
                let want = a.iter().find_map(|x| x.strip_prefix("x-amz-content-sha256: ")).unwrap();
                assert_eq!(want, sha256_hex(&body));
                self.parts.lock().unwrap().push(body);
                return ok(format!("HTTP/1.1 200 OK\r\nETag: \"etag{n}\"\r\n\r\n"));
            }
            if method == "POST" {
                let file = a.iter().find_map(|x| x.strip_prefix("@")).unwrap();
                *self.completed.lock().unwrap() = Some(std::fs::read_to_string(file).unwrap());
                return ok("<CompleteMultipartUploadResult/>".into());
            }
            ok(String::new())
        }
    }

    fn uploader_on(store: &Arc<Store>) -> Uploader {
        let creds = S3Creds { access_key: "AK".into(), secret_key: "sk".into(), session_token: None, region: "r".into(), endpoint: Some("http://s3.test".into()), part_bytes: PART_BYTES };
        Uploader { dest: Dest::parse("s3://b/p/").unwrap(), creds: Some(creds), proc: store.clone(), deadline: Duration::from_secs(5) }
    }

    #[test]
    fn a_file_over_the_single_put_limit_goes_up_as_ordered_parts_that_rejoin_to_the_file() {
        let dir = testkit::TempDir::new("put-multipart");
        let f = dir.path().join("snap");
        let bytes: Vec<u8> = (0..2_500u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&f, &bytes).unwrap();
        let store = Arc::new(Store::default());
        let up = uploader_on(&store);
        up.multipart("b", "p/snapshot/1", &f, bytes.len() as u64, 1_000).unwrap();
        let parts = store.parts.lock().unwrap();
        assert_eq!(parts.iter().map(Vec::len).collect::<Vec<_>>(), [1_000, 1_000, 500]);
        assert_eq!(parts.concat(), bytes);
        let xml = store.completed.lock().unwrap().clone().unwrap();
        assert!(xml.contains("<PartNumber>3</PartNumber><ETag>&quot;etag3&quot;</ETag>"), "{xml}");
        assert!(xml.find("PartNumber>1<").unwrap() < xml.find("PartNumber>2<").unwrap());
        let calls = store.calls.lock().unwrap();
        assert_eq!(calls.first().unwrap(), "POST http://s3.test/b/p/snapshot/1?uploads");
        assert!(calls.last().unwrap().starts_with("POST ") && calls.last().unwrap().ends_with("uploadId=up%2F1%202"));
        assert!(!dir.path().join("snap.part").exists() && !dir.path().join("snap.complete").exists());
    }

    #[test]
    fn a_part_the_store_refuses_fails_the_upload_without_an_abort_the_key_could_not_send_and_leaves_no_temporary() {
        let dir = testkit::TempDir::new("put-multipart-abort");
        let f = dir.path().join("snap");
        std::fs::write(&f, vec![7u8; 2_500]).unwrap();
        let store = Arc::new(Store { fail_part: Some(2), ..Store::default() });
        let err = uploader_on(&store).multipart("b", "k", &f, 2_500, 1_000).unwrap_err();
        assert!(err.1.contains("curl exit 22"), "{err:?}");
        assert!(store.calls.lock().unwrap().iter().all(|c| !c.starts_with("DELETE ")), "{:?}", store.calls);
        assert!(store.completed.lock().unwrap().is_none());
        assert!(!dir.path().join("snap.part").exists());
    }

    #[test]
    fn a_snapshot_past_the_single_put_limit_is_sent_by_multipart_not_refused() {
        let dir = testkit::TempDir::new("put-over-limit");
        let f = dir.path().join("snap");
        let file = std::fs::File::create(&f).unwrap();
        file.set_len(MAX_PUT_BYTES + 1).unwrap();
        let store = Arc::new(Store { fail_create: true, ..Store::default() });
        let err = uploader_on(&store).put(&f, "snapshot/1").unwrap_err();
        assert!(!err.contains("single-PUT limit"), "{err}");
        assert!(store.calls.lock().unwrap()[0].ends_with("?uploads"), "{:?}", store.calls);
    }

    #[test]
    fn the_part_size_defaults_to_256_mib_and_refuses_what_s3_would() {
        let base = S3Creds::from_vars(|k| Some(if k == "AWS_REGION" { "r".into() } else { "x".into() })).unwrap();
        assert_eq!(base.part_bytes, 256 << 20);
        assert_eq!(base.clone().with_part_bytes(10 << 20).unwrap().part_bytes, 10 << 20);
        for bad in [0, 1024, MAX_PUT_BYTES + 1] {
            assert!(base.clone().with_part_bytes(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_quoted_config_value_escapes_quotes_and_backslashes() {
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
