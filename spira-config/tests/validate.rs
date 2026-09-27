//! AC coverage per section: valid, unknown key, wrong type, bad enum, missing field — for
//! `[spira]`, `[repo.<name>]` and `[persona.<name>]` each — plus the T0 check that the
//! shipped example validates, and that the checked-in schema still matches the types.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use spira_config::{json_schema, missing_from_retirement, validate};

fn manifest_path(rel: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn t0_shipped_example_validates() {
    let text = fs::read_to_string(manifest_path("examples/spira.toml")).expect("example exists");
    validate(&text).expect("the shipped example must validate against its own schema");
}

#[test]
fn shipped_schema_matches_the_types() {
    let shipped = fs::read_to_string(manifest_path("schema/spira.schema.json"))
        .expect("schema/spira.schema.json exists");
    let shipped: serde_json::Value =
        serde_json::from_str(&shipped).expect("shipped schema is valid JSON");
    let current = serde_json::to_value(json_schema()).expect("current schema serializes");
    assert_eq!(
        shipped, current,
        "schema/spira.schema.json is stale — regenerate with `cargo run --bin spira-config -- schema`"
    );
}

mod spira_section {
    use super::validate;

    #[test]
    fn valid() {
        validate("[spira]\nhome_repo = \"home\"\nmax_aeons = 4\n").expect("valid");
    }

    #[test]
    fn unknown_key() {
        let err = validate("[spira]\nhome_repo = \"home\"\nspelled_rong = 1\n").unwrap_err();
        assert!(err.starts_with("spira.spelled_rong"), "{err}");
    }

    #[test]
    fn wrong_type() {
        let err = validate("[spira]\nmax_aeons = true\n").unwrap_err();
        assert!(err.starts_with("spira.max_aeons"), "{err}");
    }

    #[test]
    fn bad_enum() {
        let err = validate("[spira]\ncertify_suites = \"maybe\"\n").unwrap_err();
        assert!(err.starts_with("spira.certify_suites"), "{err}");
    }

    #[test]
    fn no_field_is_required() {
        // Every [spira] key is optional (conf.sh's own philosophy): an empty table is a
        // valid document, matching a clean clone that overrides nothing.
        validate("[spira]\n").expect("an empty [spira] table is valid");
    }
}

mod retired_keys {
    use super::*;
    use spira_config::validate_with_warnings;

    #[test]
    fn retired_key_warns_naming_the_key_and_bead() {
        let (doc, warnings) = validate_with_warnings("[spira]\nqueue_local_gate = 1\n")
            .expect("a retired key must validate, not error");
        assert!(doc.spira.is_some());
        assert!(
            warnings.iter().any(|w| w.contains("queue_local_gate") && w.contains("sp-vsob2")),
            "expected a warning naming queue_local_gate and sp-vsob2, got {warnings:?}"
        );
    }

    #[test]
    fn a_misspelt_key_still_fails() {
        let err = validate("[spira]\nqueue_batch_idle_cutt = 1\n").unwrap_err();
        assert!(err.starts_with("spira.queue_batch_idle_cutt"), "{err}");
    }

    #[test]
    fn dropping_a_key_without_retiring_it_fails_the_suite() {
        let history: BTreeSet<String> =
            ["home_repo", "max_aeons", "never_retired_by_anything"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let active: BTreeSet<String> =
            ["home_repo", "max_aeons"].iter().map(|s| s.to_string()).collect();
        let missing = missing_from_retirement(&history, &active);
        assert_eq!(missing, vec!["never_retired_by_anything".to_string()]);
    }

    #[test]
    fn the_schemas_current_field_set_matches_its_checked_in_history() {
        let history_text = fs::read_to_string(manifest_path("schema/spira-key-history.txt"))
            .expect("schema/spira-key-history.txt exists");
        let history: BTreeSet<String> = history_text
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect();

        let schema = serde_json::to_value(json_schema()).expect("schema serializes");
        let active: BTreeSet<String> = schema["definitions"]["SpiraSection"]["properties"]
            .as_object()
            .expect("SpiraSection has properties")
            .keys()
            .cloned()
            .collect();

        let missing = missing_from_retirement(&history, &active);
        assert!(
            missing.is_empty(),
            "key(s) dropped from the schema without retiring: {missing:?} — add to \
             spira_config::RETIRED_SPIRA_KEYS"
        );

        let unrecorded: Vec<_> = active.difference(&history).cloned().collect();
        assert!(
            unrecorded.is_empty(),
            "active key(s) missing from schema/spira-key-history.txt: {unrecorded:?}"
        );
    }
}

mod repo_section {
    use super::validate;

    #[test]
    fn valid() {
        validate("[repo.home]\npath = \"/srv/checkouts/home\"\nmode = \"push\"\n").expect("valid");
    }

    #[test]
    fn unknown_key() {
        let err = validate(
            "[repo.home]\npath = \"/srv/checkouts/home\"\nmode = \"push\"\nnickname = \"x\"\n",
        )
        .unwrap_err();
        assert!(err.starts_with("repo.home.nickname"), "{err}");
    }

    #[test]
    fn wrong_type() {
        let err = validate(
            "[repo.home]\npath = \"/srv/checkouts/home\"\nmode = \"push\"\nlanes = \"plan\"\n",
        )
        .unwrap_err();
        assert!(err.starts_with("repo.home.lanes"), "{err}");
    }

    #[test]
    fn bad_enum() {
        let err = validate("[repo.home]\npath = \"/srv/checkouts/home\"\nmode = \"sometimes\"\n")
            .unwrap_err();
        assert!(err.starts_with("repo.home.mode"), "{err}");
    }

    #[test]
    fn missing_field() {
        let err = validate("[repo.home]\nmode = \"push\"\n").unwrap_err();
        assert!(err.starts_with("repo.home"), "{err}");
    }
}

mod persona_section {
    use super::validate;

    #[test]
    fn valid() {
        validate("[persona.builder]\nmodel = \"claude-sonnet-5\"\n").expect("valid");
    }

    #[test]
    fn unknown_key() {
        let err =
            validate("[persona.builder]\nmodel = \"claude-sonnet-5\"\nfavorite_color = \"blue\"\n")
                .unwrap_err();
        assert!(err.starts_with("persona.builder.favorite_color"), "{err}");
    }

    #[test]
    fn wrong_type() {
        let err = validate("[persona.builder]\nmodel = \"claude-sonnet-5\"\ntools = \"Bash\"\n")
            .unwrap_err();
        assert!(err.starts_with("persona.builder.tools"), "{err}");
    }

    #[test]
    fn bad_enum() {
        let err = validate(
            "[persona.builder]\nmodel = \"claude-sonnet-5\"\nsystem_prompt = \"overwrite\"\n",
        )
        .unwrap_err();
        assert!(err.starts_with("persona.builder.system_prompt"), "{err}");
    }

    #[test]
    fn missing_field() {
        let err = validate("[persona.builder]\ntools = [\"Bash\"]\n").unwrap_err();
        assert!(err.starts_with("persona.builder"), "{err}");
    }
}
