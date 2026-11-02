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
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
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

    fn signed_put(&self, bucket: &str, key: &str, file: &Path) -> Result<(), String> {
        let c = self
            .creds
            .as_ref()
            .ok_or("an s3:// destination with no credentials")?;
        let mut body = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let mut hash = Sha256::new();
        let len =
            std::io::copy(&mut body, &mut hash).map_err(|e| format!("{}: {e}", file.display()))?;
        if len > MAX_PUT_BYTES {
            return Err(format!(
                "PUT {key}: {len} bytes is over S3's single-PUT limit of {MAX_PUT_BYTES}"
            ));
        }
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
        let args = [
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
            &url,
        ]
        .map(String::from);
        let out = self.proc.run(&ProcSpec {
            prog: "curl".into(),
            args: args.to_vec(),
            stdin: config.into_bytes(),
            deadline: self.deadline + Duration::from_secs(5),
        })?;
        match out.code {
            Some(0) => Ok(()),
            code => Err(format!(
                "PUT {key}: curl exit {}: {}",
                code.map_or("signal".into(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
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

    #[test]
    fn a_quoted_config_value_escapes_quotes_and_backslashes() {
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
