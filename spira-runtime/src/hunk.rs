//! hunk's local session daemon, asked in-process what `hunk session list` asks it. The daemon
//! answers only a signed session: a challenge/proof exchange under the caller key hunk keeps
//! beside its descriptor, then one signed request per call. The daemon's own signatures are not
//! verified here: the exchange is loopback, and a forged answer can only mis-steer a window.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::Ed25519KeyPair;
use serde_json::{json, Value};

const APP_ID: &str = "dev.hunk";
const APP_REVISION: u64 = 16;
const BROKER_REVISION: u64 = 1;
const AUTH_DOMAIN: &str = "dev.hunk.session-broker.v1";
const CHALLENGE_PATH: &str = "/session-auth/challenge";
const PROOF_PATH: &str = "/session-auth/proof";
const API_PATH: &str = "/session-api";
const CALL_TIMEOUT: Duration = Duration::from_secs(1);
const BODY_LIMIT: u64 = 1 << 20;

/// The ids of hunk's live sessions; an error when no daemon answered.
pub fn live_ids(runtime_dir: &Path) -> Result<Vec<String>, String> {
    let caller = Caller::read(&runtime_dir.join("security-v1/caller.json"))?;
    let mut errors = vec![];
    for origin in origins(runtime_dir)? {
        match list(&origin, &caller) {
            Ok(ids) => return Ok(ids),
            Err(e) => errors.push(format!("{origin}: {e}")),
        }
    }
    Err(errors.join("; "))
}

/// Every daemon the descriptor files name, newest first.
fn origins(runtime_dir: &Path) -> Result<Vec<String>, String> {
    let entries = fs::read_dir(runtime_dir).map_err(|e| format!("{}: {e}", runtime_dir.display()))?;
    let mut found: Vec<(SystemTime, String)> = entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("daemon-") && n.ends_with(".json")))
        .filter_map(|e| {
            let d: Value = serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok()?;
            let host = match d["host"].as_str()? {
                h @ ("127.0.0.1" | "localhost") => h.to_string(),
                "::1" => "[::1]".to_string(),
                _ => return None,
            };
            Some((e.metadata().ok()?.modified().ok()?, format!("http://{host}:{}", d["port"].as_u64()?)))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    if found.is_empty() {
        return Err(format!("no loopback daemon descriptor in {}", runtime_dir.display()));
    }
    Ok(found.into_iter().map(|(_, o)| o).collect())
}

struct Caller {
    key: Ed25519KeyPair,
    key_id: String,
    grant_id: String,
}

impl Caller {
    fn read(path: &Path) -> Result<Caller, String> {
        let v: Value = serde_json::from_str(&fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = |v: &Value| v.as_str().map(str::to_string);
        let bad = || format!("{}: not a caller credential", path.display());
        let pkcs8 = b64url_decode(v["privateKey"].as_str().ok_or_else(bad)?).ok_or_else(bad)?;
        Ok(Caller {
            key: Ed25519KeyPair::from_pkcs8_maybe_unchecked(&pkcs8).map_err(|_| bad())?,
            key_id: text(&v["keyId"]).ok_or_else(bad)?,
            grant_id: text(&v["grant"]["grantId"]).ok_or_else(bad)?,
        })
    }

    fn sign(&self, transcript: &str) -> String {
        b64url(self.key.sign(transcript.as_bytes()).as_ref())
    }
}

fn random_id() -> Result<String, String> {
    let mut raw = [0u8; 24];
    SystemRandom::new().fill(&mut raw).map_err(|_| "no randomness".to_string())?;
    Ok(format!("b_{}_0", b64url(&raw)))
}

fn post(agent: &ureq::Agent, url: &str, headers: &[(&str, &str)], body: &str) -> Result<Value, String> {
    let mut req = agent.post(url).set("content-type", "application/json");
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let resp = req.send_string(body).map_err(|e| e.to_string())?;
    let mut text = String::new();
    resp.into_reader().take(BODY_LIMIT).read_to_string(&mut text).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn list(origin: &str, caller: &Caller) -> Result<Vec<String>, String> {
    let agent = ureq::AgentBuilder::new().timeout(CALL_TIMEOUT).build();
    let endpoint = format!("{origin}{CHALLENGE_PATH}");
    let hello = json!({
        "role": "caller", "appId": APP_ID, "endpoint": endpoint,
        "keyId": caller.key_id, "grantId": caller.grant_id, "initiatorNonce": random_id()?,
        "proposal": {"brokerRevision": BROKER_REVISION, "appRevision": APP_REVISION, "features": []},
    });
    let challenge = post(&agent, &endpoint, &[], &hello.to_string()).map_err(|e| format!("challenge: {e}"))?;
    let field = |v: &Value, k: &str| v[k].as_str().map(str::to_string).ok_or_else(|| format!("answer lacks {k}"));
    let generation = field(&challenge, "generation")?;
    let transcript = canonical(&json!({
        "appId": APP_ID, "domain": format!("{AUTH_DOMAIN}/caller-hello"), "endpoint": endpoint,
        "generation": generation, "grantId": caller.grant_id, "initiatorNonce": hello["initiatorNonce"], "keyId": caller.key_id,
        "proposal": hello["proposal"], "responderNonce": field(&challenge, "responderNonce")?,
    }));
    let proof = json!({"challengeId": field(&challenge, "challengeId")?, "signature": caller.sign(&transcript)});
    let session = post(&agent, &format!("{origin}{PROOF_PATH}"), &[], &proof.to_string()).map_err(|e| format!("proof: {e}"))?;
    let session_id = field(&session, "callerSessionId")?;
    let sequence = match &session["initialSequence"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err("answer lacks initialSequence".into()),
    };
    let body = json!({"action": "list"}).to_string();
    let request_id = random_id()?;
    let signed = canonical(&json!({
        "appId": APP_ID, "bodyDigest": b64url(digest(&SHA256, body.as_bytes()).as_ref()), "callerSessionId": session_id,
        "domain": format!("{AUTH_DOMAIN}/caller-request"), "generation": generation, "grantId": caller.grant_id,
        "helloTranscriptHash": b64url(digest(&SHA256, transcript.as_bytes()).as_ref()), "keyId": caller.key_id,
        "method": "POST", "requestId": request_id, "sequence": sequence, "target": API_PATH,
    }));
    let headers = [("x-session-broker-caller-session", session_id.as_str()), ("x-session-broker-request-id", request_id.as_str()), ("x-session-broker-sequence", sequence.as_str()), ("x-session-broker-signature", &caller.sign(&signed))];
    let answer = post(&agent, &format!("{origin}{API_PATH}"), &headers, &body).map_err(|e| format!("list: {e}"))?;
    let sessions = answer["body"]["sessions"].as_array().ok_or("list answer has no sessions")?;
    Ok(sessions.iter().filter_map(|s| s["sessionId"].as_str()).filter_map(uuid_prefix).map(str::to_string).collect())
}

/// The leading UUID of `id`, which is all `hunk session list` output is scanned for.
pub fn uuid_prefix(id: &str) -> Option<&str> {
    let head = id.get(..36)?;
    let shaped = head.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    shaped.then_some(head)
}

/// JSON with object keys sorted and no whitespace: the bytes both ends sign.
pub fn canonical(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            format!("{{{}}}", keys.into_iter().map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&m[k]))).collect::<Vec<_>>().join(","))
        }
        other => other.to_string(),
    }
}

pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        assert_eq!(canonical(&json!({"b": [1, {"y": 1, "x": "a"}], "a": null})), r#"{"a":null,"b":[1,{"x":"a","y":1}]}"#);
    }

    #[test]
    fn only_a_leading_uuid_counts_as_a_session_id() {
        let id = "0f2a9c1e-8b3d-4a6e-9c1f-123456789abc";
        assert_eq!(uuid_prefix(id), Some(id));
        assert_eq!(uuid_prefix(&format!("{id}-extra")), Some(id));
        assert_eq!(uuid_prefix("not-a-uuid"), None);
        assert_eq!(uuid_prefix("0f2a9c1e_8b3d-4a6e-9c1f-123456789abc"), None);
    }

    #[test]
    fn a_descriptor_that_is_not_loopback_is_not_dialled() {
        let d = testkit::TempDir::new("hunk-origins");
        fs::write(d.path().join("daemon-a.json"), r#"{"host":"10.0.0.9","port":1}"#).unwrap();
        assert!(origins(d.path()).is_err());
        fs::write(d.path().join("daemon-b.json"), r#"{"host":"127.0.0.1","port":47657}"#).unwrap();
        assert_eq!(origins(d.path()).unwrap(), ["http://127.0.0.1:47657"]);
    }
}
