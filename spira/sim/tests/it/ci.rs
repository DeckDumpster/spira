use spira_sim::ci::{expand, steps, Context};
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
