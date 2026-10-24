use pilgrim_login::{clear_needs_auth, login_url, mask, run, Tools};
use std::path::Path;
use testkit::{write_exe, TempDir};

const CODE: &str = "Zq7secretcode9fAAAAAAAA";
const REDIRECT: &str = "http://localhost:4567/callback?code=abcdefghijklmnopqrstuvwxyz&state=s";

struct Fixture {
    dir: TempDir,
    tools: Tools,
    _env: testkit::EnvGuard,
}

fn fixture(broker_body: &str, expect: &str) -> Fixture {
    let env = testkit::env(&[("SPIRA_WORK_BEAD_ID", Some("sp-fixture"))]);
    let dir = TempDir::new("pilgrim-login");
    let p = |n: &str| dir.path().join(n);
    write_exe(p("broker"), broker_body);
    write_exe(
        p("claude"),
        &format!("#!/bin/sh\n[ \"$1 $2 $3 $4\" = 'mcp login --no-browser fixture-mcp' ] || exit 9\n\
         [ -t 0 ] || exit 8\n\
         echo 'Open: https://mcp.fixture.test/authorize?client=abcdefghijklmnopqrstuvwxyz'\n\
         IFS= read -r line\n[ \"$line\" = \"{expect}\" ] || exit 7\necho signed-in\n"),
    );
    write_exe(
        p("podman"),
        &format!(
            "#!/bin/sh\nread -r login\nread -r auth\n\
             [ \"$login\" = 'https://id.fixture.test/lc/{CODE}' ] || exit 5\n\
             case \"$auth\" in https://mcp.fixture.test/authorize*) ;; *) exit 6;; esac\n\
             echo '{REDIRECT}'\n"
        ),
    );
    let tools = Tools {
        broker: p("broker").display().to_string(),
        claude: p("claude").display().to_string(),
        podman: p("podman").display().to_string(),
    };
    Fixture { dir, tools, _env: env }
}

fn good_broker() -> String {
    format!("#!/bin/sh\n[ \"$1\" = identity-login-code ] || exit 3\n[ -n \"$SPIRA_WORK_BEAD_ID\" ] || exit 4\necho {CODE}\n")
}

#[test]
fn a_pilgrim_signs_claude_in_with_the_brokers_code_and_the_browsers_redirect() {
    let f = fixture(&good_broker(), REDIRECT);
    run(&f.tools, "fixture-mcp", f.dir.path(), "https://id.fixture.test/lc/").unwrap();
}

#[test]
fn a_redirect_that_claude_rejects_fails_the_login() {
    let f = fixture(&good_broker(), "something else");
    let e = run(&f.tools, "fixture-mcp", f.dir.path(), "https://id.fixture.test/lc/").unwrap_err();
    assert!(e.contains("did not succeed"), "{e}");
}

#[test]
fn a_refusing_broker_stops_before_claude_runs() {
    let f = fixture("#!/bin/sh\nexit 1\n", REDIRECT);
    let marker = f.dir.path().join("claude-ran");
    write_exe(f.dir.path().join("claude"), &format!("#!/bin/sh\ntouch {}\n", marker.display()));
    let e = run(&f.tools, "fixture-mcp", f.dir.path(), "https://id.fixture.test/lc/").unwrap_err();
    assert!(e.contains("refused"), "{e}");
    assert!(!marker.exists());
}

#[test]
fn a_broker_reply_that_is_not_one_token_is_refused() {
    let f = fixture("#!/bin/sh\necho 'two words'\n", REDIRECT);
    assert!(run(&f.tools, "fixture-mcp", f.dir.path(), "https://id.fixture.test/lc/").is_err());
}

#[test]
fn the_login_url_is_the_base_plus_the_code_and_the_base_must_be_http() {
    assert_eq!(login_url("https://id.x/lc/", "abc").unwrap(), "https://id.x/lc/abc");
    assert!(login_url("ssh://id.x/lc/", "abc").is_err());
}

#[test]
fn output_is_masked_of_escape_sequences_and_long_tokens() {
    let m = mask("\x1b[31mred\x1b[0m token=abcdefghijklmnopqrstuvwxyz0123");
    assert_eq!(m, "red token=<x>");
}

#[test]
fn only_the_named_servers_cached_needs_auth_is_cleared() {
    let h = TempDir::new("pilgrim-home");
    std::fs::create_dir_all(h.path().join(".claude")).unwrap();
    let cache = h.path().join(".claude/mcp-needs-auth-cache.json");
    std::fs::write(&cache, r#"{"a":1,"b":2}"#).unwrap();
    assert!(clear_needs_auth(h.path(), "a"));
    let left: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
    assert_eq!(left, serde_json::json!({"b": 2}));
    assert!(!clear_needs_auth(h.path(), "a"));
    assert!(!clear_needs_auth(Path::new("/nonexistent"), "a"));
}
