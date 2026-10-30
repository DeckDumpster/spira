//! The config delta is a fragment of `spira.toml`: a value its key's type refuses is refused
//! where the delta is checked, naming the key.

use std::path::Path;

fn home() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"))
}

fn added(text: &str) -> toml::map::Map<String, toml::Value> {
    spira_config::delta_added(text).unwrap()
}

#[test]
fn the_shipped_delta_loads_under_the_typed_schema() {
    let text = std::fs::read_to_string(home().join("config-delta.toml")).unwrap();
    spira_config::check_delta(home(), &added(&text)).unwrap();
}

#[test]
fn a_u32_key_written_as_a_string_is_refused_naming_the_key_and_type() {
    spira_config::check_delta(home(), &added("[spira]\nlocal_backlog_ref_max_age = 3600\n")).expect("control: the integer form loads");
    let e = spira_config::check_delta(home(), &added("[spira]\nlocal_backlog_ref_max_age = \"3600\"\n")).unwrap_err();
    assert!(e.contains("spira.local_backlog_ref_max_age") && e.contains("u32"), "{e}");
}

#[test]
fn the_old_added_table_is_refused() {
    let e = spira_config::delta_added("[added]\n\"spira.x\" = 1\n").unwrap_err();
    assert!(e.contains("[added]"), "{e}");
}
