use spira_sim::launch::{answer, parse, state, Call, Said, NOT_ACTIVE};

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_systemd_run_argv_is_its_unit_its_setenv_pairs_and_the_command_after_the_options() {
    let c = parse(&argv(&["--user", "--unit=round-controller-run-b1-3-0", "--collect", "--quiet", "--setenv=SPIRA_TOML=/a:/b", "--setenv=SPIRA_RELEASE=/r", "/r/bin/round-controller", "run", "--repo", "sim"])).unwrap();
    assert_eq!(
        c,
        Call::Run {
            unit: "round-controller-run-b1-3-0".into(),
            env: vec![("SPIRA_TOML".into(), "/a:/b".into()), ("SPIRA_RELEASE".into(), "/r".into())],
            command: argv(&["/r/bin/round-controller", "run", "--repo", "sim"]),
        }
    );
    assert_eq!(parse(&argv(&["--user", "is-active", "u-1"])).unwrap(), Call::IsActive("u-1".into()));
    assert!(parse(&argv(&["--user", "--unit=u"])).unwrap_err().contains("no command"));
    assert!(parse(&argv(&["--user", "prog"])).unwrap_err().contains("no --unit="));
    assert!(parse(&argv(&["--user", "is-active"])).unwrap_err().contains("one unit"));
    assert!(matches!(parse(&argv(&["--user", "--unit=u", "prog", "is-active"])).unwrap(), Call::Run { .. }), "is-active inside the command is the command's");
}

#[test]
fn a_launch_records_its_unit_runs_it_inline_with_its_env_and_is_active_answers_from_the_record() {
    let w = testkit::TempDir::new("sim-launch");
    let units = w.join("run/sim-units");
    let out = w.join("ran");
    let is_active = |u: &str| answer(&units, &Call::IsActive(u.into())).unwrap();
    assert_eq!(is_active("u-1"), Said { code: NOT_ACTIVE, out: "inactive\n".into(), err: String::new() });
    let run = Call::Run {
        unit: "u-1".into(),
        env: vec![("RC_SEEN".into(), "from-setenv".into())],
        command: argv(&["sh", "-c", &format!("printf '%s %s' \"$RC_SEEN\" \"$(cat {}/u-1)\" > {}; exit 7", units.display(), out.display())]),
    };
    assert_eq!(answer(&units, &run).unwrap(), Said { code: 0, out: String::new(), err: String::new() });
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "from-setenv active", "the unit reads active while it runs");
    assert_eq!(std::fs::read_to_string(units.join("u-1")).unwrap(), "inactive\nexit=7\n");
    assert_eq!(state(&units, "u-1"), "inactive");
    std::fs::write(units.join("u-2"), "active\n").unwrap();
    assert_eq!(is_active("u-2").code, 0);
    let again = Call::Run { unit: "u-2".into(), env: vec![], command: argv(&["true"]) };
    let said = answer(&units, &again).unwrap();
    assert_eq!(said.code, 1);
    assert!(said.err.contains("Unit u-2.service already exists"), "{}", said.err);
}
