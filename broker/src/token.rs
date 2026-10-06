// token.rs — mint and cache GitHub App installation tokens.
//
// Credentials are read from the registered keys SPIRA_GH_APP_ID,
// SPIRA_GH_APP_INSTALLATION_ID, SPIRA_GH_APP_KEY (path to RSA private key PEM) —
// the config file ONLY, no second source (per Ryan 2026-10-05, one source of config; the
// Concierge decision retiring the ~/.config/spira/github-app.env fallback this file
// used to carry). A value in the config file may itself be a path to a credential file
// (SPIRA_GH_APP_KEY, the PEM path) — that is a value pointing at a file, not a second
// place this code looks for the value itself.
//
// Tokens are cached in $SPIRA_RUN/broker/gh-token.json at mode 600 and reused
// until five minutes before expiry. The file is never world-readable; the key
// path and the token itself never appear in argv or in any log this code writes.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use serde_json::Value;

const REFRESH_BUFFER_S: u64 = 300;

struct AppCreds {
    app_id: String,
    installation_id: String,
    key_path: String,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Concierge decision: App credentials come ONLY from the config file now — no second,
// file-based source. (This used to fall through to a hand-parsed
// ~/.config/spira/github-app.env, named by the now-unused SPIRA_GH_APP_CONFIG, when
// the triplet was unset; that fallback is retired, not merely unreached.)
fn load_creds() -> Result<Option<AppCreds>, String> {
    let app_id     = spira_config::process::cfg("SPIRA_GH_APP_ID")?;
    let install_id = spira_config::process::cfg("SPIRA_GH_APP_INSTALLATION_ID")?;
    let key_path   = spira_config::process::cfg("SPIRA_GH_APP_KEY")?;
    if !app_id.is_empty() && !install_id.is_empty() && !key_path.is_empty() {
        Ok(Some(AppCreds { app_id, installation_id: install_id, key_path }))
    } else {
        Ok(None)
    }
}

// DESIGN QUESTION (flagged, not resolved here): SPIRA_RUN is registered, so this reads
// it via cfg(), but keeps the original best-effort shape — `.ok()` turns a resolution
// failure into "no cache", not a refusal, so `broker token` still mints without caching
// when SPIRA_RUN can't be resolved. That is in tension with "a missing value is a
// refusal"; left as-is because caching is optional to this subcommand and the rest of
// the crate (execute.rs, submit.rs) already refuses hard on SPIRA_RUN where it is not.
fn cache_path() -> Option<PathBuf> {
    let run = spira_config::process::cfg("SPIRA_RUN").ok().filter(|s| !s.is_empty())?;
    Some(PathBuf::from(run).join("broker").join("gh-token.json"))
}

fn read_cache() -> Option<String> {
    let path = cache_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    let token      = v["token"].as_str()?.to_string();
    let expires_at = v["expires_at"].as_u64()?;
    if now_secs() + REFRESH_BUFFER_S < expires_at {
        Some(token)
    } else {
        None
    }
}

fn write_cache(token: &str, expires_at: u64) {
    let Some(path) = cache_path() else { return; };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = serde_json::json!({
        "token": token,
        "expires_at": expires_at,
    }).to_string();
    let _ = std::fs::OpenOptions::new()
        .write(true).create(true).truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut f| f.write_all(content.as_bytes()));
}

fn b64url(data: &[u8]) -> String {
    const TABLE: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let v = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((v >> 18) & 63) as usize]);
        out.push(TABLE[((v >> 12) & 63) as usize]);
        if chunk.len() > 1 { out.push(TABLE[((v >> 6) & 63) as usize]); }
        if chunk.len() > 2 { out.push(TABLE[(v & 63) as usize]); }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn sign_rs256(key_path: &str, message: &str) -> Result<Vec<u8>, String> {
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign", key_path, "-binary"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("openssl spawn: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(message.as_bytes())
            .map_err(|e| format!("openssl write: {e}"))?;
    }
    let out = child.wait_with_output()
        .map_err(|e| format!("openssl wait: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err("openssl sign failed".to_string())
    }
}

fn parse_iso8601(s: &str) -> Option<u64> {
    // "2023-01-01T12:34:56Z" — parse without external deps
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-');
    let y: u64 = dp.next()?.parse().ok()?;
    let m: u64 = dp.next()?.parse().ok()?;
    let d: u64 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let h:   u64 = tp.next()?.parse().ok()?;
    let min: u64 = tp.next()?.parse().ok()?;
    let sec: u64 = tp.next()?.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
    let is_leap = |yr: u64| (yr % 4 == 0 && yr % 100 != 0) || (yr % 400 == 0);
    let mut days: u64 = 0;
    for yr in 1970..y { days += if is_leap(yr) { 366 } else { 365 }; }
    let month_days: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for mi in 0..(m as usize - 1) {
        days += month_days[mi];
        if mi == 1 && is_leap(y) { days += 1; }
    }
    days += d - 1;
    Some(days * 86400 + h * 3600 + min * 60 + sec)
}

fn mint_fresh(creds: &AppCreds) -> Result<(String, u64), String> {
    let now = now_secs();
    let iat = now.saturating_sub(60);
    let exp = now + 540;

    let header  = b64url(b"{\"alg\":\"RS256\",\"typ\":\"JWT\"}");
    let payload = b64url(
        format!("{{\"iat\":{},\"exp\":{},\"iss\":\"{}\"}}",
                iat, exp, creds.app_id).as_bytes()
    );
    let signing_input = format!("{}.{}", header, payload);

    let sig_bytes = sign_rs256(&creds.key_path, &signing_input)?;
    let jwt = format!("{}.{}", signing_input, b64url(&sig_bytes));

    let url = format!(
        "https://api.github.com/app/installations/{}/access_tokens",
        creds.installation_id
    );
    let out = Command::new("curl")
        .args([
            "-sf", "--max-time", "30",
            "-X", "POST",
            "-H", &format!("Authorization: Bearer {}", jwt),
            "-H", "Accept: application/vnd.github.v3+json",
            "-H", "X-GitHub-Api-Version: 2022-11-28",
            &url,
        ])
        .output()
        .map_err(|e| format!("curl: {e}"))?;

    if !out.status.success() {
        return Err(format!("GitHub API error: {}",
                          String::from_utf8_lossy(&out.stderr).trim()));
    }

    let body = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&body)
        .map_err(|e| format!("response parse: {e}: {}", body.chars().take(200).collect::<String>()))?;

    let token = v["token"].as_str()
        .ok_or_else(|| format!("no token field in response"))?
        .to_string();

    let expires_at = v["expires_at"].as_str()
        .and_then(parse_iso8601)
        .unwrap_or(now + 3600);

    Ok((token, expires_at))
}

/// Mint (or return cached) GitHub App installation token.
/// Returns Err when App credentials are not configured.
pub fn mint() -> Result<String, String> {
    let creds = load_creds()?
        .ok_or_else(|| "no App credentials (declare SPIRA_GH_APP_ID/INSTALLATION_ID/KEY in the config file)".to_string())?;

    if let Some(cached) = read_cache() {
        return Ok(cached);
    }

    let (token, expires_at) = mint_fresh(&creds)?;
    write_cache(&token, expires_at);
    Ok(token)
}

/// Environment pairs to inject into gh subprocess calls.
/// Prefers an App installation token when credentials are configured,
/// falls back to SPIRA_BROKER_GH_TOKEN (static) or SPIRA_BROKER_GH_CONFIG_DIR —
/// both registered keys, each legitimately empty when that strategy is not in use.
pub fn gh_env() -> Result<Vec<(String, String)>, String> {
    let mut pairs: Vec<(String, String)> = Vec::new();

    match mint() {
        Ok(tok) => {
            pairs.push(("GH_TOKEN".to_string(), tok));
            return Ok(pairs);
        }
        Err(e) if e.starts_with("no App credentials") => {
            // App not configured — fall through to static token check.
        }
        Err(e) => {
            eprintln!("broker: App token mint failed: {}", e);
            // Fall through; the gh call may still succeed with other auth.
        }
    }

    let tok = spira_config::process::cfg("SPIRA_BROKER_GH_TOKEN")?;
    if !tok.is_empty() {
        pairs.push(("GH_TOKEN".to_string(), tok));
        return Ok(pairs);
    }

    let dir = spira_config::process::cfg("SPIRA_BROKER_GH_CONFIG_DIR")?;
    if !dir.is_empty() {
        pairs.push(("GH_CONFIG_DIR".to_string(), dir));
    }

    Ok(pairs)
}
