//! The converter's golden test: `spira.conf` + `repo-map` + two real `chamber/*.fayth`
//! files in, one exact `spira.toml` out. Fixtures are generic (no real host, path or
//! person) but structurally the same shape as a real install's files — same quoting, same
//! `${VAR:+text}` fayth expansions, same gate command with a literal `|` inside it.

use std::fs;
use std::path::Path;
use std::process::Command;

use spira_config::convert::convert;
use spira_config::{LandMode, Lane, SystemPromptMode};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn converts_conf_repo_map_and_fayths() {
    let conf = fixture("spira.conf");
    let repo_map = fixture("repo-map");
    let builder = fixture("chamber/builder.fayth");
    let ops = fixture("chamber/ops.fayth");

    let (doc, warnings) = convert(
        &conf,
        "/opt/fixture-home",
        &repo_map,
        &[
            ("builder.fayth", builder.as_str()),
            ("ops.fayth", ops.as_str()),
        ],
    )
    .expect("fixture repo-map has no unknown lane tokens");

    assert!(
        warnings.0.is_empty(),
        "unexpected warnings: {:?}",
        warnings.0
    );

    let spira = doc.spira.clone().expect("[spira] present");
    assert_eq!(spira.home_repo, Some("home".to_string()));
    assert_eq!(
        spira.db,
        Some("/opt/fixture-home/.local/share/spira/db".to_string())
    );
    assert_eq!(
        spira.run,
        Some("/opt/fixture-home/.local/state/spira".to_string())
    );
    assert_eq!(spira.max_aeons, Some(4));
    assert_eq!(spira.fayths, Some(vec!["builder".to_string(), "ops".to_string()]));
    assert_eq!(spira.certify_suites, Some(spira_config::OnOff::Off));
    assert_eq!(
        spira.czar_stage_deadlock,
        Some(spira_config::CzarStage::Shadow)
    );
    assert_eq!(
        spira.mail_readers,
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
    // No SPIRA_SCOPE_LABEL override reaches the fayth expander through the schema's own
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

fn scratch_dir(tag: &str) -> testkit::TempDir {
    let dir = testkit::TempDir::new(&format!("spira-config-test-{tag}-{:?}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()));
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn run_convert(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spira-config"))
        .arg("convert")
        .args(args)
        .output()
        .expect("spira-config convert runs")
}

const EXISTING_TWO_REPOS_TWO_FAYTHS: &str = "\
[spira]\nid_prefix = \"sp\"\nfayths = [\"a\", \"b\"]\n\n\
[repo.alpha]\npath = \"/tmp/alpha\"\nmode = \"push\"\n\n\
[repo.beta]\npath = \"/tmp/beta\"\nmode = \"push\"\n";

const EXISTING_ONE_REPO_TWO_FAYTHS: &str = "\
[spira]\nid_prefix = \"sp\"\nfayths = [\"a\", \"b\"]\n\n\
[repo.alpha]\npath = \"/tmp/alpha\"\nmode = \"push\"\n";

const ONE_ROW_REPO_MAP: &str = "alpha | /tmp/alpha | push | origin/main | | true | plan\n";

// POSITIVE CONTROL for both refusal tests below: converting onto an existing document with
// equal or fewer counts on neither side must still write — the refusal fires on a real
// shrink only, not on every `--out` that already exists.
#[test]
fn convert_over_an_equal_document_still_writes() {
    let dir = scratch_dir("equal");
    let out = dir.join("spira.toml");
    let conf = dir.join("spira.conf");
    fs::write(&conf, "SPIRA_ID_PREFIX = sp\nSPIRA_FAYTHS = a b\n").unwrap();
    fs::write(&out, EXISTING_ONE_REPO_TWO_FAYTHS).unwrap();

    let result = run_convert(&[
        "--conf",
        conf.to_str().unwrap(),
        "--repo-map",
        &format!("{}", write_tmp(&dir, "repo-map", ONE_ROW_REPO_MAP).display()),
        "--home",
        "/opt/fixture-home",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "expected success, got: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let doc = spira_config::validate(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc.repo.len(), 1);
    fs::remove_dir_all(&dir).ok();
}

fn write_tmp(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, contents).unwrap();
    p
}

#[test]
fn convert_refuses_to_shrink_repo_tables() {
    let dir = scratch_dir("shrink-repos");
    let out = dir.join("spira.toml");
    fs::write(&out, EXISTING_TWO_REPOS_TWO_FAYTHS).unwrap();
    let conf = write_tmp(&dir, "spira.conf", "SPIRA_ID_PREFIX = sp\nSPIRA_FAYTHS = a b\n");
    let repo_map = write_tmp(&dir, "repo-map", ONE_ROW_REPO_MAP);

    let result = run_convert(&[
        "--conf",
        conf.to_str().unwrap(),
        "--repo-map",
        repo_map.to_str().unwrap(),
        "--home",
        "/opt/fixture-home",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!result.status.success(), "expected refusal");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("refuses to shrink"), "{stderr}");
    assert!(stderr.contains('2') && stderr.contains('1'), "{stderr}");
    // The existing document must survive the refused write untouched.
    assert_eq!(fs::read_to_string(&out).unwrap(), EXISTING_TWO_REPOS_TWO_FAYTHS);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn convert_refuses_to_shrink_fayths() {
    let dir = scratch_dir("shrink-fayths");
    let out = dir.join("spira.toml");
    fs::write(&out, EXISTING_ONE_REPO_TWO_FAYTHS).unwrap();
    let conf = write_tmp(&dir, "spira.conf", "SPIRA_ID_PREFIX = sp\nSPIRA_FAYTHS = a\n");
    let repo_map = write_tmp(&dir, "repo-map", ONE_ROW_REPO_MAP);

    let result = run_convert(&[
        "--conf",
        conf.to_str().unwrap(),
        "--repo-map",
        repo_map.to_str().unwrap(),
        "--home",
        "/opt/fixture-home",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!result.status.success(), "expected refusal");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("refuses to shrink"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        EXISTING_ONE_REPO_TWO_FAYTHS
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn convert_force_shrink_overrides_the_refusal() {
    let dir = scratch_dir("force-shrink");
    let out = dir.join("spira.toml");
    fs::write(&out, EXISTING_TWO_REPOS_TWO_FAYTHS).unwrap();
    let conf = write_tmp(&dir, "spira.conf", "SPIRA_ID_PREFIX = sp\nSPIRA_FAYTHS = a b\n");
    let repo_map = write_tmp(&dir, "repo-map", ONE_ROW_REPO_MAP);

    let result = run_convert(&[
        "--conf",
        conf.to_str().unwrap(),
        "--repo-map",
        repo_map.to_str().unwrap(),
        "--home",
        "/opt/fixture-home",
        "--out",
        out.to_str().unwrap(),
        "--force-shrink",
    ]);
    assert!(
        result.status.success(),
        "expected --force-shrink to override the refusal: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let doc = spira_config::validate(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc.repo.len(), 1, "the shrink must actually have landed");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn convert_write_is_atomic_no_leftover_temp_file() {
    let dir = scratch_dir("atomic");
    let out = dir.join("spira.toml");
    let conf = write_tmp(&dir, "spira.conf", "SPIRA_ID_PREFIX = sp\nSPIRA_FAYTHS = a\n");
    let repo_map = write_tmp(&dir, "repo-map", ONE_ROW_REPO_MAP);

    let result = run_convert(&[
        "--conf",
        conf.to_str().unwrap(),
        "--repo-map",
        repo_map.to_str().unwrap(),
        "--home",
        "/opt/fixture-home",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(result.status.success());
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "leftover temp file(s): {leftovers:?}");
    fs::remove_dir_all(&dir).ok();
}

// ----------------------------------------------------------------------------------------
// conf.sh's persona-only refresh (spira_toml_resolve, sp-zs04v.4) calls convert with just
// --fayth and --out — no --conf, no --repo-map — to regenerate [persona.*] from the chamber
// on top of a spira.toml that already exists. Before this fix that starved convert() of any
// [spira]/[repo] input, so it built a document from defaults and silently wrote over every
// configured scalar and repo table the omitted inputs didn't re-supply (shrink_reason only
// counts [repo.*] tables and the fayths list, so it let the [spira] wipe straight through).
// ----------------------------------------------------------------------------------------

const EXISTING_SPIRA_SCALAR_AND_REPO: &str = "\
[spira]\nid_prefix = \"sp\"\nfayths = [\"builder\"]\nbatch_mem_per_suite_mib = 256\nbd = \"/custom/bd\"\n\n\
[repo.alpha]\npath = \"/tmp/alpha\"\nmode = \"push\"\n";

// No [repo.*] table and an empty `fayths` list, so shrink_reason's two counters (the only
// things it compares) read 0-vs-0 either way and cannot catch this one — a scalar-only wipe
// is invisible to it and must fail SILENTLY (exit 0, wrong content) without this fix, exactly
// the shape the bead's own regression (test-batch-maxpar, test-bd-resolve, test-conf-toml)
// hit: SPIRA_BATCH_MEM_PER_SUITE_MIB and SPIRA_BD reverted to "unset"/PATH default with the
// convert call reporting no error at all.
const EXISTING_SPIRA_SCALAR_ONLY: &str =
    "[spira]\nid_prefix = \"sp\"\nbatch_mem_per_suite_mib = 256\nbd = \"/custom/bd\"\n";

#[test]
fn fayth_only_convert_preserves_existing_spira_scalars_and_repo_tables() {
    let dir = scratch_dir("fayth-only-preserve");
    let out = dir.join("spira.toml");
    fs::write(&out, EXISTING_SPIRA_SCALAR_AND_REPO).unwrap();
    let fayth = write_tmp(
        &dir,
        "builder.fayth",
        "FAYTH_NAME=builder\nFAYTH_MODEL=opus\n",
    );

    // POSITIVE CONTROL: prove the starting document really holds what this test claims is at
    // risk, before the run that must preserve it.
    let before = spira_config::validate(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        before.spira.as_ref().unwrap().batch_mem_per_suite_mib,
        Some(256)
    );
    assert_eq!(before.repo.len(), 1);

    let result = run_convert(&[
        "--home",
        "/opt/fixture-home",
        "--fayth",
        fayth.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "expected success, got: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let after = spira_config::validate(&fs::read_to_string(&out).unwrap()).unwrap();
    let spira = after.spira.expect("[spira] survives a fayth-only refresh");
    assert_eq!(
        spira.batch_mem_per_suite_mib,
        Some(256),
        "a scalar [spira] key must survive a persona-only refresh"
    );
    assert_eq!(spira.bd, Some("/custom/bd".to_string()));
    assert_eq!(after.repo.len(), 1, "[repo.*] tables must survive too");
    assert!(after.repo.contains_key("alpha"));
    assert_eq!(
        after.persona.get("builder").map(|p| p.model.as_str()),
        Some("opus"),
        "the fayth-derived persona table must still be regenerated"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn fayth_only_convert_does_not_silently_wipe_scalar_spira_keys() {
    let dir = scratch_dir("fayth-only-scalar-only");
    let out = dir.join("spira.toml");
    fs::write(&out, EXISTING_SPIRA_SCALAR_ONLY).unwrap();
    let fayth = write_tmp(
        &dir,
        "builder.fayth",
        "FAYTH_NAME=builder\nFAYTH_MODEL=opus\n",
    );

    let result = run_convert(&[
        "--home",
        "/opt/fixture-home",
        "--fayth",
        fayth.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "expected success, got: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let after = spira_config::validate(&fs::read_to_string(&out).unwrap()).unwrap();
    let spira = after.spira.expect("[spira] survives a fayth-only refresh");
    assert_eq!(
        spira.batch_mem_per_suite_mib,
        Some(256),
        "shrink_reason cannot see this key, so a silent wipe would exit 0 with it gone"
    );
    assert_eq!(spira.bd, Some("/custom/bd".to_string()));
    fs::remove_dir_all(&dir).ok();
}

// sp-b4oct: a legacy spira.conf that still sets the retired CPU quota keys converts — with a
// warning naming each — rather than being refused as unknown, and neither key reaches the
// converted document. Positive control for the refusal path is
// conf_key_parity::a_planted_unmapped_key_is_refused_not_dropped.
#[test]
fn retired_cpu_quota_keys_convert_with_a_warning() {
    let conf = "SPIRA_AEON_CPU_QUOTA=400\nSPIRA_LAND_CPU_QUOTA=70\n";
    let (doc, warnings) = convert(conf, "/opt/fixture-home", "", &[])
        .expect("a retired key must not refuse the whole convert");
    for key in ["SPIRA_AEON_CPU_QUOTA", "SPIRA_LAND_CPU_QUOTA"] {
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
    assert_eq!(doc.spira.as_ref().unwrap().batcher_enable.as_deref(), Some("0"));
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
    assert_eq!(doc.spira.as_ref().unwrap().mail_mute, Some(true));
}

#[test]
fn mail_mute_absent_by_default() {
    let (doc, warnings) =
        convert("SPIRA_ID_PREFIX = sp\nSPIRA_HOME_REPO=home\n", "/opt/fixture-home", "", &[]).expect("converts");
    assert!(warnings.0.is_empty(), "{:?}", warnings.0);
    assert_eq!(doc.spira.as_ref().unwrap().mail_mute, None);
}

#[test]
fn mail_mute_bad_value_warns_not_a_spelling_the_key_accepts() {
    let (doc, warnings) = convert("SPIRA_ID_PREFIX = sp\nSPIRA_MAIL_MUTE=maybe\n", "/opt/fixture-home", "", &[])
        .expect("a bad value warns, it does not refuse the whole convert");
    assert_eq!(doc.spira.as_ref().unwrap().mail_mute, None);
    assert!(
        warnings.0.iter().any(|w| w.contains("mail_mute") && w.contains("maybe")),
        "{:?}",
        warnings.0
    );
}

// sp-k6m1m: the goal is retired — a legacy SPIRA_GOAL converts with a warning and lands
// nowhere — and the id prefix it used to imply must now be written in: a conf that sets
// anything without SPIRA_ID_PREFIX is refused rather than converted into a document every
// reader would refuse.
#[test]
fn a_legacy_goal_is_retired_with_a_warning() {
    let (doc, warnings) = convert("SPIRA_ID_PREFIX = sp\nSPIRA_GOAL = sp-spira\n", "/opt/fixture-home", "", &[])
        .expect("a retired key never refuses the convert");
    assert!(warnings.0.iter().any(|w| w.contains("SPIRA_GOAL is retired (sp-k6m1m")), "{:?}", warnings.0);
    let rendered = toml::to_string_pretty(&doc).unwrap();
    assert!(!rendered.contains("goal"), "{rendered}");
    assert_eq!(doc.spira.as_ref().unwrap().id_prefix.as_deref(), Some("sp"));
}

#[test]
fn a_conf_without_an_id_prefix_converts_and_strict_validation_refuses_it() {
    let (doc, _) = convert("SPIRA_HOME_REPO = home\nSPIRA_GOAL = sp-spira\n", "/opt/fixture-home", "", &[])
        .expect("converts; the refusal is validate's");
    let text = toml::to_string_pretty(&doc).unwrap();
    let err = spira_config::validate_strict(&text).unwrap_err();
    assert!(err.starts_with("spira.id_prefix: required"), "{err}");
    // Positive control: the same conf with the prefix passes strict validation.
    let (doc, _) = convert("SPIRA_HOME_REPO = home\nSPIRA_ID_PREFIX = sp\n", "/opt/fixture-home", "", &[]).expect("converts");
    spira_config::validate_strict(&toml::to_string_pretty(&doc).unwrap()).expect("valid");
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
