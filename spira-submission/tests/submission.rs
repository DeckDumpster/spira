use spira_submission::Config;
#[test]
fn an_unconfigured_or_wildcard_address_never_listens() {
    let vals = |addr: &'static str| move |k: &str| -> Result<String, String> {
        Ok(match k {
            "SPIRA_SUBMISSION_ADDR" => addr,
            "SPIRA_SUBMISSION_PORT" => "2587",
            _ => "x",
        }
        .to_string())
    };
    assert!(Config::from_values(&vals(""), vec![]).unwrap().is_none());
    assert!(Config::from_values(&vals("0.0.0.0"), vec![]).unwrap_err().contains("wildcard"));
    assert!(Config::from_values(&vals("100.64.0.1"), vec![]).unwrap().is_some());
    let no_cert = |k: &str| -> Result<String, String> {
        Ok(match k {
            "SPIRA_SUBMISSION_ADDR" => "100.64.0.1",
            "SPIRA_SUBMISSION_PORT" => "2587",
            "SPIRA_SUBMISSION_CERT" => "",
            _ => "x",
        }
        .to_string())
    };
    assert!(Config::from_values(&no_cert, vec![]).unwrap().is_none());
}
