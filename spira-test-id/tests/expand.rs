use spira_test_id::test_id;

fn helper() -> u32 {
    7
}

macro_rules! at_closure {
    ($name:literal, $id:literal) => {
        assert!(module_path!().ends_with(concat!("::", $name)), "{}", module_path!());
        fn inner() {}
        let t = std::any::type_name_of_val(&inner);
        assert!(t.contains(concat!("::", $name, "::", $id, "::")) && t.ends_with("::inner"), "{t}");
    };
}

macro_rules! at {
    ($name:literal, $id:literal) => {
        assert!(module_path!().ends_with(concat!("::", $name)), "{}", module_path!());
        fn inner() {}
        let t = std::any::type_name_of_val(&inner);
        assert!(t.ends_with(concat!("::", $name, "::", $id, "::inner")), "{t}");
    };
}

#[test_id(t000001)]
#[test]
fn plain() {
    at!("plain", "t000001");
}

#[test_id(t000002)]
#[tokio::test]
async fn async_test() {
    at_closure!("async_test", "t000002");
}

#[test_id(t000003)]
#[test]
#[should_panic(expected = "x")]
fn panics() {
    at!("panics", "t000003");
    panic!("x");
}

#[test_id(t000004)]
#[test]
#[ignore]
fn ignored() {
    at!("ignored", "t000004");
}

#[test_id(t000005)]
#[test]
fn returns_result() -> Result<(), String> {
    at!("returns_result", "t000005");
    Ok(())
}

/// A doc comment.
#[allow(unused)]
#[test_id(t000006)]
#[test]
fn documented() {
    at!("documented", "t000006");
}

#[test_id(t000007)]
#[test]
fn calls_parent_helper() {
    assert_eq!(helper(), 7);
    at!("calls_parent_helper", "t000007");
}

proptest::proptest! {
    #[test_id(t000008)]
    #[test]
    fn in_proptest(_x in 0u8..3) {
        at_closure!("in_proptest", "t000008");
    }
}
