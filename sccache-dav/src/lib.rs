//! `sccache-dav` (sp-xjnzl, DESIGN.md): a minimal WebDAV store sccache's own `webdav`
//! backend can read and write. One directory on disk, one Axum fallback handler dispatching
//! on the HTTP method, because the whole surface sccache's `opendal` client ever issues
//! against this kind of store is five verbs: `PROPFIND` (depth 0, to check a path exists —
//! sccache's own `check()` and its sharded-directory `MKCOL` walk), `MKCOL`, `GET`, `PUT`
//! and `DELETE`. No `COPY`, `MOVE`, `PROPPATCH` or directory listing: sccache never issues
//! them (confirmed by reading `RemoteStorage` in sccache 0.18.0's own `src/cache/cache.rs`
//! — `current_size`/`max_size` return `None` without ever listing the store).
//!
//! WHY A STORE AT ALL, not sccache's local disk cache in the VM template (sp-dvfea's
//! original plan). A VM template's own disk cache is warm only for the VM's own builds,
//! never for the host's, and vice versa — two caches that can never share a dependency
//! crate's hit even though the Cargo.lock, the profile and (now) the toolchain are
//! identical. One store reachable from both sides makes a round warm from its FIRST build,
//! not its second (DESIGN-build-cache.md, sp-z61hj, already established this for the host's
//! own gate/aeon/landing builds; this crate is the same cache extended across the ssh
//! boundary, nothing new invented).
//!
//! WHY WEBDAV AND NOT A FILESYSTEM MOUNT OR REDIS. sccache 0.18.0 ships a WebDAV backend
//! that needs nothing more than an HTTP endpoint (no NFS/9p guest-agent plumbing across the
//! Proxmox boundary); a disk-backed store, not an in-RAM one, so it survives a restart and
//! does not compete with the agents already running on this box's RAM (per Ryan, 2026-10-01).

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// A PUT body larger than this is refused (413) rather than buffered without bound. sccache
/// cache entries are compiler outputs for one crate — tens of MB covers every case measured
/// on this box (spira-config/DESIGN-build-cache.md); 512 MiB leaves headroom without
/// removing the bound's point.
pub const MAX_BODY: usize = 512 * 1024 * 1024;

pub struct Config {
    /// `ip:port` to bind. Never `0.0.0.0` — this store is reachable only from the LAN
    /// address the round VMs and this host's own builds already use (G: checked in
    /// [`config_from_env`], not just in the operator's own care).
    pub addr: String,
    /// The directory cache entries live under. Created if missing.
    pub root: PathBuf,
    /// If set, every request must carry `Authorization: Bearer <token>` or get a 401.
    pub token: Option<String>,
}

/// Reads [`Config`] from `SCCACHE_DAV_ADDR` (required), `SCCACHE_DAV_ROOT` (required) and
/// `SCCACHE_DAV_TOKEN` (optional). Fails closed: a missing required variable, or an address
/// that starts with `0.0.0.0`, is an `Err` naming what is wrong rather than a silent default.
pub fn config_from_env() -> Result<Config, String> {
    let addr = std::env::var("SCCACHE_DAV_ADDR")
        .map_err(|_| "SCCACHE_DAV_ADDR must be set (e.g. 192.168.1.56:9431)".to_string())?;
    if addr.trim().is_empty() {
        return Err("SCCACHE_DAV_ADDR must be set (e.g. 192.168.1.56:9431)".to_string());
    }
    if addr.starts_with("0.0.0.0") || addr.starts_with('*') {
        return Err(format!(
            "SCCACHE_DAV_ADDR={addr:?} — this store binds to the LAN address only, never a wildcard"
        ));
    }
    let root = std::env::var("SCCACHE_DAV_ROOT")
        .map_err(|_| "SCCACHE_DAV_ROOT must be set (a directory this store owns)".to_string())?;
    if root.trim().is_empty() {
        return Err("SCCACHE_DAV_ROOT must be set (a directory this store owns)".to_string());
    }
    let token = std::env::var("SCCACHE_DAV_TOKEN").ok().filter(|t| !t.is_empty());
    Ok(Config { addr, root: PathBuf::from(root), token })
}

pub struct AppState {
    pub root: PathBuf,
    pub token: Option<String>,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new().fallback(any(handle)).with_state(state)
}

/// `/a/b/c` -> `Some(a/b/c)`; refuses anything that is not a plain, rooted, traversal-free
/// path (sccache never asks for anything else, and a webdav client exposed on a LAN socket
/// gets no benefit of the doubt on a path it did not ask for).
fn safe_rel_path(uri_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(uri_path);
    let stripped = decoded.strip_prefix('/').unwrap_or(&decoded);
    if stripped.is_empty() {
        return Some(PathBuf::new());
    }
    let mut out = PathBuf::new();
    for seg in stripped.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return None,
            s => out.push(s),
        }
    }
    Some(out)
}

/// The minimal percent-decoding sccache's webdav paths need (hex-digit crate names and
/// shards never produce anything else, but the path arrives percent-encoded regardless).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if let Some(tok) = &state.token {
        let want = format!("Bearer {tok}");
        let got_ok = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(|v| v == want)
            .unwrap_or(false);
        if !got_ok {
            return (StatusCode::UNAUTHORIZED, "sccache-dav: missing or wrong bearer token\n")
                .into_response();
        }
    }
    let uri_path = req.uri().path().to_string();
    let Some(rel) = safe_rel_path(&uri_path) else {
        return (StatusCode::BAD_REQUEST, "sccache-dav: refused path (traversal)\n").into_response();
    };
    let fs_path = state.root.join(&rel);
    let method = req.method().clone();
    match method.as_str() {
        "PROPFIND" => propfind(&fs_path, &uri_path).await,
        "MKCOL" => mkcol(&fs_path).await,
        "GET" => get_file(&fs_path, false).await,
        "HEAD" => get_file(&fs_path, true).await,
        "PUT" => put_file(&fs_path, req).await,
        "DELETE" => delete_file(&fs_path).await,
        _ => (StatusCode::METHOD_NOT_ALLOWED, "sccache-dav: unsupported method\n").into_response(),
    }
}

fn http_date(t: std::io::Result<SystemTime>) -> String {
    httpdate::fmt_http_date(t.unwrap_or_else(|_| SystemTime::now()))
}

/// Depth-0 PROPFIND only: the resource's own existence, size (files) and type. sccache's
/// `webdav_mkcol` walk and `check()` never ask for `Depth: 1`/`infinity` (no listing is used,
/// see this module's doc comment), so a shallower implementation is a correctness choice,
/// not a shortcut: there is no second case to get wrong silently.
async fn propfind(fs_path: &Path, href: &str) -> Response {
    let meta = match tokio::fs::metadata(fs_path).await {
        Ok(m) => m,
        Err(_) => return (StatusCode::NOT_FOUND, "sccache-dav: not found\n").into_response(),
    };
    let is_dir = meta.is_dir();
    let resourcetype = if is_dir {
        "<D:resourcetype><D:collection/></D:resourcetype>"
    } else {
        "<D:resourcetype></D:resourcetype>"
    };
    let len = if is_dir {
        String::new()
    } else {
        format!("<D:getcontentlength>{}</D:getcontentlength>", meta.len())
    };
    let mtime = http_date(meta.modified());
    let href_escaped = href.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>{href_escaped}</D:href><D:propstat><D:prop>\
{len}<D:getlastmodified>{mtime}</D:getlastmodified>{resourcetype}</D:prop>\
<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"
    );
    Response::builder()
        .status(207)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Always idempotent (`create_dir_all`): a collection that already exists is success too
/// (opendal's own client already tolerates 405 for that case, but 201 is simpler to reason
/// about on this side and is itself a documented success code, RFC4918 9.3.1).
async fn mkcol(fs_path: &Path) -> Response {
    match tokio::fs::create_dir_all(fs_path).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("sccache-dav: mkcol {}: {e}\n", fs_path.display()))
                .into_response()
        }
    }
}

async fn get_file(fs_path: &Path, head_only: bool) -> Response {
    let data = match tokio::fs::read(fs_path).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, "sccache-dav: not found\n").into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("sccache-dav: read {}: {e}\n", fs_path.display()))
                .into_response();
        }
    };
    let len = data.len();
    let body = if head_only { Body::empty() } else { Body::from(data) };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, len)
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Writes via a tmp file in the same directory, then renames over the target — so a reader
/// racing a writer (sccache's own concurrent builds) never sees a partial file.
async fn put_file(fs_path: &Path, req: Request) -> Response {
    let body = match axum::body::to_bytes(req.into_body(), MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "sccache-dav: body too large\n").into_response(),
    };
    if let Some(parent) = fs_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("sccache-dav: mkdir {}: {e}\n", parent.display()))
                .into_response();
        }
    }
    if let Err(e) = write_atomic(fs_path, &body).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("sccache-dav: write {}: {e}\n", fs_path.display()))
            .into_response();
    }
    StatusCode::CREATED.into_response()
}

async fn write_atomic(fs_path: &Path, data: &Bytes) -> std::io::Result<()> {
    let tmp = fs_path.with_extension(format!("tmp-{}", std::process::id()));
    tokio::fs::write(&tmp, data).await?;
    tokio::fs::rename(&tmp, fs_path).await
}

async fn delete_file(fs_path: &Path) -> Response {
    match tokio::fs::remove_file(fs_path).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, "sccache-dav: not found\n").into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("sccache-dav: delete {}: {e}\n", fs_path.display()))
                .into_response()
        }
    }
}

/// Exposed for `main`'s startup log line; also used by tests to assert the method dispatch
/// covers exactly the verbs sccache's webdav client issues.
pub const HANDLED_METHODS: &[&str] = &["PROPFIND", "MKCOL", "GET", "HEAD", "PUT", "DELETE"];

#[allow(dead_code)]
fn is_method_handled(m: &Method) -> bool {
    HANDLED_METHODS.contains(&m.as_str())
}
