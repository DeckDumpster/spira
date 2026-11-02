//! The converter's golden test: `spira.conf` + `repo-map` + two real `runes/*.rune`
//! files in, one exact `spira.toml` out. Fixtures are generic (no real host, path or
//! person) but structurally the same shape as a real install's files — same quoting, same
//! `${VAR:+text}` rune expansions, same gate command with a literal `|` inside it.

use std::fs;
use std::path::Path;

use spira_config::convert::convert;
use spira_config::{LandMode, Lane, SystemPromptMode};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn converts_conf_repo_map_and_runes() {
    let conf = fixture("spira.conf");
    let repo_map = fixture("repo-map");
    let builder = fixture("runes/builder.rune");
    let ops = fixture("runes/ops.rune");

    let (doc, warnings) = convert(
        &conf,
        "/opt/fixture-home",
        &repo_map,
        &[
            ("builder.rune", builder.as_str()),
            ("ops.rune", ops.as_str()),
        ],
    )
    .expect("fixture repo-map has no unknown lane tokens");

    assert!(
        warnings.0.is_empty(),
        "unexpected warnings: {:?}",
        warnings.0
    );

    let spira = doc.spira.clone().expect("[spira] present");
    assert_eq!(spira.get_str("SPIRA_HOME_REPO").unwrap(), Some("home".to_string()));
    assert_eq!(
        spira.get_str("SPIRA_DB").unwrap(),
        Some("/opt/fixture-home/.local/share/spira/db".to_string())
    );
    assert_eq!(
        spira.get_str("SPIRA_RUN").unwrap(),
        Some("/opt/fixture-home/.local/state/spira".to_string())
    );
    assert_eq!(spira.get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
    assert_eq!(spira.get_list("SPIRA_RUNES").unwrap(), Some(vec!["builder".to_string(), "ops".to_string()]));
    assert_eq!(spira.get_str("SPIRA_CERTIFY_SUITES").unwrap(), Some("off".to_string()));
    assert_eq!(
        spira.get_str("SPIRA_CZAR_STAGE_DEADLOCK").unwrap(),
        Some("shadow".to_string())
    );
    assert_eq!(
        spira.get_str("SPIRA_MAIL_READERS").unwrap(),
        Some("concierge=/opt/spira/concierge.sh wake".to_string())
    );

    let home = doc.repo.get("home").expect("home repo row");
    assert_eq!(home.path, "/srv/checkouts/home");
    assert_eq!(home.mode, LandMode::Push);
    assert_eq!(home.base, Some("origin/main".to_string()));
    assert_eq!(home.format, None);
    // The repo-map's gate column is not carried into spira.toml (sp-quu2w: the tree under
    // test owns its gate); the row around it must still parse — its lanes come after it.
    assert_eq!(
        home.lanes,
        vec![Lane::Plan, Lane::Incident, Lane::Groom, Lane::Spike]
    );

    let service = doc.repo.get("service").expect("service repo row");
    assert_eq!(service.mode, LandMode::Pr);
    assert_eq!(service.format, Some("cargo fmt --all".to_string()));
    // The gate column's own `2|3)` case pattern must survive the pipe-delimited parse —
    // this is the row that plants a literal `|` inside the gate column on purpose — so the
    // lanes after it still read, though the gate itself is no longer carried (sp-quu2w).
    assert_eq!(service.lanes, vec![Lane::Plan, Lane::Incident]);

    let builder = doc.persona.get("builder").expect("builder persona");
    assert_eq!(builder.model, "claude-sonnet-5");
    assert_eq!(
        builder.tools,
        vec!["Bash", "Read", "Edit", "Write", "Glob", "Grep", "TodoWrite"]
    );
    // No SPIRA_SCOPE_LABEL override reaches the rune expander through the schema's own
    // resolved [spira] section in this call (only conf-derived vars feed it), so the
    // `${SPIRA_SCOPE_LABEL:+...}` guard is empty and only the plan-label default remains.
    assert_eq!(
        builder.labels,
        vec!["myproject".to_string(), "plan".to_string()]
    );
    assert_eq!(builder.lease.as_ref().unwrap().minutes, Some(90));
    assert_eq!(builder.lease.as_ref().unwrap().heartbeat_seconds, Some(30));
    assert_eq!(builder.system_prompt, Some(SystemPromptMode::Append));

    let ops = doc.persona.get("ops").expect("ops persona");
    assert_eq!(ops.model, "claude-haiku-4-5-20251001");
    assert_eq!(ops.system_prompt, Some(SystemPromptMode::Replace));
    assert_eq!(
        ops.labels,
        vec!["myproject".to_string(), "incident".to_string()]
    );

    // The golden file: frozen output of this exact conversion, so a change to the
    // converter or the schema that alters what ships is a diff a reviewer sees, not a
    // silent drift the assertions above happen not to cover.
    let rendered = toml::to_string_pretty(&doc).expect("serializes");
    let golden = fixture("golden.toml");
    assert_eq!(
        rendered, golden,
        "converter output no longer matches the golden file"
    );
}

// sp-0inic: `queue.forge` is the explicit spelling of the `queue` alias (both land in
// LandMode::Queue / LandMode::QueueForge, distinct types but the same forge-queue mode);
// `queue.local` is the new local-landing mode with its ref in the `base` column. Before this
// change both words hit the `other` arm and were dropped with a warning rather than
// converted — this is the regression test for that (law-a-regression-test-must-be-seen-to-fail).
#[test]
fn queue_forge_and_queue_local_land_modes_convert() {
    let repo_map = "alpha | /tmp/alpha | queue.forge | origin/main | | |\n\
                     beta  | /tmp/beta  | queue.local | local/main  | | |\n";
    let (doc, warnings) =
        convert("", "/opt/fixture-home", repo_map, &[]).expect("both land modes must convert");
    assert!(
        warnings.0.is_empty(),
        "unexpected warnings: {:?}",
        warnings.0
    );
    assert_eq!(doc.repo.get("alpha").unwrap().mode, LandMode::QueueForge);
    assert_eq!(
        doc.repo.get("alpha").unwrap().base,
        Some("origin/main".to_string())
    );
    assert_eq!(doc.repo.get("beta").unwrap().mode, LandMode::QueueLocal);
    assert_eq!(
        doc.repo.get("beta").unwrap().base,
        Some("local/main".to_string())
    );
}

// POSITIVE CONTROL for the two refusal tests below: a valid mode word and a valid explicit
// lane list both still convert, so a check pointed at the wrong thing and a check that found
// nothing look different (law-absence-needs-a-positive-control).
#[test]
fn valid_lane_mode_and_label_convert() {
    let repo_map = "alpha | /tmp/alpha | push | origin/main | | true | develop\n\
                     beta  | /tmp/beta  | push | origin/main | | true | plan,groom\n";
    let (doc, _warnings) =
        convert("", "/opt/fixture-home", repo_map, &[]).expect("valid lanes must convert");
    assert_eq!(
        doc.repo.get("alpha").unwrap().lanes,
        vec![Lane::Plan, Lane::Incident, Lane::Groom, Lane::Spike]
    );
    assert_eq!(
        doc.repo.get("beta").unwrap().lanes,
        vec![Lane::Plan, Lane::Groom]
    );
}

#[test]
fn an_empty_lanes_column_admits_every_lane() {
    let repo_map = "blank | /tmp/blank | push | origin/main | | true |\n\
                     short | /tmp/short | push | origin/main | | true\n";
    let (doc, _warnings) = convert("", "/opt/fixture-home", repo_map, &[]).expect("converts");
    let every = vec![
        Lane::Plan,
        Lane::Incident,
        Lane::Groom,
        Lane::Spike,
        Lane::MimirSweep,
        Lane::CzarTrigger,
    ];
    for name in ["blank", "short"] {
        let mut got = doc.repo.get(name).expect(name).lanes.clone();
        got.sort();
        let mut want = every.clone();
        want.sort();
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn unknown_lane_mode_is_refused_not_warned() {
    let repo_map = "alpha | /tmp/alpha | push | origin/main | | true | fullaccess\n";
    let errors = convert("", "/opt/fixture-home", repo_map, &[])
        .expect_err("an unknown lane-mode word must refuse the whole convert");
    assert!(
        errors.iter().any(|e| e.contains("alpha") && e.contains("fullaccess")),
        "expected an error naming the row and the bad token, got {errors:?}"
    );
}

#[test]
fn unknown_lane_label_is_refused_not_warned() {
    let repo_map = "alpha | /tmp/alpha | push | origin/main | | true | plan,bogus-lane\n";
    let errors = convert("", "/opt/fixture-home", repo_map, &[])
        .expect_err("an unknown lane label must refuse the whole convert");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("alpha") && e.contains("bogus-lane")),
        "expected an error naming the row and the bad token, got {errors:?}"
    );
}

// ----------------------------------------------------------------------------------------
// CLI-level tests of `spira-config convert`'s shrink refusal and atomic write — sp-q5hzx.
// Exercised through the real binary (not the library's `convert()` alone) because the
// refusal, the `--force-shrink` override and the temp+rename write all live in main.rs's
// `--out` handling, not in the converter itself.
// ----------------------------------------------------------------------------------------

// ----------------------------------------------------------------------------------------
// conf.sh's persona-only refresh (spira_toml_resolve, sp-zs04v.4) calls convert with just
// --rune and --out — no --conf, no --repo-map — to regenerate [persona.*] from the runes
// on top of a spira.toml that already exists. Before this fix that starved convert() of any
// [spira]/[repo] input, so it built a document from defaults and silently wrote over every
// configured scalar and repo table the omitted inputs didn't re-supply (shrink_reason only
// counts [repo.*] tables and the runes list, so it let the [spira] wipe straight through).
// ----------------------------------------------------------------------------------------

// sp-b4oct: a legacy spira.conf that still sets the retired CPU quota keys converts — with a
// warning naming each — rather than being refused as unknown, and neither key reaches the
// converted document. Positive control for the refusal path is
// conf_key_parity::a_planted_unmapped_key_is_refused_not_dropped.
#[test]
fn retired_cpu_quota_keys_convert_with_a_warning() {
    let conf = "SPIRA_DWARF_CPU_QUOTA=400\nSPIRA_LAND_CPU_QUOTA=70\n";
    let (doc, warnings) = convert(conf, "/opt/fixture-home", "", &[])
        .expect("a retired key must not refuse the whole convert");
    for key in ["SPIRA_DWARF_CPU_QUOTA", "SPIRA_LAND_CPU_QUOTA"] {
        assert!(
            warnings.0.iter().any(|w| w.contains(key) && w.contains("sp-b4oct")),
            "no warning for {key}: {:?}",
            warnings.0
        );
    }
    let sh = spira_config::export_sh(&doc);
    assert!(!sh.contains("CPU_QUOTA"), "a retired key leaked into export --sh: {sh}");
}

// sp-gypjk: a legacy SPIRA_*_BIN converts with a warning, never refused; a non-batcher
// SPIRA_BATCHER_BIN keeps the cuts off as batcher_enable = "0".
#[test]
fn retired_tool_path_keys_convert_with_a_warning() {
    let (doc, warnings) = convert("SPIRA_ID_PREFIX = sp\nSPIRA_BATCHER_BIN = /bin/true\nSPIRA_LC_BIN = /x/spira-lc\n", "/opt/fixture-home", "", &[])
        .expect("retired keys never refuse the convert");
    assert_eq!(doc.spira.as_ref().unwrap().get_str("SPIRA_BATCHER_ENABLE").unwrap().as_deref(), Some("0"));
    assert!(warnings.0.iter().any(|w| w.contains("SPIRA_LC_BIN is retired (sp-gypjk")), "{:?}", warnings.0);
}

// sp-9hwim: the mail-mute override becomes a typed key. A legacy SPIRA_MAIL_MUTE=1 converts
// to spira.mail_mute = true (the same 0/1 spelling every other bool key here uses); an
// unset conf carries no mail_mute at all, matching "default off, mail flows normally".
#[test]
fn legacy_mail_mute_converts_to_the_typed_key() {
    let (doc, warnings) = convert("SPIRA_ID_PREFIX = sp\nSPIRA_MAIL_MUTE=1\n", "/opt/fixture-home", "", &[])
        .expect("a known key must not refuse the convert");
    assert!(warnings.0.is_empty(), "{:?}", warnings.0);
    assert_eq!(doc.spira.as_ref().unwrap().get_bool("SPIRA_MAIL_MUTE").unwrap(), Some(true));
}

#[test]
fn mail_mute_absent_by_default() {
    let (doc, warnings) =
        convert("SPIRA_ID_PREFIX = sp\nSPIRA_HOME_REPO=home\n", "/opt/fixture-home", "", &[]).expect("converts");
    assert!(warnings.0.is_empty(), "{:?}", warnings.0);
    assert_eq!(doc.spira.as_ref().unwrap().get_bool("SPIRA_MAIL_MUTE").unwrap(), None);
}

#[test]
fn mail_mute_bad_value_warns_not_a_spelling_the_key_accepts() {
    let (doc, warnings) = convert("SPIRA_ID_PREFIX = sp\nSPIRA_MAIL_MUTE=maybe\n", "/opt/fixture-home", "", &[])
        .expect("a bad value warns, it does not refuse the whole convert");
    assert_eq!(doc.spira.as_ref().unwrap().get_bool("SPIRA_MAIL_MUTE").unwrap(), None);
    assert!(
        warnings.0.iter().any(|w| w.contains("mail_mute") && w.contains("maybe")),
        "{:?}",
        warnings.0
    );
}

// sp-op2c2: a spira.conf still setting the retired quarantine-reactivation key converts with a
// warning naming the bead, never refused, and the key does not reach the converted document.
#[test]
fn retired_quarantine_clean_runs_converts_with_a_warning() {
    let (doc, warnings) = convert("SPIRA_QUARANTINE_CLEAN_RUNS=10\n", "/opt/fixture-home", "", &[])
        .expect("a retired key must not refuse the whole convert");
    assert!(
        warnings.0.iter().any(|w| w.contains("SPIRA_QUARANTINE_CLEAN_RUNS") && w.contains("sp-op2c2")),
        "no warning: {:?}",
        warnings.0
    );
    let sh = spira_config::export_sh(&doc);
    assert!(!sh.contains("CLEAN_RUNS"), "a retired key leaked into export --sh: {sh}");
}
