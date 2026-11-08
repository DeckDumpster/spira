use spira_sim::agent::{parse, Step};

#[test]
fn a_slain_step_parses_with_no_argument_and_is_refused_with_one() {
    assert_eq!(parse("sp-a slain\nsp-a call\nsp-a submit\n", "sp-a").unwrap(), vec![vec![Step::Slain], vec![Step::Submit]]);
    assert!(parse("sp-a slain now\n", "sp-a").is_err());
}
