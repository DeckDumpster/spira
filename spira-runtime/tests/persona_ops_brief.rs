//! The persona.ops brief edits the process environment (`SPIRA_HOME`, `SPIRA_TOML`, …) for the
//! whole run of `run_brief`: its own test binary, so no test in `tests/it` reads a swapped home.

#[macro_use]
extern crate spira_test_id;
use spira_runtime::tasks::f::persona_ops::*;

fn brief_spec(cache: &std::path::Path) -> runtime::EffectSpec {
    let input = serde_json::json!({
        "cfg": {"SPIRA_AGENT": "claude", "SPIRA_MEMORIES_CACHE": cache.display().to_string(), "SPIRA_MEMORIES_CACHE_AGE": "86400"},
        "act": {"run_id": "r1"},
    });
    runtime::EffectSpec::keyed(BRIEF, "k", input.to_string(), 30_000)
}

fn brief_with_spec(toml: Option<&std::path::Path>) -> Result<String, String> {
    let ws = testkit::TempDir::new("persona-ops-brief");
    let home = ws.join("home");
    std::fs::create_dir_all(home.join("runes")).unwrap();
    std::fs::write(home.join("runes").join("ops.rune"), "RUNE_NAME=ops\nRUNE_MEMORY_PREFIXES=\"law-,sop-\"\nRUNE_TOOLS=Read\nRUNE_SYSTEM_PROMPT=replace\n").unwrap();
    for registry in ["conf.d", "conf.toml.d"] {
        std::os::unix::fs::symlink(
            testkit::manifest_dir().join("../spira").join(registry),
            home.join(registry),
        )
        .unwrap();
    }
    let cache = ws.join("memories.json");
    std::fs::write(&cache, r#"{"law-a":"be careful"}"#).unwrap();
    let run = ws.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let toml = toml.map(|p| p.display().to_string());
    let release = ws.join("release");
    std::fs::create_dir_all(release.join("model-bin")).unwrap();
    testkit::write_exe(release.join("model-bin/work"), "");
    let _env = testkit::env(&[
        ("SPIRA_RELEASE", Some(&release.display().to_string())),
        ("SPIRA_HOME", Some(&home.display().to_string())),
        (
            "SPIRA_RUNES_DIR",
            Some(&home.join("runes").display().to_string()),
        ),
        ("SPIRA_TOML", toml.as_deref()),
    ]);
    let e = spira_runtime::Effects {
        world: spira_runtime::world::World {
            run_dir: run,
            ..Default::default()
        },
        ..Default::default()
    };
    run_brief(&e, "k", &brief_spec(&cache))
}

#[test_id(tcm3kxw)]
#[test]
fn the_brief_takes_the_persona_model_from_the_spec_the_runtime_was_started_with() {
    let dir = testkit::TempDir::new("persona-ops-spec");
    let toml = spira_config::process::fixture_toml(&dir, &[]);
    spira_config::set_paths_in_file(&toml, &[("persona.ops.model", "m-test")])
        .expect("the scratch spec declares the model");
    let out = brief_with_spec(Some(&toml)).expect("the brief succeeds");
    let b: dwarf::ops_sweep::SweepBrief = serde_json::from_str(&out).expect("a SweepBrief");
    let at = b
        .argv
        .iter()
        .position(|a| a == "--model")
        .expect("--model is in the argv");
    assert_eq!(b.argv[at + 1], "m-test", "{:?}", b.argv);
}

#[test_id(t8y1moj)]
#[test]
fn the_brief_with_no_spec_is_refused_by_spira_configs_own_message() {
    let e = brief_with_spec(None).expect_err("no spec, no brief");
    assert!(e.contains("SPIRA_TOML is not set"), "{e}");
}
