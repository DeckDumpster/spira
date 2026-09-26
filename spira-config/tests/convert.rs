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
    assert_eq!(spira.fayths, vec!["builder".to_string(), "ops".to_string()]);
    assert_eq!(spira.certify_suites, Some(spira_config::OnOff::Off));
    assert_eq!(spira.cert_idle_skip, Some(false));
    assert_eq!(spira.queue_local_gate, Some(false));
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
    assert_eq!(
        home.gate,
        Some(
            "bash spira/inventory.sh && bash spira/testenv-batch.sh \"$SPIRA_GATE_BRANCH\""
                .to_string()
        )
    );
    assert_eq!(
        home.lanes,
        vec![Lane::Plan, Lane::Incident, Lane::Groom, Lane::Spike]
    );

    let service = doc.repo.get("service").expect("service repo row");
    assert_eq!(service.mode, LandMode::Pr);
    assert_eq!(service.format, Some("cargo fmt --all".to_string()));
    // The gate command's own `2|3)` case pattern must survive the pipe-delimited parse —
    // this is the row that plants a literal `|` inside the gate column on purpose.
    assert!(service
        .gate
        .as_deref()
        .unwrap()
        .contains("case \"$_b\" in 2|3) exit 75"));
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

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spira-config-test-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
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
[spira]\nfayths = [\"a\", \"b\"]\n\n\
[repo.alpha]\npath = \"/tmp/alpha\"\nmode = \"push\"\n\n\
[repo.beta]\npath = \"/tmp/beta\"\nmode = \"push\"\n";

const EXISTING_ONE_REPO_TWO_FAYTHS: &str = "\
[spira]\nfayths = [\"a\", \"b\"]\n\n\
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
    fs::write(&conf, "SPIRA_FAYTHS = a b\n").unwrap();
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
    let conf = write_tmp(&dir, "spira.conf", "SPIRA_FAYTHS = a b\n");
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
    let conf = write_tmp(&dir, "spira.conf", "SPIRA_FAYTHS = a\n");
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
    let conf = write_tmp(&dir, "spira.conf", "SPIRA_FAYTHS = a b\n");
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
    let conf = write_tmp(&dir, "spira.conf", "SPIRA_FAYTHS = a\n");
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
