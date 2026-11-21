use super::*;

#[test_id(tgzsqbt)]
#[test]
fn a_response_is_a_header_line_then_the_raw_bytes_once() {
    use std::io::Read;
    let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
    let body = "{\"q\": \"x\"}\nsecond line";
    super::write_response(&a, 3, body, "").unwrap();
    drop(a);
    let mut got = String::new();
    b.read_to_string(&mut got).unwrap();
    assert_eq!(got, format!("{{\"exit_code\":3,\"len\":{}}}\n{body}", body.len()));
}

#[test_id(tx2sb1z)]
#[test]
fn peer_pid_reads_the_connecting_process() {
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    assert_eq!(super::peer_pid(&a), Some(std::process::id() as i32));
}

#[test_id(tkahntd)]
#[test]
fn peer_uid_reads_the_connecting_process() {
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    let me = std::fs::metadata(system_paths::PROC_SELF).unwrap();
    assert_eq!(super::peer_uid(&a), Some(std::os::unix::fs::MetadataExt::uid(&me)));
}

/// sp-6n7nuv: a verb the socket does not serve is refused by the server itself — the
/// git-proved caller verbs included — and never falls through to anything.
#[test_id(toh434f)]
#[test]
fn the_real_server_answers_a_composed_verb_and_refuses_an_unserved_one() {
    let (reply, code) = ask(None, "[\"hold\"]\n");
    assert_eq!(code, 2, "{reply}");
    assert!(reply.contains("<poison|ask|wait|manual>"), "the composed verb answered its own usage: {reply}");
    for verb in ["deliver", "content-on-base", "no-such-verb"] {
        let (reply, code) = ask(None, &format!("[\"{verb}\", \"sp-a\"]\n"));
        assert_eq!(code, 2, "{verb}: {reply}");
        assert!(reply.contains("usage: spira-lc show"), "{verb} is refused, not run: {reply}");
    }
}

#[test_id(to19lgr)]
#[test]
fn a_request_body_is_read_to_exactly_its_length() {
    let mut r = std::io::Cursor::new(b"{\"argv\":[\"close\",\"sp-a\"],\"len\":5}\nhelloEXTRA".to_vec());
    let (req, _) = super::read_request(&mut r).unwrap().unwrap();
    assert_eq!((req.argv, req.stdin.as_deref()), (vec!["close".to_string(), "sp-a".into()], Some("hello")));
    let mut short = std::io::Cursor::new(b"{\"argv\":[\"close\"],\"len\":9}\nhi".to_vec());
    assert!(super::read_request(&mut short).unwrap_err().1.starts_with("bad request"));
    let mut huge = std::io::Cursor::new(format!("{{\"argv\":[\"x\"],\"len\":{}}}\n", super::MAX_BODY + 1).into_bytes());
    assert!(super::read_request(&mut huge).is_err(), "an oversized body is refused before it is read");
}

#[test_id(trs9zhy)]
#[test]
fn a_stderr_answer_rides_after_stdout_with_its_own_length() {
    use std::io::Read;
    let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
    super::write_response(&a, 3, "out", "why\n").unwrap();
    drop(a);
    let mut got = String::new();
    b.read_to_string(&mut got).unwrap();
    assert_eq!(got, "{\"exit_code\":3,\"len\":3,\"err_len\":4}\noutwhy\n");
}
