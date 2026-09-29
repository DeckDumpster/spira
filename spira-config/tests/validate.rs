//! AC coverage per section: valid, unknown key, wrong type, bad enum, missing field — for
//! `[spira]`, `[repo.<name>]` and `[persona.<name>]` each — plus the T0 check that the
//! shipped example validates, and that the checked-in schema still matches the types.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use spira_config::{is_secret_shaped, json_schema, missing_from_retirement, validate};

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
fn stack_max_depths_schema_ceiling_is_four() {
    // Hard ceiling per Ryan 2026-09-28 ("4 is a good place to start, no higher") — raising
    // it is a schema/design change, not a config edit.
    let schema = serde_json::to_value(json_schema()).expect("schema serializes");
    let field = &schema["definitions"]["SpiraSection"]["properties"]["stack_max_depth"];
    assert_eq!(field["maximum"], serde_json::json!(4.0), "{field}");
}

// spira.toml must never hold a secret (design's credential-storage requirement): a real
// credential belongs in its own 0600 file, never a field this schema types and `export --sh`
// can put into every shell's environment. This is the schema's half of that rule — a new
// field named like one is refused here before it ever ships, rather than relying on someone
// noticing at review time.
//
// GRANDFATHERED: `broker_gh_token` predates this rule and is not exported (see
// `export_sh_omits_a_secret_shaped_field` in lib.rs's own tests); migrating it out of
// `spira.toml` entirely is separate, coordinated work. No other field may be added to this
// list — a new secret-shaped field is a defect, not a second exception.
const GRANDFATHERED_SECRET_SHAPED_FIELDS: &[&str] = &["broker_gh_token"];

fn collect_property_names(schema: &serde_json::Value, out: &mut BTreeSet<String>) {
    match schema {
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::Object(props)) = obj.get("properties") {
                for key in props.keys() {
                    out.insert(key.clone());
                }
            }
            for v in obj.values() {
                collect_property_names(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                collect_property_names(v, out);
            }
        }
        _ => {}
    }
}

#[test]
fn schema_refuses_new_secret_shaped_field_names() {
    let schema = serde_json::to_value(json_schema()).expect("schema serializes");
    let mut names = BTreeSet::new();
    collect_property_names(&schema, &mut names);
    assert!(names.len() > 20, "sanity: expected many property names, got {}", names.len());

    let offenders: Vec<&String> = names
        .iter()
        .filter(|n| is_secret_shaped(n))
        .filter(|n| !GRANDFATHERED_SECRET_SHAPED_FIELDS.contains(&n.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "secret-shaped field name(s) added to the schema: {offenders:?} — a credential \
         belongs in its own file, never spira.toml"
    );
}

// POSITIVE CONTROL: a planted secret-shaped name must be caught by the same walk, proving
// the check above could have found a real offender instead of passing vacuously.
#[test]
fn schema_refuses_new_secret_shaped_field_names_positive_control() {
    let mut names = BTreeSet::new();
    names.insert("db_password".to_string());
    let offenders: Vec<&String> = names
        .iter()
        .filter(|n| is_secret_shaped(n))
        .filter(|n| !GRANDFATHERED_SECRET_SHAPED_FIELDS.contains(&n.as_str()))
        .collect();
    assert_eq!(offenders, vec!["db_password"]);
}

mod spira_section {
    use super::validate;

    #[test]
    fn valid() {
        validate("[spira]\nid_prefix = \"sp\"\nhome_repo = \"home\"\nmax_aeons = 4\n").expect("valid");
    }

    #[test]
    fn unknown_key() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nhome_repo = \"home\"\nspelled_rong = 1\n").unwrap_err();
        assert!(err.starts_with("spira.spelled_rong"), "{err}");
    }

    #[test]
    fn wrong_type() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nmax_aeons = true\n").unwrap_err();
        assert!(err.starts_with("spira.max_aeons"), "{err}");
    }

    #[test]
    fn bad_enum() {
        let err = validate("[spira]\nid_prefix = \"sp\"\ncertify_suites = \"maybe\"\n").unwrap_err();
        assert!(err.starts_with("spira.certify_suites"), "{err}");
    }

    #[test]
    fn an_empty_table_needs_no_field() {
        // An empty [spira] table configures nothing, matching a clean clone that overrides
        // nothing — so not even id_prefix is required of it.
        validate("[spira]\n").expect("an empty [spira] table is valid");
    }

    // sp-k6m1m: id_prefix is the one required [spira] key. It used to be derived from the
    // goal epic's id; with the goal retired, `spira-config validate` (validate_strict: doctor,
    // pre-activate) refuses a table that sets anything and leaves the prefix out, naming the
    // key. The typed readers still load such a document — refusing the whole file would drop
    // every other key to its default.
    #[test]
    fn id_prefix_is_required_by_strict_validation_once_the_table_sets_anything() {
        let text = "[spira]\nhome_repo = \"home\"\n";
        let err = spira_config::validate_strict(text).unwrap_err();
        assert!(err.starts_with("spira.id_prefix: required"), "{err}");
        assert!(err.contains("sp-k6m1m"), "{err}");
        validate(text).expect("a reader still loads it");
        // Positive control: the same table with the prefix passes strict validation.
        let (doc, _) = spira_config::validate_strict("[spira]\nhome_repo = \"home\"\nid_prefix = \"sp\"\n").expect("valid");
        assert_eq!(doc.spira.unwrap().id_prefix.as_deref(), Some("sp"));
        spira_config::validate_strict("[spira]\n").expect("an empty table configures nothing");
        spira_config::validate_strict("[repo.a]\npath = \"/a\"\nmode = \"push\"\n").expect("no [spira] at all");
    }

    #[test]
    fn an_unusable_id_prefix_is_refused() {
        for bad in ["", "sp-", "s p", "sp-spira"] {
            let err = spira_config::validate_strict(&format!("[spira]\nid_prefix = {bad:?}\n")).unwrap_err();
            assert!(err.starts_with("spira.id_prefix:"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn the_retired_goal_key_warns_and_does_not_stand_in_for_the_prefix() {
        let (doc, w) = spira_config::validate_strict("[spira]\nid_prefix = \"sp\"\ngoal = \"sp-spira\"\n")
            .expect("a retired key is a warning, not an error");
        assert!(w.iter().any(|w| w.contains("goal is retired (sp-k6m1m)")), "{w:?}");
        assert!(!spira_config::export_sh(&doc).contains("GOAL"));
        // Production's shape before the migration: goal set, id_prefix absent — refused.
        let err = spira_config::validate_strict("[spira]\ngoal = \"sp-spira\"\ndb = \"/db\"\n").unwrap_err();
        assert!(err.starts_with("spira.id_prefix: required"), "{err}");
    }

    #[test]
    fn the_migration_is_one_set() {
        let text = "[spira]\ngoal = \"sp-spira\"\ndb = \"/db\"\n";
        let doc = validate(text).expect("the unmigrated file still opens for writing");
        let fixed = spira_config::set_path(&doc, "spira.id_prefix", "sp").unwrap();
        let out = toml::to_string_pretty(&fixed).unwrap();
        let (back, _) = spira_config::validate_strict(&out).expect("repaired");
        assert_eq!(back.spira.unwrap().db.as_deref(), Some("/db"), "the rest of [spira] survives");
        assert!(!out.contains("goal"), "and the retired key is gone from the rewrite: {out}");
    }

    #[test]
    fn stack_max_depth_at_the_pinned_non_default_value_is_valid() {
        // Pinned away from the shipped default (4) per this repo's own fixture rule: 2
        // would still pass if the code had the default hard-coded instead of actually
        // reading the key.
        let doc = validate("[spira]\nid_prefix = \"sp\"\nstack_max_depth = 2\n").expect("valid");
        assert_eq!(doc.spira.unwrap().stack_max_depth, Some(2));
    }

    #[test]
    fn stack_max_depth_zero_is_valid() {
        // stacked-dependents-2026-09-28 §1: 0 reproduces today's no-stacking behaviour.
        let doc = validate("[spira]\nid_prefix = \"sp\"\nstack_max_depth = 0\n").expect("valid");
        assert_eq!(doc.spira.unwrap().stack_max_depth, Some(0));
    }

    #[test]
    fn mail_mute_true_is_valid() {
        // sp-9hwim: the typed replacement for the mail-mute file-existence override.
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmail_mute = true\n").expect("valid");
        assert_eq!(doc.spira.unwrap().mail_mute, Some(true));
    }

    #[test]
    fn mail_mute_wrong_type_is_refused() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nmail_mute = \"yes\"\n").unwrap_err();
        assert!(err.starts_with("spira.mail_mute"), "{err}");
    }

    #[test]
    fn stack_max_depth_above_the_hard_ceiling_is_refused_by_validate_itself() {
        // The shipped schema's `maximum` is never run against the document — validate()
        // deserializes TOML directly — so the ceiling must be enforced here too, not only
        // by the lifecycle crate's own claim-time check.
        let err = validate("[spira]\nid_prefix = \"sp\"\nstack_max_depth = 5\n").unwrap_err();
        assert!(err.starts_with("spira.stack_max_depth"), "{err}");
    }

    #[test]
    fn stack_max_depth_at_the_hard_ceiling_is_valid() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nstack_max_depth = 4\n").expect("valid");
        assert_eq!(doc.spira.unwrap().stack_max_depth, Some(4));
    }
}

mod retired_keys {
    use super::*;
    use spira_config::validate_with_warnings;

    #[test]
    fn retired_key_warns_naming_the_key_and_bead() {
        let (doc, warnings) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nqueue_local_gate = 1\n")
            .expect("a retired key must validate, not error");
        assert!(doc.spira.is_some());
        assert!(
            warnings.iter().any(|w| w.contains("queue_local_gate") && w.contains("sp-vsob2")),
            "expected a warning naming queue_local_gate and sp-vsob2, got {warnings:?}"
        );
    }

    #[test]
    fn a_misspelt_key_still_fails() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nqueue_batch_idle_cutt = 1\n").unwrap_err();
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

// sp-gypjk: tool-path keys are retired, but prod's `batcher_bin = "/bin/true"` switched the
// batcher's cuts off, and dropping it silently would switch them back on.
#[test]
fn a_non_batcher_batcher_bin_is_read_as_batcher_enable_0() {
    let (doc, w) = spira_config::validate_with_warnings("[spira]\nid_prefix = \"sp\"\nbatcher_bin = \"/bin/true\"\nlc_bin = \"/x/spira-lc\"\n").unwrap();
    assert_eq!(doc.spira.as_ref().unwrap().batcher_enable.as_deref(), Some("0"));
    assert!(w.iter().any(|m| m.contains("batcher_enable = \"0\"")), "{w:?}");
    assert!(w.iter().any(|m| m.starts_with("lc_bin is retired (sp-gypjk)")), "{w:?}");
    // The batcher itself is not an off switch, and an explicit batcher_enable wins.
    let (doc, _) = spira_config::validate_with_warnings("[spira]\nid_prefix = \"sp\"\nbatcher_bin = \"/r/bin/batcher\"\n").unwrap();
    assert_eq!(doc.spira.as_ref().unwrap().batcher_enable, None);
    let (doc, _) = spira_config::validate_with_warnings("[spira]\nid_prefix = \"sp\"\nbatcher_bin = \"/bin/true\"\nbatcher_enable = \"1\"\n").unwrap();
    assert_eq!(doc.spira.as_ref().unwrap().batcher_enable.as_deref(), Some("1"));
}
