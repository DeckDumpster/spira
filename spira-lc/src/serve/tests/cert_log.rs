use super::*;

fn certify_request(id: &str) -> super::super::Request {
    let argv = ["certify", id, "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1", "pass", "keyA"];
    super::super::Request { argv: argv.map(String::from).to_vec(), stdin: None, caller: Default::default(), task: None, request_id: None }
}

/// With `lifecycle-cert.log` a FIFO nobody reads, an append to it never returns; the request
/// is answered all the same, and the line is the writer's to wait on.
#[test_id(tcfqlh1)]
#[test]
fn a_certify_is_answered_while_its_log_is_a_fifo_nobody_reads() {
    let dir = testkit::TempDir::new("lc-cert-fifo");
    let fifo = dir.path().join("lifecycle-cert.log");
    let path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: `path` is a valid NUL-terminated string for the call.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let run = dir.path().to_path_buf();
    let (answered, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let writer = Arc::new(crate::disk::LogWriter::new(Arc::new(ports::RealFs), runtime::offload::Pool::new(1, 4)));
        let w = World::new();
        filed(&w, "sp-a");
        let (mut m, mut b) = (&w, &w);
        let ans = crate::disk::with_writer(writer, || super::super::answer(certify_request("sp-a"), &mut m, &mut b, &Content::default(), &|| Ok(run.clone())));
        let _ = answered.send(ans.cert_log.is_some());
    });
    let logged = rx.recv_timeout(Duration::from_secs(30)).expect("the certify was answered");
    assert!(logged, "the certify reached a verdict that logs a line");
}
