//! Throwaway MCP channel server for the live wake proof: relays `spira-sub` deliveries as
//! `notifications/claude/channel`.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Value};
use spira_sub::client;

/// A server on a later revision than the client knows channels for is silently not registered.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

type Out<W> = Arc<Mutex<W>>;

/// The reply one line of the client's is owed, and whether it was `initialize`.
fn answer(line: &str) -> Option<(Value, bool)> {
    let error = |id: Value, code: i64, why: &str| Some((json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": why}}), false));
    let Ok(message) = serde_json::from_str::<Value>(line) else { return error(Value::Null, PARSE_ERROR, "parse error") };
    if !message.is_object() {
        return error(Value::Null, INVALID_REQUEST, "not a request object");
    }
    // A notification has no id and a response no method; neither is owed anything.
    let (id, method) = (message.get("id")?, message.get("method")?);
    if method != "initialize" {
        return error(id.clone(), METHOD_NOT_FOUND, "method not found");
    }
    let result = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"experimental": {"claude/channel": {}}},
        "serverInfo": {"name": "spira-sub", "version": env!("CARGO_PKG_VERSION")},
    });
    Some((json!({"jsonrpc": "2.0", "id": id, "result": result}), true))
}

/// One message as one whole line, so the two threads' messages never interleave.
fn send(out: &Out<impl Write>, message: &Value) -> std::io::Result<()> {
    let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
    out.write_all(format!("{message}\n").as_bytes())?;
    out.flush()
}

/// What `follow` writes into: it hands over each delivery, batch or not, in one write.
struct Channel<W> {
    out: Out<W>,
    seq: u64,
}

impl<W: Write> Write for Channel<W> {
    fn write(&mut self, delivery: &[u8]) -> std::io::Result<usize> {
        self.seq += 1;
        let content = String::from_utf8_lossy(delivery);
        let params = json!({"content": content.trim_end_matches('\n'), "meta": {"seq": self.seq.to_string()}});
        send(&self.out, &json!({"jsonrpc": "2.0", "method": "notifications/claude/channel", "params": params}))?;
        Ok(delivery.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Follows on its own thread, so neither it nor the client's stdin waits for the other.
fn follow_aside(socket: PathBuf, out: Out<impl Write + Send + 'static>) -> Result<JoinHandle<Result<(), String>>, String> {
    std::thread::Builder::new()
        .name("channel-spike-follow".into())
        .spawn(move || {
            let mut note = |what: &str| eprintln!("channel-spike: {what}");
            client::follow(&socket, &mut Channel { out, seq: 0 }, &mut note).inspect_err(|why| eprintln!("channel-spike: {why}"))
        })
        .map_err(|e| format!("cannot start the follow thread: {e}"))
}

/// Serves the MCP client until it closes its end. Deliveries of the `spira-sub run` on `socket`
/// start once the client has sent `initialize`; the follow thread outlives the call.
pub fn relay(socket: &Path, from_client: impl BufRead, to_client: impl Write + Send + 'static) -> Result<(), String> {
    let out = Arc::new(Mutex::new(to_client));
    let mut following = None;
    for line in from_client.lines() {
        let line = line.map_err(|e| format!("cannot read from the client: {e}"))?;
        let Some((reply, initialized)) = answer(&line) else { continue };
        send(&out, &reply).map_err(|e| format!("cannot write to the client: {e}"))?;
        if initialized && following.is_none() {
            following = Some(follow_aside(socket.to_path_buf(), out.clone())?);
        }
    }
    match following.filter(JoinHandle::is_finished) {
        Some(ended) => ended.join().map_err(|_| "the follow thread panicked".to_string())?,
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Stdout(Arc<Mutex<Vec<u8>>>);

    impl Write for Stdout {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test_id(tj2eiyk)]
    #[test]
    fn a_request_other_than_initialize_gets_an_error_and_stdout_carries_only_json_rpc() {
        let client = [
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":8,"result":{}}"#,
            "not json",
        ]
        .join("\n");
        let stdout = Stdout::default();

        relay(Path::new("never-followed.sock"), client.as_bytes(), stdout.clone()).unwrap();

        let written = String::from_utf8(stdout.0.lock().unwrap().clone()).unwrap();
        let replies: Vec<Value> = written.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert!(replies.iter().all(|r| r["jsonrpc"] == "2.0"), "{written}");
        let errors: Vec<(Value, Value)> = replies.iter().map(|r| (r["id"].clone(), r["error"]["code"].clone())).collect();
        assert_eq!(errors, [(json!(7), json!(-32601)), (json!("p"), json!(-32601)), (Value::Null, json!(-32700))], "{written}");
    }
}
