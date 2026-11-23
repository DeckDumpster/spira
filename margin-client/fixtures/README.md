# Margin recordings

`scenario.json` is one session with the real Margin reader at commit `b95de3b`: each request
(method, path, query, JSON body), its status and its reply, in order. The bearer is never written.
`src/scenario.rs` is the session; the tests in `src/http/tests.rs` replay it from an in-process
listener and require every request to equal the recording.

Re-record when the reader's routes change:

```sh
git clone <margin repo> scratch && git -C scratch checkout b95de3b && cargo build -p margin --manifest-path scratch/Cargo.toml
cat > margin.toml <<TOML
listen = "127.0.0.1:18931"
base_url = "http://127.0.0.1:18931"
database = "$PWD/margin.db"
agents_file = "$PWD/agents"
otlp_endpoint = "http://127.0.0.1:1"
TOML
margin agent-token add concierge --agents-file agents --token-file token
margin serve --config margin.toml &
cargo build -p margin-client --features record --example record_margin
target/debug/examples/record_margin --base http://127.0.0.1:18931 --token-file token --out margin-client/fixtures/scenario.json
```

Ids and times in the file are the reader's own, so a new recording changes the ids the tests read
from it; the tests assert shapes and relations, not literal ids, except the first reply.
