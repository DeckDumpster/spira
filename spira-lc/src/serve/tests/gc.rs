#[test]
fn a_gc_pass_is_logged_only_for_what_it_collected_or_failed() {
    let idle = state_store::gc::Tick::default();
    assert!(super::gc_lines(&idle, 0).is_empty(), "a pass that did nothing says nothing");
    let busy = state_store::gc::Tick { orphans: vec!["a".into(), "b".into()], ..Default::default() };
    let lines = super::gc_lines(&busy, 0);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].ends_with("control-store gc: 2 orphans, 0 expired"), "{}", lines[0]);
    assert!(!lines[0].contains("scan"), "{}", lines[0]);
    let failed = state_store::gc::Tick {
        failed: vec![state_store::gc::StepFailure { step: "orphan", error: state_store::Error::Backend("x".into()) }],
        ..Default::default()
    };
    let lines = super::gc_lines(&failed, 0);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("control-store gc orphan: "), "{}", lines[0]);
}
