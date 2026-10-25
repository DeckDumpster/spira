use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

const HARD_CAP: Duration = Duration::from_secs(240);

fn run() -> Option<String> {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let endpoint = spira_config::process::metrics_endpoint();
    let sink = spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), "spira-habits", Duration::from_millis(300));
    let state = std::env::temp_dir().join("spira-habits");
    std::panic::catch_unwind(|| habits::dispatch(&raw, &state, &sink)).ok().flatten()
}

fn main() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run());
    });
    if let Ok(Some(out)) = rx.recv_timeout(HARD_CAP) {
        println!("{out}");
    }
}
