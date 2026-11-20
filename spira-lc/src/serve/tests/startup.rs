#[test]
fn notify_ready_reaches_an_abstract_namespace_socket() {
    let _env = testkit::env_read();
    use std::os::linux::net::SocketAddrExt;
    let name = format!("spira-lc-notify-{}", std::process::id());
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let rx = std::os::unix::net::UnixDatagram::bind_addr(&addr).unwrap();
    super::notify_ready(Some(&format!("@{name}"))).unwrap();
    let mut buf = [0u8; 64];
    let n = rx.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"READY=1");
}

#[test]
fn notify_ready_sends_one_ready_datagram_to_the_notify_socket_and_nothing_when_unset() {
    let _env = testkit::env_read();
    let dir = testkit::TempDir::new("notify");
    let path = dir.join("notify.sock");
    let rx = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
    rx.set_nonblocking(true).unwrap();
    super::notify_ready(None).unwrap();
    super::notify_ready(Some("")).unwrap();
    let mut buf = [0u8; 64];
    assert!(rx.recv(&mut buf).is_err(), "nothing sent while unset");
    super::notify_ready(Some(path.to_str().unwrap())).unwrap();
    let n = rx.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"READY=1");
    assert!(super::notify_ready(Some(dir.join("absent").to_str().unwrap())).is_err());
}

#[test]
fn start_up_logs_one_line_per_phase_in_order_and_ready_after_their_sum() {
    let _env = testkit::env_read();
    use std::time::Duration;
    let phases = [("config", Duration::from_millis(3)), ("open-sqlite", Duration::from_millis(52)), ("bind", Duration::from_millis(5))];
    let lines = super::startup_lines(&phases, 0);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[0].ends_with("start-up config 3 ms"), "{}", lines[0]);
    assert!(lines[1].ends_with("start-up open-sqlite 52 ms"), "{}", lines[1]);
    assert!(lines[2].ends_with("start-up bind 5 ms"), "{}", lines[2]);
    assert!(lines[3].ends_with("ready after 60 ms"), "{}", lines[3]);
    assert!(lines[3].starts_with("spira-lc serve: 1970-01-01T00:00:00"), "{}", lines[3]);
}

#[test]
fn serve_refuses_to_start_naming_the_ledger_key_when_it_is_absent() {
    let dir = testkit::TempDir::new("lc-ledger-key");
    let toml = spira_config::process::fixture_toml(&dir, &[]);
    let text = std::fs::read_to_string(&toml).unwrap();
    assert!(text.contains("lc_request_ledger_secs"), "the fixture declares the key");
    let kept: String = text.lines().filter(|l| !l.contains("lc_request_ledger_secs")).map(|l| format!("{l}\n")).collect();
    std::fs::write(&toml, kept).unwrap();
    let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
    let fresh = |k: &str| spira_config::process::fresh(&[k.to_string()]).map(|mut m| m.remove(k).unwrap());
    let err = super::ledger_ttl_from(fresh).unwrap_err();
    assert!(err.contains("lc_request_ledger_secs"), "{err}");
}
