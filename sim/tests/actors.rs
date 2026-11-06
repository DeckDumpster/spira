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

/// The tree's actors are sim-only, and each one's timer and the service it starts are under
/// `sim/units`; a shipped actor (a unit the loop ran, now a task of the service) is refused.
#[test]
fn every_actor_is_sim_only_and_its_units_are_in_the_tree() {
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let specs = sim::actors::parse_specs(&std::fs::read_to_string(tree.join("sim/actors.toml")).unwrap()).unwrap();
    for s in &specs {
        let timer = std::fs::read_to_string(s.units_in(&tree).join(&s.timer)).unwrap_or_else(|e| panic!("{}: {e}", s.timer));
        let service = sim::units::timer_target(&s.timer, &timer);
        assert!(s.units_in(&tree).join(&service).is_file(), "{}: {service}", s.name);
        parse_timer(&timer).unwrap();
    }
    let shipped = "[[actor]]\nname = \"sift\"\ncommand = \"round-controller sifts\"\ntimer = \"spira-sift.timer\"\nduration = \"d\"\n";
    assert!(sim::actors::parse_specs(shipped).unwrap_err().contains("ruling D6"));
}
