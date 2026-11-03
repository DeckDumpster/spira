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
}

impl S3Creds {
    pub fn from_env() -> Result<S3Creds, String> {
        Self::from_vars(|k| std::env::var(k).ok())
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
            access_key,
            secret_key,
            session_token: var("AWS_SESSION_TOKEN"),
            region,
            endpoint: var("AWS_ENDPOINT_URL_S3"),
        })
    }
}

/// S3's ceiling on one `PutObject`.
pub const MAX_PUT_BYTES: u64 = 5 << 30;

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
            return Err(unsent(format!(
                "PUT {key}: {len} bytes is over S3's single-PUT limit of {MAX_PUT_BYTES}"
            )));
        }
        let mut body = std::fs::File::open(file).map_err(io)?;
        let mut hash = Sha256::new();
        std::io::copy(&mut body, &mut hash).map_err(io)?;
        let sum: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let url = match &c.endpoint {
            Some(e) => format!("{}/{bucket}/{key}", e.trim_end_matches('/')),
            None => format!("https://{bucket}.s3.{}.amazonaws.com/{key}", c.region),
        };
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
        let creds = S3Creds { access_key: "AK".into(), secret_key: "sk".into(), session_token: None, region: "r".into(), endpoint: Some("http://s3.test".into()) };
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

    #[test]
    fn a_quoted_config_value_escapes_quotes_and_backslashes() {
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
