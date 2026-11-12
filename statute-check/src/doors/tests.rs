use super::*;

const REGISTRY: &str = include_str!("../../../spira/doors.toml");

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn every_usage_in_the_registry_is_verbatim_in_the_tool_that_prints_it() {
    let reg = parse(REGISTRY).unwrap();
    let mut checked = 0;
    for r in &reg.resource {
        let Some(file) = &r.usage_in else { continue };
        let src = std::fs::read_to_string(root().join(file)).unwrap_or_else(|e| panic!("{}: {file}: {e}", r.name));
        assert!(src.contains(&r.usage), "{}: usage is not in {file}: {}", r.name, r.usage);
        checked += 1;
    }
    assert!(checked >= 6, "the check must have had something to check, saw {checked}");
}

#[test]
fn glob_stops_a_star_at_a_path_part_and_lets_a_leading_star_slash_cross() {
    assert!(glob("*/rounds-pace/*/pause", "/r/x/run/rounds-pace/spira/pause"));
    assert!(glob("*/rounds-pace/*/pause", "run/rounds-pace/spira/pause"));
    assert!(glob("*/worktree/sp-*", "/r/worktree/sp-1"));
    assert!(!glob("*/worktree/sp-*", "/r/worktree/sp-1/src/main.rs"));
    assert!(glob("refs/heads/spira/*", "refs/heads/spira/sp-1"));
    assert!(!glob("refs/heads/spira/*", "refs/heads/spiral"));
}

#[test]
fn every_resource_names_a_way_to_be_touched_and_a_sentence_of_scar() {
    let reg = parse(REGISTRY).unwrap();
    for r in &reg.resource {
        let t = &r.touched_by;
        assert!(![&t.verbs, &t.paths, &t.refs, &t.units, &t.binaries, &t.tracers].iter().all(|v| v.is_empty()), "{} is touched by nothing", r.name);
        assert!(!r.why.trim().is_empty() && !r.usage.trim().is_empty(), "{}", r.name);
    }
}
