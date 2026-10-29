//! The round artifact store: one tree's release executables, stored once under the build's
//! identity and fetched by every later consumer (DESIGN.md "artifacts").
//!
//! Wire layout, on the box's shared WebDAV store: `artifacts/<key>/bin.tar` then, last,
//! `artifacts/<key>/MANIFEST`. The manifest is the commit record — a reader that finds no
//! manifest, or one whose checksums its tarball does not meet, has a miss, never a partial.
//! The round VM writes and reads the same layout from bash (`round-vm` REMOTE_SCRIPT).

use crate::fsutil;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// What was built: the whole workspace's executables, optimised.
pub const PROFILE: &str = "release-workspace";
pub const TARBALL: &str = "bin.tar";
pub const MANIFEST: &str = "MANIFEST";
const TRANSFER_SECS: &str = "600";

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(p: &Path) -> Result<String, String> {
    let mut f = fs::File::open(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The compiler's identity from `rustc -vV`: its release and commit-hash, space-joined.
pub fn toolchain_id(rustc_vv: &str) -> String {
    let v: Vec<&str> = rustc_vv.lines().filter_map(|l| l.strip_prefix("release: ").or_else(|| l.strip_prefix("commit-hash: "))).map(str::trim).collect();
    v.join(" ")
}

/// The build's identity: exactly the inputs that decide the executables. An artifact is used
/// only on an exact match of this key.
pub fn key(tree: &str, toolchain: &str) -> String {
    sha256_hex(format!("{tree} {toolchain} {PROFILE}\n").as_bytes())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    pub key: String,
    pub tree: String,
    pub toolchain: String,
    pub tarball_sha256: String,
    pub tarball_bytes: u64,
    /// `(name, sha256)` of every executable in the tarball, sorted by name.
    pub files: Vec<(String, String)>,
}

impl Manifest {
    pub fn render(&self) -> String {
        let mut s = format!("key={}\ntree={}\ntoolchain={}\ntarball_sha256={}\ntarball_bytes={}\n", self.key, self.tree, self.toolchain, self.tarball_sha256, self.tarball_bytes);
        for (n, h) in &self.files {
            s.push_str(&format!("file {h} {n}\n"));
        }
        s
    }

    pub fn parse(text: &str) -> Result<Manifest, String> {
        let (mut key, mut tree, mut toolchain, mut sum, mut bytes) = (None, None, None, None, None);
        let mut files = Vec::new();
        for l in text.lines() {
            if let Some(rest) = l.strip_prefix("file ") {
                let (h, n) = rest.split_once(' ').ok_or("a file line without a name")?;
                files.push((n.to_string(), h.to_string()));
            } else if let Some((k, v)) = l.split_once('=') {
                match k {
                    "key" => key = Some(v.to_string()),
                    "tree" => tree = Some(v.to_string()),
                    "toolchain" => toolchain = Some(v.to_string()),
                    "tarball_sha256" => sum = Some(v.to_string()),
                    "tarball_bytes" => bytes = Some(v.parse::<u64>().map_err(|_| "tarball_bytes is not a number")?),
                    _ => {}
                }
            }
        }
        let need = |v: Option<String>, k: &str| v.ok_or(format!("no {k}= line"));
        let m = Manifest { key: need(key, "key")?, tree: need(tree, "tree")?, toolchain: toolchain.unwrap_or_default(), tarball_sha256: need(sum, "tarball_sha256")?, tarball_bytes: bytes.ok_or("no tarball_bytes= line")?, files };
        if m.files.is_empty() {
            return Err("lists no files".into());
        }
        Ok(m)
    }
}

/// The store's wire: a path under its endpoint, moved to or from a local file.
pub trait Dav {
    /// Fetches `path` into `dest`; `Ok(false)` is a 404.
    fn get_to(&self, path: &str, dest: &Path) -> Result<bool, String>;
    fn put_from(&self, path: &str, src: &Path) -> Result<(), String>;
}

pub struct CurlDav {
    pub endpoint: String,
}

impl CurlDav {
    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.endpoint.trim_end_matches('/'))
    }
}

impl Dav for CurlDav {
    fn get_to(&self, path: &str, dest: &Path) -> Result<bool, String> {
        let out = spira_config::bounded::bounded_for("curl", TRANSFER_SECS)
            .args(["-sS", "-o"])
            .arg(dest)
            .args(["-w", "%{http_code}"])
            .arg(self.url(path))
            .output()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        match String::from_utf8_lossy(&out.stdout).trim() {
            "200" => Ok(true),
            "404" => Ok(false),
            c => Err(format!("GET {path}: status {c:?} {}", String::from_utf8_lossy(&out.stderr).trim())),
        }
    }

    fn put_from(&self, path: &str, src: &Path) -> Result<(), String> {
        let st = spira_config::bounded::bounded_for("curl", TRANSFER_SECS)
            .args(["-sS", "-f", "-o", "/dev/null", "-T"])
            .arg(src)
            .arg(self.url(path))
            .status()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("PUT {path} failed ({st})"))
        }
    }
}

pub fn executables(dir: &Path) -> Result<Vec<String>, String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut v: Vec<String> = rd.flatten().filter(|e| fs::metadata(e.path()).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    Ok(v)
}

/// Stores `dir`'s executables under `key`: tarball first, manifest last.
pub fn publish(dav: &dyn Dav, key: &str, tree: &str, toolchain: &str, dir: &Path, scratch: &Path) -> Result<(), String> {
    let names = executables(dir)?;
    if names.is_empty() {
        return Err(format!("{} holds no executables", dir.display()));
    }
    let tarball = scratch.join(TARBALL);
    let st = spira_config::bounded::bounded_for("tar", TRANSFER_SECS).arg("-cf").arg(&tarball).arg("-C").arg(dir).args(&names).status().map_err(|e| format!("cannot run tar: {e}"))?;
    if !st.success() {
        return Err(format!("tar -c of {} failed ({st})", dir.display()));
    }
    let files = names.iter().map(|n| Ok((n.clone(), sha256_file(&dir.join(n))?))).collect::<Result<Vec<_>, String>>()?;
    let m = Manifest { key: key.into(), tree: tree.into(), toolchain: toolchain.into(), tarball_sha256: sha256_file(&tarball)?, tarball_bytes: fs::metadata(&tarball).map_err(|e| e.to_string())?.len(), files };
    dav.put_from(&format!("artifacts/{key}/{TARBALL}"), &tarball)?;
    let mf = scratch.join(MANIFEST);
    fs::write(&mf, m.render()).map_err(|e| format!("cannot write {}: {e}", mf.display()))?;
    dav.put_from(&format!("artifacts/{key}/{MANIFEST}"), &mf)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    Hit,
    /// Nothing usable under this key; the reason is for the log. The caller builds.
    Miss(String),
}

/// Fetches `key` into the new directory `into`. Anything short of a manifest whose every
/// checksum the bytes meet is a miss, and leaves `into` absent.
pub fn fetch(dav: &dyn Dav, key: &str, into: &Path, scratch: &Path) -> Fetched {
    match try_fetch(dav, key, into, scratch) {
        Ok(Some(())) => Fetched::Hit,
        Ok(None) => Fetched::Miss("not stored".into()),
        Err(e) => {
            let _ = fsutil::remove_tree(into);
            Fetched::Miss(e)
        }
    }
}

fn try_fetch(dav: &dyn Dav, key: &str, into: &Path, scratch: &Path) -> Result<Option<()>, String> {
    let mf = scratch.join(MANIFEST);
    if !dav.get_to(&format!("artifacts/{key}/{MANIFEST}"), &mf)? {
        return Ok(None);
    }
    let m = Manifest::parse(&fs::read_to_string(&mf).map_err(|e| e.to_string())?).map_err(|e| format!("manifest {e}"))?;
    if m.key != key {
        return Err(format!("manifest is for key {}, not {key}", m.key));
    }
    let tarball = scratch.join(TARBALL);
    if !dav.get_to(&format!("artifacts/{key}/{TARBALL}"), &tarball)? {
        return Err("manifest present but tarball absent".into());
    }
    let len = fs::metadata(&tarball).map_err(|e| e.to_string())?.len();
    if len != m.tarball_bytes || sha256_file(&tarball)? != m.tarball_sha256 {
        return Err(format!("tarball ({len} bytes) does not match its manifest"));
    }
    fs::create_dir_all(into).map_err(|e| format!("cannot create {}: {e}", into.display()))?;
    let st = spira_config::bounded::bounded_for("tar", TRANSFER_SECS).arg("-xf").arg(&tarball).arg("-C").arg(into).status().map_err(|e| format!("cannot run tar: {e}"))?;
    if !st.success() {
        return Err(format!("tar -x failed ({st})"));
    }
    let want: Vec<&String> = m.files.iter().map(|f| &f.0).collect();
    let got = executables(into)?;
    if got.iter().collect::<Vec<_>>() != want {
        return Err("tarball members differ from the manifest's list".into());
    }
    for (n, h) in &m.files {
        if &sha256_file(&into.join(n))? != h {
            return Err(format!("{n} does not match its manifest checksum"));
        }
    }
    Ok(Some(()))
}

/// The store this box names, as the release producer reaches it; `None` when none is configured.
pub fn configured_dav() -> Option<CurlDav> {
    spira_config::build::Store::from_env().map(|s| CurlDav { endpoint: s.endpoint })
}

/// `rustc -vV` for the compiler a build here would use.
pub fn local_toolchain() -> Result<String, String> {
    let out = spira_config::bounded::bounded("rustc").arg("-vV").output().map_err(|e| format!("cannot run rustc: {e}"))?;
    let id = toolchain_id(&String::from_utf8_lossy(&out.stdout));
    if id.is_empty() {
        return Err("rustc -vV named no release".into());
    }
    Ok(id)
}

pub fn scratch_dir(under: &Path, tag: &str) -> Result<PathBuf, String> {
    let d = under.join(format!(".artifacts-{tag}-{}", std::process::id()));
    let _ = fsutil::remove_tree(&d);
    fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    Ok(d)
}
