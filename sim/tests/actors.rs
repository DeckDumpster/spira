use sim::fit::fit_files;
use sim::units::parse_timer;
use testkit::TempDir;
#[test]
fn unsupported_or_dead_timers_are_refused() {
    assert!(parse_timer("[Timer]\nOnUnitActiveSec=5min\n").is_err());
    assert!(parse_timer("[Timer]\nAccuracySec=1s\n").is_err());
    assert!(parse_timer("[Timer]\nOnCalendar=monthly\n").is_err());
    assert!(parse_timer("[Timer]\nOnCalendar=daily\n").is_ok(), "the catalogue runs daily and weekly timers");
}

#[test]
fn fit_refuses_missing_and_empty_logs() {
    let tmp = TempDir::new("fit");
    let dir = tmp.path().to_path_buf();
    let gate = dir.join("gate.log");
    let land = dir.join("land.tsv");
    assert!(fit_files(&gate, &land).is_err());
    std::fs::write(&gate, "").unwrap();
    std::fs::write(&land, "").unwrap();
    assert!(fit_files(&gate, &land).unwrap_err().contains("empty"));
    std::fs::write(&gate, "garbage\n").unwrap();
    std::fs::write(&land, "garbage\n").unwrap();
    assert!(fit_files(&gate, &land).is_err());
}
