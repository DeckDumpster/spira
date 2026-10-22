// tsd-fake-store — a stand-in for the metrics store, for suites that drive the real tsd-write and
// tsd-read binaries: takes OTLP pushes and answers /api/v1/export from them, until killed.
//
// usage: tsd-fake-store [--bind <host:port>]   prints the address it listens on, then serves

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bind = args.iter().position(|a| a == "--bind").and_then(|i| args.get(i + 1)).map_or("127.0.0.1:0", String::as_str);
    let store = spira_otel::fixture::FakeStore::start_on(bind);
    println!("{}", store.addr());
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
