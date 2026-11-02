use spira_sim::roundvm::{parse_verdict, write_verdict, Verdict, VERDICT_FILE};

#[test]
fn verdicts_round_trip() {
    for v in [Verdict::Green, Verdict::Red(vec!["test-a.sh".into(), "test-b.sh".into()]), Verdict::RedEarly(vec!["spira-lint".into()]), Verdict::Fault("vm lost".into())] {
        let t = testkit::TempDir::new("simrvm");
        write_verdict(&t, &v).unwrap();
        assert_eq!(parse_verdict(&std::fs::read_to_string(t.join(VERDICT_FILE)).unwrap()).unwrap(), v);
    }
}
