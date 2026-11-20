#[path = "../src/id.rs"]
#[allow(dead_code)]
mod id;

use id::check_id;

#[test]
fn an_id_of_the_grammar_is_accepted() {
    assert!(check_id("t7k3m9q").is_ok());
}

#[test]
fn a_short_id_is_refused_naming_the_grammar_and_stamp() {
    let e = check_id("t7k3m9").unwrap_err();
    assert!(e.contains("test-registry stamp") && e.contains("[0-9a-z]"), "{e}");
}

#[test]
fn an_id_without_the_t_prefix_is_refused() {
    assert!(check_id("x7k3m9q").is_err());
}

#[test]
fn an_id_with_an_uppercase_character_is_refused() {
    assert!(check_id("t7K3m9q").is_err());
}
