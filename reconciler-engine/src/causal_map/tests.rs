use super::*;

const KINDS: [&str; 3] = ["RoundController", "Alarms", "Lifecycle"];

fn line(kind: &str, weight: &str) -> String {
    format!("[[weight]]\nkind = \"{kind}\"\nweight = {weight}\nunit = \"u\"\nsource = \"s\"\n")
}

#[test]
fn the_shipped_seed_weighs_every_published_kind_and_lifecycle_weighs_nothing() {
    let m = CausalMap::seed().unwrap();
    assert_eq!(m.weight("RoundController"), Some(10.0));
    assert_eq!(m.weight("Alarms"), Some(1.0));
    assert_eq!(m.weight("Lifecycle"), Some(0.0));
    assert_eq!(m.weight("Probe"), None);
}

#[test]
fn a_kind_with_no_weight_line_is_refused_at_load_naming_it() {
    let text = [line("RoundController", "10"), line("Lifecycle", "0")].concat();
    let e = CausalMap::load(&text, &KINDS).unwrap_err();
    assert!(e.contains("Alarms"), "{e}");
    assert!(CausalMap::load(&[text.as_str(), &line("Alarms", "1")].concat(), &KINDS).is_ok());
}

#[test]
fn a_line_that_cannot_be_a_weight_is_refused_by_what_is_wrong_with_it() {
    let all = |extra: &str| [line("RoundController", "10"), line("Alarms", "1"), line("Lifecycle", "0"), extra.to_string()].concat();
    assert!(CausalMap::load(&all(&line("Alarms", "2")), &KINDS).unwrap_err().contains("twice"));
    assert!(CausalMap::load(&all(&line("Nonesuch", "2")), &KINDS).unwrap_err().contains("Nonesuch"));
    assert!(CausalMap::load(&all("").replace("weight = 10", "weight = -1"), &KINDS).unwrap_err().contains("RoundController"));
    assert!(CausalMap::load(&all("").replace("source = \"s\"", "source = \" \""), &KINDS).is_err());
    assert!(CausalMap::load(&all("").replace("unit = \"u\"", "unit = \"\""), &KINDS).is_err());
    assert!(CausalMap::load("not toml [", &KINDS).unwrap_err().contains("does not parse"));
    assert!(CausalMap::load(&all("").replace("weight = 10", "weight = inf"), &KINDS).is_err());
}
