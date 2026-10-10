use spira_sim::ci::{cargo_shim, expand, run_job, steps, Context, Job, CARGO_FROM_ENV};
use std::path::Path;

const TOY: &str = "name: t
jobs:
  other:
    steps:
      - name: Not this
        run: echo no
  cut:
    runs-on: x
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - name: Say
        # a comment between keys
        env:
          WHO: ${{ secrets.GITHUB_TOKEN }}
          # not a variable
          PLAIN: 'two words'
        run: |
          echo \"sha=${{ github.sha }} who=$WHO plain=$PLAIN\"

          echo second >> \"$GITHUB_OUTPUT\"
      - name: Path
        run: echo /somewhere >> \"$GITHUB_PATH\"
      - name: Fails
        run: |
          echo \"path=$PATH image=${IMAGE:-no}\"
          exit 7
";

fn gate() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/gate.yml")).unwrap()
}

#[test]
fn a_jobs_steps_are_read_with_their_env_and_script_and_no_other_jobs() {
    let s = steps(TOY, "cut").unwrap();
    assert_eq!(s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["Checkout", "Say", "Path", "Fails"]);
    assert!(s[0].run.is_none());
    assert_eq!(s[1].env, vec![("WHO".to_string(), "${{ secrets.GITHUB_TOKEN }}".to_string()), ("PLAIN".to_string(), "two words".to_string())]);
    assert_eq!(s[1].run.as_deref(), Some("echo \"sha=${{ github.sha }} who=$WHO plain=$PLAIN\"\n\necho second >> \"$GITHUB_OUTPUT\"\n"));
    assert!(steps(TOY, "nope").is_err());
}

#[test]
fn the_shipped_cut_job_reads_as_its_named_script_steps() {
    let s = steps(&gate(), "cut").unwrap();
    let scripts: Vec<&str> = s.iter().filter(|s| s.run.is_some()).map(|s| s.name.as_str()).collect();
    assert!(scripts.contains(&"Assert gate check") && scripts.contains(&"Cut release tag"), "{scripts:?}");
    let cut = s.iter().find(|s| s.name == "Cut release tag").unwrap();
    assert!(cut.run.as_deref().unwrap().contains("release.sh cut"));
    assert!(cut.env.iter().any(|(k, _)| k == "GIT_COMMITTER_EMAIL"));
}

#[test]
fn only_the_expressions_a_runner_provides_are_expanded() {
    let cx = Context { sha: "abc", workspace: Path::new("/w"), temp: Path::new("/t") };
    assert_eq!(expand("${{ github.sha }}:${{github.workspace}}:${{ github.head_ref }}|", &cx).unwrap(), "abc:/w:|");
    assert!(expand("${{ matrix.os }}", &cx).unwrap_err().contains("matrix.os"));
    assert!(expand("${{ github.sha", &cx).is_err());
}

fn scratch(tag: &str) -> testkit::TempDir {
    testkit::TempDir::new(tag)
}

#[test]
fn a_job_runs_its_steps_in_a_clean_env_and_stops_at_the_first_failure() {
    let t = scratch("simci");
    let ws = t.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let all = steps(TOY, "cut").unwrap();
    let mut log = String::new();
    let job = Job { steps: &all, skip: &[], cx: Context { sha: "s1", workspace: &ws, temp: &t.join("scratch") }, scratch: &t.join("scratch"), gh_dir: &t.join("gh"), release_bin: &t.join("rel"), path: &[std::path::PathBuf::from("/opt/runner-image")], env: &[("IMAGE".to_string(), "yes".to_string())] };
    let code = run_job(&job, Path::new("/bin/true"), |l| log.push_str(l)).unwrap();
    assert_eq!(code, 7, "{log}");
    assert!(log.contains("sha=s1 who=sim-token plain=two words"), "{log}");
    assert!(log.contains("image=yes"), "{log}");
    assert!(log.contains("path=/somewhere:/opt/runner-image:"), "GITHUB_PATH carries to the next step, ahead of the image's: {log}");
    let skipped = ["Fails".to_string()];
    let job = Job { skip: &skipped, ..job };
    let mut log = String::new();
    assert_eq!(run_job(&job, Path::new("/bin/true"), |l| log.push_str(l)).unwrap(), 0, "{log}");
    assert!(log.contains("--- skipped: Fails"));
}

#[test]
fn the_cargo_shim_builds_only_spira_config_by_copying_the_worlds_release() {
    let t = scratch("simcargo");
    let rel = t.join("rel");
    std::fs::create_dir_all(&rel).unwrap();
    testkit::write_exe(rel.join("spira-config"), "#!/bin/sh\necho cfg\n");
    let ws = t.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let rel_s = rel.display().to_string();
    let env = |k: &str| (k == CARGO_FROM_ENV).then(|| rel_s.clone());
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    cargo_shim(&a(&["build", "--release", "--locked", "-p", "spira-config"]), &ws, &env).unwrap();
    assert!(ws.join("target/release/spira-config").is_file());
    for bad in [&["build", "-p", "other"][..], &["test"], &["build", "--workspace"], &["build"]] {
        assert!(cargo_shim(&a(bad), &ws, &env).is_err(), "{bad:?}");
    }
    assert!(cargo_shim(&a(&["build", "-p", "spira-config"]), &ws, &|_| None).is_err());
}
