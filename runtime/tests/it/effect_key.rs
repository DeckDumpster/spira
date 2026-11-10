//! Every effect carries its idempotency key: a spec without one is not an effect.

use runtime::event::EffectSpec;

#[test]
fn effect_spec_without_a_key_does_not_deserialize() {
    let spec = serde_json::from_str::<EffectSpec>(r#"{"name":"t.do","input":"","deadline_ms":5}"#);
    assert!(spec.is_err(), "an EffectSpec without a key deserialized: {spec:?}");
}

#[test]
fn effect_spec_with_a_key_round_trips() {
    let json = r#"{"name":"t.do","input":"x","deadline_ms":5,"key":"t.do/sp-a"}"#;
    let spec: EffectSpec = serde_json::from_str(json).expect("a spec with a key deserializes");
    let back = serde_json::to_value(&spec).unwrap();
    assert_eq!(back, serde_json::from_str::<serde_json::Value>(json).unwrap(), "the key survives the round trip");
}

#[test]
fn a_keyed_effect_carries_exactly_the_key_it_was_given() {
    let spec = EffectSpec::keyed("t.do", "t/sp-a", "x", 5);
    assert_eq!((spec.name.as_str(), spec.key.as_str(), spec.input.as_str()), ("t.do", "t/sp-a", "x"));
}

#[test]
fn a_probe_is_keyed_by_its_name_whatever_its_input() {
    assert_eq!(EffectSpec::probe("t.observe", "a", 5).key, EffectSpec::probe("t.observe", "b", 5).key);
    assert_eq!(EffectSpec::probe("t.observe", "a", 5).key, "t.observe");
}
