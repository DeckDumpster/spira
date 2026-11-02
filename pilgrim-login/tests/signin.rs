use pilgrim_login::login_url;
#[test]
fn the_login_url_is_the_base_plus_the_code_and_the_base_must_be_http() {
    assert_eq!(login_url("https://id.x/lc/", "abc").unwrap(), "https://id.x/lc/abc");
    assert!(login_url("ssh://id.x/lc/", "abc").is_err());
}
