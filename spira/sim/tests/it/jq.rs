use spira_sim::jq::jq_r;

#[test]
fn an_unsupported_construct_is_an_error_not_an_empty_answer() {
    for expr in [".a | frobnicate", ".a +", "$x", ".[] as $y | $y"] {
        assert!(jq_r("{}", expr).is_err(), "{expr}");
    }
    assert!(jq_r("not json", ".a").is_err());
}
